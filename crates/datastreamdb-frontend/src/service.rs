//! `api.DataStreamDB` service (`edgraph/server.go` request paths).
//!
//! [`DataStreamDBService`] serves queries, mutations, alters, commits,
//! namespaces, and version/ID leases over a [`TxnCoordinator`].
//! External snapshots stay `Unimplemented` until M6.
use std::collections::HashMap;
use std::sync::Arc;

use datastreamdb_proto::api::{
    AllocateIDsRequest, AllocateIDsResponse, Check, CreateNamespaceRequest,
    CreateNamespaceResponse, DropNamespaceRequest, DropNamespaceResponse, Latency,
    ListNamespacesRequest, ListNamespacesResponse, LoginRequest, Metrics, Operation, Payload,
    Request, Response, RunDqlRequest, TxnContext, UpdateExtSnapshotStreamingStateRequest,
    UpdateExtSnapshotStreamingStateResponse, Version,
    data_stream_db_server::DataStreamDb as DataStreamDbTrait, request::RespFormat,
};

use crate::txn::{CoordError, TxnCoordinator};

/// Maps coordinator failures to gRPC status.
fn status(err: CoordError) -> tonic::Status {
    match err {
        CoordError::Invalid(message) => tonic::Status::invalid_argument(message),
        CoordError::Unavailable(message) => tonic::Status::unavailable(message),
        CoordError::Aborted => tonic::Status::aborted("transaction aborted"),
        CoordError::Unauthenticated(message) => tonic::Status::unauthenticated(message),
        CoordError::PermissionDenied(message) => tonic::Status::permission_denied(message),
        CoordError::Internal(message) => tonic::Status::internal(message),
    }
}

/// Client-facing `DataStreamDB` API over one alpha.
pub struct DataStreamDBService {
    /// Transaction coordinator.
    coordinator: Arc<TxnCoordinator>,
    /// Audit sink (`None` disables auditing).
    auditor: Option<datastreamdb_audit::SharedAuditor>,
}

