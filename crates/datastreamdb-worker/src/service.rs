//! `Worker` gRPC service (`worker/mutation.go`, `worker/task.go`).
//!
//! [`WorkerService`] serves one alpha group: `Mutate` (propose or forward),
//! `ServeTask` (local task processor), `Schema` (local schema slice),
//! `DeleteNamespace` (group-local propose), plus `Backup`, `Restore`, and
//! `Export` (local destinations; S3 stays `Unimplemented`), plus
//! `MovePredicate`/`ReceivePredicate` (zero orchestration deferred).
//! CDC and multi-group sort stay `Unimplemented` until M6.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use datastreamdb_posting::PostingEngine;
use datastreamdb_proto::Directive;
use datastreamdb_proto::api::TxnContext;
use datastreamdb_proto::pb::{
    BackupRequest, BackupResponse, DeleteNsRequest, ExportRequest, ExportResponse, Kvs,
    MovePredicatePayload, Mutations, Proposal, RestoreRequest, SchemaNode, SchemaRequest,
    SchemaResult, Snapshot as PbSnapshot, SortMessage, SortResult, Status, SubscriptionRequest,
    TaskStatusRequest, TaskStatusResponse, UpdateGraphQlSchemaRequest, UpdateGraphQlSchemaResponse,
    worker_client::WorkerClient, worker_server::Worker,
};
use datastreamdb_proto::{TaskQuery, TaskResult};
use datastreamdb_raft::{Node, NodeError};
use prost::Message as _;
use tonic::transport::Channel;

use crate::applier::{AlphaApplier, conflict_key_string, parse_outcome, txn_pred_keys};
use crate::config::{MutationsMode, Options};
use crate::groups::GroupState;

/// Default schema fields when the request names none (mirrors Go).
const ALL_SCHEMA_FIELDS: [&str; 11] = [
    "type",
    "index",
    "tokenizer",
    "reverse",
    "count",
    "list",
    "upsert",
    "unique",
    "lang",
    "noconflict",
    "vector_specs",
];

/// Worker RPC errors shared by handlers.
fn not_served(group_id: u32) -> tonic::Status {
    tonic::Status::failed_precondition(format!("this server doesn't serve group id: {group_id}"))
}

/// Alpha group server: engine, membership, raft, and peer dialing.
pub struct WorkerService {
    /// Posting engine (tasks, schema, oracle).
    engine: Arc<PostingEngine>,
    /// Membership/tablet view.
    groups: Arc<GroupState>,
    /// Group raft node (proposals).
    node: Node<AlphaApplier>,
    /// Applier handle (staged conflict keys for commit contexts).
    applier: AlphaApplier,
    /// Server options (mutation modes).
    options: Options,
    /// Peer worker clients by address.
    peers: Mutex<HashMap<String, WorkerClient<Channel>>>,
}

impl WorkerService {
    /// Serves `engine`/`groups`/`node` under `options`.
    pub fn new(
        engine: Arc<PostingEngine>,
        groups: Arc<GroupState>,
        node: Node<AlphaApplier>,
        applier: AlphaApplier,
        options: Options,
    ) -> Self {
        Self {
            engine,
            groups,
            node,
            applier,
            options,
            peers: Mutex::new(HashMap::new()),
        }
    }

    /// Dials (caching) a peer worker.
    async fn peer(&self, addr: &str) -> Result<WorkerClient<Channel>, tonic::Status> {
        if let Some(client) = self.peers.lock().expect("peer lock").get(addr).cloned() {
            return Ok(client);
        }
        let endpoint = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
            .map_err(|err| tonic::Status::invalid_argument(err.to_string()))?;
        let channel = endpoint
            .connect()
            .await
            .map_err(|err| tonic::Status::unavailable(err.to_string()))?;
        let client = WorkerClient::new(channel);
        self.peers
            .lock()
            .expect("peer lock")
            .insert(addr.to_owned(), client.clone());
        Ok(client)
    }

    /// Leader address of `gid`, if one is flagged.
    fn leader_addr(&self, gid: u32) -> Option<String> {
        self.groups.leader_of(gid).map(|member| member.addr)
    }

