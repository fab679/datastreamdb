//! Binary backup writer (Go `worker/backup.go`, `BackupProcessor`).
//!
//! [`write_backup`] streams one group's posting lists as framed
//! `KVList` records (`u64` little-endian length prefix plus protobuf
//! bytes) through Snappy framing and optional AES-CTR encryption,
//! exactly like Go's `WriteBackup`. Keys are stored as marshalled
//! [`BackupKey`] protos and values as rolled-up
//! [`BackupPostingList`](datastreamdb_proto::BackupPostingList) protos.
//! Schema and type entries are appended last with version 1.
//!
//! Go compresses with `s2` (whose reader also accepts Snappy framing)
//! and records `"snappy"` in the manifest; we emit Snappy framing via
//! the `snap` crate under the same manifest value, so Go's restore
//! reads our backups and ours reads Go's.

use std::collections::HashSet;
use std::io::Write;

use datastreamdb_proto::badgerpb4::{Kv, KvList};
use datastreamdb_proto::pb::BackupKey;
use prost::Message as _;

use crate::manifest::DropOperation;

/// Maximum KVs per framed `KVList` batch.
const BATCH_KVS: usize = 256;

/// Backup writer failures (messages mirror Go's).
#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    /// Underlying storage failure.
    #[error("backup storage failed: {0}")]
    Storage(String),
    /// Output write failure.
    #[error("backup write failed: {0}")]
    Io(String),
    /// Protobuf encode failure.
    #[error("backup encode failed: {0}")]
    Encode(String),
    /// Encryption setup failure.
    #[error("backup encryption failed: {0}")]
    Crypto(String),
    /// Key rebuild failure.
    #[error("backup key failed: {0}")]
    Key(String),
}

/// Backup request (Go `pb.BackupRequest`, embedded subset).
#[derive(Debug, Clone, Default)]
pub struct BackupRequest {
    /// Timestamp the backup reads at.
    pub read_ts: u64,
    /// Incremental floor: lists older than this are skipped (0 = full).
    pub since_ts: u64,
    /// Group being backed up (manifest bookkeeping).
    pub group_id: u32,
    /// Namespaced predicates to include (empty = all).
    pub predicates: Vec<String>,
    /// Encryption key (`None` = plaintext, like Go's nil key).
    pub encryption_key: Option<datastreamdb_enc::keys::DataKey>,
}

/// Backup writer counters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackupStats {
    /// KVs written (data plus schema/type entries).
    pub kvs: u64,
    /// Framed `KVList` batches written.
    pub batches: u64,
    /// Highest list version observed.
    pub max_version: u64,
    /// Drop operations recorded from `datastreamdb.drop.op` lists.
    pub drop_operations: Vec<DropOperation>,
}

/// Converts a parsed store key to its backup form (Go `ToBackupKey`).
fn to_backup_key(key: &datastreamdb_x::keycodec::ParsedKey) -> Result<BackupKey, BackupError> {
    use datastreamdb_proto::pb::backup_key::KeyType;
    let (namespace, attr) = key
        .attr
        .split_once('-')
        .and_then(|(ns, rest)| u64::from_str_radix(ns, 16).ok().map(|ns| (ns, rest)))
        .ok_or_else(|| BackupError::Key(format!("unprefixed attr {}", key.attr)))?;
    let key_type = if key.is_data() {
        KeyType::Data
    } else if key.is_index() {
        KeyType::Index
    } else if key.is_reverse() {
        KeyType::Reverse
    } else if key.is_count() {
        KeyType::Count
    } else if key.is_count_rev() {
        KeyType::CountRev
    } else if key.is_schema() {
        KeyType::Schema
    } else if key.is_type() {
        KeyType::Type
    } else {
        KeyType::Unknown
    };
    Ok(BackupKey {
        r#type: key_type as i32,
        attr: attr.to_owned(),
        uid: key.uid,
        start_uid: key.start_uid,
        term: key.term.clone(),
        count: key.count,
        namespace,
    })
}

/// Parses a `datastreamdb.drop.op` value (Go `checkAndGetDropOp`).
fn parse_drop_op(value: &[u8]) -> Result<Option<DropOperation>, BackupError> {
    let text = String::from_utf8_lossy(value);
    let Some((kind, rest)) = text.split_once(';') else {
        return Err(BackupError::Encode(format!(
            "unexpected value for datastreamdb.drop.op: {text}"
        )));
    };
    // Go `pb.DropOperation_DropOp`: ALL=0, DATA=1, ATTR=2, NS=3.
    let (drop_op, drop_value) = match kind {
        "DROP_ALL" => (0, String::new()),
        "DROP_DATA" => (1, rest.to_owned()),
        "DROP_ATTR" => (2, rest.to_owned()),
        "DROP_NS" => (3, rest.to_owned()),
        _ => {
            return Err(BackupError::Encode(format!(
                "unexpected value for datastreamdb.drop.op: {text}"
            )));
        }
    };
    Ok(Some(DropOperation {
        drop_op,
        drop_value,
    }))
}

