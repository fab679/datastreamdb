//! Change-data-capture emitter (Go `worker/cdc.go`).
//!
//! Committed mutations become JSON events on a [`CdcSink`]: the file
//! sink appends `{ "key": "<ns>", "value": <event> }` lines (Go
//! `fileSink`), the channel sink feeds embedded consumers, and Kafka
//! stays deferred. Events buffer by start timestamp until their
//! commit decision lands, exactly like Go's pending map; drops flush
//! immediately.
//!
//! Go tails the Raft WAL on a ticker; the applier observes the same
//! committed proposals inline, so no log tailing (or `seenIndex`) is
//! needed. A failed sink send drops the batch with an error log,
//! where Go would retry on the next tick.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use datastreamdb_proto::pb::Mutations;

/// Default event topic (Go `defaultEventTopic`).
pub const DEFAULT_EVENT_TOPIC: &str = "datastreamdb-cdc";
/// File-sink name inside its directory (Go `defaultSinkFileName`).
pub const SINK_FILE_NAME: &str = "sink.log";

/// CDC failures.
#[derive(Debug, thiserror::Error)]
pub enum CdcError {
    /// Sink setup failure.
    #[error("cdc sink failed: {0}")]
    Sink(String),
    /// Config parse failure.
    #[error("bad cdc flag: {0}")]
    Config(String),
    /// Send failure (batch dropped, like Go's logged send errors).
    #[error("cdc send failed: {0}")]
    Send(String),
}

/// One sink-bound message (Go `SinkMessage`).
#[derive(Debug, Clone)]
pub struct SinkMessage {
    /// Topic (always [`DEFAULT_EVENT_TOPIC`]).
    pub topic: String,
    /// Namespace key (8-byte big-endian).
    pub key: Vec<u8>,
    /// Marshalled [`CdcEvent`] JSON.
    pub value: Vec<u8>,
}

/// Event batch sink (Go `Sink`, minus `Close`).
pub trait CdcSink: Send + Sync + std::fmt::Debug {
    /// Sends one batch.
    ///
    /// # Errors
    ///
    /// Returns [`CdcError::Send`] when the batch cannot be delivered.
    fn send(&self, batch: &[SinkMessage]) -> Result<(), CdcError>;
}

/// Appends `{ "key": "<ns>", "value": <event> }` lines to
/// `<dir>/sink.log` (Go `fileSink`; rotation deferred).
#[derive(Debug)]
pub struct FileSink {
    /// Destination file (appended under a lock).
    file: Mutex<std::fs::File>,
}

impl FileSink {
    /// Creates the directory and opens `sink.log` for appending.
    ///
    /// # Errors
    ///
    /// Returns [`CdcError::Sink`] when the directory or file fails.
    pub fn open(dir: &std::path::Path) -> Result<Self, CdcError> {
        std::fs::create_dir_all(dir).map_err(|err| CdcError::Sink(err.to_string()))?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(SINK_FILE_NAME))
            .map_err(|err| CdcError::Sink(err.to_string()))?;
        Ok(Self {
            file: Mutex::new(file),
        })
    }
}

impl CdcSink for FileSink {
    fn send(&self, batch: &[SinkMessage]) -> Result<(), CdcError> {
        use std::io::Write as _;
        let mut file = self.file.lock().expect("cdc sink lock");
        for message in batch {
            let mut key = [0u8; 8];
            if message.key.len() == 8 {
                key.copy_from_slice(&message.key);
            }
            writeln!(
                file,
                "{{ \"key\": \"{}\", \"value\": {} }}",
                u64::from_be_bytes(key),
                String::from_utf8_lossy(&message.value)
            )
            .map_err(|err| CdcError::Send(err.to_string()))?;
        }
        file.flush().map_err(|err| CdcError::Send(err.to_string()))
    }
}

/// Forwards batches to an in-process channel (embedded consumers).
#[derive(Debug)]
pub struct ChannelSink {
    /// Batch receiver feed.
    tx: tokio::sync::mpsc::UnboundedSender<Vec<SinkMessage>>,
}

