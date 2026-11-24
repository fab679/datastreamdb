//! Worker service tests: Mutate/ServeTask/Schema over a live group.

use std::sync::Arc;
use std::time::Duration;

use datastreamdb_posting::PostingEngine;
use datastreamdb_proto::ValType;
use datastreamdb_proto::pb::{
    DirectedEdge, Group, Member, MembershipState, Mutations, OracleDelta, Proposal, SchemaRequest,
    SchemaUpdate, Tablet, TxnStatus, worker_server::Worker,
};
use datastreamdb_proto::{EdgeOp, TaskQuery};
use datastreamdb_schema::SchemaState;
use datastreamdb_store::MemoryBackend;
use datastreamdb_worker::{
    AlphaApplier, GroupState, MutationsMode, Options, WorkerService, parse_outcome,
};
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

fn options() -> Options {
    Options {
        posting_dir: "/tmp/datastreamdb-posting".into(),
        wal_dir: "/tmp/datastreamdb-wal".into(),
        mutations_mode: MutationsMode::Allow,
        auth_token: String::new(),
        cache_mb: 64,
        remove_on_update: false,
        type_filter_uid_limit: 1000,
        encryption_key: None,
        cdc_conf: String::new(),
    }
}

/// Group 1 serving `0-name`, led by member 1.
fn membership() -> MembershipState {
    MembershipState {
        counter: 1,
        groups: [(
            1_u32,
            Group {
                members: [(
                    1_u64,
                    Member {
                        id: 1,
                        group_id: 1,
                        addr: "alpha1:7080".to_owned(),
                        leader: true,
                        ..Member::default()
                    },
                )]
                .into_iter()
                .collect(),
                tablets: [(
                    "0-name".to_owned(),
                    Tablet {
                        group_id: 1,
                        predicate: "0-name".to_owned(),
                        ..Tablet::default()
                    },
                )]
                .into_iter()
                .collect(),
                ..Group::default()
            },
        )]
        .into_iter()
        .collect(),
        ..MembershipState::default()
    }
}

struct Fixture {
    engine: Arc<PostingEngine>,
    service: WorkerService,
    groups: Arc<GroupState>,
    node: datastreamdb_raft::Node<AlphaApplier>,
    _dir: tempfile::TempDir,
}

async fn fixture(mode: MutationsMode) -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let engine = PostingEngine::open(Arc::new(MemoryBackend::new()), test_schema(), 64);
    let groups = Arc::new(GroupState::new(1, 1, "alpha1:7080".to_owned()));
    groups.apply_state(&membership());
    let applier = AlphaApplier::new(engine.clone(), 1, Some(groups.clone()));

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

    // Connected alphas track zero's maximum; advance past test timestamps.
    engine.process_delta(&datastreamdb_proto::OracleDelta {
        max_assigned: 100,
        ..datastreamdb_proto::OracleDelta::default()
    });

    let mut opts = options();
    opts.mutations_mode = mode;
    let service = WorkerService::new(engine.clone(), groups.clone(), node.clone(), applier, opts);
    Fixture {
        engine,
        service,
        groups,
        node,
        _dir: dir,
    }
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
async fn mutate_rpc_stages_and_fills_context() {
    let fx = fixture(MutationsMode::Allow).await;
    let response = fx
        .service
        .mutate(tonic::Request::new(Mutations {
            group_id: 1,
            start_ts: 5,
            edges: vec![set_edge(2, "0-name", b"Ada")],
            ..Mutations::default()
        }))
        .await
        .expect("mutate")
        .into_inner();
    assert_eq!(response.start_ts, 5);
    assert!(!response.keys.is_empty(), "conflict keys filled");
    assert_eq!(response.preds, vec!["1-0-name".to_owned()]);

    // Finalize through the group, then read back.
    let delta = Proposal {
        delta: Some(OracleDelta {
            txns: vec![TxnStatus {
                start_ts: 5,
                commit_ts: 10,
            }],
            max_assigned: 10,
            ..OracleDelta::default()
        }),
        ..Proposal::default()
    };
    let out = fx
        .node
        .propose(delta.encode_to_vec())
        .await
        .expect("propose");
    parse_outcome(&out).expect("finalize ok");
    assert_eq!(
        read_values(&fx.engine, "0-name", 2, 10),
        vec![b"Ada".to_vec()]
    );
}

