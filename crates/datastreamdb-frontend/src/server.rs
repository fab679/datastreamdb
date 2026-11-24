//! Alpha server assembly: engines, group, and serving.
//!
//! [`AlphaServer::boot`] runs the startup sequence (engines, zero join,
//! group bootstrap, UID lease, background streams); [`AlphaServer::serve`]
//! exposes the gRPC (`DataStreamDB`, `Worker`, `Raft`, `RaftTransport`) and HTTP
//! surfaces until shutdown.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use datastreamdb_posting::PostingEngine;
use datastreamdb_proto::pb::{OracleDelta, Proposal};
use datastreamdb_query::QueryEngine;
use datastreamdb_raft::Node;
use datastreamdb_schema::SchemaState;
use datastreamdb_store::{FjallBackend, MemoryBackend};
use datastreamdb_worker::{
    AlphaApplier, GroupState, Options as WorkerOptions, WorkerService, ZeroConn,
};
use prost::Message as _;
use tokio::sync::mpsc;

use crate::http::{HttpState, router};
use crate::service::DataStreamDBService;
use crate::txn::TxnCoordinator;

/// Alpha configuration (flags in `datastreamdb-server`, struct here).
#[derive(Debug, Clone)]
pub struct AlphaConfig {
    /// HTTP address (`127.0.0.1:8080`).
    pub http_addr: SocketAddr,
    /// gRPC address for every service (`127.0.0.1:7080`).
    pub grpc_addr: SocketAddr,
    /// This alpha's advertised address (`ip:grpc-port`).
    pub my_addr: String,
    /// Zero addresses to join.
    pub zero_addrs: Vec<String>,
    /// Posting directory (`:memory:` for the memory backend).
    pub posting_dir: PathBuf,
    /// WAL directory for the group log.
    pub wal_dir: PathBuf,
    /// Raft group to serve (1 unless sharded).
    pub group_id: u32,
    /// Bootstrap a fresh group instead of joining one.
    pub bootstrap: bool,
    /// Worker options (mutation modes, caches).
    pub options: WorkerOptions,
    /// Raft heartbeat interval in milliseconds.
    pub heartbeat_ms: u64,
    /// Raft election timeout range in milliseconds.
    pub election_ms: (u64, u64),
    /// ACL settings (`None` leaves every request open, like ACL-off mode).
    pub acl: Option<crate::auth::AclSettings>,
    /// Audit settings (empty output disables auditing).
    pub audit: datastreamdb_audit::AuditSettings,
}

/// Boot failures.
#[derive(Debug, thiserror::Error)]
pub enum BootError {
    /// Zero is unreachable.
    #[error("cannot reach zero: {0}")]
    Zero(String),
    /// Group bootstrap/join failed.
    #[error("group failed: {0}")]
    Group(String),
    /// Storage failure.
    #[error("storage failed: {0}")]
    Storage(String),
    /// Engine failure.
    #[error("engine failed: {0}")]
    Engine(String),
    /// Bad flag/config value.
    #[error("bad config: {0}")]
    Config(String),
}

/// Serving tasks: HTTP router plus the combined gRPC server.
type ServerTasks = (
    tokio::task::JoinHandle<Result<(), BootError>>,
    tokio::task::JoinHandle<Result<(), BootError>>,
);

/// One running alpha: engines, group, clients, and serving handles.
pub struct AlphaServer {
    /// Transaction coordinator (`DataStreamDB` + HTTP handlers share it).
    pub coordinator: Arc<TxnCoordinator>,
    /// Membership/tablet view.
    pub groups: Arc<GroupState>,
    /// Group raft node.
    pub node: Node<AlphaApplier>,
    /// Background tasks (aborted on shutdown).
    tasks: Vec<tokio::task::JoinHandle<()>>,
    /// Serving tasks (HTTP, gRPC).
    servers: ServerTasks,
}