    /// Proposes mutations locally and fills the commit context.
    async fn propose_local(&self, mutations: &Mutations) -> Result<TxnContext, tonic::Status> {
        let engine = Arc::clone(&self.engine);
        let start_ts = mutations.start_ts;
        // Blocking oracle wait goes on the blocking pool (up to 10 s).
        tokio::task::spawn_blocking(move || engine.oracle().wait_for_ts(start_ts))
            .await
            .map_err(|err| tonic::Status::internal(err.to_string()))?;

        let proposal = Proposal {
            mutations: Some(mutations.clone()),
            ..Proposal::default()
        };
        let response =
            self.node
                .propose(proposal.encode_to_vec())
                .await
                .map_err(|err| match err {
                    NodeError::NotLeader { leader } => {
                        let mut status = tonic::Status::unavailable("group has no leader");
                        if let Some(id) = leader {
                            status = tonic::Status::unavailable(format!("group led by {id}"));
                        }
                        status
                    }
                    other => tonic::Status::internal(other.to_string()),
                })?;
        parse_outcome(&response).map_err(tonic::Status::aborted)?;

        let (conflicts, preds) = self.applier.staged_conflicts(mutations.start_ts);
        let mut keys: Vec<String> = conflicts.into_iter().map(conflict_key_string).collect();
        keys.sort();
        Ok(TxnContext {
            start_ts: mutations.start_ts,
            keys,
            preds: txn_pred_keys(self.groups.group_id(), &preds),
            ..TxnContext::default()
        })
    }

    /// Serves one predicate's schema slice with field filtering.
    fn populate_schema(&self, attr: &str, fields: &[String]) -> Option<SchemaNode> {
        let schema = self.engine.schema();
        let update = schema.get(attr, false)?;
        let type_name = datastreamdb_types::TypeId::try_from(update.value_type)
            .map(|id| id.as_str().to_owned())
            .unwrap_or_default();
        let mut node = SchemaNode {
            predicate: attr.to_owned(),
            ..SchemaNode::default()
        };
        for field in fields {
            match field.as_str() {
                "type" => node.r#type.clone_from(&type_name),
                "index" => node.index = !update.tokenizer.is_empty(),
                "tokenizer" => {
                    if !update.tokenizer.is_empty() {
                        node.tokenizer.clone_from(&update.tokenizer);
                    }
                    if !update.index_specs.is_empty() {
                        schema
                            .vector_indexes(attr, false)
                            .clone_into(&mut node.tokenizer);
                    }
                }
                "reverse" => node.reverse = update.directive == Directive::Reverse as i32,
                "count" => node.count = update.count,
                "list" => node.list = update.list,
                "upsert" => node.upsert = update.upsert,
                "unique" => node.unique = update.unique,
                "lang" => node.lang = update.lang,
                "noconflict" => node.no_conflict = update.no_conflict,
                "vector_specs" => node.index_specs.clone_from(&update.index_specs),
                _ => {}
            }
        }
        Some(node)
    }
}

