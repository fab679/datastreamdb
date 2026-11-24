//! frontend API tests: gRPC + HTTP over a live group and mock zero.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use datastreamdb_frontend::{DataStreamDBService, TxnCoordinator};
use datastreamdb_posting::PostingEngine;
use datastreamdb_proto::api::{
    Mutation, Payload, Request, Response, TxnContext,
    data_stream_db_server::DataStreamDb as DataStreamDbTrait,
};
use datastreamdb_proto::pb::{
    AssignedIds, ConnectionState, Group, Member, MembershipState, MoveTabletRequest, Num,
    OracleDelta, RemoveNodeRequest, Status, Tablet, TabletRequest, TabletResponse, TxnTimestamps,
    zero_server::Zero,
};
use datastreamdb_proto::{SchemaUpdate, ValType};
use datastreamdb_query::QueryEngine;
use datastreamdb_schema::SchemaState;
use datastreamdb_store::MemoryBackend;
use datastreamdb_worker::{
    AlphaApplier, GroupState, MutationsMode, Options, WorkerService, ZeroConn,
};

/// Scripted zero with monotonic leases and recording commits.
struct MockZero {
    next_uid: AtomicU64,
    next_ts: AtomicU64,
}

impl MockZero {
    fn new() -> Self {
        Self {
            next_uid: AtomicU64::new(1001),
            next_ts: AtomicU64::new(2001),
        }
    }