impl AlphaServer {
    /// Boots engines, joins zero + group, and starts background streams.
    ///
    /// # Errors
    ///
    /// Returns [`BootError`] when storage, zero, or the group fails.
    pub async fn boot(config: AlphaConfig) -> Result<Self, BootError> {
        let started = Instant::now();
        config
            .options
            .validate()
            .map_err(|err| BootError::Storage(err.to_string()))?;

        let (posting, query) = Self::open_engines(&config)?;
        let (groups, zero, my_id, group_id) = Self::join_zero(&config).await?;
        let config = AlphaConfig { group_id, ..config };
        // ACL rule refresh fires when finalized txns touch ACL predicates.
        let acl_refresh = Self::acl_refresh_notify(&config);
        let cdc = Self::build_cdc(&config)?;
        let (node, applier) = Self::open_group(
            &config,
            &posting,
            &groups,
            my_id,
            acl_refresh.clone(),
            cdc.clone(),
        )
        .await?;
        Self::install_uid_lease(&query, &zero).await?;

        // Services.
        let worker = Arc::new(WorkerService::new(
            posting.clone(),
            groups.clone(),
            node.clone(),
            applier.clone(),
            config.options.clone(),
        ));
        let coordinator = TxnCoordinator::new(query, groups.clone(), zero.clone(), worker.clone());
        let coordinator = Arc::new(Self::attach_auth(coordinator, &config)?);
        // Serving tasks first: joiners must be dialable (leader dials back).
        let auditor = Self::build_auditor(&config)?;
        let http_router = router(HttpState {
            coordinator: coordinator.clone(),
            groups: groups.clone(),
            started,
            addr: config.my_addr.clone(),
            auditor: auditor.clone(),
            admin: None,
        });
        let http_task = Self::spawn_http(http_router, config.http_addr);
        let grpc_task = Self::spawn_grpc(
            &coordinator,
            &posting,
            &groups,
            &node,
            &applier,
            &config,
            auditor.clone(),
        );
        let servers: ServerTasks = (http_task, grpc_task);
        // Background streams start before joining: the join itself needs
        // live membership (leader flags), and deltas must survive it.
        let mut tasks = Self::spawn_streams(&zero, &node, &posting);
        // Join only after serving: the leader dials back for replication.
        tokio::time::sleep(Duration::from_millis(200)).await;
        Self::join_or_bootstrap(
            &node,
            &groups,
            config.bootstrap,
            my_id,
            config.group_id,
            &config.my_addr,
        )
        .await?;
        node.wait_for_leader(Duration::from_secs(60))
            .await
            .map_err(|err| BootError::Group(err.to_string()))?;

        // ACL bootstrap (guardians + groot) runs once, on the group
        // bootstrapper, after the group can take mutations.
        if config.bootstrap {
            coordinator
                .bootstrap_acl(datastreamdb_x::ROOT_NAMESPACE, None)
                .await
                .map_err(|err| BootError::Engine(err.to_string()))?;
        }

        // Leadership reporting: zero learns group leaders for joins.
        tasks.push(Self::spawn_leadership_report(&node, &groups, &zero, my_id));

        // ACL rule refresh: finalized rule writes reload the cache.
        if let Some(notify) = acl_refresh {
            tasks.push(Self::spawn_acl_refresh(&coordinator, notify));
        }

        // CDC flush-mark replication for failover continuity.
        if let Some(emitter) = cdc {
            tasks.push(Self::spawn_cdc_state(&node, &emitter));
        }

        Ok(Self {
            coordinator,
            groups,
            node,
            tasks,
            servers,
        })
    }