impl ChannelSink {
    /// Creates the sink half; the caller drains the receiver.
    #[must_use]
    pub fn channel() -> (Self, tokio::sync::mpsc::UnboundedReceiver<Vec<SinkMessage>>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (Self { tx }, rx)
    }
}

impl CdcSink for ChannelSink {
    fn send(&self, batch: &[SinkMessage]) -> Result<(), CdcError> {
        self.tx
            .send(batch.to_vec())
            .map_err(|err| CdcError::Send(err.to_string()))
    }
}

/// CDC superflag (`file=...; kafka=...`, like Go's `ChangeDataConf`).
#[derive(Debug, Clone, Default)]
pub struct CdcConfig {
    /// File-sink directory (empty disables the file sink).
    pub file: String,
    /// Kafka brokers (reserved; the Kafka sink is deferred).
    pub kafka: String,
}

/// Parses `k=v;...` pairs (audit-flag style).
///
/// # Errors
///
/// Returns [`CdcError::Config`] on malformed pairs or when Kafka is
/// requested (deferred).
pub fn parse_cdc_flag(flag: &str) -> Result<CdcConfig, CdcError> {
    let mut config = CdcConfig::default();
    for part in flag.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let Some((key, value)) = part.split_once('=') else {
            return Err(CdcError::Config(format!("malformed pair {part:?}")));
        };
        match key.trim() {
            "file" => value.trim().clone_into(&mut config.file),
            "kafka" => value.trim().clone_into(&mut config.kafka),
            other => return Err(CdcError::Config(format!("unknown key {other:?}"))),
        }
    }
    if !config.kafka.is_empty() {
        return Err(CdcError::Config(
            "kafka sink is deferred; use file= or the channel sink".to_owned(),
        ));
    }
    Ok(config)
}

/// Builds the configured sink (`None` when CDC is disabled).
///
/// # Errors
///
/// Returns [`CdcError`] when the file sink cannot open.
pub fn sink_for_config(config: &CdcConfig) -> Result<Option<FileSink>, CdcError> {
    if config.file.is_empty() {
        return Ok(None);
    }
    FileSink::open(std::path::Path::new(&config.file)).map(Some)
}

/// Event envelope (Go `CDCEvent` JSON shape).
#[derive(Debug, Clone, serde::Serialize)]
pub struct CdcEvent {
    /// Commit metadata (only `commit_ts` serializes, like Go).
    pub meta: EventMeta,
    /// `"mutation"` or `"drop"`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Payload.
    pub event: CdcPayload,
}

/// Commit metadata (Go `EventMeta`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct EventMeta {
    /// Commit timestamp (0 until the decision lands).
    pub commit_ts: u64,
    /// Namespace key (8-byte big-endian; skipped in JSON like Go).
    #[serde(skip)]
    pub namespace: Vec<u8>,
}

/// Event payload (Go `MutationEvent`/`DropEvent`).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(untagged)]
pub enum CdcPayload {
    /// Edge mutation.
    Mutation(MutationEvent),
    /// Drop operation.
    Drop(DropEvent),
}

/// Edge mutation (Go `MutationEvent`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct MutationEvent {
    /// Lowercased edge op (`set`, `del`).
    pub operation: String,
    /// Subject UID.
    pub uid: u64,
    /// Bare predicate.
    pub attr: String,
    /// Converted value (`****` for passwords).
    pub value: serde_json::Value,
    /// Value type name.
    pub value_type: String,
}

/// Drop operation (Go `DropEvent`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct DropEvent {
    /// Lowercased drop op (`all`, `data`, `predicate`, ...).
    pub operation: String,
    /// Drop target type (types only).
    #[serde(rename = "type")]
    pub drop_type: String,
    /// Dropped predicate (attr drops only).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pred: String,
}

/// Whether the predicate is reserved (`datastreamdb.*`, like Go).
fn is_reserved(attr: &str) -> bool {
    datastreamdb_x::keys::parse_attr(attr)
        .to_ascii_lowercase()
        .starts_with("datastreamdb.")
}

