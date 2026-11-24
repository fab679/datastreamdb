//! Alpha applier tests: stage/finalize, conflicts, drops, snapshots.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use datastreamdb_posting::PostingEngine;
use datastreamdb_proto::pb::{Mutations, OracleDelta, Proposal, TxnStatus};
use datastreamdb_proto::{DirectedEdge, EdgeOp, SchemaUpdate, ValType};
use datastreamdb_raft::Applier as _;
use datastreamdb_schema::SchemaState;
use datastreamdb_store::MemoryBackend;
use datastreamdb_worker::{AlphaApplier, conflict_key_string, parse_outcome, txn_pred_keys};
use prost::Message as _;

fn test_schema() -> Arc<SchemaState> {
    let schema = Arc::new(SchemaState::new());
    schema.set(
        "0-name",
        SchemaUpdate {
            predicate: "0-name".to_owned(),
            value_type: ValType::String as i32,
            ..SchemaUpdate::default()
        },
    );
    schema
}

fn engine() -> Arc<PostingEngine> {
    PostingEngine::open(Arc::new(MemoryBackend::new()), test_schema(), 64)
}

fn set_edge(entity: u64, attr: &str, value: &[u8]) -> DirectedEdge {
    DirectedEdge {
        entity,
        attr: attr.to_owned(),
        value: value.to_vec(),
        value_type: ValType::String as i32,
        op: EdgeOp::Set as i32,
        ..DirectedEdge::default()
    }
}

fn mutations_proposal(start_ts: u64, edges: Vec<DirectedEdge>) -> Vec<u8> {
    Proposal {
        mutations: Some(Mutations {
            start_ts,
            edges,
            ..Mutations::default()
        }),
        ..Proposal::default()
    }
    .encode_to_vec()
}

fn delta_proposal(txns: Vec<(u64, u64)>) -> Vec<u8> {
    let max_assigned = txns.iter().map(|(_, commit)| *commit).max().unwrap_or(0);
    Proposal {
        delta: Some(OracleDelta {
            txns: txns
                .into_iter()
                .map(|(start_ts, commit_ts)| TxnStatus {
                    start_ts,
                    commit_ts,
                })
                .collect(),
            max_assigned,
            ..OracleDelta::default()
        }),
        ..Proposal::default()
    }
    .encode_to_vec()
}

fn read_values(engine: &Arc<PostingEngine>, attr: &str, uid: u64, read_ts: u64) -> Vec<Vec<u8>> {
    let txn = engine.begin();
    let key = datastreamdb_x::data_key(attr, uid);
    let list = txn.get(&key).expect("list");
    let mut values = Vec::new();
    list.iterate(read_ts, 0, &mut |posting| {
        values.push(posting.value.clone());
        Ok(())
    })
    .expect("iterate");
    values
}

#[tokio::test]
async fn stage_finalize_and_read_back() {
    let engine = engine();
    let applier = AlphaApplier::new(engine.clone(), 1, None);

    let bytes = mutations_proposal(5, vec![set_edge(2, "0-name", b"Alice")]);
    let response = applier.apply(&bytes).await.expect("apply");
    parse_outcome(&response).expect("stage ok");
    let (conflicts, preds) = applier.staged_conflicts(5);
    assert!(!conflicts.is_empty(), "conflicts recorded");
    assert!(preds.contains("0-name"));

    // Not visible before the commit delta lands.
    assert!(read_values(&engine, "0-name", 2, 9).is_empty());

    let response = applier
        .apply(&delta_proposal(vec![(5, 10)]))
        .await
        .expect("apply");
    parse_outcome(&response).expect("finalize ok");
    assert_eq!(
        read_values(&engine, "0-name", 2, 10),
        vec![b"Alice".to_vec()]
    );
    assert!(engine.oracle().max_assigned() >= 10);
    assert!(applier.staged_conflicts(5).0.is_empty());
}

