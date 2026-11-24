//! [`ZeroConn`](datastreamdb_worker::ZeroConn) tests against a scripted mock zero.

use std::sync::Arc;
use std::time::Duration;

use datastreamdb_proto::api::{Payload, TxnContext};
use datastreamdb_proto::pb::{
    AssignedIds, ConnectionState, Group, Member, MembershipState, MoveTabletRequest, Num,
    OracleDelta, RemoveNodeRequest, Status, Tablet, TabletRequest, TabletResponse, TxnStatus,
    TxnTimestamps, zero_server::Zero,
};
use datastreamdb_worker::{GroupState, ZeroConn};

/// Scripted zero: canned answers, one-shot streams.
struct MockZero;

type MembershipStream = futures::channel::mpsc::Receiver<Result<MembershipState, tonic::Status>>;
type OracleStream = futures::channel::mpsc::Receiver<Result<OracleDelta, tonic::Status>>;

fn membership() -> MembershipState {
    MembershipState {
        counter: 3,
        groups: [(
            1_u32,
            Group {
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
        zeros: [(
            1_u64,
            Member {
                id: 1,
                addr: "zero:5080".to_owned(),
                ..Member::default()
            },
        )]
        .into_iter()
        .collect(),
        max_uid: 1000,
        max_txn_ts: 2000,
        ..MembershipState::default()
    }
}

#[tonic::async_trait]
impl Zero for MockZero {
    async fn connect(
        &self,
        request: tonic::Request<Member>,
    ) -> Result<tonic::Response<ConnectionState>, tonic::Status> {
        let mut member = request.into_inner();
        if member.id == 0 {
            member.id = 7;
        }
        Ok(tonic::Response::new(ConnectionState {
            member: Some(member),
            state: Some(membership()),
            max_pending: 2000,
        }))
    }

    async fn update_membership(
        &self,
        _request: tonic::Request<Group>,
    ) -> Result<tonic::Response<Payload>, tonic::Status> {
        Ok(tonic::Response::new(Payload::default()))
    }

    type StreamMembershipStream = MembershipStream;

    async fn stream_membership(
        &self,
        _request: tonic::Request<Payload>,
    ) -> Result<tonic::Response<Self::StreamMembershipStream>, tonic::Status> {
        let (mut tx, rx) = futures::channel::mpsc::channel(8);
        tx.try_send(Ok(membership())).expect("send");
        Ok(tonic::Response::new(rx))
    }

    type OracleStream = OracleStream;

    async fn oracle(
        &self,
        _request: tonic::Request<Payload>,
    ) -> Result<tonic::Response<Self::OracleStream>, tonic::Status> {
        let (mut tx, rx) = futures::channel::mpsc::channel(8);
        tx.try_send(Ok(OracleDelta {
            txns: vec![TxnStatus {
                start_ts: 5,
                commit_ts: 10,
            }],
            max_assigned: 10,
            ..OracleDelta::default()
        }))
        .expect("send");
        Ok(tonic::Response::new(rx))
    }

    async fn should_serve(
        &self,
        request: tonic::Request<Tablet>,
    ) -> Result<tonic::Response<Tablet>, tonic::Status> {
        let mut tablet = request.into_inner();
        tablet.group_id = 1;
        Ok(tonic::Response::new(tablet))
    }

    async fn inform(
        &self,
        request: tonic::Request<TabletRequest>,
    ) -> Result<tonic::Response<TabletResponse>, tonic::Status> {
        let req = request.into_inner();
        Ok(tonic::Response::new(TabletResponse {
            tablets: req.tablets,
        }))
    }

    async fn assign_ids(
        &self,
        _request: tonic::Request<Num>,
    ) -> Result<tonic::Response<AssignedIds>, tonic::Status> {
        Ok(tonic::Response::new(AssignedIds {
            start_id: 1001,
            end_id: 2000,
            ..AssignedIds::default()
        }))
    }

    async fn timestamps(
        &self,
        _request: tonic::Request<Num>,
    ) -> Result<tonic::Response<AssignedIds>, tonic::Status> {
        Ok(tonic::Response::new(AssignedIds {
            start_id: 2001,
            end_id: 2001,
            ..AssignedIds::default()
        }))
    }

    async fn commit_or_abort(
        &self,
        request: tonic::Request<TxnContext>,
    ) -> Result<tonic::Response<TxnContext>, tonic::Status> {
        let mut txn = request.into_inner();
        if !txn.aborted {
            txn.commit_ts = 3000;
        }
        Ok(tonic::Response::new(txn))
    }

    async fn try_abort(
        &self,
        request: tonic::Request<TxnTimestamps>,
    ) -> Result<tonic::Response<OracleDelta>, tonic::Status> {
        let req = request.into_inner();
        Ok(tonic::Response::new(OracleDelta {
            txns: req
                .ts
                .into_iter()
                .map(|start_ts| TxnStatus {
                    start_ts,
                    commit_ts: 0,
                })
                .collect(),
            ..OracleDelta::default()
        }))
    }

    async fn delete_namespace(
        &self,
        _request: tonic::Request<datastreamdb_proto::pb::DeleteNsRequest>,
    ) -> Result<tonic::Response<Status>, tonic::Status> {
        Ok(tonic::Response::new(Status::default()))
    }

    async fn remove_node(
        &self,
        _request: tonic::Request<RemoveNodeRequest>,
    ) -> Result<tonic::Response<Status>, tonic::Status> {
        Ok(tonic::Response::new(Status::default()))
    }

    async fn move_tablet(
        &self,
        _request: tonic::Request<MoveTabletRequest>,
    ) -> Result<tonic::Response<Status>, tonic::Status> {
        Ok(tonic::Response::new(Status::default()))
    }
}

async fn serve() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr").to_string();
    drop(listener);
    let socket: std::net::SocketAddr = addr.parse().expect("addr");
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(datastreamdb_proto::pb::zero_server::ZeroServer::new(
                MockZero,
            ))
            .serve(socket)
            .await
            .expect("serve");
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    addr
}

fn conn(addr: &str) -> (ZeroConn, Arc<GroupState>) {
    let groups = Arc::new(GroupState::new(0, 1, "alpha:7080".to_owned()));
    let conn = ZeroConn::new(
        vec![addr.to_owned()],
        groups.clone(),
        0,
        "alpha:7080".to_owned(),
        1,
    );
    (conn, groups)
}

#[tokio::test]
async fn unary_passthroughs_reach_zero() {
    let addr = serve().await;
    let (conn, _) = conn(&addr);

    let state = conn.connect().await.expect("connect");
    assert_eq!(state.member.expect("member").id, 7);
    assert_eq!(state.max_pending, 2000);

    let ids = conn.assign_ids(Num::default()).await.expect("assign");
    assert_eq!((ids.start_id, ids.end_id), (1001, 2000));

    let ts = conn.timestamps(Num::default()).await.expect("timestamps");
    assert_eq!((ts.start_id, ts.end_id), (2001, 2001));

    let txn = conn
        .commit_or_abort(TxnContext {
            start_ts: 9,
            ..TxnContext::default()
        })
        .await
        .expect("commit");
    assert_eq!(txn.commit_ts, 3000);

    let delta = conn
        .try_abort(TxnTimestamps { ts: vec![11, 12] })
        .await
        .expect("try abort");
    assert_eq!(delta.txns.len(), 2);

    let tablet = conn
        .should_serve(Tablet {
            predicate: "0-name".to_owned(),
            ..Tablet::default()
        })
        .await
        .expect("should serve");
    assert_eq!(tablet.group_id, 1);

    let informed = conn
        .inform(TabletRequest {
            tablets: vec![Tablet {
                predicate: "0-name".to_owned(),
                ..Tablet::default()
            }],
            group_id: 1,
        })
        .await
        .expect("inform");
    assert_eq!(informed.tablets.len(), 1);

    conn.delete_namespace(datastreamdb_proto::pb::DeleteNsRequest::default())
        .await
        .expect("delete ns");
    conn.remove_node(RemoveNodeRequest::default())
        .await
        .expect("remove");
    conn.move_tablet(MoveTabletRequest::default())
        .await
        .expect("move");
}

#[tokio::test]
async fn streams_apply_and_forward() {
    let addr = serve().await;
    let (conn, groups) = conn(&addr);
    let conn = Arc::new(conn);

    let membership = tokio::spawn({
        let conn = conn.clone();
        async move { conn.run_membership_stream().await }
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if groups.belongs_to("0-name") == Some(1) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "membership never applied"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    membership.abort();

    let (delta_tx, mut delta_rx) = tokio::sync::mpsc::channel(8);
    let oracle = tokio::spawn({
        let conn = conn.clone();
        async move { conn.run_oracle_stream(delta_tx).await }
    });
    let delta = tokio::time::timeout(Duration::from_secs(10), delta_rx.recv())
        .await
        .expect("delta timeout")
        .expect("delta");
    assert_eq!(delta.max_assigned, 10);
    assert_eq!(delta.txns.len(), 1);
    oracle.abort();
}