    fn membership() -> MembershipState {
        MembershipState {
            counter: 3,
            groups: [(
                1_u32,
                Group {
                    members: [(
                        1_u64,
                        Member {
                            id: 1,
                            group_id: 1,
                            addr: "alpha:7080".to_owned(),
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
            max_uid: 100_000,
            max_txn_ts: 100_000,
            ..MembershipState::default()
        }
    }
}

type MembershipStream = futures::channel::mpsc::Receiver<Result<MembershipState, tonic::Status>>;
type OracleStream = futures::channel::mpsc::Receiver<Result<OracleDelta, tonic::Status>>;

#[tonic::async_trait]
impl Zero for MockZero {
    async fn connect(
        &self,
        request: tonic::Request<Member>,
    ) -> Result<tonic::Response<ConnectionState>, tonic::Status> {
        let mut member = request.into_inner();
        if member.id == 0 {
            member.id = 1;
        }
        Ok(tonic::Response::new(ConnectionState {
            member: Some(member),
            state: Some(Self::membership()),
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
        tx.try_send(Ok(Self::membership())).expect("send");
        Ok(tonic::Response::new(rx))
    }

    type OracleStream = OracleStream;

    async fn oracle(
        &self,
        _request: tonic::Request<Payload>,
    ) -> Result<tonic::Response<Self::OracleStream>, tonic::Status> {
        let (tx, rx) = futures::channel::mpsc::channel(8);
        drop(tx);
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
        request: tonic::Request<Num>,
    ) -> Result<tonic::Response<AssignedIds>, tonic::Status> {
        let num = request.into_inner();
        let start = self.next_uid.fetch_add(num.val.max(1), Ordering::SeqCst);
        Ok(tonic::Response::new(AssignedIds {
            start_id: start,
            end_id: start + num.val.max(1) - 1,
            ..AssignedIds::default()
        }))
    }

    async fn timestamps(
        &self,
        request: tonic::Request<Num>,
    ) -> Result<tonic::Response<AssignedIds>, tonic::Status> {
        let num = request.into_inner();
        if num.read_only {
            return Ok(tonic::Response::new(AssignedIds {
                read_only: self.next_ts.load(Ordering::SeqCst),
                ..AssignedIds::default()
            }));
        }
        let start = self.next_ts.fetch_add(num.val.max(1), Ordering::SeqCst);
        Ok(tonic::Response::new(AssignedIds {
            start_id: start,
            end_id: start + num.val.max(1) - 1,
            ..AssignedIds::default()
        }))
    }

    async fn commit_or_abort(
        &self,
        request: tonic::Request<TxnContext>,
    ) -> Result<tonic::Response<TxnContext>, tonic::Status> {
        let mut txn = request.into_inner();
        if !txn.aborted {
            txn.commit_ts = self.next_ts.fetch_add(1, Ordering::SeqCst);
        }
        Ok(tonic::Response::new(txn))
    }

    async fn try_abort(
        &self,
        _request: tonic::Request<TxnTimestamps>,
    ) -> Result<tonic::Response<OracleDelta>, tonic::Status> {
        Ok(tonic::Response::new(OracleDelta::default()))
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

fn test_schema() -> Arc<SchemaState> {
    let schema = Arc::new(SchemaState::new());
    // Production boot installs the initial schema/types for the root
    // namespace; the fixture mirrors it so tenancy sees namespace 0.
    for update in datastreamdb_schema::initial_schema(0, false) {
        schema.set(&update.predicate.clone(), update);
    }
    for update in datastreamdb_schema::initial_types(0, false) {
        schema.set_type(&update.type_name.clone(), update);
    }
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

struct Fixture {
    service: DataStreamDBService,
    coordinator: Arc<TxnCoordinator>,
    node: datastreamdb_raft::Node<datastreamdb_worker::AlphaApplier>,
    _dir: tempfile::TempDir,
}

impl Fixture {
    /// Proposes an oracle delta to the group (what the zero stream does).
    async fn settle(&self, start_ts: u64, commit_ts: u64) {
        use prost::Message as _;
        let delta = datastreamdb_proto::pb::Proposal {
            delta: Some(datastreamdb_proto::pb::OracleDelta {
                txns: vec![datastreamdb_proto::pb::TxnStatus {
                    start_ts,
                    commit_ts,
                }],
                max_assigned: commit_ts.max(start_ts),
                ..datastreamdb_proto::pb::OracleDelta::default()
            }),
            ..datastreamdb_proto::pb::Proposal::default()
        };
        let out = self
            .node
            .propose(delta.encode_to_vec())
            .await
            .expect("propose delta");
        datastreamdb_worker::parse_outcome(&out).expect("delta ok");
    }
}

async fn fixture() -> Fixture {
    fixture_inner(false).await
}

async fn fixture_inner(with_auth: bool) -> Fixture {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let zero_addr = listener.local_addr().expect("addr").to_string();
    drop(listener);
    let socket: std::net::SocketAddr = zero_addr.parse().expect("addr");
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(datastreamdb_proto::pb::zero_server::ZeroServer::new(
                MockZero::new(),
            ))
            .serve(socket)
            .await
            .expect("serve");
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let dir = tempfile::tempdir().expect("tempdir");
    let posting = PostingEngine::open(Arc::new(MemoryBackend::new()), test_schema(), 64);
    let query = QueryEngine::open(posting).expect("query");
    let groups = Arc::new(GroupState::new(1, 1, "alpha:7080".to_owned()));
    groups.apply_state(&MockZero::membership());
    let zero = Arc::new(ZeroConn::new(
        vec![zero_addr],
        groups.clone(),
        1,
        "alpha:7080".to_owned(),
        1,
    ));

    let applier = AlphaApplier::new(query.posting().clone(), 1, Some(groups.clone()));
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

    let options = Options {
        posting_dir: "/tmp/datastreamdb-posting".into(),
        wal_dir: "/tmp/datastreamdb-wal".into(),
        mutations_mode: MutationsMode::Allow,
        auth_token: String::new(),
        cache_mb: 64,
        remove_on_update: false,
        type_filter_uid_limit: 1000,
        encryption_key: None,
        cdc_conf: String::new(),
    };
    let worker = Arc::new(WorkerService::new(
        query.posting().clone(),
        groups.clone(),
        node.clone(),
        applier,
        options,
    ));
    let base = TxnCoordinator::new(query, groups, zero, worker);
    let coordinator = if with_auth {
        let settings = datastreamdb_frontend::auth::AclSettings {
            alg: "HS256".to_owned(),
            secret: b"test-secret-key-for-admin-tests-00".to_vec(),
            access_ttl_secs: 3600,
            refresh_ttl_secs: 3600,
        };
        let auth =
            datastreamdb_frontend::auth::AuthContext::from_settings(&settings).expect("auth");
        Arc::new(base.with_auth(Arc::new(auth)))
    } else {
        Arc::new(base)
    };
    // Connected alphas track zero's maximum; simulate a caught-up oracle.
    coordinator
        .posting()
        .process_delta(&datastreamdb_proto::OracleDelta {
            max_assigned: 100_000,
            ..datastreamdb_proto::OracleDelta::default()
        });
    let service = DataStreamDBService::new(coordinator.clone(), None);
    Fixture {
        service,
        coordinator,
        node,
        _dir: dir,
    }
}

fn response_json(response: &Response) -> serde_json::Value {
    serde_json::from_slice(&response.json).expect("response json")
}

#[tokio::test]
async fn grpc_mutate_query_roundtrip() {
    let fx = fixture().await;
    let response = fx
        .service
        .query(tonic::Request::new(Request {
            mutations: vec![Mutation {
                set_nquads: b"_:a <name> \"Ada\" .".to_vec(),
                commit_now: true,
                ..Mutation::default()
            }],
            commit_now: true,
            ..Request::default()
        }))
        .await
        .expect("mutate")
        .into_inner();
    let txn = response.txn.expect("txn");
    assert!(!txn.aborted);
    assert!(txn.commit_ts > 0);
    assert_eq!(response.uids.len(), 1);
    fx.settle(txn.start_ts, txn.commit_ts).await;

    let response = fx
        .service
        .query(tonic::Request::new(Request {
            query: "{ q(func: has(name)) { uid name } }".to_owned(),
            ..Request::default()
        }))
        .await
        .expect("query")
        .into_inner();
    let json = response_json(&response);
    assert!(json.to_string().contains("Ada"), "{json}");
}

#[tokio::test]
async fn commit_abort_flows() {
    let fx = fixture().await;
    // Explicit commit.
    let response = fx
        .service
        .query(tonic::Request::new(Request {
            mutations: vec![Mutation {
                set_nquads: b"_:a <name> \"Grace\" .".to_vec(),
                ..Mutation::default()
            }],
            ..Request::default()
        }))
        .await
        .expect("mutate")
        .into_inner();
    let txn = response.txn.expect("txn");
    assert_eq!(txn.commit_ts, 0);
    let committed = fx
        .service
        .commit_or_abort(tonic::Request::new(txn.clone()))
        .await
        .expect("commit")
        .into_inner();
    assert!(!committed.aborted);
    assert!(committed.commit_ts > 0);
    fx.settle(txn.start_ts, committed.commit_ts).await;

    let response = fx
        .service
        .query(tonic::Request::new(Request {
            query: "{ q(func: has(name)) { uid name } }".to_owned(),
            ..Request::default()
        }))
        .await
        .expect("query")
        .into_inner();
    let json = response_json(&response);
    assert!(json.to_string().contains("Grace"), "{json}");

    // Explicit abort.
    let response = fx
        .service
        .query(tonic::Request::new(Request {
            mutations: vec![Mutation {
                set_nquads: b"_:b <name> \"Dropped\" .".to_vec(),
                ..Mutation::default()
            }],
            ..Request::default()
        }))
        .await
        .expect("mutate")
        .into_inner();
    let mut txn = response.txn.expect("txn");
    txn.aborted = true;
    let aborted = fx
        .service
        .commit_or_abort(tonic::Request::new(txn))
        .await
        .expect("abort")
        .into_inner();
    assert!(aborted.aborted);
}

#[tokio::test]
async fn alter_schema_and_drop() {
    let fx = fixture().await;
    fx.service
        .alter(tonic::Request::new(datastreamdb_proto::api::Operation {
            schema: "age: int .".to_owned(),
            ..datastreamdb_proto::api::Operation::default()
        }))
        .await
        .expect("alter");
    assert!(
        fx.coordinator
            .posting()
            .schema()
            .get("0-age", false)
            .is_some()
    );

    // Drop the attribute back out.
    fx.service
        .alter(tonic::Request::new(datastreamdb_proto::api::Operation {
            drop_op: datastreamdb_proto::api::operation::DropOp::Attr as i32,
            drop_value: "age".to_owned(),
            ..datastreamdb_proto::api::Operation::default()
        }))
        .await
        .expect("drop");
    assert!(
        fx.coordinator
            .posting()
            .schema()
            .get("0-age", false)
            .is_none()
    );
}

#[tokio::test]
async fn cond_guards_mutation() {
    let fx = fixture().await;
    // Seed one name.
    let seed = fx
        .service
        .query(tonic::Request::new(Request {
            mutations: vec![Mutation {
                set_nquads: b"_:a <name> \"Ada\" .".to_vec(),
                commit_now: true,
                ..Mutation::default()
            }],
            commit_now: true,
            ..Request::default()
        }))
        .await
        .expect("seed")
        .into_inner();
    let seed_txn = seed.txn.expect("txn");
    fx.settle(seed_txn.start_ts, seed_txn.commit_ts).await;

    // False guard skips the mutation (no new data, no new uids).
    let response = fx
        .service
        .query(tonic::Request::new(Request {
            query: "{ var(func: has(name)) { x as uid } q(func: uid(x)) { uid name } }".to_owned(),
            mutations: vec![Mutation {
                set_nquads: b"_:b <name> \"Skipped\" .".to_vec(),
                cond: "gt(len(x), 99)".to_owned(),
                commit_now: true,
                ..Mutation::default()
            }],
            commit_now: true,
            ..Request::default()
        }))
        .await
        .expect("guarded")
        .into_inner();
    assert!(response.uids.is_empty());

    // True guard applies.
    let response = fx
        .service
        .query(tonic::Request::new(Request {
            query: "{ var(func: has(name)) { x as uid } q(func: uid(x)) { uid name } }".to_owned(),
            mutations: vec![Mutation {
                set_nquads: b"_:c <name> \"Kept\" .".to_vec(),
                cond: "gt(len(x), 0)".to_owned(),
                commit_now: true,
                ..Mutation::default()
            }],
            commit_now: true,
            ..Request::default()
        }))
        .await
        .expect("guarded")
        .into_inner();
    assert_eq!(response.uids.len(), 1);
    let kept = response.txn.expect("txn");
    fx.settle(kept.start_ts, kept.commit_ts).await;
    let response = fx
        .service
        .query(tonic::Request::new(Request {
            query: "{ q(func: has(name)) { uid name } }".to_owned(),
            ..Request::default()
        }))
        .await
        .expect("query")
        .into_inner();
    let json = response_json(&response);
    assert!(json.to_string().contains("Kept"), "{json}");
    assert!(!json.to_string().contains("Skipped"), "{json}");
}

#[tokio::test]
async fn http_endpoints_roundtrip() {
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt as _;

    let fx = fixture().await;
    let state = datastreamdb_frontend::http::HttpState {
        coordinator: fx.coordinator.clone(),
        groups: fx.coordinator.groups().clone(),
        started: std::time::Instant::now(),
        addr: "alpha:7080".to_owned(),
        auditor: None,
        admin: None,
    };
    let app = datastreamdb_frontend::http::router(state);

    // Health + state.
    let response = app
        .clone()
        .oneshot(
            HttpRequest::get("/health")
                .body(Body::empty())
                .expect("req"),
        )
        .await
        .expect("health");
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let response = app
        .clone()
        .oneshot(HttpRequest::get("/state").body(Body::empty()).expect("req"))
        .await
        .expect("state");
    assert_eq!(response.status(), axum::http::StatusCode::OK);

    // Alter, mutate (commitNow), query.
    let response = app
        .clone()
        .oneshot(
            HttpRequest::post("/alter")
                .body(Body::from("age: int ."))
                .expect("req"),
        )
        .await
        .expect("alter");
    assert_eq!(response.status(), axum::http::StatusCode::OK);

    let response = app
        .clone()
        .oneshot(
            HttpRequest::post("/mutate?commitNow=true")
                .header("content-type", "application/rdf")
                .body(Body::from("_:a <name> \"Http\" ."))
                .expect("req"),
        )
        .await
        .expect("mutate");
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    let receipt: serde_json::Value = serde_json::from_slice(&body).expect("receipt");
    let start_ts = receipt["extensions"]["txn"]["start_ts"]
        .as_u64()
        .expect("start");
    let commit_ts = receipt["extensions"]["txn"]["commit_ts"]
        .as_u64()
        .expect("commit");
    fx.settle(start_ts, commit_ts).await;

    let response = app
        .oneshot(
            HttpRequest::post("/query")
                .body(Body::from("{ q(func: has(name)) { uid name } }"))
                .expect("req"),
        )
        .await
        .expect("query");
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    assert!(String::from_utf8_lossy(&body).contains("Http"), "{body:?}");
}

/// Builds an `/admin` router over the fixture coordinator.
fn admin_app(fx: &Fixture) -> axum::Router {
    datastreamdb_frontend::http::router(datastreamdb_frontend::http::HttpState {
        coordinator: fx.coordinator.clone(),
        groups: fx.coordinator.groups().clone(),
        started: std::time::Instant::now(),
        addr: "alpha:7080".to_owned(),
        auditor: None,
        admin: None,
    })
}

/// POSTs one GraphQL operation to `/admin`.
async fn post_admin(
    app: axum::Router,
    token: Option<&str>,
    query: &str,
) -> (axum::http::StatusCode, serde_json::Value) {
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt as _;
    let mut request = HttpRequest::post("/admin").body(Body::from(
        serde_json::json!({ "query": query }).to_string(),
    ));
    if let (Ok(req), Some(token)) = (request.as_mut(), token) {
        req.headers_mut().insert(
            datastreamdb_frontend::auth::ACCESS_JWT_HEADER,
            token.parse().expect("token header"),
        );
    }
    let response = app.oneshot(request.expect("req")).await.expect("admin");
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    // Rejections carry plain-text bodies; only successes must be JSON.
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
    (status, json)
}

#[tokio::test]
async fn namespaces_create_list_drop() {
    let fx = fixture().await;
    let ns = fx
        .coordinator
        .create_namespace(Some("groot-pw".to_owned()))
        .await
        .expect("create");
    assert!(ns > 0);
    let listed = fx.coordinator.list_namespaces();
    assert!(listed.contains(&0));
    assert!(listed.contains(&ns));
    assert!(fx.coordinator.drop_namespace(0).await.is_err());
    assert!(fx.coordinator.drop_namespace(999_999).await.is_err());
    fx.coordinator.drop_namespace(ns).await.expect("drop");
    assert!(!fx.coordinator.list_namespaces().contains(&ns));
}

#[tokio::test]
async fn grpc_namespace_rpc_roundtrip() {
    let fx = fixture().await;
    let created = fx
        .service
        .create_namespace(tonic::Request::new(
            datastreamdb_proto::api::CreateNamespaceRequest {},
        ))
        .await
        .expect("create")
        .into_inner();
    assert!(created.namespace > 0);
    let listed = fx
        .service
        .list_namespaces(tonic::Request::new(
            datastreamdb_proto::api::ListNamespacesRequest {},
        ))
        .await
        .expect("list")
        .into_inner();
    assert!(listed.namespaces.contains_key(&created.namespace));
    assert!(listed.namespaces.contains_key(&0));
    let err = fx
        .service
        .drop_namespace(tonic::Request::new(
            datastreamdb_proto::api::DropNamespaceRequest { namespace: 0 },
        ))
        .await
        .expect_err("root drop rejected");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    fx.service
        .drop_namespace(tonic::Request::new(
            datastreamdb_proto::api::DropNamespaceRequest {
                namespace: created.namespace,
            },
        ))
        .await
        .expect("drop");
}

#[tokio::test]
async fn admin_http_health_backup_export_restore() {
    let fx = fixture().await;
    let (status, json) = post_admin(
        admin_app(&fx),
        None,
        "{ health { instance status version } }",
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(json["data"]["health"][0]["instance"], "alpha");
    assert_eq!(json["data"]["health"][0]["status"], "healthy");

    let dir = tempfile::tempdir().expect("tempdir");
    let dest = dir.path().to_string_lossy().replace('\\', "/");
    let (status, json) = post_admin(
        admin_app(&fx),
        None,
        &format!(r#"mutation {{ backup(input: {{destination: "{dest}"}}) {{ response {{ code message }} }} }}"#),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{json}");
    assert_eq!(json["data"]["backup"]["response"]["code"], "Success");
    assert!(
        std::fs::read_dir(dir.path()).expect("readdir").count() > 0,
        "backup wrote files"
    );

    let export_dir = tempfile::tempdir().expect("tempdir");
    let export_dest = export_dir.path().to_string_lossy().replace('\\', "/");
    let (status, json) = post_admin(
        admin_app(&fx),
        None,
        &format!(
            r#"mutation {{ export(input: {{format: "rdf", destination: "{export_dest}"}}) {{ response {{ code message }} }} }}"#
        ),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{json}");
    assert_eq!(json["data"]["export"]["response"]["code"], "Success");

    let (status, json) = post_admin(
        admin_app(&fx),
        None,
        &format!(r#"mutation {{ restore(input: {{location: "{dest}"}}) {{ response {{ code message }} }} }}"#),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{json}");
    assert_eq!(json["data"]["restore"]["response"]["code"], "Success");

    let (status, json) = post_admin(
        admin_app(&fx),
        None,
        r#"mutation { export(input: {format: "xml"}) { response { code } } }"#,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert!(json.get("errors").is_some(), "{json}");
}

#[tokio::test]
async fn admin_http_namespaces_roundtrip() {
    let fx = fixture().await;
    let (status, json) = post_admin(
        admin_app(&fx),
        None,
        r#"mutation { addNamespace(input: {password: "pw"}) { namespaceId message } }"#,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{json}");
    let ns = json["data"]["addNamespace"]["namespaceId"]
        .as_u64()
        .expect("ns");
    assert!(ns > 0);
    assert_eq!(
        json["data"]["addNamespace"]["message"],
        "Created namespace successfully"
    );
    let (status, json) = post_admin(
        admin_app(&fx),
        None,
        &format!(
            "mutation {{ deleteNamespace(input: {{namespaceId: {ns}}}) {{ namespaceId message }} }}"
        ),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{json}");
    assert_eq!(
        json["data"]["deleteNamespace"]["message"],
        "Deleted namespace successfully"
    );
    let (status, json) = post_admin(
        admin_app(&fx),
        None,
        "mutation { deleteNamespace(input: {namespaceId: 0}) { namespaceId } }",
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert!(json.get("errors").is_some(), "{json}");
}

#[tokio::test]
async fn admin_http_guardian_gate() {
    let fx = fixture_inner(true).await;
    let secrets = fx.coordinator.auth().expect("auth").secrets();
    let guardian =
        datastreamdb_acl::jwt::mint_access(secrets, "groot", &["guardians".to_owned()], 0, 3600)
            .expect("mint");
    let plain = datastreamdb_acl::jwt::mint_access(secrets, "alice", &[], 0, 3600).expect("mint");

    let (status, _) = post_admin(admin_app(&fx), None, "{ health { instance } }").await;
    assert_eq!(status, axum::http::StatusCode::FORBIDDEN);
    let (status, _) = post_admin(admin_app(&fx), Some(&plain), "{ health { instance } }").await;
    assert_eq!(status, axum::http::StatusCode::FORBIDDEN);
    let (status, json) =
        post_admin(admin_app(&fx), Some(&guardian), "{ health { instance } }").await;
    assert_eq!(status, axum::http::StatusCode::OK, "{json}");
    assert_eq!(json["data"]["health"][0]["instance"], "alpha");

    // gRPC namespace RPCs enforce the same gate.
    let err = fx
        .service
        .create_namespace(tonic::Request::new(
            datastreamdb_proto::api::CreateNamespaceRequest {},
        ))
        .await
        .expect_err("anonymous rejected");
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
}

#[tokio::test]
async fn http_rdf_delete_block_on_indexed_predicate() {
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt as _;
    let fx = fixture().await;
    let app = datastreamdb_frontend::http::router(datastreamdb_frontend::http::HttpState {
        coordinator: fx.coordinator.clone(),
        groups: fx.coordinator.groups().clone(),
        started: std::time::Instant::now(),
        addr: "alpha:7080".to_owned(),
        auditor: None,
        admin: None,
    });
    // Seed via a set block.
    let response = app
        .clone()
        .oneshot(
            HttpRequest::post("/mutate?commitNow=true")
                .header("content-type", "application/rdf")
                .body(Body::from("{ set { _:a <name> \"Ada\" . } }"))
                .expect("req"),
        )
        .await
        .expect("mutate");
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    let receipt: serde_json::Value = serde_json::from_slice(&body).expect("receipt");
    let uid = receipt["data"]["uids"]["_:a"]
        .as_str()
        .expect("uid")
        .to_owned();
    // Star-delete the indexed predicate through a delete block.
    let response = app
        .clone()
        .oneshot(
            HttpRequest::post("/mutate?commitNow=true")
                .header("content-type", "application/rdf")
                .body(Body::from(format!("{{ delete {{ <{uid}> <name> * . }} }}")))
                .expect("req"),
        )
        .await
        .expect("mutate");
    assert_eq!(
        response.status(),
        axum::http::StatusCode::OK,
        "{response:?}"
    );
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    let receipt: serde_json::Value = serde_json::from_slice(&body).expect("receipt");
    assert_eq!(receipt["extensions"]["txn"]["aborted"], false);
    // The name is gone.
    let response = app
        .oneshot(
            HttpRequest::post("/query")
                .body(Body::from("{ q(func: has(name)) { uid name } }"))
                .expect("req"),
        )
        .await
        .expect("query");
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    assert!(!body.windows(3).any(|w| w == b"Ada"), "{body:?}");
}
