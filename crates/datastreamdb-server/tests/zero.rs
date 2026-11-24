//! Zero tests: membership, leases, commits, tablets, streams.

use std::sync::Arc;
use std::time::Duration;

use datastreamdb_proto::api::TxnContext;
use datastreamdb_proto::pb::{Member, Num, Tablet, num::LeaseType, zero_server::Zero as ZeroTrait};
use datastreamdb_server::zero_leases::ZeroLeases;
use datastreamdb_server::zero_oracle::ZeroOracle;
use datastreamdb_server::zero_service::ZeroService;
use datastreamdb_server::zero_state::{ZeroApplier, ZeroState};

struct Fixture {
    service: ZeroService,
    _dir: tempfile::TempDir,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(ZeroState::new());
    let oracle = Arc::new(ZeroOracle::new());
    let leases = Arc::new(ZeroLeases::new());
    let applier = ZeroApplier::new(state.clone(), oracle.clone());

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
        applier,
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

    let service = ZeroService::new(state, oracle, leases, node, 1);
    Fixture { service, _dir: dir }
}

fn uid_num(val: u64) -> Num {
    Num {
        val,
        r#type: LeaseType::Uid as i32,
        ..Num::default()
    }
}

fn ts_num(val: u64) -> Num {
    Num {
        val,
        r#type: LeaseType::TxnTs as i32,
        ..Num::default()
    }
}

#[tokio::test]
async fn connect_assigns_ids_and_groups() {
    let fx = fixture().await;
    // First alpha: id + group 1.
    let first = fx
        .service
        .connect(tonic::Request::new(Member {
            addr: "alpha1:7080".to_owned(),
            ..Member::default()
        }))
        .await
        .expect("connect")
        .into_inner();
    let member = first.member.expect("member");
    assert_eq!(member.id, 1);
    assert_eq!(member.group_id, 1);

    // Same address reconnects to the same member.
    let again = fx
        .service
        .connect(tonic::Request::new(Member {
            addr: "alpha1:7080".to_owned(),
            ..Member::default()
        }))
        .await
        .expect("reconnect")
        .into_inner();
    assert_eq!(again.member.expect("member").id, 1);

    // Second alpha overflows group 1 (replicas 1) into group 2.
    let second = fx
        .service
        .connect(tonic::Request::new(Member {
            addr: "alpha2:7080".to_owned(),
            ..Member::default()
        }))
        .await
        .expect("connect")
        .into_inner();
    let member = second.member.expect("member");
    assert_eq!(member.id, 2);
    assert_eq!(member.group_id, 2);

    // Info-only connects return state without joining.
    let info = fx
        .service
        .connect(tonic::Request::new(Member {
            cluster_info_only: true,
            ..Member::default()
        }))
        .await
        .expect("info")
        .into_inner();
    assert!(info.member.is_none());
    assert!(info.state.is_some());
}

#[tokio::test]
async fn leases_slice_and_renew() {
    let fx = fixture().await;
    let first = fx
        .service
        .assign_ids(tonic::Request::new(uid_num(5)))
        .await
        .expect("lease")
        .into_inner();
    assert_eq!((first.start_id, first.end_id), (1, 5));

    let second = fx
        .service
        .assign_ids(tonic::Request::new(uid_num(3)))
        .await
        .expect("lease")
        .into_inner();
    assert_eq!((second.start_id, second.end_id), (6, 8));

    // Timestamps lease independently.
    let ts = fx
        .service
        .timestamps(tonic::Request::new(ts_num(1)))
        .await
        .expect("timestamps")
        .into_inner();
    assert_eq!((ts.start_id, ts.end_id), (1, 1));

    // Read-only timestamps share without consuming much.
    let ro = fx
        .service
        .timestamps(tonic::Request::new(Num {
            read_only: true,
            r#type: LeaseType::TxnTs as i32,
            ..Num::default()
        }))
        .await
        .expect("readonly")
        .into_inner();
    assert!(ro.read_only > 0);

    // Bumps advance to at least the requested value.
    let bumped = fx
        .service
        .assign_ids(tonic::Request::new(Num {
            val: 50_000,
            bump: true,
            r#type: LeaseType::Uid as i32,
            ..Num::default()
        }))
        .await
        .expect("bump")
        .into_inner();
    assert_eq!(bumped.end_id, 50_000);

    // Bumping below the ceiling fails cleanly.
    let err = fx
        .service
        .assign_ids(tonic::Request::new(Num {
            val: 10,
            bump: true,
            r#type: LeaseType::Uid as i32,
            ..Num::default()
        }))
        .await
        .expect_err("bump below fails");
    assert_eq!(err.code(), tonic::Code::FailedPrecondition);
}