/// Fan-out helper: schema slices from every named group.
///
/// `dial` connects to a group leader; local groups answer directly. Mirrors
/// Go's `GetSchemaOverNetwork` (without its cross-group retry).
///
/// # Errors
///
/// Returns [`tonic::Status`] when a group has no leader or a peer fails.
pub async fn schema_over_network<F, Fut>(
    groups: &Arc<GroupState>,
    local: &Arc<PostingEngine>,
    request: &SchemaRequest,
    mut dial: F,
) -> Result<SchemaResult, tonic::Status>
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<WorkerClient<Channel>, tonic::Status>>,
{
    use std::collections::BTreeMap;
    let mut by_group: BTreeMap<u32, SchemaRequest> = BTreeMap::new();
    if request.predicates.is_empty() {
        for gid in groups.known_groups() {
            if gid == 0 {
                continue;
            }
            by_group.insert(
                gid,
                SchemaRequest {
                    group_id: gid,
                    fields: request.fields.clone(),
                    ..SchemaRequest::default()
                },
            );
        }
    } else {
        for attr in &request.predicates {
            let Some(gid) = groups.belongs_to(attr) else {
                continue;
            };
            by_group
                .entry(gid)
                .or_insert_with(|| SchemaRequest {
                    group_id: gid,
                    fields: request.fields.clone(),
                    ..SchemaRequest::default()
                })
                .predicates
                .push(attr.clone());
        }
    }
    let mut out = SchemaResult::default();
    for (gid, req) in &by_group {
        if groups.serves_group(*gid) {
            let schema = local.schema();
            let preds = if req.predicates.is_empty() {
                schema.predicates()
            } else {
                req.predicates.clone()
            };
            let fields = if req.fields.is_empty() {
                ALL_SCHEMA_FIELDS.iter().map(ToString::to_string).collect()
            } else {
                req.fields.clone()
            };
            for attr in &preds {
                let Some(update) = schema.get(attr, false) else {
                    continue;
                };
                let type_name = datastreamdb_types::TypeId::try_from(update.value_type)
                    .map(|id| id.as_str().to_owned())
                    .unwrap_or_default();
                let mut node = SchemaNode {
                    predicate: attr.clone(),
                    ..SchemaNode::default()
                };
                for field in &fields {
                    match field.as_str() {
                        "type" => node.r#type.clone_from(&type_name),
                        "index" => node.index = !update.tokenizer.is_empty(),
                        "tokenizer" => node.tokenizer.clone_from(&update.tokenizer),
                        "reverse" => node.reverse = update.directive == Directive::Reverse as i32,
                        "count" => node.count = update.count,
                        "list" => node.list = update.list,
                        "upsert" => node.upsert = update.upsert,
                        "unique" => node.unique = update.unique,
                        "lang" => node.lang = update.lang,
                        "noconflict" => node.no_conflict = update.no_conflict,
                        "vector_specs" => node.index_specs.clone_from(&update.index_specs),
                        _ => {}
                    }
                }
                out.schema.push(node);
            }
        } else {
            let Some(leader) = groups.leader_of(*gid) else {
                return Err(tonic::Status::unavailable(format!(
                    "group {gid} has no leader"
                )));
            };
            let mut client = dial(leader.addr).await?;
            let remote = client.schema(req.clone()).await?.into_inner();
            out.schema.extend(remote.schema);
        }
    }
    Ok(out)
}

impl WorkerService {
    /// Groups a cluster operation covers: `only` alone when set,
    /// else every known group (this one when membership is empty).
    fn fanout_groups(&self, only: u32) -> Vec<u32> {
        if only != 0 {
            return vec![only];
        }
        let mut groups: Vec<u32> = self
            .groups
            .known_groups()
            .into_iter()
            .filter(|gid| *gid != 0)
            .collect();
        if groups.is_empty() {
            groups.push(self.groups.group_id());
        }
        groups.sort_unstable();
        groups.dedup();
        groups
    }

    /// Dials a group's leader for one fan-out leg.
    async fn group_leader(&self, gid: u32) -> Result<WorkerClient<Channel>, tonic::Status> {
        let Some(addr) = self.leader_addr(gid) else {
            return Err(tonic::Status::unavailable(format!(
                "group {gid} has no leader"
            )));
        };
        self.peer(&addr).await
    }

    /// Backs up the cluster (Go `ProcessBackupRequest` entry).
    ///
    /// # Errors
    ///
    /// Returns [`tonic::Status`] when a group fails or the
    /// destination is unusable.
    pub async fn backup_cluster(
        &self,
        req: &BackupRequest,
    ) -> Result<BackupResponse, tonic::Status> {
        crate::backup::process_backup_request(
            &self.engine,
            &self.groups,
            &self.peers,
            req,
            self.options.encryption_key.as_ref(),
        )
        .await
    }