/// Elapsed nanoseconds saturating into `u64` (latency fields).
fn nanos_since(start: std::time::Instant) -> u64 {
    u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

impl DataStreamDBService {
    /// Serves `coordinator` (namespace pinned to root until M6).
    pub fn new(
        coordinator: Arc<TxnCoordinator>,
        auditor: Option<datastreamdb_audit::SharedAuditor>,
    ) -> Self {
        Self {
            coordinator,
            auditor,
        }
    }

    /// Namespace for every request (multi-tenancy is M6).
    fn namespace() -> u64 {
        datastreamdb_x::ROOT_NAMESPACE
    }

    /// Captures audit context from an incoming request.
    fn capture_call<T: std::fmt::Debug>(
        method: &'static str,
        request: &tonic::Request<T>,
    ) -> datastreamdb_audit::GrpcCall<'static> {
        datastreamdb_audit::GrpcCall {
            method,
            peer: request
                .remote_addr()
                .map(|addr| addr.to_string())
                .unwrap_or_default(),
            body: format!("{:?}", request.get_ref()),
            token_present: crate::auth::extract_token(request.metadata()).is_some(),
            poor_man: request.metadata().get("auth-token").is_some(),
            namespace: request
                .metadata()
                .get("namespace")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse().ok()),
        }
    }

    /// Resolves the caller without failing (audit labels the outcome).
    fn resolve_identity<T>(
        &self,
        request: &tonic::Request<T>,
    ) -> Option<datastreamdb_acl::jwt::Identity> {
        crate::auth::authenticate(self.coordinator.auth().map(Arc::as_ref), request.metadata())
            .unwrap_or(None)
    }

    /// Requires a guardian identity when ACL is configured (Go
    /// `AuthorizeCapability(CapClusterAdmin)`); open without auth.
    fn require_guardian(
        auth: Option<&crate::auth::AuthContext>,
        metadata: &tonic::metadata::MetadataMap,
        verb: &str,
    ) -> Result<Option<datastreamdb_acl::jwt::Identity>, tonic::Status> {
        let identity = crate::auth::authenticate(auth, metadata)?;
        if auth.is_none() {
            return Ok(identity);
        }
        let guardian = identity
            .as_ref()
            .is_some_and(|identity| identity.groups.iter().any(|group| group == "guardians"));
        if guardian {
            Ok(identity)
        } else {
            Err(tonic::Status::permission_denied(format!(
                "Non superadmin user cannot {verb} namespace."
            )))
        }
    }

    /// Emits one audit event (no-op without an auditor).
    fn audit_call<U>(
        &self,
        call: &datastreamdb_audit::GrpcCall<'_>,
        identity: Option<&datastreamdb_acl::jwt::Identity>,
        outcome: &Result<U, tonic::Status>,
    ) {
        let Some(auditor) = &self.auditor else {
            return;
        };
        let status = match outcome {
            Ok(_) => "OK".to_owned(),
            Err(status) => format!("{:?}", status.code()),
        };
        auditor.grpc_call(
            call,
            identity.as_ref().map(|id| id.user_id.as_str()),
            &status,
        );
    }

    async fn query_inner(
        &self,
        request: tonic::Request<Request>,
        identity: Option<datastreamdb_acl::jwt::Identity>,
    ) -> Result<tonic::Response<Response>, tonic::Status> {
        use std::time::Instant;
        let total = Instant::now();
        self.coordinator
            .ensure_acl_loaded(Self::namespace())
            .await
            .map_err(status)?;
        let req = request.into_inner();
        let read_only = req.read_only || req.mutations.is_empty();
        let rdf = req.resp_format == RespFormat::Rdf as i32;

        // Timestamp: explicit, best-effort max, or a fresh lease.
        let assign_start = Instant::now();
        let start_ts = if req.start_ts > 0 {
            req.start_ts
        } else if req.best_effort {
            self.coordinator.posting().oracle().max_assigned()
        } else {
            self.coordinator.begin(read_only).await.map_err(status)?
        };
        let assign_ns = nanos_since(assign_start);

        // Query first (when present), harvesting var counts for conds.
        let mut counts = std::collections::HashMap::new();
        let mut body = Vec::new();
        let mut metrics = HashMap::new();
        let mut latency = (0, 0, 0);
        if !req.query.is_empty() {
            let out = TxnCoordinator::execute_query_blocking(
                Arc::clone(&self.coordinator),
                Self::namespace(),
                req.query.clone(),
                req.vars.clone(),
                start_ts,
                rdf,
                identity.clone(),
            )
            .await
            .map_err(status)?;
            counts = out.var_counts;
            metrics = out.metrics;
            latency = out.latency_ns;
            body = out.body;
        }

        // Mutations (conditioned on the query results).
        let mut ctx = TxnContext {
            start_ts,
            ..TxnContext::default()
        };
        let mut uids = HashMap::new();
        if !req.mutations.is_empty() {
            let mutation_commit = req.mutations.iter().any(|m| m.commit_now) || req.commit_now;
            let out = self
                .coordinator
                .execute_mutations(
                    Self::namespace(),
                    &req.mutations,
                    start_ts,
                    &counts,
                    identity.as_ref(),
                )
                .await
                .map_err(status)?;
            ctx.keys = out.ctx.keys;
            ctx.preds = out.ctx.preds;
            uids = datastreamdb_query::request::uids_to_hex(&out.assigned);
            if mutation_commit {
                match self.coordinator.commit(ctx.clone()).await {
                    Ok(committed) => ctx = committed,
                    Err(CoordError::Aborted) => {
                        ctx.aborted = true;
                        ctx.commit_ts = 0;
                    }
                    Err(err) => return Err(status(err)),
                }
            }
        }

        let (parsing_ns, processing_ns, encoding_ns) = latency;
        Ok(tonic::Response::new(Response {
            json: if rdf { Vec::new() } else { body.clone() },
            rdf: if rdf { body } else { Vec::new() },
            txn: Some(ctx),
            latency: Some(Latency {
                parsing_ns,
                processing_ns,
                encoding_ns,
                assign_timestamp_ns: assign_ns,
                total_ns: nanos_since(total),
            }),
            metrics: Some(Metrics { num_uids: metrics }),
            uids,
            ..Response::default()
        }))
    }

    async fn run_dql_inner(
        &self,
        request: tonic::Request<RunDqlRequest>,
        identity: Option<datastreamdb_acl::jwt::Identity>,
    ) -> Result<tonic::Response<Response>, tonic::Status> {
        use std::time::Instant;
        let total = Instant::now();
        self.coordinator
            .ensure_acl_loaded(Self::namespace())
            .await
            .map_err(status)?;
        let req = request.into_inner();
        let start_ts = if req.best_effort {
            self.coordinator.posting().oracle().max_assigned()
        } else {
            self.coordinator
                .begin(req.read_only)
                .await
                .map_err(status)?
        };
        let rdf = req.resp_format == RespFormat::Rdf as i32;
        let out = TxnCoordinator::execute_query_blocking(
            Arc::clone(&self.coordinator),
            Self::namespace(),
            req.dql_query.clone(),
            req.vars.clone(),
            start_ts,
            rdf,
            identity,
        )
        .await
        .map_err(status)?;
        let (parsing_ns, processing_ns, encoding_ns) = out.latency_ns;
        Ok(tonic::Response::new(Response {
            json: if rdf { Vec::new() } else { out.body.clone() },
            rdf: if rdf { out.body } else { Vec::new() },
            txn: Some(TxnContext {
                start_ts,
                ..TxnContext::default()
            }),
            latency: Some(Latency {
                parsing_ns,
                processing_ns,
                encoding_ns,
                assign_timestamp_ns: 0,
                total_ns: nanos_since(total),
            }),
            metrics: Some(Metrics {
                num_uids: out.metrics,
            }),
            ..Response::default()
        }))
    }
}