    /// Spawns the zero background streams (membership, oracle, deltas).
    ///
    /// Watermark-only deltas apply directly; decision deltas propose
    /// through the group so followers stage before finalizing.
    fn spawn_streams(
        zero: &Arc<ZeroConn>,
        node: &Node<AlphaApplier>,
        posting: &Arc<PostingEngine>,
    ) -> Vec<tokio::task::JoinHandle<()>> {
        let mut tasks = Vec::new();
        tasks.push(tokio::spawn({
            let zero = zero.clone();
            async move { zero.run_membership_stream().await }
        }));
        let (delta_tx, mut delta_rx) = mpsc::channel::<OracleDelta>(1024);
        tasks.push(tokio::spawn({
            let zero = zero.clone();
            async move { zero.run_oracle_stream(delta_tx).await }
        }));
        tasks.push(tokio::spawn({
            let node = node.clone();
            let posting = posting.clone();
            async move {
                while let Some(delta) = delta_rx.recv().await {
                    if delta.txns.is_empty() {
                        // Watermark-only (lease advances): no stage/finalize
                        // ordering to protect, so skip raft like Go does.
                        posting.process_delta(&delta);
                        continue;
                    }
                    let proposal = Proposal {
                        delta: Some(delta),
                        ..Proposal::default()
                    };
                    // Pre-join proposes fail with NotLeader; retry instead
                    // of dying (finalize is idempotent, so no delta is lost
                    // or applied twice).
                    while node.propose(proposal.encode_to_vec()).await.is_err() {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
            }
        }));
        tasks
    }

    /// Builds the CDC emitter from config (`None` when disabled).
    fn build_cdc(
        config: &AlphaConfig,
    ) -> Result<Option<std::sync::Arc<datastreamdb_worker::cdc::Cdc>>, BootError> {
        if config.options.cdc_conf.trim().is_empty() {
            return Ok(None);
        }
        let parsed = datastreamdb_worker::cdc::parse_cdc_flag(&config.options.cdc_conf)
            .map_err(|err| BootError::Config(err.to_string()))?;
        let Some(sink) = datastreamdb_worker::cdc::sink_for_config(&parsed)
            .map_err(|err| BootError::Config(err.to_string()))?
        else {
            return Ok(None);
        };
        Ok(Some(std::sync::Arc::new(
            datastreamdb_worker::cdc::Cdc::new(std::sync::Arc::new(sink)),
        )))
    }

    /// Replicates the CDC flush mark while leading (Go's 3-minute
    /// `proposalTick`; here every minute).
    fn spawn_cdc_state(
        node: &Node<AlphaApplier>,
        cdc: &std::sync::Arc<datastreamdb_worker::cdc::Cdc>,
    ) -> tokio::task::JoinHandle<()> {
        let node = node.clone();
        let cdc = cdc.clone();
        tokio::spawn(async move {
            let mut last_sent = 0;
            loop {
                tokio::time::sleep(Duration::from_secs(60)).await;
                if !node.is_leader().await {
                    continue;
                }
                let sent = cdc.sent_ts();
                if sent == last_sent {
                    continue;
                }
                let proposal = datastreamdb_proto::pb::Proposal {
                    cdc_state: Some(datastreamdb_proto::pb::CdcState { sent_ts: sent }),
                    ..datastreamdb_proto::pb::Proposal::default()
                };
                let _ = node.propose(prost::Message::encode_to_vec(&proposal)).await;
                last_sent = sent;
            }
        })
    }

    /// Builds the auditor from config (`None` when output is empty).
    fn build_auditor(
        config: &AlphaConfig,
    ) -> Result<Option<datastreamdb_audit::SharedAuditor>, BootError> {
        if config.audit.output.is_empty() {
            return Ok(None);
        }
        let sink: datastreamdb_audit::SharedSink = if config.audit.output == "stdout" {
            std::sync::Arc::new(datastreamdb_audit::StdoutSink::new())
        } else {
            let max_bytes = config.audit.size_mb.saturating_mul(1024 * 1024);
            let max_age = std::time::Duration::from_secs(config.audit.days.saturating_mul(86_400));
            std::sync::Arc::new(
                datastreamdb_audit::FileSink::open(
                    std::path::Path::new(&config.audit.output),
                    max_bytes,
                    max_age,
                )
                .map_err(|err| BootError::Storage(err.to_string()))?,
            )
        };
        Ok(Some(std::sync::Arc::new(datastreamdb_audit::Auditor::new(
            config.my_addr.clone(),
            config.acl.is_some(),
            vec![sink],
        ))))
    }

    /// Installs the auth context from config (no-op without ACL).
    fn attach_auth(
        coordinator: TxnCoordinator,
        config: &AlphaConfig,
    ) -> Result<TxnCoordinator, BootError> {
        let Some(settings) = &config.acl else {
            return Ok(coordinator);
        };
        let auth = crate::auth::AuthContext::from_settings(settings)
            .map_err(|err| BootError::Engine(err.to_string()))?;
        Ok(coordinator.with_auth(Arc::new(auth)))
    }

    /// Creates the ACL refresh signal when ACL is configured.
    fn acl_refresh_notify(config: &AlphaConfig) -> Option<Arc<tokio::sync::Notify>> {
        config
            .acl
            .as_ref()
            .map(|_| Arc::new(tokio::sync::Notify::new()))
    }

    /// Reloads ACL rules whenever finalized txns touch ACL predicates.
    fn spawn_acl_refresh(
        coordinator: &Arc<TxnCoordinator>,
        notify: Arc<tokio::sync::Notify>,
    ) -> tokio::task::JoinHandle<()> {
        let coordinator = coordinator.clone();
        tokio::spawn(async move {
            loop {
                notify.notified().await;
                if let Err(err) = coordinator
                    .refresh_acl_cache(datastreamdb_x::ROOT_NAMESPACE)
                    .await
                {
                    tracing::warn!("acl refresh failed: {err}");
                }
            }
        })
    }

    /// Serves the gRPC surfaces (`DataStreamDB`, Worker, Raft, transport).
    #[allow(clippy::too_many_arguments)]
    fn spawn_grpc(
        coordinator: &Arc<TxnCoordinator>,
        posting: &Arc<PostingEngine>,
        groups: &Arc<GroupState>,
        node: &Node<AlphaApplier>,
        applier: &AlphaApplier,
        config: &AlphaConfig,
        auditor: Option<datastreamdb_audit::SharedAuditor>,
    ) -> tokio::task::JoinHandle<Result<(), BootError>> {
        let datastreamdb = DataStreamDBService::new(coordinator.clone(), auditor);
        let worker_service = WorkerService::new(
            posting.clone(),
            groups.clone(),
            node.clone(),
            applier.clone(),
            config.options.clone(),
        );
        let raft_service = datastreamdb_worker::RaftService::new(node.clone(), groups.clone());
        let transport = datastreamdb_raft::TransportServer::new(node.handle());
        let grpc_addr = config.grpc_addr;
        tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(datastreamdb_proto::api::data_stream_db_server::DataStreamDbServer::new(datastreamdb))
                .add_service(datastreamdb_proto::pb::worker_server::WorkerServer::new(
                    worker_service,
                ))
                .add_service(datastreamdb_proto::pb::raft_server::RaftServer::new(raft_service))
                .add_service(
                    datastreamdb_raft::network::transport::raft_transport_server::RaftTransportServer::new(
                        transport,
                    ),
                )
                .serve(grpc_addr)
                .await
                .map_err(|err| BootError::Storage(err.to_string()))
        })
    }