/// Splits `nshex-attr` into namespace bytes and the bare name.
fn split_ns(attr: &str) -> (Vec<u8>, String) {
    attr.split_once('-')
        .and_then(|(ns, rest)| {
            u64::from_str_radix(ns, 16)
                .ok()
                .map(|ns| (ns.to_be_bytes().to_vec(), rest.to_owned()))
        })
        .unwrap_or_else(|| (0u64.to_be_bytes().to_vec(), attr.to_owned()))
}

/// Converts a stored value to its event JSON form.
fn event_value(tid: datastreamdb_types::TypeId, bytes: &[u8]) -> serde_json::Value {
    use base64::Engine as _;
    let from = datastreamdb_types::Val {
        tid: datastreamdb_types::TypeId::Binary,
        value: datastreamdb_types::Value::Binary(bytes.to_vec()),
    };
    let Ok(converted) = datastreamdb_types::convert(&from, tid) else {
        return serde_json::Value::Null;
    };
    match converted.value {
        datastreamdb_types::Value::String(text) => serde_json::Value::String(text),
        datastreamdb_types::Value::Int(v) => serde_json::json!(v),
        datastreamdb_types::Value::Float(v) => serde_json::json!(v),
        datastreamdb_types::Value::Bool(v) => serde_json::Value::Bool(v),
        datastreamdb_types::Value::Binary(raw) => {
            serde_json::Value::String(base64::engine::general_purpose::STANDARD.encode(raw))
        }
        datastreamdb_types::Value::DateTime(ts) => serde_json::Value::String(ts.to_rfc3339()),
        datastreamdb_types::Value::BigFloat(v) => serde_json::Value::String(v.to_string()),
        datastreamdb_types::Value::Geo(geo) => {
            serde_json::to_value(geo).unwrap_or(serde_json::Value::Null)
        }
        datastreamdb_types::Value::Password(_) => serde_json::Value::String("****".to_owned()),
        datastreamdb_types::Value::Null
        | datastreamdb_types::Value::Uid(_)
        | datastreamdb_types::Value::VFloat(_) => serde_json::Value::Null,
    }
}

/// Converts staged mutations to CDC events (Go `toCDCEvent`).
/// Schema/type updates are skipped, like Go's TODO.
#[must_use]
pub fn to_cdc_events(mutations: &Mutations) -> Vec<CdcEvent> {
    use datastreamdb_proto::pb::mutations::DropOp;
    if !mutations.schema.is_empty() || !mutations.types.is_empty() {
        return Vec::new();
    }
    if mutations.drop_op != DropOp::None as i32 {
        let (namespace, drop_type) = match DropOp::try_from(mutations.drop_op) {
            Ok(DropOp::All) => (0u64.to_be_bytes().to_vec(), String::new()),
            Ok(DropOp::Data) => {
                let ns = mutations.drop_value.parse::<u64>().unwrap_or(0);
                (ns.to_be_bytes().to_vec(), String::new())
            }
            Ok(DropOp::Type) => {
                let (ns, name) = split_ns(&mutations.drop_value);
                (ns, name)
            }
            _ => return Vec::new(),
        };
        let operation = match DropOp::try_from(mutations.drop_op) {
            Ok(DropOp::All) => "all",
            Ok(DropOp::Data) => "data",
            Ok(DropOp::Type) => "type",
            Ok(DropOp::AllInNs) => "all_in_ns",
            _ => return Vec::new(),
        }
        .to_owned();
        // `ALL_IN_NS` reuses the data namespace slot.
        let namespace = if mutations.drop_op == DropOp::AllInNs as i32 {
            let ns = mutations.drop_value.parse::<u64>().unwrap_or(0);
            ns.to_be_bytes().to_vec()
        } else {
            namespace
        };
        return vec![CdcEvent {
            meta: EventMeta {
                commit_ts: 0,
                namespace,
            },
            kind: "drop".to_owned(),
            event: CdcPayload::Drop(DropEvent {
                operation,
                drop_type,
                pred: String::new(),
            }),
        }];
    }
    let mut events = Vec::new();
    for edge in &mutations.edges {
        if is_reserved(&edge.attr) {
            continue;
        }
        let (ns, attr) = split_ns(&edge.attr);
        if edge.entity == 0 && edge.value == b"*" {
            return vec![CdcEvent {
                meta: EventMeta {
                    commit_ts: 0,
                    namespace: ns,
                },
                kind: "drop".to_owned(),
                event: CdcPayload::Drop(DropEvent {
                    operation: "predicate".to_owned(),
                    drop_type: String::new(),
                    pred: attr,
                }),
            }];
        }
        let tid = datastreamdb_types::TypeId::try_from(edge.value_type)
            .unwrap_or(datastreamdb_types::TypeId::Default);
        let value = if tid == datastreamdb_types::TypeId::Uid {
            serde_json::json!(edge.value_id)
        } else if tid == datastreamdb_types::TypeId::Password {
            serde_json::Value::String("****".to_owned())
        } else {
            event_value(tid, &edge.value)
        };
        let operation = match datastreamdb_proto::pb::directed_edge::Op::try_from(edge.op) {
            Ok(datastreamdb_proto::pb::directed_edge::Op::Del) => "del",
            _ => "set",
        }
        .to_owned();
        events.push(CdcEvent {
            meta: EventMeta {
                commit_ts: 0,
                namespace: ns,
            },
            kind: "mutation".to_owned(),
            event: CdcPayload::Mutation(MutationEvent {
                operation,
                uid: edge.entity,
                attr,
                value,
                value_type: tid.as_str().to_owned(),
            }),
        });
    }
    events
}

