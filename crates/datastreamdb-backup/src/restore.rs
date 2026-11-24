//! Backup restore: decode backup streams into a live engine.
//!
//! Go restores through the bulk loader's map/reduce over Badger; the
//! embedded single-group equivalent applies each `KV` directly:
//! [`restore_stream`] rebuilds store keys (Go `FromBackupKey`), installs
//! rolled-up lists as versioned bases, and reinstalls schema/type
//! entries. [`apply_drop_operation`] replays manifest drop records.
//!
//! Compression follows Go's `WithCompression`: `"snappy"` selects the
//! Snappy/S2 framed reader (Go writes `s2`, which we also accept via
//! the `snap` decoder), while `"gzip"` and `""` select gzip.

use std::collections::HashSet;
use std::io::Read;

use datastreamdb_proto::badgerpb4::KvList;
use datastreamdb_proto::pb::BackupKey;
use prost::Message as _;

use crate::manifest::DropOperation;

/// Restore failures (messages mirror Go's).
#[derive(Debug, thiserror::Error)]
pub enum RestoreError {
    /// Underlying storage failure.
    #[error("restore storage failed: {0}")]
    Storage(String),
    /// Input read failure.
    #[error("restore read failed: {0}")]
    Io(String),
    /// Protobuf decode failure.
    #[error("restore decode failed: {0}")]
    Decode(String),
    /// Decryption setup failure.
    #[error("restore decryption failed: {0}")]
    Crypto(String),
    /// Key rebuild failure.
    #[error("restore key failed: {0}")]
    Key(String),
    /// Unknown manifest compression codec.
    #[error("unknown compression for backup: {0}")]
    Compression(String),
}

/// Restore request (Go `pb.RestoreRequest`, embedded subset).
#[derive(Debug, Clone, Default)]
pub struct RestoreRequest {
    /// Rewrite every version to this timestamp (0 keeps originals).
    pub restore_ts: u64,
    /// Namespaced predicates to restore (empty = all).
    pub predicates: Vec<String>,
    /// Decryption key (`None` = plaintext, like Go's nil key).
    pub encryption_key: Option<datastreamdb_enc::keys::DataKey>,
    /// Compression codec from the manifest (`"snappy"`, `"gzip"`, `""`).
    pub compression: String,
}

/// Restore counters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestoreStats {
    /// Framed `KVList` batches consumed.
    pub batches: u64,
    /// KVs consumed.
    pub kvs: u64,
    /// Store keys and schema entries written.
    pub written: u64,
    /// KVs skipped (filter or unknown key type).
    pub skipped: u64,
    /// Highest version installed.
    pub max_version: u64,
}

/// Drop-application counters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DropStats {
    /// Store keys deleted.
    pub keys: u64,
    /// Schema predicates deleted.
    pub preds: u64,
    /// Types deleted.
    pub types: u64,
}