    /// Exports every fanned-out group, merging file lists (Go
    /// `ExportOverNetwork` shape, one merged response).
    ///
    /// # Errors
    ///
    /// Returns [`tonic::Status`] when a group is unreachable or its
    /// export fails.
    pub async fn export_cluster(
        &self,
        req: &ExportRequest,
    ) -> Result<ExportResponse, tonic::Status> {
        let mut files = Vec::new();
        let mut failure: Option<(i32, String)> = None;
        for gid in self.fanout_groups(req.group_id) {
            let mut leg = req.clone();
            leg.group_id = gid;
            let response = if gid == self.groups.group_id() {
                Worker::export(self, tonic::Request::new(leg))
                    .await?
                    .into_inner()
            } else {
                let mut client = self.group_leader(gid).await?;
                client.export(leg).await?.into_inner()
            };
            if response.code != 0 && failure.is_none() {
                failure = Some((response.code, response.msg.clone()));
            }
            files.extend(response.files);
        }
        let (code, msg) = failure.unwrap_or_default();
        Ok(ExportResponse { code, msg, files })
    }

    /// Restores every fanned-out group (Go admin-restore shape).
    ///
    /// # Errors
    ///
    /// Returns [`tonic::Status`] when a group is unreachable or its
    /// restore fails.
    pub async fn restore_cluster(&self, req: &RestoreRequest) -> Result<Status, tonic::Status> {
        let mut failure: Option<Status> = None;
        for gid in self.fanout_groups(req.group_id) {
            let mut leg = req.clone();
            leg.group_id = gid;
            let response = if gid == self.groups.group_id() {
                Worker::restore(self, tonic::Request::new(leg))
                    .await?
                    .into_inner()
            } else {
                let mut client = self.group_leader(gid).await?;
                client.restore(leg).await?.into_inner()
            };
            if response.code != 0 && failure.is_none() {
                failure = Some(response);
            }
        }
        Ok(failure.unwrap_or_default())
    }

    /// Drops one namespace from every fanned-out group (Go
    /// `ProcessDeleteNsRequest`, minus the trailing zero send, which
    /// is the coordinator's job so tests can observe the order).
    ///
    /// # Errors
    ///
    /// Returns [`tonic::Status`] when a group is unreachable or its
    /// drop fails.
    pub async fn delete_namespace_cluster(&self, ns: u64) -> Result<(), tonic::Status> {
        for gid in self.fanout_groups(0) {
            let req = DeleteNsRequest {
                namespace: ns,
                group_id: gid,
            };
            if gid == self.groups.group_id() {
                Worker::delete_namespace(self, tonic::Request::new(req)).await?;
            } else {
                let mut client = self.group_leader(gid).await?;
                client.delete_namespace(req).await?;
            }
        }
        Ok(())
    }
}

#[tonic::async_trait]
impl Worker for WorkerService {
    async fn mutate(
        &self,
        request: tonic::Request<Mutations>,
    ) -> Result<tonic::Response<TxnContext>, tonic::Status> {
        let mutations = request.into_inner();
        if !self.groups.serves_group(mutations.group_id) {
            let Some(addr) = self.leader_addr(mutations.group_id) else {
                return Err(not_served(mutations.group_id));
            };
            let mut peer = self.peer(&addr).await?;
            return peer.mutate(mutations).await;
        }
        match self.options.mutations_mode {
            MutationsMode::Disallow => {
                return Err(tonic::Status::failed_precondition(
                    "mutations are disallowed",
                ));
            }
            MutationsMode::Strict => {
                for edge in &mutations.edges {
                    if self.engine.schema().type_of(&edge.attr).is_err() {
                        return Err(tonic::Status::failed_precondition(format!(
                            "strict mutations: unknown predicate {}",
                            edge.attr
                        )));
                    }
                }
            }
            MutationsMode::Allow => {}
        }
        match self.propose_local(&mutations).await {
            Ok(ctx) => Ok(tonic::Response::new(ctx)),
            Err(status) if status.code() == tonic::Code::Unavailable => {
                // Not the raft leader: forward once to the group leader.
                let Some(addr) = self.leader_addr(mutations.group_id) else {
                    return Err(status);
                };
                let mut peer = self.peer(&addr).await?;
                peer.mutate(mutations).await
            }
            Err(status) => Err(status),
        }
    }