#[tokio::test]
async fn cdc_emits_commits_not_stages() {
    use datastreamdb_worker::cdc::{Cdc, ChannelSink};
    let engine = engine();
    let (sink, mut rx) = ChannelSink::channel();
    let applier =
        AlphaApplier::new(engine.clone(), 1, None).with_cdc(Arc::new(Cdc::new(Arc::new(sink))));

    // Staging alone emits nothing.
    let bytes = mutations_proposal(5, vec![set_edge(2, "0-name", b"Alice")]);
    applier.apply(&bytes).await.expect("apply");
    assert!(rx.try_recv().is_err());

    // The commit decision flushes one stamped event.
    let bytes = delta_proposal(vec![(5, 10)]);
    applier.apply(&bytes).await.expect("apply");
    let batch = rx.try_recv().expect("batch");
    assert_eq!(batch.len(), 1);
    let value: serde_json::Value = serde_json::from_slice(&batch[0].value).expect("json");
    assert_eq!(value["meta"]["commit_ts"], 10);
    assert_eq!(value["event"]["value"], "Alice");
    assert_eq!(value["event"]["attr"], "name");
}

#[tokio::test]
async fn second_committer_conflicts() {
    let engine = engine();
    let applier = AlphaApplier::new(engine.clone(), 1, None);

    applier
        .apply(&mutations_proposal(
            5,
            vec![set_edge(2, "0-name", b"Alice")],
        ))
        .await
        .expect("apply");
    applier
        .apply(&mutations_proposal(6, vec![set_edge(2, "0-name", b"Bob")]))
        .await
        .expect("apply");

    applier
        .apply(&delta_proposal(vec![(5, 10)]))
        .await
        .expect("apply");
    // Bob's finalize conflicts (Alice committed at 10 > 6); the batch still
    // finalizes without wedging.
    applier
        .apply(&delta_proposal(vec![(6, 11)]))
        .await
        .expect("apply");

    assert_eq!(
        read_values(&engine, "0-name", 2, 11),
        vec![b"Alice".to_vec()]
    );
}

#[tokio::test]
async fn abort_delta_drops_staged() {
    let engine = engine();
    let applier = AlphaApplier::new(engine.clone(), 1, None);

    applier
        .apply(&mutations_proposal(
            5,
            vec![set_edge(2, "0-name", b"Alice")],
        ))
        .await
        .expect("apply");
    applier
        .apply(&delta_proposal(vec![(5, 0)]))
        .await
        .expect("apply");

    assert!(read_values(&engine, "0-name", 2, 100).is_empty());
    assert!(applier.staged_conflicts(5).0.is_empty());
}

#[tokio::test]
async fn schema_proposal_installs_and_derives() {
    let engine = engine();
    let applier = AlphaApplier::new(engine.clone(), 1, None);

    // Unknown predicate: staging derives its schema.
    applier
        .apply(&mutations_proposal(5, vec![set_edge(2, "0-nick", b"Al")]))
        .await
        .expect("apply");
    let derived = engine.schema().get("0-nick", false).expect("derived");
    assert_eq!(derived.value_type, ValType::String as i32);

    // Explicit schema proposal with an index rebuilds without error.
    let bytes = Proposal {
        mutations: Some(Mutations {
            start_ts: 6,
            schema: vec![SchemaUpdate {
                predicate: "0-nick".to_owned(),
                value_type: ValType::String as i32,
                directive: datastreamdb_proto::Directive::Index as i32,
                tokenizer: vec!["exact".to_owned()],
                ..SchemaUpdate::default()
            }],
            ..Mutations::default()
        }),
        ..Proposal::default()
    }
    .encode_to_vec();
    let response = applier.apply(&bytes).await.expect("apply");
    parse_outcome(&response).expect("schema ok");
    let updated = engine.schema().get("0-nick", false).expect("schema");
    assert_eq!(updated.tokenizer, vec!["exact".to_owned()]);
}