    /// Serves the HTTP router until the listener fails.
    fn spawn_http(
        http_router: axum::Router,
        http_addr: SocketAddr,
    ) -> tokio::task::JoinHandle<Result<(), BootError>> {
        tokio::spawn(async move {
            let listener = tokio::net::TcpListener::bind(http_addr)
                .await
                .map_err(|err| BootError::Storage(err.to_string()))?;
            axum::serve(
                listener,
                http_router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .map_err(|err| BootError::Storage(err.to_string()))
        })
    }

    /// Reports this member's leadership flag to zero every two seconds.
    fn spawn_leadership_report(
        node: &Node<AlphaApplier>,
        groups: &Arc<GroupState>,
        zero: &Arc<ZeroConn>,
        my_id: u64,
    ) -> tokio::task::JoinHandle<()> {
        let node = node.clone();
        let groups = groups.clone();
        let zero = zero.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                let leader = node.is_leader().await;
                let mut members = groups.members(groups.group_id());
                if let Some(member) = members.get_mut(&my_id) {
                    member.leader = leader;
                }
                let group = datastreamdb_proto::pb::Group {
                    members,
                    ..datastreamdb_proto::pb::Group::default()
                };
                let _ = zero.update_membership(group).await;
            }
        })
    }

    /// Serves HTTP + gRPC until a listener fails (boot fully readies).
    ///
    /// # Errors
    ///
    /// Returns [`BootError`] when a listener fails.
    pub async fn serve(self) -> Result<(), BootError> {
        let (http_task, grpc_task) = self.servers;
        let (http_out, grpc_out) = tokio::join!(http_task, grpc_task);
        http_out.map_err(|err| BootError::Storage(err.to_string()))??;
        grpc_out.map_err(|err| BootError::Storage(err.to_string()))??;
        Ok(())
    }

    /// Bootstraps a fresh group or joins an existing one via its leader.
    async fn join_or_bootstrap(
        node: &Node<AlphaApplier>,
        groups: &Arc<GroupState>,
        bootstrap: bool,
        my_id: u64,
        group_id: u32,
        my_addr: &str,
    ) -> Result<(), BootError> {
        if bootstrap {
            let mut voters = std::collections::BTreeMap::new();
            voters.insert(my_id, my_addr.to_owned());
            match node.initialize(voters).await {
                Ok(()) | Err(datastreamdb_raft::NodeError::AlreadyInitialized) => return Ok(()),
                Err(err) => return Err(BootError::Group(err.to_string())),
            }
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
        loop {
            if tokio::time::Instant::now() >= deadline {
                return Err(BootError::Group("group join timed out".to_owned()));
            }
            let Some(leader) = groups.leader_of(group_id) else {
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            };
            match Self::join_via(&leader.addr, my_id, group_id, my_addr).await {
                Ok(()) => return Ok(()),
                Err(_) => tokio::time::sleep(Duration::from_secs(1)).await,
            }
        }
    }

    /// Asks the group leader to seat this member as a voter.
    async fn join_via(
        leader_addr: &str,
        my_id: u64,
        group_id: u32,
        my_addr: &str,
    ) -> Result<(), BootError> {
        let endpoint = tonic::transport::Endpoint::from_shared(format!("http://{leader_addr}"))
            .map_err(|err| BootError::Group(err.to_string()))?;
        let channel = endpoint
            .connect()
            .await
            .map_err(|err| BootError::Group(err.to_string()))?;
        let mut client = datastreamdb_proto::pb::raft_client::RaftClient::new(channel);
        client
            .join_cluster(datastreamdb_proto::pb::RaftContext {
                id: my_id,
                addr: my_addr.to_owned(),
                group: group_id,
                ..datastreamdb_proto::pb::RaftContext::default()
            })
            .await
            .map_err(|err| BootError::Group(err.to_string()))?;
        Ok(())
    }

    /// Opens the posting store and query engine.
    fn open_engines(
        config: &AlphaConfig,
    ) -> Result<(Arc<PostingEngine>, Arc<QueryEngine>), BootError> {
        let store: Arc<dyn datastreamdb_store::Backend> =
            if config.posting_dir.as_os_str() == ":memory:" {
                Arc::new(MemoryBackend::new())
            } else {
                std::fs::create_dir_all(&config.posting_dir)
                    .map_err(|err| BootError::Storage(err.to_string()))?;
                Arc::new(
                    FjallBackend::open(&config.posting_dir)
                        .map_err(|err| BootError::Storage(err.to_string()))?,
                )
            };
        let schema = Arc::new(SchemaState::new());
        // Initial schema (plus ACL predicates when configured), like Go's
        // boot-time schematize. Schema state is memory-only until the
        // disk-sync slice, so reinstalling unconditionally is correct.
        let acl_on = config.acl.is_some();
        for update in datastreamdb_schema::initial_schema(datastreamdb_x::ROOT_NAMESPACE, acl_on) {
            schema.set(&update.predicate.clone(), update);
        }
        for update in datastreamdb_schema::initial_types(datastreamdb_x::ROOT_NAMESPACE, acl_on) {
            schema.set_type(&update.type_name.clone(), update);
        }
        let cache = usize::try_from(config.options.cache_mb.max(1)).unwrap_or(64);
        let posting = PostingEngine::open(store, schema, cache);
        let query =
            QueryEngine::open(posting.clone()).map_err(|err| BootError::Engine(err.to_string()))?;
        Ok((posting, query))
    }

    /// Joins zero (id 0 requests an assignment) and seeds membership.
    ///
    /// Returns the adopted raft and group IDs (zero may place us).
    async fn join_zero(
        config: &AlphaConfig,
    ) -> Result<(Arc<GroupState>, Arc<ZeroConn>, u64, u32), BootError> {
        let groups = Arc::new(GroupState::new(0, config.group_id, config.my_addr.clone()));
        let zero = Arc::new(ZeroConn::new(
            config.zero_addrs.clone(),
            groups.clone(),
            0,
            config.my_addr.clone(),
            config.group_id,
        ));
        let joined = zero
            .connect()
            .await
            .map_err(|err| BootError::Zero(err.to_string()))?;
        let member = joined
            .member
            .ok_or_else(|| BootError::Zero("zero assigned no member".to_owned()))?;
        if member.id == 0 {
            return Err(BootError::Zero("zero assigned no raft id".to_owned()));
        }
        groups.set_ids(member.id, member.group_id);
        if let Some(state) = joined.state {
            groups.apply_state(&state);
        }
        Ok((groups, zero, member.id, member.group_id))
    }

    /// Opens the group raft node, bootstrapping or joining as configured.
    async fn open_group(
        config: &AlphaConfig,
        posting: &Arc<PostingEngine>,
        groups: &Arc<GroupState>,
        my_id: u64,
        acl_refresh: Option<Arc<tokio::sync::Notify>>,
        cdc: Option<Arc<datastreamdb_worker::cdc::Cdc>>,
    ) -> Result<(Node<AlphaApplier>, AlphaApplier), BootError> {
        std::fs::create_dir_all(&config.wal_dir)
            .map_err(|err| BootError::Storage(err.to_string()))?;
        let group_dir = config.wal_dir.join(format!("group-{}", config.group_id));
        std::fs::create_dir_all(&group_dir).map_err(|err| BootError::Storage(err.to_string()))?;
        let mut applier = AlphaApplier::new(posting.clone(), config.group_id, Some(groups.clone()));
        if let Some(notify) = acl_refresh {
            applier = applier.with_refresh_notify(notify);
        }
        if let Some(emitter) = cdc.clone() {
            applier = applier.with_cdc(emitter);
        }
        let raft_config = Arc::new(openraft::Config {
            heartbeat_interval: config.heartbeat_ms,
            election_timeout_min: config.election_ms.0,
            election_timeout_max: config.election_ms.1,
            ..Default::default()
        });
        // Multi-alpha groups dial over TCP (single members never use it).
        let node = Node::open(
            my_id,
            &group_dir,
            raft_config,
            applier.clone(),
            datastreamdb_raft::TonicFactory::new(my_id),
        )
        .await
        .map_err(|err| BootError::Group(err.to_string()))?;
        Ok((node, applier))
    }

    /// Bumps zero past the local high-water mark, then installs a UID lease.
    async fn install_uid_lease(
        query: &Arc<QueryEngine>,
        zero: &Arc<ZeroConn>,
    ) -> Result<(), BootError> {
        let local_max = query.allocator().next().saturating_sub(1).max(1);
        zero.bump_uids(local_max)
            .await
            .map_err(|err| BootError::Zero(err.to_string()))?;
        let lease = zero
            .assign_ids(datastreamdb_proto::pb::Num {
                val: 10_000,
                r#type: datastreamdb_proto::pb::num::LeaseType::Uid as i32,
                ..datastreamdb_proto::pb::Num::default()
            })
            .await
            .map_err(|err| BootError::Zero(err.to_string()))?;
        query
            .allocator()
            .install_lease(lease.start_id, lease.end_id);
        Ok(())
    }

    /// Aborts serving + background tasks and shuts the group node down.
    pub async fn shutdown(self) {
        let (http_task, grpc_task) = self.servers;
        http_task.abort();
        grpc_task.abort();
        for task in self.tasks {
            task.abort();
        }
        self.node.shutdown().await;
    }
}