/// Change-data-capture emitter: buffers staged-mutation events by
/// start timestamp and flushes them on commit decisions.
#[derive(Debug)]
pub struct Cdc {
    /// Event sink.
    sink: Arc<dyn CdcSink>,
    /// Staged events by start timestamp (Go `pendingTxnEvents`).
    pending: Mutex<HashMap<u64, Vec<CdcEvent>>>,
    /// Highest commit timestamp flushed (Go `sentTs`).
    sent_ts: AtomicU64,
}

impl Cdc {
    /// Creates an emitter over `sink`.
    #[must_use]
    pub fn new(sink: Arc<dyn CdcSink>) -> Self {
        Self {
            sink,
            pending: Mutex::new(HashMap::new()),
            sent_ts: AtomicU64::new(0),
        }
    }

    /// Highest commit timestamp flushed so far.
    #[must_use]
    pub fn sent_ts(&self) -> u64 {
        self.sent_ts.load(Ordering::SeqCst)
    }

    /// Adopts a replicated flush mark (Go `updateCDCState`).
    pub fn set_sent_ts(&self, ts: u64) {
        self.sent_ts.fetch_max(ts, Ordering::SeqCst);
    }

    /// Stages mutation events; drops flush immediately (Go
    /// `handleEntry` mutation arm).
    ///
    /// # Panics
    ///
    /// Panics when the pending lock is poisoned.
    pub fn on_mutations(&self, mutations: &Mutations) {
        use datastreamdb_proto::pb::mutations::DropOp;
        let events = to_cdc_events(mutations);
        if events.is_empty() {
            return;
        }
        if mutations.drop_op != DropOp::None as i32 {
            if mutations.drop_op == DropOp::All as i32 {
                self.pending.lock().expect("cdc lock").clear();
            } else if mutations.drop_op == DropOp::Data as i32 {
                if let Ok(ns) = mutations.drop_value.parse::<u64>() {
                    self.pending.lock().expect("cdc lock").retain(|_, batch| {
                        batch
                            .iter()
                            .any(|event| event.meta.namespace != ns.to_be_bytes())
                    });
                }
            }
            self.send(events, mutations.start_ts);
            return;
        }
        if mutations.edges.len() == 1
            && mutations.edges[0].entity == 0
            && mutations.edges[0].value == b"*"
        {
            let attr = datastreamdb_x::keys::parse_attr(&mutations.edges[0].attr).to_owned();
            if !self.has_pending(&attr) {
                self.send(events, mutations.start_ts);
            }
            return;
        }
        self.pending
            .lock()
            .expect("cdc lock")
            .insert(mutations.start_ts, events);
    }