#[tonic::async_trait]
impl DataStreamDbTrait for DataStreamDBService {
    async fn login(
        &self,
        request: tonic::Request<LoginRequest>,
    ) -> Result<tonic::Response<Response>, tonic::Status> {
        let call = Self::capture_call("/api.DataStreamDB/Login", &request);
        let identity = self.resolve_identity(&request);
        let req = request.into_inner();
        let outcome = self
            .coordinator
            .login(&req)
            .await
            .map_err(status)
            .map(tonic::Response::new);
        self.audit_call(&call, identity.as_ref(), &outcome);
        outcome
    }

    async fn query(
        &self,
        request: tonic::Request<Request>,
    ) -> Result<tonic::Response<Response>, tonic::Status> {
        let call = Self::capture_call("/api.DataStreamDB/Query", &request);
        let identity = self.resolve_identity(&request);
        let outcome = self.query_inner(request, identity.clone()).await;
        self.audit_call(&call, identity.as_ref(), &outcome);
        outcome
    }

    async fn alter(
        &self,
        request: tonic::Request<Operation>,
    ) -> Result<tonic::Response<Payload>, tonic::Status> {
        let call = Self::capture_call("/api.DataStreamDB/Alter", &request);
        let identity = self.resolve_identity(&request);
        let outcome = async {
            self.coordinator
                .ensure_acl_loaded(Self::namespace())
                .await
                .map_err(status)?;
            let op = request.into_inner();
            self.coordinator
                .alter(Self::namespace(), &op, identity.as_ref())
                .await
                .map_err(status)?;
            Ok(tonic::Response::new(Payload::default()))
        }
        .await;
        self.audit_call(&call, identity.as_ref(), &outcome);
        outcome
    }

    async fn commit_or_abort(
        &self,
        request: tonic::Request<TxnContext>,
    ) -> Result<tonic::Response<TxnContext>, tonic::Status> {
        let call = Self::capture_call("/api.DataStreamDB/CommitOrAbort", &request);
        let identity = self.resolve_identity(&request);
        let outcome = async {
            let ctx = request.into_inner();
            if ctx.aborted {
                self.coordinator
                    .abort(vec![ctx.start_ts])
                    .await
                    .map_err(status)?;
                return Ok(tonic::Response::new(ctx));
            }
            match self.coordinator.commit(ctx.clone()).await {
                Ok(committed) => Ok(tonic::Response::new(committed)),
                Err(CoordError::Aborted) => Ok(tonic::Response::new(TxnContext {
                    aborted: true,
                    ..ctx
                })),
                Err(err) => Err(status(err)),
            }
        }
        .await;
        self.audit_call(&call, identity.as_ref(), &outcome);
        outcome
    }

