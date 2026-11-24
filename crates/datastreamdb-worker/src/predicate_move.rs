//! Predicate moves (Go `worker/predicate_move.go`).
//!
//! [`run_move_predicate`] streams one predicate's schema entry plus
//! its rolled-up posting lists to the destination group's leader,
//! which applies them through raft proposals ([`apply_received`]).
//! Each `KVS.data` payload carries length-delimited `badgerpb4::KV`
//! frames (`u64` little-endian length plus protobuf bytes); data
//! values are rolled-up `BackupPostingList` protos stamped at the
//! move timestamp, exactly like Go's rolled-up move stream.
//!
//! Zero-driven orchestration (checksums, tablet reassignment,
//! source-side delete) stays deferred: the `dest_gid == 0` clean path
//! applies locally, and callers update membership themselves.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use datastreamdb_posting::PostingEngine;
use datastreamdb_proto::badgerpb4::Kv;
use datastreamdb_proto::pb::{Kvs, MovePredicatePayload, Proposal};
use datastreamdb_raft::Node;
use prost::Message as _;
use tonic::transport::Channel;

use crate::applier::AlphaApplier;
use crate::groups::GroupState;

/// Maximum `KVS.data` payload per stream message.
pub const MOVE_BATCH_BYTES: usize = 4 << 20;
/// Maximum KV bytes per raft proposal (Go proposes at 32 MiB).
pub const PROPOSE_BATCH_BYTES: usize = 32 << 20;

/// Predicate-move failures.
#[derive(Debug, thiserror::Error)]
pub enum MoveError {
    /// This member does not lead its group.
    #[error("not the leader of the group")]
    NotLeader,
    /// This server does not serve the source group.
    #[error("this server doesn't serve group id: {0}")]
    NotServed(u32),
    /// Empty predicate name.
    #[error("predicate must not be empty")]
    EmptyPredicate,
    /// No tablet serves the predicate.
    #[error("no tablet serves predicate {0}")]
    UnknownTablet(String),
    /// This group does not serve the predicate.
    #[error("this group doesn't serve predicate {0}")]
    UnservedTablet(String),
    /// No leader is flagged for the destination group.
    #[error("no leader known for group {0}")]
    NoDestLeader(u32),
    /// Raft proposal failure.
    #[error("move proposal failed: {0}")]
    Propose(String),
    /// Storage failure.
    #[error("move storage failed: {0}")]
    Storage(String),
    /// Frame decode failure.
    #[error("move decode failed: {0}")]
    Decode(String),
}

impl From<MoveError> for tonic::Status {
    fn from(err: MoveError) -> Self {
        match &err {
            MoveError::NotLeader => Self::failed_precondition(err.to_string()),
            MoveError::NotServed(_) | MoveError::UnservedTablet(_) => {
                Self::failed_precondition(err.to_string())
            }
            MoveError::EmptyPredicate | MoveError::UnknownTablet(_) => {
                Self::invalid_argument(err.to_string())
            }
            _ => Self::internal(err.to_string()),
        }
    }
}

/// Collects one predicate's move KVs: the schema entry first (when
/// one exists), then every list rolled up at the move timestamp (Go
/// `movePredicateHelper`; split parts roll into their main lists).
///
/// # Errors
///
/// Returns [`MoveError::Storage`] on backend failure.
pub fn encode_predicate(
    engine: &PostingEngine,
    pred: &str,
    txn_ts: u64,
) -> Result<Vec<Kv>, MoveError> {
    let mut kvs = Vec::new();
    if let Some(update) = engine.schema().get(pred, false) {
        let store_key = datastreamdb_x::keycodec::schema_key(pred);
        kvs.push(Kv {
            key: store_key,
            value: update.encode_to_vec(),
            user_meta: Vec::new(),
            version: txn_ts,
        });
    }
    let pairs = engine
        .store()
        .scan_prefix(datastreamdb_posting::store::PARTITION, b"")
        .map_err(|err| MoveError::Storage(err.to_string()))?;
    for (key_bytes, _) in &pairs {
        let Ok(key) = datastreamdb_x::keycodec::parse(key_bytes) else {
            continue;
        };
        if key.attr != pred || key.has_start_uid || key.is_schema() || key.is_type() {
            continue;
        }
        let list = engine
            .get_list(key_bytes, u64::MAX)
            .map_err(|err| MoveError::Storage(err.to_string()))?;
        let rolled = list
            .rollup(u64::MAX, false)
            .map_err(|err| MoveError::Storage(err.to_string()))?;
        let Some(rolled) = rolled else { continue };
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
        kvs.push(Kv {
            key: key_bytes.clone(),
            value: backup_list.encode_to_vec(),
            user_meta: vec![if empty {
                datastreamdb_posting::list::BIT_EMPTY_POSTING
            } else {
                datastreamdb_posting::list::BIT_COMPLETE_POSTING
            }],
            version: txn_ts,
        });
    }
    Ok(kvs)
}