/// Rebuilds a store key from its backup form (Go `FromBackupKey`).
fn from_backup_key(backup: &BackupKey) -> Result<Vec<u8>, RestoreError> {
    use datastreamdb_proto::pb::backup_key::KeyType;
    let attr = format!("{:x}-{}", backup.namespace, backup.attr);
    let key_type = KeyType::try_from(backup.r#type)
        .map_err(|_| RestoreError::Key(format!("bad key type {}", backup.r#type)))?;
    let mut key = match key_type {
        KeyType::Data => datastreamdb_x::keycodec::data_key(&attr, backup.uid),
        KeyType::Index => datastreamdb_x::keycodec::index_key(&attr, &backup.term),
        KeyType::Reverse => datastreamdb_x::keycodec::reverse_key(&attr, backup.uid),
        KeyType::Count => datastreamdb_x::keycodec::count_key(&attr, backup.count, false),
        KeyType::CountRev => datastreamdb_x::keycodec::count_key(&attr, backup.count, true),
        KeyType::Schema => datastreamdb_x::keycodec::schema_key(&attr),
        KeyType::Type => datastreamdb_x::keycodec::type_key(&attr),
        KeyType::Unknown => {
            return Err(RestoreError::Key("unknown backup key type".to_owned()));
        }
    };
    if backup.start_uid > 0 {
        key = datastreamdb_x::keycodec::split_key(&key, backup.start_uid)
            .map_err(|err| RestoreError::Key(err.to_string()))?;
    }
    Ok(key)
}

/// Opens the decrypting + decompressing reader stack (Go
/// `readerFrom(...).WithEncryption(...).WithCompression(...)`).
fn open_reader<'reader, R: Read + 'reader>(
    inner: R,
    request: &RestoreRequest,
) -> Result<Box<dyn Read + 'reader>, RestoreError> {
    let decrypted =
        datastreamdb_enc::stream::decrypting_reader(request.encryption_key.as_ref(), inner)
            .map_err(|err| RestoreError::Crypto(err.to_string()))?;
    match request.compression.as_str() {
        "snappy" => Ok(Box::new(snap::read::FrameDecoder::new(decrypted))),
        "gzip" | "" => Ok(Box::new(flate2::read::GzDecoder::new(decrypted))),
        other => Err(RestoreError::Compression(other.to_owned())),
    }
}

/// Applies one decoded KV to the store or schema (shared by restore
/// and predicate moves): schema/type values reinstall into `schema`,
/// data values decode as `BackupPostingList` protos installed as
/// rolled-up bases at `version`.
///
/// # Errors
///
/// Returns [`RestoreError`] on key, decode, or storage failure.
pub fn apply_store_kv(
    engine: &datastreamdb_posting::PostingEngine,
    schema: &datastreamdb_schema::SchemaState,
    store_key: &[u8],
    value: &[u8],
    user_meta: &[u8],
    version: u64,
) -> Result<(), RestoreError> {
    let parsed = datastreamdb_x::keycodec::parse(store_key)
        .map_err(|err| RestoreError::Key(err.to_string()))?;
    if parsed.is_schema() {
        let update = datastreamdb_proto::SchemaUpdate::decode(value)
            .map_err(|err| RestoreError::Decode(err.to_string()))?;
        schema.set(&update.predicate.clone(), update);
    } else if parsed.is_type() {
        let update = datastreamdb_proto::TypeUpdate::decode(value)
            .map_err(|err| RestoreError::Decode(err.to_string()))?;
        schema.set_type(&update.type_name.clone(), update);
    } else {
        let backup_list = datastreamdb_proto::BackupPostingList::decode(value)
            .map_err(|err| RestoreError::Decode(err.to_string()))?;
        let empty = user_meta == [datastreamdb_posting::list::BIT_EMPTY_POSTING];
        let plist = if empty {
            None
        } else {
            Some(datastreamdb_posting::List::from_backup_posting_list(
                &backup_list,
            ))
        };
        datastreamdb_posting::store::write_base(engine.store().as_ref(), store_key, version, plist)
            .map_err(|err| RestoreError::Storage(err.to_string()))?;
    }
    Ok(())
}

/// Reads one length-delimited batch; `Ok(None)` at clean EOF.
fn read_batch(reader: &mut dyn Read) -> Result<Option<KvList>, RestoreError> {
    let mut len_bytes = [0u8; 8];
    let mut read = 0;
    while read < len_bytes.len() {
        match reader.read(&mut len_bytes[read..]) {
            Ok(0) => break,
            Ok(count) => read += count,
            Err(err) => return Err(RestoreError::Io(err.to_string())),
        }
    }
    if read == 0 {
        return Ok(None);
    }
    if read != len_bytes.len() {
        return Err(RestoreError::Decode("truncated batch length".to_owned()));
    }
    let len = u64::from_le_bytes(len_bytes);
    let Ok(len) = usize::try_from(len) else {
        return Err(RestoreError::Decode(format!("absurd batch length {len}")));
    };
    let mut bytes = vec![0u8; len];
    reader
        .read_exact(&mut bytes)
        .map_err(|err| RestoreError::Decode(format!("truncated batch: {err}")))?;
    KvList::decode(bytes.as_slice())
        .map(Some)
        .map_err(|err| RestoreError::Decode(err.to_string()))
}

/// Restores one backup stream into a live engine.
///
/// Schema/type entries reinstall into `schema`; data postings install
/// as rolled-up bases at their backup versions (or
/// [`RestoreRequest::restore_ts`] when nonzero). The engine oracle
/// advances past the highest installed version so later commits never
/// reuse restored timestamps.
///
/// # Errors
///
/// Returns [`RestoreError`] on read, decode, crypto, key, or storage
/// failure.
pub fn restore_stream<R: Read>(
    engine: &datastreamdb_posting::PostingEngine,
    schema: &datastreamdb_schema::SchemaState,
    request: &RestoreRequest,
    input: R,
) -> Result<RestoreStats, RestoreError> {
    let mut stats = RestoreStats::default();
    let filter: Option<HashSet<&str>> = if request.predicates.is_empty() {
        None
    } else {
        Some(request.predicates.iter().map(String::as_str).collect())
    };
    let mut reader = open_reader(input, request)?;
    while let Some(batch) = read_batch(reader.as_mut())? {
        stats.batches += 1;
        for kv in &batch.kv {
            stats.kvs += 1;
            let backup_key = BackupKey::decode(kv.key.as_slice())
                .map_err(|err| RestoreError::Decode(err.to_string()))?;
            let full_attr = format!("{:x}-{}", backup_key.namespace, backup_key.attr);
            if let Some(allowed) = &filter {
                if !allowed.contains(full_attr.as_str()) {
                    stats.skipped += 1;
                    continue;
                }
            }
            let Ok(store_key) = from_backup_key(&backup_key) else {
                tracing::warn!("skipping unknown backup key type");
                stats.skipped += 1;
                continue;
            };
            let version = if request.restore_ts > 0 {
                request.restore_ts
            } else {
                kv.version
            };
            stats.max_version = stats.max_version.max(version);
            apply_store_kv(
                engine,
                schema,
                &store_key,
                &kv.value,
                &kv.user_meta,
                version,
            )?;
            stats.written += 1;
        }
    }
    engine.oracle().advance_to(stats.max_version);
    Ok(stats)
}

/// Splits `nshex-attr` (Go `ParseNamespaceAttr`).
fn split_ns_attr(attr: &str) -> Option<(u64, &str)> {
    attr.split_once('-')
        .and_then(|(ns, rest)| u64::from_str_radix(ns, 16).ok().map(|ns| (ns, rest)))
}

/// Deletes every posting key matching `matches`.
fn delete_matching(
    engine: &datastreamdb_posting::PostingEngine,
    matches: &dyn Fn(&str) -> bool,
) -> Result<u64, RestoreError> {
    let pairs = engine
        .store()
        .scan_prefix(datastreamdb_posting::store::PARTITION, b"")
        .map_err(|err| RestoreError::Storage(err.to_string()))?;
    let mut deleted = 0;
    for (key_bytes, _) in &pairs {
        let Ok(key) = datastreamdb_x::keycodec::parse(key_bytes) else {
            continue;
        };
        if !matches(&key.attr) {
            continue;
        }
        datastreamdb_posting::store::delete_chain(engine.store().as_ref(), key_bytes)
            .map_err(|err| RestoreError::Storage(err.to_string()))?;
        deleted += 1;
    }
    Ok(deleted)
}

/// Replays one manifest drop record (Go restore drop handling).
///
/// Kinds mirror Go's `DropOperation_DropOp`: 0 = all, 1 = data in the
/// named namespace, 2 = one predicate, 3 = a whole namespace.
///
/// # Errors
///
/// Returns [`RestoreError::Storage`] on backend failure.
pub fn apply_drop_operation(
    engine: &datastreamdb_posting::PostingEngine,
    schema: &datastreamdb_schema::SchemaState,
    op: &DropOperation,
) -> Result<DropStats, RestoreError> {
    let mut stats = DropStats::default();
    match op.drop_op {
        0 => {
            stats.keys = delete_matching(engine, &|_| true)?;
            schema.delete_all();
        }
        1 | 3 => {
            let ns = op
                .drop_value
                .parse::<u64>()
                .map_err(|_| RestoreError::Key(format!("bad drop ns {}", op.drop_value)))?;
            stats.keys = delete_matching(engine, &|attr| {
                split_ns_attr(attr).is_some_and(|(key_ns, _)| key_ns == ns)
            })?;
            if op.drop_op == 3 {
                for pred in schema.predicates() {
                    if split_ns_attr(&pred).is_some_and(|(key_ns, _)| key_ns == ns) {
                        schema.delete(&pred);
                        stats.preds += 1;
                    }
                }
                for name in schema.types() {
                    if split_ns_attr(&name).is_some_and(|(key_ns, _)| key_ns == ns) {
                        schema.delete_type(&name);
                        stats.types += 1;
                    }
                }
            }
        }
        2 => {
            let pred = op.drop_value.clone();
            stats.keys = delete_matching(engine, &|attr| attr == pred)?;
            schema.delete(&pred);
            stats.preds += 1;
        }
        other => {
            return Err(RestoreError::Key(format!("unknown drop op {other}")));
        }
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn test_schema() -> Arc<datastreamdb_schema::SchemaState> {
        use datastreamdb_proto::{SchemaUpdate, TypeUpdate, ValType};
        let schema = Arc::new(datastreamdb_schema::SchemaState::new());
        schema.set(
            "0-name",
            SchemaUpdate {
                predicate: "0-name".to_owned(),
                value_type: ValType::String as i32,
                ..SchemaUpdate::default()
            },
        );
        schema.set_type(
            "0-person",
            TypeUpdate {
                type_name: "0-person".to_owned(),
                fields: vec![SchemaUpdate {
                    predicate: "0-name".to_owned(),
                    ..SchemaUpdate::default()
                }],
            },
        );
        schema
    }

    fn mutate(
        engine: &Arc<datastreamdb_posting::PostingEngine>,
        entity: u64,
        attr: &str,
        value: Vec<u8>,
    ) {
        use datastreamdb_proto::{DirectedEdge, EdgeOp, ValType};
        let txn = engine.begin();
        let mut edge = DirectedEdge {
            entity,
            attr: attr.to_owned(),
            value,
            value_type: ValType::String as i32,
            op: EdgeOp::Set as i32,
            ..DirectedEdge::default()
        };
        let key = datastreamdb_x::data_key(attr, entity);
        txn.get(&key)
            .expect("list")
            .add_mutation_with_index(&mut edge, &txn)
            .expect("mutate");
        engine.commit(&txn).expect("commit");
    }

    fn test_engine(
        schema: Arc<datastreamdb_schema::SchemaState>,
    ) -> Arc<datastreamdb_posting::PostingEngine> {
        let engine = datastreamdb_posting::PostingEngine::open(
            Arc::new(datastreamdb_store::MemoryBackend::new()),
            schema,
            64,
        );
        mutate(&engine, 2, "0-name", b"Alice".to_vec());
        mutate(&engine, 3, "0-name", b"Bob".to_vec());
        engine
    }

    fn export_text(engine: &datastreamdb_posting::PostingEngine, read_ts: u64) -> String {
        let mut out = Vec::new();
        crate::export::export_rdf(engine, 0, read_ts, &mut out).expect("export");
        let mut lines: Vec<String> = String::from_utf8(out)
            .expect("utf8")
            .lines()
            .map(str::to_owned)
            .collect();
        lines.sort();
        lines.join("\n")
    }

    fn backup_bytes(
        engine: &datastreamdb_posting::PostingEngine,
        schema: &datastreamdb_schema::SchemaState,
        request: &crate::backup::BackupRequest,
    ) -> (Vec<u8>, crate::backup::BackupStats) {
        let mut out = Vec::new();
        let stats = crate::backup::write_backup(engine, schema, request, &mut out).expect("backup");
        (out, stats)
    }

    #[test]
    fn backup_restore_roundtrip() {
        let schema = test_schema();
        let engine = test_engine(schema.clone());
        let read_ts = engine.oracle().max_assigned();
        let request = crate::backup::BackupRequest {
            read_ts,
            group_id: 1,
            ..crate::backup::BackupRequest::default()
        };
        let (bytes, stats) = backup_bytes(&engine, &schema, &request);
        // Two data lists plus one schema entry plus one type entry.
        assert_eq!(stats.kvs, 4);
        assert!(stats.batches >= 1);

        let fresh_schema = Arc::new(datastreamdb_schema::SchemaState::new());
        let fresh = datastreamdb_posting::PostingEngine::open(
            Arc::new(datastreamdb_store::MemoryBackend::new()),
            fresh_schema.clone(),
            64,
        );
        let restore_request = RestoreRequest {
            compression: "snappy".to_owned(),
            ..RestoreRequest::default()
        };
        let restore_stats =
            restore_stream(&fresh, &fresh_schema, &restore_request, bytes.as_slice())
                .expect("restore");
        assert_eq!(restore_stats.written, 4);

        let restored_ts = fresh.oracle().max_assigned();
        assert_eq!(
            export_text(&fresh, restored_ts),
            export_text(&engine, read_ts)
        );
        assert_eq!(fresh_schema.predicates(), vec!["0-name".to_owned()]);
        assert_eq!(fresh_schema.types(), vec!["0-person".to_owned()]);
        assert!(restored_ts >= read_ts);
    }

    #[test]
    fn incremental_skips_unchanged_lists() {
        let schema = test_schema();
        let engine = test_engine(schema.clone());
        let ts1 = engine.oracle().max_assigned();
        mutate(&engine, 4, "0-name", b"Carol".to_vec());
        let ts2 = engine.oracle().max_assigned();

        let full = crate::backup::BackupRequest {
            read_ts: ts2,
            ..crate::backup::BackupRequest::default()
        };
        let (_, full_stats) = backup_bytes(&engine, &schema, &full);
        let incremental = crate::backup::BackupRequest {
            read_ts: ts2,
            since_ts: ts1 + 1,
            ..crate::backup::BackupRequest::default()
        };
        let (inc_bytes, inc_stats) = backup_bytes(&engine, &schema, &incremental);
        // Only Carol's list changed; schema/type entries always ride along.
        assert_eq!(inc_stats.kvs, 3);
        assert!(inc_stats.kvs < full_stats.kvs);

        // Full then incremental restores the complete graph.
        let fresh_schema = Arc::new(datastreamdb_schema::SchemaState::new());
        let fresh = datastreamdb_posting::PostingEngine::open(
            Arc::new(datastreamdb_store::MemoryBackend::new()),
            fresh_schema.clone(),
            64,
        );
        let base = crate::backup::BackupRequest {
            read_ts: ts1,
            ..crate::backup::BackupRequest::default()
        };
        let (base_bytes, _) = backup_bytes(&engine, &schema, &base);
        let restore_request = RestoreRequest {
            compression: "snappy".to_owned(),
            ..RestoreRequest::default()
        };
        restore_stream(
            &fresh,
            &fresh_schema,
            &restore_request,
            base_bytes.as_slice(),
        )
        .expect("restore base");
        restore_stream(
            &fresh,
            &fresh_schema,
            &restore_request,
            inc_bytes.as_slice(),
        )
        .expect("restore inc");
        let restored_ts = fresh.oracle().max_assigned();
        assert_eq!(export_text(&fresh, restored_ts), export_text(&engine, ts2));
    }

    #[test]
    fn encrypted_roundtrip() {
        let schema = test_schema();
        let engine = test_engine(schema.clone());
        let read_ts = engine.oracle().max_assigned();
        let key = datastreamdb_enc::keys::DataKey::new(vec![7u8; 32]).expect("key");
        let request = crate::backup::BackupRequest {
            read_ts,
            encryption_key: Some(key),
            ..crate::backup::BackupRequest::default()
        };
        let (bytes, _) = backup_bytes(&engine, &schema, &request);

        let fresh_schema = Arc::new(datastreamdb_schema::SchemaState::new());
        let fresh = datastreamdb_posting::PostingEngine::open(
            Arc::new(datastreamdb_store::MemoryBackend::new()),
            fresh_schema.clone(),
            64,
        );
        let key = datastreamdb_enc::keys::DataKey::new(vec![7u8; 32]).expect("key");
        let restore_request = RestoreRequest {
            compression: "snappy".to_owned(),
            encryption_key: Some(key),
            ..RestoreRequest::default()
        };
        restore_stream(&fresh, &fresh_schema, &restore_request, bytes.as_slice()).expect("restore");
        let restored_ts = fresh.oracle().max_assigned();
        assert_eq!(
            export_text(&fresh, restored_ts),
            export_text(&engine, read_ts)
        );

        // A wrong key fails instead of restoring garbage.
        let wrong_schema = Arc::new(datastreamdb_schema::SchemaState::new());
        let wrong = datastreamdb_posting::PostingEngine::open(
            Arc::new(datastreamdb_store::MemoryBackend::new()),
            wrong_schema.clone(),
            64,
        );
        let wrong_key = datastreamdb_enc::keys::DataKey::new(vec![8u8; 32]).expect("key");
        let wrong_request = RestoreRequest {
            compression: "snappy".to_owned(),
            encryption_key: Some(wrong_key),
            ..RestoreRequest::default()
        };
        assert!(restore_stream(&wrong, &wrong_schema, &wrong_request, bytes.as_slice()).is_err());
    }

    #[test]
    fn drop_operations_recorded_and_applied() {
        let schema = test_schema();
        let engine = test_engine(schema.clone());
        mutate(
            &engine,
            1,
            "0-datastreamdb.drop.op",
            b"DROP_ATTR;0-name".to_vec(),
        );
        let read_ts = engine.oracle().max_assigned();
        let request = crate::backup::BackupRequest {
            read_ts,
            ..crate::backup::BackupRequest::default()
        };
        let (bytes, stats) = backup_bytes(&engine, &schema, &request);
        assert_eq!(stats.drop_operations.len(), 1);
        assert_eq!(stats.drop_operations[0].drop_op, 2);
        assert_eq!(stats.drop_operations[0].drop_value, "0-name");

        let fresh_schema = test_schema();
        let fresh = test_engine(fresh_schema.clone());
        let restore_request = RestoreRequest {
            compression: "snappy".to_owned(),
            ..RestoreRequest::default()
        };
        restore_stream(&fresh, &fresh_schema, &restore_request, bytes.as_slice()).expect("restore");
        let drop_stats =
            apply_drop_operation(&fresh, &fresh_schema, &stats.drop_operations[0]).expect("drop");
        assert!(drop_stats.keys >= 2);
        assert_eq!(drop_stats.preds, 1);
        assert!(fresh_schema.get("0-name", false).is_none());
        let restored_ts = fresh.oracle().max_assigned();
        assert!(!export_text(&fresh, restored_ts).contains("Alice"));
    }

    #[test]
    fn manifest_record_shape() {
        use std::collections::HashMap;
        let request = crate::backup::BackupRequest {
            read_ts: 42,
            since_ts: 7,
            group_id: 2,
            ..crate::backup::BackupRequest::default()
        };
        let stats = crate::backup::BackupStats {
            drop_operations: vec![DropOperation {
                drop_op: 0,
                drop_value: String::new(),
            }],
            ..crate::backup::BackupStats::default()
        };
        let manifest = crate::backup::manifest_for_backup(
            &request,
            &stats,
            "incremental",
            "series-1",
            3,
            "datastreamdb.0102.1504",
            HashMap::from([(2u32, vec!["0-name".to_owned()])]),
        );
        let value: serde_json::Value = serde_json::to_value(&manifest).expect("json");
        assert_eq!(value["type"], "incremental");
        assert_eq!(value["since"], 7);
        assert_eq!(value["read_ts"], 42);
        assert_eq!(value["backup_id"], "series-1");
        assert_eq!(value["backup_num"], 3);
        assert_eq!(value["compression"], "snappy");
        assert_eq!(value["encrypted"], false);
        assert_eq!(value["groups"]["2"], serde_json::json!(["0-name"]));
        assert_eq!(value["drop_operations"][0]["drop_op"], 0);
    }
}