    async fn check_version(
        &self,
        request: tonic::Request<Check>,
    ) -> Result<tonic::Response<Version>, tonic::Status> {
        let call = Self::capture_call("/api.DataStreamDB/CheckVersion", &request);
        let identity = self.resolve_identity(&request);
        let outcome = Ok(tonic::Response::new(Version {
            tag: env!("CARGO_PKG_VERSION").to_owned(),
        }));
        self.audit_call(&call, identity.as_ref(), &outcome);
        outcome
    }

    async fn run_dql(
        &self,
        request: tonic::Request<RunDqlRequest>,
    ) -> Result<tonic::Response<Response>, tonic::Status> {
        let call = Self::capture_call("/api.DataStreamDB/RunDQL", &request);
        let identity = self.resolve_identity(&request);
        let outcome = self.run_dql_inner(request, identity.clone()).await;
        self.audit_call(&call, identity.as_ref(), &outcome);
        outcome
    }

    async fn allocate_i_ds(
        &self,
        _request: tonic::Request<AllocateIDsRequest>,
    ) -> Result<tonic::Response<AllocateIDsResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented(
            "direct ID allocation is M6 loader scope; alphas lease internally",
        ))
    }

    async fn update_ext_snapshot_streaming_state(
        &self,
        _request: tonic::Request<UpdateExtSnapshotStreamingStateRequest>,
    ) -> Result<tonic::Response<UpdateExtSnapshotStreamingStateResponse>, tonic::Status> {
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

    async fn create_namespace(
        &self,
        request: tonic::Request<CreateNamespaceRequest>,
    ) -> Result<tonic::Response<CreateNamespaceResponse>, tonic::Status> {
        let call = Self::capture_call("/api.DataStreamDB/CreateNamespace", &request);
        let identity = Self::require_guardian(
            self.coordinator.auth().map(Arc::as_ref),
            request.metadata(),
            "create",
        )?;
        let outcome = self
            .coordinator
            .create_namespace(None)
            .await
            .map(|ns| tonic::Response::new(CreateNamespaceResponse { namespace: ns }))
            .map_err(status);
        self.audit_call(&call, identity.as_ref(), &outcome);
        outcome
    }

    async fn drop_namespace(
        &self,
        request: tonic::Request<DropNamespaceRequest>,
    ) -> Result<tonic::Response<DropNamespaceResponse>, tonic::Status> {
        let call = Self::capture_call("/api.DataStreamDB/DropNamespace", &request);
        let identity = Self::require_guardian(
            self.coordinator.auth().map(Arc::as_ref),
            request.metadata(),
            "drop",
        )?;
        let outcome = async {
            let req = request.into_inner();
            self.coordinator
                .drop_namespace(req.namespace)
                .await
                .map_err(status)?;
            Ok(tonic::Response::new(DropNamespaceResponse {}))
        }
        .await;
        self.audit_call(&call, identity.as_ref(), &outcome);
        outcome
    }

    async fn list_namespaces(
        &self,
        request: tonic::Request<ListNamespacesRequest>,
    ) -> Result<tonic::Response<ListNamespacesResponse>, tonic::Status> {
        let call = Self::capture_call("/api.DataStreamDB/ListNamespaces", &request);
        let identity = Self::require_guardian(
            self.coordinator.auth().map(Arc::as_ref),
            request.metadata(),
            "list",
        )?;
        let namespaces = self
            .coordinator
            .list_namespaces()
            .into_iter()
            .map(|ns| (ns, datastreamdb_proto::api::Namespace { id: ns }))
            .collect();
        let outcome = Ok(tonic::Response::new(ListNamespacesResponse { namespaces }));
        self.audit_call(&call, identity.as_ref(), &outcome);
        outcome
    }
}