/// Frames KVs as `u64`-length-delimited protobufs (one `KVS.data`).
#[must_use]
pub fn frame_kvs(kvs: &[Kv]) -> Vec<u8> {
    let mut out = Vec::new();
    for kv in kvs {
        let bytes = kv.encode_to_vec();
        out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        out.extend_from_slice(&bytes);
    }
    out
}

/// Splits KVs into `KVS.data` payloads of at most `max_bytes`.
#[must_use]
pub fn chunk_kvs(kvs: Vec<Kv>, max_bytes: usize) -> Vec<Vec<u8>> {
    let mut chunks = Vec::new();
    let mut pending: Vec<Kv> = Vec::new();
    let mut size = 0;
    for kv in kvs {
        size += kv.key.len() + kv.value.len();
        pending.push(kv);
        if size >= max_bytes {
            chunks.push(frame_kvs(&pending));
            pending = Vec::new();
            size = 0;
        }
    }
    if !pending.is_empty() {
        chunks.push(frame_kvs(&pending));
    }
    chunks
}

/// Decodes one `KVS.data` payload.
///
/// # Errors
///
/// Returns [`MoveError::Decode`] on truncated or corrupt frames.
pub fn decode_frames(data: &[u8]) -> Result<Vec<Kv>, MoveError> {
    let mut kvs = Vec::new();
    let mut cursor = 0;
    while cursor < data.len() {
        if data.len() - cursor < 8 {
            return Err(MoveError::Decode("truncated frame length".to_owned()));
        }
        let mut len_bytes = [0u8; 8];
        len_bytes.copy_from_slice(&data[cursor..cursor + 8]);
        cursor += 8;
        let len = usize::try_from(u64::from_le_bytes(len_bytes))
            .map_err(|_| MoveError::Decode("absurd frame length".to_owned()))?;
        if data.len() - cursor < len {
            return Err(MoveError::Decode("truncated frame".to_owned()));
        }
        let kv = Kv::decode(&data[cursor..cursor + len])
            .map_err(|err| MoveError::Decode(err.to_string()))?;
        cursor += len;
        kvs.push(kv);
    }
    Ok(kvs)
}

/// Proposes one payload and maps raft errors.
async fn propose(node: &Node<AlphaApplier>, proposal: &Proposal) -> Result<(), MoveError> {
    node.propose(proposal.encode_to_vec())
        .await
        .map_err(|err| MoveError::Propose(err.to_string()))?;
    Ok(())
}

/// Applies received move payloads: cleans the moved predicate, then
/// proposes the KVs in 32 MiB batches (Go
/// `batchAndProposeKeyValues`). Returns the received key count.
///
/// The predicate is derived from the first KV (Go requires it to be
/// the schema entry; schemaless moves carry data first and are
/// accepted too).
///
/// # Errors
///
/// Returns [`MoveError`] on decode or raft-proposal failure.
pub async fn apply_received(
    node: &Node<AlphaApplier>,
    payloads: &[Vec<u8>],
) -> Result<u64, MoveError> {
    let mut count = 0;
    let mut first_attr: Option<String> = None;
    let mut first_version = 0;
    let mut pending: Vec<Kv> = Vec::new();
    let mut size = 0;
    for payload in payloads {
        for kv in decode_frames(payload)? {
            if first_attr.is_none() {
                let parsed = datastreamdb_x::keycodec::parse(&kv.key)
                    .map_err(|err| MoveError::Decode(err.to_string()))?;
                first_attr = Some(parsed.attr);
                first_version = kv.version;
            }
            count += 1;
            size += kv.key.len() + kv.value.len();
            pending.push(kv);
            if size >= PROPOSE_BATCH_BYTES {
                propose(
                    node,
                    &Proposal {
                        kv: std::mem::take(&mut pending),
                        ..Proposal::default()
                    },
                )
                .await?;
                size = 0;
            }
        }
    }
    if let Some(attr) = first_attr {
        propose(
            node,
            &Proposal {
                clean_predicate: attr,
                start_ts: first_version.saturating_sub(1),
                ..Proposal::default()
            },
        )
        .await?;
    }
    if !pending.is_empty() {
        propose(
            node,
            &Proposal {
                kv: std::mem::take(&mut pending),
                ..Proposal::default()
            },
        )
        .await?;
    }
    Ok(count)
}