/// Length-delimited `KVList` framing (Go `writeKVList`).
fn write_kv_list(list: &KvList, out: &mut dyn Write) -> Result<(), BackupError> {
    let bytes = list.encode_to_vec();
    let len = bytes.len() as u64;
    out.write_all(&len.to_le_bytes())
        .map_err(|err| BackupError::Io(err.to_string()))?;
    out.write_all(&bytes)
        .map_err(|err| BackupError::Io(err.to_string()))
}

/// Flushes the pending batch when full.
fn maybe_flush(
    pending: &mut Vec<Kv>,
    out: &mut dyn Write,
    stats: &mut BackupStats,
) -> Result<(), BackupError> {
    if pending.len() < BATCH_KVS {
        return Ok(());
    }
    let list = KvList {
        kv: std::mem::take(pending),
    };
    write_kv_list(&list, out)?;
    stats.batches += 1;
    Ok(())
}

/// Builds the backup KV for one data key, or `None` when the key is
/// skipped (predicate filter, incremental floor, or nothing visible).
/// Drop-op lists additionally yield their manifest record.
fn data_kv_for_key(
    engine: &datastreamdb_posting::PostingEngine,
    request: &BackupRequest,
    filter: Option<&HashSet<&str>>,
    key_bytes: &[u8],
    stats: &mut BackupStats,
) -> Result<Option<Kv>, BackupError> {
    let Ok(key) = datastreamdb_x::keycodec::parse(key_bytes) else {
        tracing::error!("skipping unparseable key during backup");
        return Ok(None);
    };
    // Split parts roll into the main list; schema/type keys are
    // appended separately below.
    if key.has_start_uid || key.is_schema() || key.is_type() {
        return Ok(None);
    }
    if let Some(allowed) = filter {
        if !allowed.contains(key.attr.as_str()) {
            return Ok(None);
        }
    }
    // Incremental floor: skip lists with nothing at/after `since_ts`.
    let chain_max = datastreamdb_posting::store::read_chain(engine.store().as_ref(), key_bytes)
        .map_err(|err| BackupError::Storage(err.to_string()))?
        .map_or(0, |chain| chain.max_ts());
    if chain_max < request.since_ts {
        return Ok(None);
    }
    let list = engine
        .get_list(key_bytes, request.read_ts)
        .map_err(|err| BackupError::Storage(err.to_string()))?;
    let rolled = list
        .rollup(request.read_ts, false)
        .map_err(|err| BackupError::Storage(err.to_string()))?;
    let Some(rolled) = rolled else {
        return Ok(None);
    };
    let empty = rolled.plist.postings.is_empty() && rolled.plist.pack.is_none();
    let uid_bytes = rolled
        .plist
        .pack
        .as_ref()
        .map(datastreamdb_codec::pack::decode_to_uvarints)
        .unwrap_or_default();
    let backup_list = datastreamdb_proto::BackupPostingList {
        uids: Vec::new(),
        postings: rolled.plist.postings,
        commit_ts: rolled.plist.commit_ts,
        splits: Vec::new(),
        uid_bytes,
    };
    stats.max_version = stats.max_version.max(rolled.new_min_ts);
    if stats.max_version > request.read_ts {
        tracing::error!(
            "max timestamp seen during backup ({}) exceeds read_ts ({})",
            stats.max_version,
            request.read_ts
        );
    }
    let backup_key = to_backup_key(&key)?;
    // `datastreamdb.drop.op` lists double as manifest records.
    if backup_key.attr.as_str() == "datastreamdb.drop.op" {
        match list.all_values(request.read_ts) {
            Ok(values) if values.len() == 1 => {
                if let datastreamdb_types::Value::Binary(bytes) = &values[0].value {
                    if let Some(op) = parse_drop_op(bytes)? {
                        stats.drop_operations.push(op);
                    }
                }
            }
            Ok(_) => {}
            Err(err) => {
                tracing::error!("cannot read datastreamdb.drop.op value: {err}");
            }
        }
    }
    Ok(Some(Kv {
        key: backup_key.encode_to_vec(),
        value: backup_list.encode_to_vec(),
        user_meta: vec![if empty {
            datastreamdb_posting::list::BIT_EMPTY_POSTING
        } else {
            datastreamdb_posting::list::BIT_COMPLETE_POSTING
        }],
        version: rolled.new_min_ts,
    }))
}