#[tokio::test]
async fn drop_all_clears_and_reinstalls_initial() {
    let engine = engine();
    let applier = AlphaApplier::new(engine.clone(), 1, None);

    applier
        .apply(&mutations_proposal(
            5,
            vec![set_edge(2, "0-name", b"Alice")],
        ))
        .await
        .expect("apply");
    applier
        .apply(&delta_proposal(vec![(5, 10)]))
        .await
        .expect("apply");
    assert!(!read_values(&engine, "0-name", 2, 10).is_empty());

    let bytes = Proposal {
        mutations: Some(Mutations {
            start_ts: 11,
            drop_op: datastreamdb_proto::pb::mutations::DropOp::All as i32,
            ..Mutations::default()
        }),
        ..Proposal::default()
    }
    .encode_to_vec();
    let response = applier.apply(&bytes).await.expect("apply");
    parse_outcome(&response).expect("drop ok");

    assert!(read_values(&engine, "0-name", 2, 100).is_empty());
    // Initial schema is back (datastreamdb.* predicates exist on group 1).
    assert!(!engine.schema().predicates().is_empty());
}

#[tokio::test]
async fn snapshot_restore_roundtrip() {
    let engine = engine();
    let applier = AlphaApplier::new(engine.clone(), 1, None);

    applier
        .apply(&mutations_proposal(
            5,
            vec![set_edge(2, "0-name", b"Alice")],
        ))
        .await
        .expect("apply");
    applier
        .apply(&delta_proposal(vec![(5, 10)]))
        .await
        .expect("apply");

    let bytes = applier.snapshot().await.expect("snapshot");
    assert!(!bytes.is_empty());

    let fresh = PostingEngine::open(
        Arc::new(MemoryBackend::new()),
        Arc::new(SchemaState::new()),
        64,
    );
    let restore = AlphaApplier::new(fresh.clone(), 1, None);
    restore.restore(&bytes).await.expect("restore");

    assert_eq!(
        read_values(&fresh, "0-name", 2, 10),
        vec![b"Alice".to_vec()]
    );
    assert!(fresh.schema().get("0-name", false).is_some());
    assert!(fresh.oracle().max_assigned() >= 10);
}

#[test]
fn conflict_key_base36_vectors() {
    assert_eq!(conflict_key_string(0), "0");
    assert_eq!(conflict_key_string(10), "a");
    assert_eq!(conflict_key_string(35), "z");
    assert_eq!(conflict_key_string(36), "10");
    assert_eq!(conflict_key_string(37), "11");
    assert_eq!(conflict_key_string(1296), "100");
    // u64::MAX in base 36 (cross-checked against Go's FormatUint).
    assert_eq!(conflict_key_string(u64::MAX), "3w5e11264sgsf");
}

#[test]
fn pred_keys_carry_group_prefix() {
    let preds: HashSet<String> = ["0-name".to_owned(), "0-age".to_owned()]
        .into_iter()
        .collect();
    assert_eq!(txn_pred_keys(1, &preds), vec!["1-0-age", "1-0-name"]);
}

#[tokio::test]
async fn propose_mutations_through_raft_group() {
    let dir = tempfile::tempdir().expect("tempdir");
    let engine = engine();
    let applier = AlphaApplier::new(engine.clone(), 1, None);

    let registry = datastreamdb_raft::ChannelRegistry::default();
    let config = Arc::new(openraft::Config {
        election_timeout_min: 150,
        election_timeout_max: 300,
        heartbeat_interval: 50,
        ..Default::default()
    });
    let node = datastreamdb_raft::Node::open(
        1,
        dir.path(),
        config,
        applier.clone(),
        datastreamdb_raft::ChannelFactory::new(registry.clone()),
    )
    .await
    .expect("open");
    registry.register(1, node.handle());
    node.initialize([(1, "channel:1".to_owned())].into_iter().collect())
        .await
        .expect("initialize");
    node.wait_for_leader(Duration::from_secs(10))
        .await
        .expect("leader");

    let bytes = mutations_proposal(5, vec![set_edge(2, "0-name", b"Raft")]);
    let response = node.propose(bytes).await.expect("propose");
    parse_outcome(&response).expect("stage ok");

    let response = node
        .propose(delta_proposal(vec![(5, 10)]))
        .await
        .expect("propose");
    parse_outcome(&response).expect("finalize ok");

    assert_eq!(
        read_values(&engine, "0-name", 2, 10),
        vec![b"Raft".to_vec()]
    );
}