#[tokio::test]
async fn commits_arbitrate_conflicts() {
    let fx = fixture().await;
    let begin = async |fx: &Fixture| {
        fx.service
            .timestamps(tonic::Request::new(ts_num(1)))
            .await
            .expect("begin")
            .into_inner()
            .end_id
    };
    let s1 = begin(&fx).await;
    let s2 = begin(&fx).await;
    let first = fx
        .service
        .commit_or_abort(tonic::Request::new(TxnContext {
            start_ts: s1,
            keys: vec!["aa".to_owned()],
            ..TxnContext::default()
        }))
        .await
        .expect("commit")
        .into_inner();
    assert!(!first.aborted);
    assert!(first.commit_ts > 0);

    // Overlapping key committed after start: aborted.
    let second = fx
        .service
        .commit_or_abort(tonic::Request::new(TxnContext {
            start_ts: s2,
            keys: vec!["aa".to_owned()],
            ..TxnContext::default()
        }))
        .await
        .expect("conflict")
        .into_inner();
    assert!(second.aborted);

    // Disjoint keys: commits.
    let s3 = begin(&fx).await;
    let third = fx
        .service
        .commit_or_abort(tonic::Request::new(TxnContext {
            start_ts: s3,
            keys: vec!["bb".to_owned()],
            ..TxnContext::default()
        }))
        .await
        .expect("commit")
        .into_inner();
    assert!(!third.aborted);

    // Explicit aborts echo.
    let aborted = fx
        .service
        .commit_or_abort(tonic::Request::new(TxnContext {
            start_ts: 9,
            aborted: true,
            ..TxnContext::default()
        }))
        .await
        .expect("abort")
        .into_inner();
    assert!(aborted.aborted);
    assert_eq!(aborted.commit_ts, 0);
}

#[tokio::test]
async fn tablets_assign_and_pin_reserved() {
    let fx = fixture().await;
    // Join group 1 first so tablets have somewhere to land.
    fx.service
        .connect(tonic::Request::new(Member {
            addr: "alpha1:7080".to_owned(),
            ..Member::default()
        }))
        .await
        .expect("connect");

    let tablet = fx
        .service
        .should_serve(tonic::Request::new(Tablet {
            predicate: "0-name".to_owned(),
            group_id: 1,
            ..Tablet::default()
        }))
        .await
        .expect("serve")
        .into_inner();
    assert_eq!(tablet.group_id, 1);

    // Second claim returns the existing assignment.
    let again = fx
        .service
        .should_serve(tonic::Request::new(Tablet {
            predicate: "0-name".to_owned(),
            group_id: 2,
            ..Tablet::default()
        }))
        .await
        .expect("serve")
        .into_inner();
    assert_eq!(again.group_id, 1);

    // Reserved predicates pin to group 1 even when asked elsewhere.
    let reserved = fx
        .service
        .should_serve(tonic::Request::new(Tablet {
            predicate: "0-datastreamdb.type".to_owned(),
            group_id: 2,
            ..Tablet::default()
        }))
        .await
        .expect("serve")
        .into_inner();
    assert_eq!(reserved.group_id, 1);
}

#[tokio::test]
async fn oracle_stream_pushes_decisions() {
    use futures::StreamExt as _;
    let fx = fixture().await;
    let mut stream = fx
        .service
        .oracle(tonic::Request::new(
            datastreamdb_proto::api::Payload::default(),
        ))
        .await
        .expect("oracle")
        .into_inner();

    // Initial (empty) state first.
    let head = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("head timeout")
        .expect("head")
        .expect("head ok");
    assert!(head.txns.is_empty());

    fx.service
        .commit_or_abort(tonic::Request::new(TxnContext {
            start_ts: 5,
            keys: vec!["k".to_owned()],
            ..TxnContext::default()
        }))
        .await
        .expect("commit");

    let delta = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("delta timeout")
        .expect("delta")
        .expect("delta ok");
    assert_eq!(delta.txns.len(), 1);
    assert_eq!(delta.txns[0].start_ts, 5);
    assert!(delta.txns[0].commit_ts > 0);
}

#[tokio::test]
async fn membership_stream_follows_counter() {
    use futures::StreamExt as _;
    let fx = fixture().await;
    let mut stream = fx
        .service
        .stream_membership(tonic::Request::new(
            datastreamdb_proto::api::Payload::default(),
        ))
        .await
        .expect("stream")
        .into_inner();
    let head = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("head timeout")
        .expect("head")
        .expect("head ok");
    assert!(head.groups.is_empty());

    fx.service
        .connect(tonic::Request::new(Member {
            addr: "alpha1:7080".to_owned(),
            ..Member::default()
        }))
        .await
        .expect("connect");

    let updated = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("update timeout")
        .expect("update")
        .expect("update ok");
    assert!(updated.groups.contains_key(&1));
}

#[tokio::test]
async fn remove_node_drops_member() {
    let fx = fixture().await;
    let joined = fx
        .service
        .connect(tonic::Request::new(Member {
            addr: "alpha1:7080".to_owned(),
            ..Member::default()
        }))
        .await
        .expect("connect")
        .into_inner();
    let member = joined.member.expect("member");

    fx.service
        .remove_node(tonic::Request::new(
            datastreamdb_proto::pb::RemoveNodeRequest {
                group_id: member.group_id,
                node_id: member.id,
            },
        ))
        .await
        .expect("remove");

    let info = fx
        .service
        .connect(tonic::Request::new(Member {
            cluster_info_only: true,
            ..Member::default()
        }))
        .await
        .expect("info")
        .into_inner();
    let state = info.state.expect("state");
    assert!(
        !state.groups[&member.group_id]
            .members
            .contains_key(&member.id)
    );
    assert_eq!(state.removed.len(), 1);
}