/// Builds the schema/type KVs (Go `writePrefix`).
fn schema_kvs(
    schema: &datastreamdb_schema::SchemaState,
    filter: Option<&HashSet<&str>>,
) -> Result<Vec<Kv>, BackupError> {
    let mut kvs = Vec::new();
    let mut preds = schema.predicates();
    preds.sort();
    for pred in preds {
        if let Some(allowed) = filter {
            if !allowed.contains(pred.as_str()) {
                continue;
            }
        }
        let Some(update) = schema.get(&pred, false) else {
            continue;
        };
        let store_key = datastreamdb_x::keycodec::schema_key(&pred);
        let parsed = datastreamdb_x::keycodec::parse(&store_key)
            .map_err(|err| BackupError::Key(err.to_string()))?;
        kvs.push(Kv {
            key: to_backup_key(&parsed)?.encode_to_vec(),
            value: update.encode_to_vec(),
            user_meta: Vec::new(),
            version: 1,
        });
    }
    let mut types = schema.types();
    types.sort();
    for name in types {
        let Some(update) = schema.get_type(&name) else {
            continue;
        };
        let store_key = datastreamdb_x::keycodec::type_key(&name);
        let parsed = datastreamdb_x::keycodec::parse(&store_key)
            .map_err(|err| BackupError::Key(err.to_string()))?;
        kvs.push(Kv {
            key: to_backup_key(&parsed)?.encode_to_vec(),
            value: update.encode_to_vec(),
            user_meta: Vec::new(),
            version: 1,
        });
    }
    Ok(kvs)
}

/// Writes one group's backup stream (Go `BackupProcessor.WriteBackup`).
///
/// `schema` supplies the schema/type entries (Go reads them from Badger;
/// our schema state lives in memory). Drop operations encountered in
/// `datastreamdb.drop.op` lists are collected into
/// [`BackupStats::drop_operations`] for the manifest.
///
/// # Errors
///
/// Returns [`BackupError`] on storage, encode, crypto, or write failure.
pub fn write_backup(
    engine: &datastreamdb_posting::PostingEngine,
    schema: &datastreamdb_schema::SchemaState,
    request: &BackupRequest,
    out: &mut dyn Write,
) -> Result<BackupStats, BackupError> {
    let mut stats = BackupStats::default();
    let filter: Option<HashSet<&str>> = if request.predicates.is_empty() {
        None
    } else {
        Some(request.predicates.iter().map(String::as_str).collect())
    };

    let encrypted =
        datastreamdb_enc::stream::encrypting_writer(request.encryption_key.as_ref(), out)
            .map_err(|err| BackupError::Crypto(err.to_string()))?;
    let mut framed = snap::write::FrameEncoder::new(encrypted);
    let mut pending: Vec<Kv> = Vec::new();

    let pairs = engine
        .store()
        .scan_prefix(datastreamdb_posting::store::PARTITION, b"")
        .map_err(|err| BackupError::Storage(err.to_string()))?;
    for (key_bytes, _) in &pairs {
        let Some(kv) = data_kv_for_key(engine, request, filter.as_ref(), key_bytes, &mut stats)?
        else {
            continue;
        };
        pending.push(kv);
        stats.kvs += 1;
        maybe_flush(&mut pending, &mut framed, &mut stats)?;
    }

    // Schema and type entries always ride the backup (Go `writePrefix`).
    for kv in schema_kvs(schema, filter.as_ref())? {
        pending.push(kv);
        stats.kvs += 1;
        maybe_flush(&mut pending, &mut framed, &mut stats)?;
    }

    if !pending.is_empty() {
        let list = KvList {
            kv: std::mem::take(&mut pending),
        };
        write_kv_list(&list, &mut framed)?;
        stats.batches += 1;
    }
    let mut encrypted = framed
        .into_inner()
        .map_err(|err| BackupError::Io(err.to_string()))?;
    encrypted
        .flush()
        .map_err(|err| BackupError::Io(err.to_string()))?;
    Ok(stats)
}

/// Manifest format version (Go `x.ManifestVersion`).
pub const MANIFEST_VERSION: i64 = 2105;

/// Builds the manifest record for a finished backup (Go `CompleteBackup`
/// payload): `kind` is `"full"` or `"incremental"`.
#[must_use]
pub fn manifest_for_backup(
    request: &BackupRequest,
    stats: &BackupStats,
    kind: &str,
    backup_id: &str,
    backup_num: u64,
    path: &str,
    groups: impl IntoIterator<Item = (u32, Vec<String>)>,
) -> crate::manifest::Manifest {
    crate::manifest::Manifest {
        base: crate::manifest::ManifestBase {
            backup_type: kind.to_owned(),
            since_ts: request.since_ts,
            read_ts: request.read_ts,
            backup_id: backup_id.to_owned(),
            backup_num,
            version: MANIFEST_VERSION,
            path: path.to_owned(),
            encrypted: request.encryption_key.is_some(),
            compression: "snappy".to_owned(),
        },
        groups: groups.into_iter().collect(),
        drop_operations: stats.drop_operations.clone(),
    }
}