    async fn serve_task(
        &self,
        request: tonic::Request<TaskQuery>,
    ) -> Result<tonic::Response<TaskResult>, tonic::Status> {
        let query = request.into_inner();
        let engine = Arc::clone(&self.engine);
        let cache = Arc::new(datastreamdb_posting::LocalCache::new(
            Arc::clone(&engine),
            query.read_ts,
        ));
        // Task processing blocks on timestamp waits; it must not stall a
        // runtime thread (single-threaded runtimes would freeze outright).
        tokio::task::spawn_blocking(move || {
            datastreamdb_task::process::process_task(&engine, &cache, query)
        })
        .await
        .map_err(|err| tonic::Status::internal(err.to_string()))?
        .map(tonic::Response::new)
        .map_err(|err| match &err {
            datastreamdb_task::errors::TaskError::Invalid(_) => {
                tonic::Status::invalid_argument(err.to_string())
            }
            _ => tonic::Status::internal(err.to_string()),
        })
    }

    type StreamSnapshotStream = futures::stream::Empty<Result<Kvs, tonic::Status>>;

    async fn stream_snapshot(
        &self,
        _request: tonic::Request<tonic::Streaming<PbSnapshot>>,
    ) -> Result<tonic::Response<Self::StreamSnapshotStream>, tonic::Status> {
        Err(tonic::Status::unimplemented(
            "tablet streaming snapshots need M6 predicate moves",
        ))
    }

    async fn sort(
        &self,
        _request: tonic::Request<SortMessage>,
    ) -> Result<tonic::Response<SortResult>, tonic::Status> {
        Err(tonic::Status::unimplemented(
            "distributed sort needs M6 multi-group ordering",
        ))
    }

    async fn schema(
        &self,
        request: tonic::Request<SchemaRequest>,
    ) -> Result<tonic::Response<SchemaResult>, tonic::Status> {
        let req = request.into_inner();
        if !self.groups.serves_group(req.group_id) {
            return Err(not_served(req.group_id));
        }
        let preds = if req.predicates.is_empty() {
            self.engine.schema().predicates()
        } else {
            req.predicates.clone()
        };
        let fields = if req.fields.is_empty() {
            ALL_SCHEMA_FIELDS.iter().map(ToString::to_string).collect()
        } else {
            req.fields.clone()
        };
        let mut out = SchemaResult::default();
        for attr in &preds {
            let Some(gid) = self.groups.belongs_to(attr) else {
                return Err(tonic::Status::not_found(format!("no tablet serves {attr}")));
            };
            if gid != self.groups.group_id() {
                continue;
            }
            if let Some(node) = self.populate_schema(attr, &fields) {
                out.schema.push(node);
            }
        }
        Ok(tonic::Response::new(out))
    }

    async fn backup(
        &self,
        request: tonic::Request<BackupRequest>,
    ) -> Result<tonic::Response<BackupResponse>, tonic::Status> {
        let engine = self.engine.clone();
        let groups = self.groups.clone();
        let key = self.options.encryption_key.clone();
        let req = request.into_inner();
        tokio::task::spawn_blocking(move || {
            crate::backup::run_backup_group(&engine, &groups, &req, key.as_ref())
        })
        .await
        .map_err(|err| tonic::Status::internal(format!("backup task failed: {err}")))?
        .map(tonic::Response::new)
    }

    async fn restore(
        &self,
        request: tonic::Request<RestoreRequest>,
    ) -> Result<tonic::Response<Status>, tonic::Status> {
        let engine = self.engine.clone();
        let groups = self.groups.clone();
        let key = self.options.encryption_key.clone();
        let req = request.into_inner();
        tokio::task::spawn_blocking(move || {
            crate::backup::run_restore(&engine, &groups, &req, key.as_ref())
        })
        .await
        .map_err(|err| tonic::Status::internal(format!("restore task failed: {err}")))?
        .map(tonic::Response::new)
    }

    async fn export(
        &self,
        request: tonic::Request<ExportRequest>,
    ) -> Result<tonic::Response<ExportResponse>, tonic::Status> {
        let engine = self.engine.clone();
        let groups = self.groups.clone();
        let key = self.options.encryption_key.clone();
        let req = request.into_inner();
        tokio::task::spawn_blocking(move || {
            crate::backup::run_export(&engine, &groups, &req, key.as_ref())
        })
        .await
        .map_err(|err| tonic::Status::internal(format!("export task failed: {err}")))?
        .map(tonic::Response::new)
    }

