//! Zero server assembly: raft group, services, and HTTP.
//!
//! [`ZeroServer::boot`] opens the zero raft group (WAL under `wal_dir`),
//! self-announces into the zeros map, and starts serving the `Zero` gRPC
//! service plus `/health`, `/state`, `/assign`, and `/removeNode`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::response::IntoResponse;
use datastreamdb_proto::pb::{Member, ZeroProposal};
use datastreamdb_raft::Node;
use prost::Message as _;

use crate::zero_leases::ZeroLeases;
use crate::zero_oracle::ZeroOracle;
use crate::zero_service::ZeroService;
use crate::zero_state::{ZeroApplier, ZeroState};

/// Zero configuration (flags in the `zero` binary).
#[derive(Debug, Clone)]
pub struct ZeroConfig {
    /// HTTP address (`127.0.0.1:6080`).
    pub http_addr: SocketAddr,
    /// gRPC address (`127.0.0.1:5080`).
    pub grpc_addr: SocketAddr,
    /// This zero's advertised address (`ip:grpc-port`).
    pub my_addr: String,
    /// WAL directory for the zero group log.
    pub wal_dir: PathBuf,
    /// Bootstrap a fresh zero group instead of joining one.
    pub bootstrap: bool,
    /// Peer zero gRPC addrs to join through (empty on bootstrap).
    pub join: Vec<String>,
    /// Alpha replicas wanted per group (Connect placement).
    pub num_replicas: u32,
    /// Raft heartbeat interval in milliseconds.
    pub heartbeat_ms: u64,
    /// Raft election timeout range in milliseconds.
    pub election_ms: (u64, u64),
}

/// Boot failures.
#[derive(Debug, thiserror::Error)]
pub enum ZeroBootError {
    /// Group bootstrap/join failed.
    #[error("zero group failed: {0}")]
    Group(String),
    /// Storage failure.
    #[error("storage failed: {0}")]
    Storage(String),
}

/// Serving tasks: HTTP router plus the combined gRPC server.
type ServerTasks = (
    tokio::task::JoinHandle<Result<(), ZeroBootError>>,
    tokio::task::JoinHandle<Result<(), ZeroBootError>>,
);

/// One running zero: state, oracle, leases, group, and serving.
pub struct ZeroServer {
    /// Cluster state (shared with the service).
    pub state: Arc<ZeroState>,
    /// Zero raft group.
    node: Node<ZeroApplier>,
    /// Serving tasks (HTTP, gRPC).
    servers: ServerTasks,
}

/// Stable joiner ID from an address (FNV-1a 64-bit, never 0 so it
/// is always a valid voter; a 1-collision with the bootstrap voter is
/// a 2^-64 event the leader's idempotent seat absorbs).
#[must_use]
pub fn zero_id_for(addr: &str) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x100_0000_01b3;
    let mut hash = OFFSET;
    for byte in addr.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(PRIME);
    }
    (hash % (u64::MAX - 1)) + 1
}

impl ZeroServer {
    /// Boots the zero group and self-announces into the zeros map.
    ///
    /// # Errors
    ///
    /// Returns [`ZeroBootError`] when storage or the group fails.
    pub async fn boot(config: ZeroConfig) -> Result<Self, ZeroBootError> {
        std::fs::create_dir_all(&config.wal_dir)
            .map_err(|err| ZeroBootError::Storage(err.to_string()))?;
        let group_dir = config.wal_dir.join("group-0");
        std::fs::create_dir_all(&group_dir)
            .map_err(|err| ZeroBootError::Storage(err.to_string()))?;

        let state = Arc::new(ZeroState::new());
        let oracle = Arc::new(ZeroOracle::new());
        let leases = Arc::new(ZeroLeases::new());
        let applier = ZeroApplier::new(state.clone(), oracle.clone());

        // Zero ID 1 on bootstrap; joiners take a stable address hash
        // so restarts rejoin as the same voter.
        let my_id = if config.bootstrap {
            1
        } else {
            zero_id_for(&config.my_addr)
        };
        let raft_config = Arc::new(openraft::Config {
            heartbeat_interval: config.heartbeat_ms,
            election_timeout_min: config.election_ms.0,
            election_timeout_max: config.election_ms.1,
            ..Default::default()
        });
        let node = Node::open(
            my_id,
            &group_dir,
            raft_config,
            applier,
            datastreamdb_raft::TonicFactory::new(my_id),
        )
        .await
        .map_err(|err| ZeroBootError::Group(err.to_string()))?;

        if config.bootstrap {
            let mut voters = std::collections::BTreeMap::new();
            voters.insert(my_id, config.my_addr.clone());
            match node.initialize(voters).await {
                Ok(()) | Err(datastreamdb_raft::NodeError::AlreadyInitialized) => {}
                Err(err) => return Err(ZeroBootError::Group(err.to_string())),
            }
        }

        let service = ZeroService::new(
            state.clone(),
            oracle,
            leases,
            node.clone(),
            config.num_replicas,
        );
        let (http_task, grpc_task) = Self::spawn_servers(&state, service, &node, &config);
        // Serve before joining: the leader dials back for replication.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        if !config.bootstrap {
            Self::join_via_peers(&config.join, my_id, &config.my_addr).await?;
        }
        node.wait_for_leader(std::time::Duration::from_secs(60))
            .await
            .map_err(|err| ZeroBootError::Group(err.to_string()))?;