#[tokio::test]
async fn mutate_rejects_foreign_group_and_strict_unknown() {
    let fx = fixture(MutationsMode::Allow).await;
    let err = fx
        .service
        .mutate(tonic::Request::new(Mutations {
            group_id: 2,
            start_ts: 5,
            ..Mutations::default()
        }))
        .await
        .expect_err("foreign group rejected");
    assert_eq!(err.code(), tonic::Code::FailedPrecondition);

    let strict = fixture(MutationsMode::Strict).await;
    let err = strict
        .service
        .mutate(tonic::Request::new(Mutations {
            group_id: 1,
            start_ts: 5,
            edges: vec![set_edge(2, "0-unknown", b"x")],
            ..Mutations::default()
        }))
        .await
        .expect_err("strict unknown rejected");
    assert_eq!(err.code(), tonic::Code::FailedPrecondition);

    let blocked = fixture(MutationsMode::Disallow).await;
    let err = blocked
        .service
        .mutate(tonic::Request::new(Mutations {
            group_id: 1,
            start_ts: 5,
            ..Mutations::default()
        }))
        .await
        .expect_err("disallowed rejected");
    assert_eq!(err.code(), tonic::Code::FailedPrecondition);
}

#[tokio::test]
async fn serve_task_and_schema_answer_locally() {
    let fx = fixture(MutationsMode::Allow).await;
    fx.service
        .mutate(tonic::Request::new(Mutations {
            group_id: 1,
            start_ts: 5,
            edges: vec![set_edge(2, "0-name", b"Ada")],
            ..Mutations::default()
        }))
        .await
        .expect("mutate");
    let delta = Proposal {
        delta: Some(OracleDelta {
            txns: vec![TxnStatus {
                start_ts: 5,
                commit_ts: 10,
            }],
            max_assigned: 10,
            ..OracleDelta::default()
        }),
        ..Proposal::default()
    };
    fx.node
        .propose(delta.encode_to_vec())
        .await
        .expect("propose");

    let task = fx
        .service
        .serve_task(tonic::Request::new(TaskQuery {
            attr: "0-name".to_owned(),
            uid_list: Some(datastreamdb_proto::List { uids: vec![2] }),
            read_ts: 10,
            ..TaskQuery::default()
        }))
        .await
        .expect("task")
        .into_inner();
    assert_eq!(task.value_matrix.len(), 1);
    assert_eq!(task.value_matrix[0].values.len(), 1);

    let schema = fx
        .service
        .schema(tonic::Request::new(SchemaRequest {
            group_id: 1,
            predicates: vec!["0-name".to_owned()],
            fields: vec!["type".to_owned(), "list".to_owned()],
            ..SchemaRequest::default()
        }))
        .await
        .expect("schema")
        .into_inner();
    assert_eq!(schema.schema.len(), 1);
    assert_eq!(schema.schema[0].predicate, "0-name");
    assert_eq!(schema.schema[0].r#type, "string");
    assert!(!schema.schema[0].list);

    // Empty predicates + fields list everything with all fields.
    let schema = fx
        .service
        .schema(tonic::Request::new(SchemaRequest {
            group_id: 1,
            ..SchemaRequest::default()
        }))
        .await
        .expect("schema")
        .into_inner();
    assert!(schema.schema.iter().any(|node| node.predicate == "0-name"));
}

#[tokio::test]
async fn unimplemented_rpcs_report_cleanly() {
    let fx = fixture(MutationsMode::Allow).await;
    let err = fx
        .service
        .sort(tonic::Request::new(
            datastreamdb_proto::pb::SortMessage::default(),
        ))
        .await
        .expect_err("sort stubbed");
    assert_eq!(err.code(), tonic::Code::Unimplemented);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn predicate_move_applies_through_raft() {
    use datastreamdb_worker::predicate_move::{
        MOVE_BATCH_BYTES, apply_received, chunk_kvs, encode_predicate,
    };
    let fx = fixture(MutationsMode::Allow).await;
    // Source engine holding the moved predicate.
    let source = PostingEngine::open(Arc::new(MemoryBackend::new()), test_schema(), 64);
    let txn = source.begin();
    let mut edge = DirectedEdge {
        entity: 2,
        attr: "0-name".to_owned(),
        value: b"Alice".to_vec(),
        value_type: ValType::String as i32,
        op: EdgeOp::Set as i32,
        ..DirectedEdge::default()
    };
    let key = datastreamdb_x::data_key("0-name", 2);
    txn.get(&key)
        .expect("list")
        .add_mutation_with_index(&mut edge, &txn)
        .expect("mutate");
    source.commit(&txn).expect("commit");

    let kvs = encode_predicate(&source, "0-name", 200).expect("encode");
    assert!(kvs.len() >= 2);
    let chunks = chunk_kvs(kvs, MOVE_BATCH_BYTES);
    let count = apply_received(&fx.node, &chunks).await.expect("apply");
    assert!(count >= 2);

    // The fixture engine (behind the applier) now serves the moved data.
    let read_ts = fx.engine.oracle().max_assigned().max(200);
    let list = fx.engine.get_list(&key, read_ts).expect("list");
    assert_eq!(list.all_values(read_ts).expect("values").len(), 1);
    assert!(fx.engine.schema().get("0-name", false).is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn move_predicate_validates_before_dialing() {
    use datastreamdb_worker::predicate_move::run_move_predicate;
    let fx = fixture(MutationsMode::Allow).await;
    let peers = std::sync::Mutex::new(std::collections::HashMap::new());
    // Wrong source group.
    let err = run_move_predicate(
        &fx.engine,
        &fx.groups,
        &fx.node,
        &peers,
        &datastreamdb_proto::pb::MovePredicatePayload {
            predicate: "0-name".to_owned(),
            source_gid: 7,
            dest_gid: 2,
            ..datastreamdb_proto::pb::MovePredicatePayload::default()
        },
    )
    .await
    .expect_err("group");
    assert_eq!(err.to_string(), "this server doesn't serve group id: 7");
    // Empty predicate.
    let err = run_move_predicate(
        &fx.engine,
        &fx.groups,
        &fx.node,
        &peers,
        &datastreamdb_proto::pb::MovePredicatePayload {
            source_gid: 1,
            dest_gid: 2,
            ..datastreamdb_proto::pb::MovePredicatePayload::default()
        },
    )
    .await
    .expect_err("empty");
    assert_eq!(err.to_string(), "predicate must not be empty");
    // Unknown tablet.
    let err = run_move_predicate(
        &fx.engine,
        &fx.groups,
        &fx.node,
        &peers,
        &datastreamdb_proto::pb::MovePredicatePayload {
            predicate: "0-nope".to_owned(),
            source_gid: 1,
            dest_gid: 2,
            ..datastreamdb_proto::pb::MovePredicatePayload::default()
        },
    )
    .await
    .expect_err("tablet");
    assert!(err.to_string().contains("0-nope"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backup_export_restore_rpcs_roundtrip() {
    let fx = fixture(MutationsMode::Allow).await;
    let dir = tempfile::tempdir().expect("tempdir");
    let dest = dir.path().to_string_lossy().into_owned();

    // The RPC writes one group's data file (Go parity: no manifest).
    fx.service
        .backup(tonic::Request::new(datastreamdb_proto::pb::BackupRequest {
            group_id: 1,
            destination: dest.clone(),
            ..datastreamdb_proto::pb::BackupRequest::default()
        }))
        .await
        .expect("backup");
    assert!(!dir.path().join("manifest.json").exists());
    // The coordinator assembles the manifest chain instead.
    let peers = std::sync::Mutex::new(std::collections::HashMap::new());
    datastreamdb_worker::backup::process_backup_request(
        &fx.engine,
        &fx.groups,
        &peers,
        &datastreamdb_proto::pb::BackupRequest {
            destination: dest.clone(),
            ..datastreamdb_proto::pb::BackupRequest::default()
        },
        None,
    )
    .await
    .expect("coordinate");
    assert!(dir.path().join("manifest.json").exists());

    let export = fx
        .service
        .export(tonic::Request::new(datastreamdb_proto::pb::ExportRequest {
            group_id: 1,
            format: "rdf".to_owned(),
            destination: dest.clone(),
            ..datastreamdb_proto::pb::ExportRequest::default()
        }))
        .await
        .expect("export")
        .into_inner();
    assert_eq!(export.code, 0);
    assert_eq!(export.files.len(), 3);

    let status = fx
        .service
        .restore(tonic::Request::new(
            datastreamdb_proto::pb::RestoreRequest {
                group_id: 1,
                location: dest,
                ..datastreamdb_proto::pb::RestoreRequest::default()
            },
        ))
        .await
        .expect("restore")
        .into_inner();
    assert_eq!(status.code, 0);
}