/// Dials (caching) a peer worker.
pub(crate) async fn dial(
    peers: &Mutex<
        HashMap<
            String,
            datastreamdb_proto::pb::worker_client::WorkerClient<Channel>,
            impl std::hash::BuildHasher,
        >,
    >,
    addr: &str,
) -> Result<datastreamdb_proto::pb::worker_client::WorkerClient<Channel>, MoveError> {
    if let Some(client) = peers.lock().expect("peer lock").get(addr).cloned() {
        return Ok(client);
    }
    let endpoint = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
        .map_err(|err| MoveError::Decode(err.to_string()))?;
    let channel = endpoint
        .connect()
        .await
        .map_err(|err| MoveError::Decode(format!("dial {addr}: {err}")))?;
    let client = datastreamdb_proto::pb::worker_client::WorkerClient::new(channel);
    peers
        .lock()
        .expect("peer lock")
        .insert(addr.to_owned(), client.clone());
    Ok(client)
}

/// Runs one predicate move from this (source-leader) member.
///
/// With `dest_gid == 0`, proposes the tablet cleanup locally (Go's
/// delete-tablet path); otherwise streams the predicate to the
/// destination leader and returns its received key count.
///
/// # Errors
///
/// Returns [`MoveError`] when this member does not lead, serves
/// neither the source group nor the tablet, or the stream fails.
pub async fn run_move_predicate(
    engine: &Arc<PostingEngine>,
    groups: &GroupState,
    node: &Node<AlphaApplier>,
    peers: &Mutex<
        HashMap<
            String,
            datastreamdb_proto::pb::worker_client::WorkerClient<Channel>,
            impl std::hash::BuildHasher,
        >,
    >,
    req: &MovePredicatePayload,
) -> Result<u64, MoveError> {
    if !node.is_leader().await {
        return Err(MoveError::NotLeader);
    }
    if groups.group_id() != req.source_gid {
        return Err(MoveError::NotServed(req.source_gid));
    }
    if req.predicate.is_empty() {
        return Err(MoveError::EmptyPredicate);
    }
    if req.dest_gid == 0 {
        propose(
            node,
            &Proposal {
                clean_predicate: req.predicate.clone(),
                expected_checksum: req.expected_checksum,
                start_ts: req.txn_ts,
                ..Proposal::default()
            },
        )
        .await?;
        return Ok(0);
    }
    engine.oracle().wait_for_ts(req.txn_ts);
    match groups.belongs_to(&req.predicate) {
        None => return Err(MoveError::UnknownTablet(req.predicate.clone())),
        Some(gid) if gid != groups.group_id() => {
            return Err(MoveError::UnservedTablet(req.predicate.clone()));
        }
        _ => {}
    }
    let dest = groups
        .leader_of(req.dest_gid)
        .map(|member| member.addr)
        .ok_or(MoveError::NoDestLeader(req.dest_gid))?;
    let kvs = encode_predicate(engine, &req.predicate, req.txn_ts)?;
    let chunks = chunk_kvs(kvs, MOVE_BATCH_BYTES);
    let mut client = dial(peers, &dest).await?;
    let outbound = futures::stream::iter(chunks.into_iter().map(|data| Kvs {
        data,
        ..Kvs::default()
    }));
    let response = client
        .receive_predicate(outbound)
        .await
        .map_err(|err| MoveError::Decode(format!("streaming to {dest}: {err}")))?;
    let count = String::from_utf8_lossy(&response.into_inner().data)
        .parse::<u64>()
        .map_err(|err| MoveError::Decode(format!("bad key count: {err}")))?;
    tracing::info!(
        "moved predicate {} to group {dest} ({count} keys)",
        req.predicate,
        dest = req.dest_gid
    );
    Ok(count)
}