        // Self-announce into the zeros map (leader writes it). Joiners
        // are announced by the leader inside `JoinCluster`.
        if config.bootstrap {
            let announce = ZeroProposal {
                member: Some(Member {
                    id: my_id,
                    group_id: 0,
                    addr: config.my_addr.clone(),
                    ..Member::default()
                }),
                ..ZeroProposal::default()
            };
            node.propose(announce.encode_to_vec())
                .await
                .map_err(|err| ZeroBootError::Group(err.to_string()))?;
        }
        Ok(Self {
            state,
            node,
            servers: (http_task, grpc_task),
        })
    }

    /// Spawns the HTTP router plus the combined gRPC server (`Zero`,
    /// `RaftTransport`, `Raft`).
    fn spawn_servers(
        state: &Arc<ZeroState>,
        service: ZeroService,
        node: &Node<ZeroApplier>,
        config: &ZeroConfig,
    ) -> ServerTasks {
        let http_state = HttpState {
            state: state.clone(),
            service: service.clone(),
            addr: config.my_addr.clone(),
        };
        let http_addr = config.http_addr;
        let http_task = tokio::spawn(async move {
            let listener = tokio::net::TcpListener::bind(http_addr)
                .await
                .map_err(|err| ZeroBootError::Storage(err.to_string()))?;
            axum::serve(listener, router(http_state))
                .await
                .map_err(|err| ZeroBootError::Storage(err.to_string()))
        });
        let transport = datastreamdb_raft::TransportServer::new(node.handle());
        let raft_service = crate::zero_raft::ZeroRaftService::new(node.clone());
        let grpc_addr = config.grpc_addr;
        let grpc_task = tokio::spawn(async move {
            tonic::transport::Server::builder()
            .add_service(datastreamdb_proto::pb::zero_server::ZeroServer::new(service.clone()))
            .add_service(
                datastreamdb_raft::network::transport::raft_transport_server::RaftTransportServer::new(
                    transport,
                ),
            )
            .add_service(datastreamdb_proto::pb::raft_server::RaftServer::new(
                raft_service,
            ))
            .serve(grpc_addr)
            .await
            .map_err(|err| ZeroBootError::Storage(err.to_string()))
        });
        (http_task, grpc_task)
    }

    /// Asks each peer in turn to seat this zero until one succeeds.
    async fn join_via_peers(
        peers: &[String],
        my_id: u64,
        my_addr: &str,
    ) -> Result<(), ZeroBootError> {
        if peers.is_empty() {
            return Err(ZeroBootError::Group(
                "zero join needs at least one peer address".to_owned(),
            ));
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
        loop {
            if tokio::time::Instant::now() >= deadline {
                return Err(ZeroBootError::Group("zero join timed out".to_owned()));
            }
            for peer in peers {
                match Self::join_via(peer, my_id, my_addr).await {
                    Ok(()) => return Ok(()),
                    Err(err) => tracing::warn!("zero join via {peer} failed: {err}"),
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    /// Asks one peer's leader path to seat this zero as a voter.
    async fn join_via(peer: &str, my_id: u64, my_addr: &str) -> Result<(), ZeroBootError> {
        let endpoint = tonic::transport::Endpoint::from_shared(format!("http://{peer}"))
            .map_err(|err| ZeroBootError::Group(err.to_string()))?;
        let channel = endpoint
            .connect()
            .await
            .map_err(|err| ZeroBootError::Group(err.to_string()))?;
        let mut client = datastreamdb_proto::pb::raft_client::RaftClient::new(channel);
        client
            .join_cluster(datastreamdb_proto::pb::RaftContext {
                id: my_id,
                addr: my_addr.to_owned(),
                group: 0,
                ..datastreamdb_proto::pb::RaftContext::default()
            })
            .await
            .map(|_| ())
            .map_err(|err| ZeroBootError::Group(err.to_string()))
    }

    /// Serves HTTP + gRPC until a listener fails (boot fully readies).
    ///
    /// # Errors
    ///
    /// Returns [`ZeroBootError`] when a listener fails.
    pub async fn serve(self) -> Result<(), ZeroBootError> {
        let (http_task, grpc_task) = self.servers;
        let (http_out, grpc_out) = tokio::join!(http_task, grpc_task);
        http_out.map_err(|err| ZeroBootError::Storage(err.to_string()))??;
        grpc_out.map_err(|err| ZeroBootError::Storage(err.to_string()))??;
        Ok(())
    }

    /// Aborts serving tasks and shuts the zero group down.
    pub async fn shutdown(self) {
        let (http_task, grpc_task) = self.servers;
        http_task.abort();
        grpc_task.abort();
        self.node.shutdown().await;
    }
}

/// Shared state behind zero HTTP routes.
#[derive(Clone)]
struct HttpState {
    /// Cluster state.
    state: Arc<ZeroState>,
    /// gRPC service (RPC reuse for admin routes).
    service: ZeroService,
    /// Advertised address.
    addr: String,
}

/// Builds the zero HTTP router.
fn router(state: HttpState) -> axum::Router {
    use axum::routing::get;
    axum::Router::new()
        .route("/health", get(health))
        .route("/state", get(cluster_state))
        .route("/assign", get(assign))
        .route("/removeNode", get(remove_node))
        .with_state(state)
}

/// Health payload.
#[derive(serde::Serialize)]
struct Health {
    /// Always `"ok"` when the router serves.
    status: &'static str,
    /// Crate version of the serving binary.
    version: &'static str,
    /// Advertised address.
    instance: String,
}

async fn health(
    axum::extract::State(state): axum::extract::State<HttpState>,
) -> axum::Json<Health> {
    axum::Json(Health {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        instance: state.addr.clone(),
    })
}

/// Membership snapshot as plain JSON.
async fn cluster_state(
    axum::extract::State(state): axum::extract::State<HttpState>,
) -> axum::Json<serde_json::Value> {
    let view = state.state.snapshot_state();
    let groups: serde_json::Map<String, serde_json::Value> = view
        .groups
        .into_iter()
        .map(|(gid, group)| {
            let members: Vec<serde_json::Value> = group
                .members
                .into_values()
                .map(|member| {
                    serde_json::json!({
                        "id": member.id,
                        "addr": member.addr,
                        "leader": member.leader,
                    })
                })
                .collect();
            let tablets: Vec<&str> = group.tablets.keys().map(String::as_str).collect();
            (
                gid.to_string(),
                serde_json::json!({"members": members, "tablets": tablets}),
            )
        })
        .collect();
    axum::Json(serde_json::json!({
        "counter": view.counter,
        "groups": groups,
        "maxUid": view.max_uid,
        "maxTxnTs": view.max_txn_ts,
        "maxNsId": view.max_ns_id,
        "maxRaftId": view.max_raft_id,
        "cid": view.cid,
    }))
}

/// Lease query: `?what=uid|txn|ns&num=N[&bump=true]`.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct AssignQuery {
    /// Lease kind.
    #[serde(default)]
    what: String,
    /// How many IDs.
    #[serde(default)]
    num: u64,
}

/// `GET /assign`: lease IDs over HTTP (debugging surface).
async fn assign(
    axum::extract::State(state): axum::extract::State<HttpState>,
    axum::extract::Query(query): axum::extract::Query<AssignQuery>,
) -> axum::response::Response {
    use datastreamdb_proto::pb::num::LeaseType;
    use datastreamdb_proto::pb::zero_server::Zero as _;
    let typ = match query.what.as_str() {
        "uid" => LeaseType::Uid,
        "txn" | "ts" => LeaseType::TxnTs,
        "ns" => LeaseType::NsId,
        _ => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                "what must be uid, txn, or ns",
            )
                .into_response();
        }
    };
    let num = datastreamdb_proto::pb::Num {
        val: query.num.max(1),
        r#type: typ as i32,
        ..datastreamdb_proto::pb::Num::default()
    };
    match state.service.assign_ids(tonic::Request::new(num)).await {
        Ok(response) => {
            let ids = response.into_inner();
            axum::Json(serde_json::json!({
                "startId": ids.start_id,
                "endId": ids.end_id,
                "readOnly": ids.read_only,
            }))
            .into_response()
        }
        Err(status) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            status.message().to_owned(),
        )
            .into_response(),
    }
}

/// Remove-node query: `?group=G&id=N`.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoveQuery {
    /// Group ID.
    #[serde(default)]
    group: u32,
    /// Raft ID.
    #[serde(default)]
    id: u64,
}

/// `GET /removeNode`: drop a member.
async fn remove_node(
    axum::extract::State(state): axum::extract::State<HttpState>,
    axum::extract::Query(query): axum::extract::Query<RemoveQuery>,
) -> axum::response::Response {
    use datastreamdb_proto::pb::zero_server::Zero as _;
    let req = datastreamdb_proto::pb::RemoveNodeRequest {
        group_id: query.group,
        node_id: query.id,
    };
    match state.service.remove_node(tonic::Request::new(req)).await {
        Ok(_) => (axum::http::StatusCode::OK, "removed").into_response(),
        Err(status) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            status.message().to_owned(),
        )
            .into_response(),
    }
}