    /// Flushes committed transactions and drops aborted ones (Go
    /// `handleEntry` delta arm).
    ///
    /// # Panics
    ///
    /// Panics when the pending lock is poisoned.
    pub fn on_delta(&self, txns: &[(u64, u64)]) {
        for (start_ts, commit_ts) in txns {
            if *commit_ts > 0 && self.sent_ts() < *commit_ts {
                let events = self
                    .pending
                    .lock()
                    .expect("cdc lock")
                    .get(start_ts)
                    .cloned()
                    .unwrap_or_default();
                self.send(events, *commit_ts);
            }
            self.pending.lock().expect("cdc lock").remove(start_ts);
        }
    }

    /// Whether any pending batch touches `attr` (bare name).
    fn has_pending(&self, attr: &str) -> bool {
        self.pending
            .lock()
            .expect("cdc lock")
            .values()
            .any(|batch| {
                batch.iter().any(|event| match &event.event {
                    CdcPayload::Mutation(mutation) => mutation.attr == attr,
                    CdcPayload::Drop(_) => false,
                })
            })
    }

    /// Marshals and sinks one batch stamped with `commit_ts`.
    fn send(&self, mut events: Vec<CdcEvent>, commit_ts: u64) {
        let mut batch = Vec::with_capacity(events.len());
        for event in &mut events {
            event.meta.commit_ts = commit_ts;
            let Ok(value) = serde_json::to_vec(event) else {
                continue;
            };
            batch.push(SinkMessage {
                topic: DEFAULT_EVENT_TOPIC.to_owned(),
                key: event.meta.namespace.clone(),
                value,
            });
        }
        if batch.is_empty() {
            return;
        }
        if let Err(err) = self.sink.send(&batch) {
            tracing::error!("cdc send failed: {err}");
            return;
        }
        self.sent_ts.fetch_max(commit_ts, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mutations() -> Mutations {
        Mutations {
            start_ts: 7,
            edges: vec![
                datastreamdb_proto::DirectedEdge {
                    entity: 2,
                    attr: "0-name".to_owned(),
                    value: b"Alice".to_vec(),
                    value_type: datastreamdb_proto::ValType::String as i32,
                    ..datastreamdb_proto::DirectedEdge::default()
                },
                datastreamdb_proto::DirectedEdge {
                    entity: 2,
                    attr: "0-age".to_owned(),
                    value: 30i64.to_le_bytes().to_vec(),
                    value_type: datastreamdb_proto::ValType::Int as i32,
                    ..datastreamdb_proto::DirectedEdge::default()
                },
            ],
            ..Mutations::default()
        }
    }

    #[test]
    fn events_match_go_json_shape() {
        let events = to_cdc_events(&mutations());
        assert_eq!(events.len(), 2);
        let text = serde_json::to_string(&events[0]).expect("json");
        let value: serde_json::Value = serde_json::from_str(&text).expect("parse");
        assert_eq!(value["type"], "mutation");
        assert_eq!(value["event"]["operation"], "set");
        assert_eq!(value["event"]["uid"], 2);
        assert_eq!(value["event"]["attr"], "name");
        assert_eq!(value["event"]["value"], "Alice");
        assert_eq!(value["event"]["value_type"], "string");
        assert_eq!(value["meta"]["commit_ts"], 0);
        assert!(value["meta"].get("namespace").is_none());
        let text = serde_json::to_string(&events[1]).expect("json");
        let value: serde_json::Value = serde_json::from_str(&text).expect("parse");
        assert_eq!(value["event"]["value"], 30);
        assert_eq!(value["event"]["value_type"], "int");
    }

    #[test]
    fn drops_and_masks_match_go() {
        use datastreamdb_proto::pb::mutations::DropOp;
        let drop = Mutations {
            start_ts: 9,
            drop_op: DropOp::All as i32,
            ..Mutations::default()
        };
        let events = to_cdc_events(&drop);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "drop");
        let text = serde_json::to_string(&events[0]).expect("json");
        assert!(text.contains("\"operation\":\"all\""), "{text}");

        let secret = Mutations {
            start_ts: 10,
            edges: vec![datastreamdb_proto::DirectedEdge {
                entity: 2,
                attr: "0-pass".to_owned(),
                value: b"hash".to_vec(),
                value_type: datastreamdb_proto::ValType::Password as i32,
                ..datastreamdb_proto::DirectedEdge::default()
            }],
            ..Mutations::default()
        };
        let events = to_cdc_events(&secret);
        let text = serde_json::to_string(&events[0]).expect("json");
        assert!(text.contains("\"value\":\"****\""), "{text}");

        let reserved = Mutations {
            start_ts: 11,
            edges: vec![datastreamdb_proto::DirectedEdge {
                entity: 2,
                attr: "0-datastreamdb.type".to_owned(),
                value: b"x".to_vec(),
                ..datastreamdb_proto::DirectedEdge::default()
            }],
            ..Mutations::default()
        };
        assert!(to_cdc_events(&reserved).is_empty());
    }

    #[test]
    fn pending_flushes_on_commit_not_abort() {
        let (sink, mut rx) = ChannelSink::channel();
        let cdc = Cdc::new(Arc::new(sink));
        cdc.on_mutations(&mutations());
        cdc.on_mutations(&Mutations {
            start_ts: 8,
            ..mutations()
        });
        // Abort drops without sending.
        cdc.on_delta(&[(8, 0)]);
        assert!(rx.try_recv().is_err());
        // Commit stamps and sends.
        cdc.on_delta(&[(7, 42)]);
        let batch = rx.try_recv().expect("batch");
        assert_eq!(batch.len(), 2);
        assert_eq!(batch[0].topic, DEFAULT_EVENT_TOPIC);
        assert_eq!(batch[0].key, 0u64.to_be_bytes());
        let value: serde_json::Value = serde_json::from_slice(&batch[0].value).expect("json");
        assert_eq!(value["meta"]["commit_ts"], 42);
        assert_eq!(cdc.sent_ts(), 42);
        // Replays below the flush mark stay silent.
        cdc.on_delta(&[(7, 42)]);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn drops_flush_immediately_and_clear_pending() {
        use datastreamdb_proto::pb::mutations::DropOp;
        let (sink, mut rx) = ChannelSink::channel();
        let cdc = Cdc::new(Arc::new(sink));
        cdc.on_mutations(&mutations());
        cdc.on_mutations(&Mutations {
            start_ts: 50,
            drop_op: DropOp::All as i32,
            ..Mutations::default()
        });
        let batch = rx.try_recv().expect("drop batch");
        assert_eq!(batch.len(), 1);
        // The pending mutation was cleared; its commit finds nothing.
        cdc.on_delta(&[(7, 60)]);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn file_sink_matches_go_line_format() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sink = FileSink::open(dir.path()).expect("open");
        let (channel, mut rx) = ChannelSink::channel();
        let cdc = Cdc::new(Arc::new(channel));
        cdc.on_mutations(&mutations());
        cdc.on_delta(&[(7, 42)]);
        let batch = rx.try_recv().expect("batch");
        sink.send(&batch).expect("send");
        let text = std::fs::read_to_string(dir.path().join(SINK_FILE_NAME)).expect("read");
        assert!(text.contains("\"key\": \"0\""), "{text}");
        assert!(text.contains("\"commit_ts\":42"), "{text}");
    }

    #[test]
    fn flag_parses_file_and_rejects_kafka() {
        let config = parse_cdc_flag("file=/tmp/cdc; kafka=").expect("parse");
        assert_eq!(config.file, "/tmp/cdc");
        assert!(parse_cdc_flag("kafka=b:9092").is_err());
        assert!(parse_cdc_flag("bogus").is_err());
        assert!(parse_cdc_flag("nope=1").is_err());
    }
}