/// Deletes every chain of `pred` plus its schema entry (the
/// `CleanPredicate` apply path).
///
/// # Errors
///
/// Returns [`MoveError::Storage`] on backend failure.
pub fn clean_predicate_local(engine: &PostingEngine, pred: &str) -> Result<u64, MoveError> {
    let pairs = engine
        .store()
        .scan_prefix(datastreamdb_posting::store::PARTITION, b"")
        .map_err(|err| MoveError::Storage(err.to_string()))?;
    let mut deleted = 0;
    for (key_bytes, _) in &pairs {
        let Ok(key) = datastreamdb_x::keycodec::parse(key_bytes) else {
            continue;
        };
        if key.attr != pred {
            continue;
        }
        datastreamdb_posting::store::delete_chain(engine.store().as_ref(), key_bytes)
            .map_err(|err| MoveError::Storage(err.to_string()))?;
        deleted += 1;
    }
    engine.schema().delete(pred);
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_engine() -> Arc<PostingEngine> {
        let schema = Arc::new(datastreamdb_schema::SchemaState::new());
        schema.set(
            "0-name",
            datastreamdb_proto::SchemaUpdate {
                predicate: "0-name".to_owned(),
                value_type: datastreamdb_proto::ValType::String as i32,
                ..datastreamdb_proto::SchemaUpdate::default()
            },
        );
        let engine = PostingEngine::open(
            Arc::new(datastreamdb_store::MemoryBackend::new()),
            schema,
            64,
        );
        let txn = engine.begin();
        let mut edge = datastreamdb_proto::DirectedEdge {
            entity: 2,
            attr: "0-name".to_owned(),
            value: b"Alice".to_vec(),
            value_type: datastreamdb_proto::ValType::String as i32,
            op: datastreamdb_proto::EdgeOp::Set as i32,
            ..datastreamdb_proto::DirectedEdge::default()
        };
        let key = datastreamdb_x::data_key("0-name", 2);
        txn.get(&key)
            .expect("list")
            .add_mutation_with_index(&mut edge, &txn)
            .expect("mutate");
        engine.commit(&txn).expect("commit");
        engine
    }

    #[test]
    fn encode_decode_roundtrip_schema_first() {
        let engine = test_engine();
        let kvs = encode_predicate(&engine, "0-name", 42).expect("encode");
        assert!(kvs.len() >= 2);
        let first = datastreamdb_x::keycodec::parse(&kvs[0].key).expect("key");
        assert!(first.is_schema());
        assert_eq!(kvs[0].version, 42);
        let framed = frame_kvs(&kvs);
        let back = decode_frames(&framed).expect("decode");
        assert_eq!(back.len(), kvs.len());
        assert_eq!(back[0].key, kvs[0].key);
    }

    #[test]
    fn decode_rejects_truncation() {
        assert!(decode_frames(&[1, 2, 3]).is_err());
        let framed = frame_kvs(&[Kv {
            key: b"k".to_vec(),
            value: b"v".to_vec(),
            ..Kv::default()
        }]);
        assert!(decode_frames(&framed[..framed.len() - 2]).is_err());
    }

    #[test]
    fn chunks_respect_cap() {
        let kvs = (0..10)
            .map(|index| Kv {
                key: vec![index],
                value: vec![0u8; 100],
                ..Kv::default()
            })
            .collect::<Vec<_>>();
        let chunks = chunk_kvs(kvs, 250);
        assert!(chunks.len() >= 4);
        let total: usize = chunks
            .iter()
            .map(|chunk| decode_frames(chunk).expect("decode").len())
            .sum();
        assert_eq!(total, 10);
    }

    #[test]
    fn clean_removes_data_and_schema() {
        let engine = test_engine();
        let deleted = clean_predicate_local(&engine, "0-name").expect("clean");
        assert!(deleted >= 1);
        assert!(engine.schema().get("0-name", false).is_none());
        let pairs = engine
            .store()
            .scan_prefix(datastreamdb_posting::store::PARTITION, b"")
            .expect("scan");
        assert!(pairs.is_empty());
    }

    #[test]
    fn moved_kvs_apply_on_dest() {
        let source = test_engine();
        let kvs = encode_predicate(&source, "0-name", 42).expect("encode");
        let dest_schema = Arc::new(datastreamdb_schema::SchemaState::new());
        let dest = PostingEngine::open(
            Arc::new(datastreamdb_store::MemoryBackend::new()),
            dest_schema.clone(),
            64,
        );
        for kv in &kvs {
            datastreamdb_backup::restore::apply_store_kv(
                &dest,
                &dest_schema,
                &kv.key,
                &kv.value,
                &kv.user_meta,
                kv.version,
            )
            .expect("apply");
        }
        assert!(dest_schema.get("0-name", false).is_some());
        let key = datastreamdb_x::data_key("0-name", 2);
        let list = dest.get_list(&key, 42).expect("list");
        assert_eq!(list.all_values(42).expect("values").len(), 1);
    }
}