    async fn receive_predicate(
        &self,
        request: tonic::Request<tonic::Streaming<Kvs>>,
    ) -> Result<tonic::Response<datastreamdb_proto::api::Payload>, tonic::Status> {
        if !self.node.is_leader().await {
            return Err(tonic::Status::failed_precondition(
                "ReceivePredicate failed: Not the leader of group",
            ));
        }
        let mut stream = request.into_inner();
        let mut payloads = Vec::new();
        while let Some(batch) = stream
            .message()
            .await
            .map_err(|err| tonic::Status::internal(format!("move stream failed: {err}")))?
        {
            payloads.push(batch.data);
        }
        let count = crate::predicate_move::apply_received(&self.node, &payloads)
            .await
            .map_err(tonic::Status::from)?;
        Ok(tonic::Response::new(datastreamdb_proto::api::Payload {
            data: count.to_string().into_bytes(),
        }))
    }

    async fn move_predicate(
        &self,
        request: tonic::Request<MovePredicatePayload>,
    ) -> Result<tonic::Response<datastreamdb_proto::api::Payload>, tonic::Status> {
        let req = request.into_inner();
        crate::predicate_move::run_move_predicate(
            &self.engine,
            &self.groups,
            &self.node,
            &self.peers,
            &req,
        )
        .await
        .map(|_| tonic::Response::new(datastreamdb_proto::api::Payload::default()))
        .map_err(tonic::Status::from)
    }

    type SubscribeStream =
        futures::stream::Empty<Result<datastreamdb_proto::badgerpb4::KvList, tonic::Status>>;

    async fn subscribe(
        &self,
        _request: tonic::Request<SubscriptionRequest>,
    ) -> Result<tonic::Response<Self::SubscribeStream>, tonic::Status> {
        Err(tonic::Status::unimplemented(
            "CDC subscriptions are M6 scope",
        ))
    }

    async fn update_graph_ql_schema(
        &self,
        _request: tonic::Request<UpdateGraphQlSchemaRequest>,
    ) -> Result<tonic::Response<UpdateGraphQlSchemaResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("GraphQL admin is M6 scope"))
    }

    async fn delete_namespace(
        &self,
        request: tonic::Request<DeleteNsRequest>,
    ) -> Result<tonic::Response<Status>, tonic::Status> {
        let req = request.into_inner();
        if !self.groups.serves_group(req.group_id) {
            return Err(not_served(req.group_id));
        }
        let proposal = Proposal {
            delete_ns: Some(req),
            ..Proposal::default()
        };
        let response = self
            .node
            .propose(proposal.encode_to_vec())
            .await
            .map_err(|err| tonic::Status::internal(err.to_string()))?;
        parse_outcome(&response)
            .map(|()| {
                tonic::Response::new(Status {
                    code: 0,
                    msg: String::new(),
                })
            })
            .map_err(tonic::Status::aborted)
    }

    async fn task_status(
        &self,
        _request: tonic::Request<TaskStatusRequest>,
    ) -> Result<tonic::Response<TaskStatusResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("task status is M6 scope"))
    }

    async fn update_ext_snapshot_streaming_state(
        &self,
        _request: tonic::Request<datastreamdb_proto::api::UpdateExtSnapshotStreamingStateRequest>,
    ) -> Result<tonic::Response<Status>, tonic::Status> {
        Err(tonic::Status::unimplemented(
            "external snapshots are M6 scope",
        ))
    }

    type StreamExtSnapshotStream = futures::stream::Empty<
        Result<datastreamdb_proto::api::StreamExtSnapshotResponse, tonic::Status>,
    >;

    async fn stream_ext_snapshot(
        &self,
        _request: tonic::Request<
            tonic::Streaming<datastreamdb_proto::api::StreamExtSnapshotRequest>,
        >,
    ) -> Result<tonic::Response<Self::StreamExtSnapshotStream>, tonic::Status> {
        Err(tonic::Status::unimplemented(
            "external snapshots are M6 scope",
        ))
    }
}
