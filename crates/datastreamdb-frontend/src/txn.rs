//! Transaction coordination (`edgraph/server.go` + `worker/mutation.go`).
//!
//! [`TxnCoordinator`] runs the client-facing flows: timestamp leases,
//! DQL queries, mutation parse/stage/commit, and schema alters. Group
//! routing mirrors Go's `populateMutationMap`; unknown tablets resolve
//! lazily through zero (`ShouldServe`) and cache locally.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use datastreamdb_posting::PostingEngine;
use datastreamdb_proto::api::{Mutation, Operation, TxnContext};
use datastreamdb_proto::pb::{Mutations, Num};
use datastreamdb_proto::{DirectedEdge, EdgeOp};
use datastreamdb_query::QueryEngine;
use datastreamdb_worker::{GroupState, WorkerService, ZeroConn};

/// Coordinator failures (mapped to gRPC/HTTP status by callers).
#[derive(Debug, thiserror::Error)]
pub enum CoordError {
    /// Client error (bad query, mutation, or alter).
    #[error("{0}")]
    Invalid(String),
    /// Zero or a group is unreachable.
    #[error("cluster unavailable: {0}")]
    Unavailable(String),
    /// Transaction aborted (conflict or explicit abort).
    #[error("transaction aborted")]
    Aborted,
    /// Missing or invalid credentials.
    #[error("unauthenticated: {0}")]
    Unauthenticated(String),
    /// Valid identity, forbidden operation.
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    /// Internal failure.
    #[error("internal: {0}")]
    Internal(String),
}

/// Elapsed nanoseconds saturating into `u64` (latency fields).
fn nanos_since(start: std::time::Instant) -> u64 {
    u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// Login user lookup with `checkpwd` verdict (Go `authorizeUser` query).
const QUERY_USER: &str = "query search($userid: string, $password: string){ \
    user(func: eq(datastreamdb.xid, $userid)) @filter(type(datastreamdb.type.User)) { \
    uid datastreamdb.xid password_match: checkpwd(datastreamdb.password, $password) \
    datastreamdb.user.group { uid datastreamdb.xid } } }";

/// Full user/group/rule scan for cache refreshes (forward edges only).
const QUERY_GROUPS: &str = "{ users(func: has(datastreamdb.xid)) \
    @filter(type(datastreamdb.type.User)) { datastreamdb.xid \
    datastreamdb.user.group { datastreamdb.xid \
    datastreamdb.acl.rule { datastreamdb.rule.predicate datastreamdb.rule.permission } } } }";

/// Extracts the first `uid` (`0x…` or decimal) from a single-block response.
fn parse_single_uid(body: &[u8]) -> Option<u64> {
    let json: serde_json::Value = serde_json::from_slice(body).ok()?;
    let first = json.as_object()?.values().next()?;
    let uid = first.as_array()?.first()?.get("uid")?.as_str()?;
    uid.strip_prefix("0x")
        .map_or_else(|| uid.parse().ok(), |hex| u64::from_str_radix(hex, 16).ok())
}

impl From<tonic::Status> for CoordError {
    fn from(status: tonic::Status) -> Self {
        match status.code() {
            tonic::Code::InvalidArgument | tonic::Code::FailedPrecondition => {
                Self::Invalid(status.message().to_owned())
            }
            tonic::Code::Unavailable => Self::Unavailable(status.message().to_owned()),
            tonic::Code::Aborted => Self::Aborted,
            tonic::Code::Unauthenticated => Self::Unauthenticated(status.message().to_owned()),
            tonic::Code::PermissionDenied => Self::PermissionDenied(status.message().to_owned()),
            _ => Self::Internal(status.message().to_owned()),
        }
    }
}

/// Query output plus the var counts upsert conditions need.
pub struct QueryOutput {
    /// Rendered payload (JSON or RDF bytes).
    pub body: Vec<u8>,
    /// Per-variable match counts (`len(var)` inputs).
    pub var_counts: HashMap<String, usize>,
    /// Predicate UID metrics.
    pub metrics: HashMap<String, u64>,
    /// Parsing/processing/encoding nanoseconds.
    pub latency_ns: (u64, u64, u64),
}

/// Mutation output: merged commit context plus blank assignments.
pub struct MutateOutput {
    /// Merged transaction context (keys/preds across groups).
    pub ctx: TxnContext,
    /// Blank label to UID assignments (`_:a` labels intact).
    pub assigned: HashMap<String, u64>,
}

/// Transaction coordinator over one alpha's engines and cluster clients.
pub struct TxnCoordinator {
    /// DQL query engine (local reads + mutation conversion).
    query: Arc<QueryEngine>,
    /// Posting engine (direct schema/oracle access).
    posting: Arc<PostingEngine>,
    /// Membership/tablet view.
    groups: Arc<GroupState>,
    /// Zero client (leases, commits, tablets).
    zero: Arc<ZeroConn>,
    /// Local worker service (group proposals + forwarding).
    worker: Arc<WorkerService>,
    /// Highest UID known leased (verifyUid bound).
    max_uid: AtomicU64,
    /// Auth context (`None` leaves every request open, like ACL-off mode).
    auth: Option<Arc<crate::auth::AuthContext>>,
}

impl TxnCoordinator {
    /// Wires a coordinator over live engines and clients.
    pub fn new(
        query: Arc<QueryEngine>,
        groups: Arc<GroupState>,
        zero: Arc<ZeroConn>,
        worker: Arc<WorkerService>,
    ) -> Self {
        let posting = query.posting().clone();
        let max_uid = groups.max_uid();
        Self {
            query,
            posting,
            groups,
            zero,
            worker,
            max_uid: AtomicU64::new(max_uid),
            auth: None,
        }
    }

    /// Installs the auth context (login + request authorization).
    #[must_use]
    pub fn with_auth(mut self, auth: Arc<crate::auth::AuthContext>) -> Self {
        self.auth = Some(auth);
        self
    }

    /// Auth context, when ACL is configured.
    #[must_use]
    pub fn auth(&self) -> Option<&Arc<crate::auth::AuthContext>> {
        self.auth.as_ref()
    }

    /// Posting engine.
    #[must_use]
    pub fn posting(&self) -> &Arc<PostingEngine> {
        &self.posting
    }

    /// Membership/tablet view.
    #[must_use]
    pub fn groups(&self) -> &Arc<GroupState> {
        &self.groups
    }

    /// Zero client (leases, commits, tablets, namespaces).
    #[must_use]
    pub fn zero(&self) -> &Arc<ZeroConn> {
        &self.zero
    }

    /// Local worker service (group proposals + cluster fan-out).
    #[must_use]
    pub fn worker(&self) -> &Arc<WorkerService> {
        &self.worker
    }

    /// Starts a timestamp: read-only leases share, writes lease one.
    ///
    /// # Errors
    ///
    /// Returns [`CoordError`] when zero refuses.
    pub async fn begin(&self, read_only: bool) -> Result<u64, CoordError> {
        let num = Num {
            val: u64::from(!read_only),
            read_only,
            ..Num::default()
        };
        let leased = self.zero.timestamps(num).await?;
        if read_only {
            Ok(leased.read_only)
        } else {
            Ok(leased.end_id)
        }
    }

    /// Ensures the UID lease covers `need` blanks, leasing top-ups.
    ///
    /// # Errors
    ///
    /// Returns [`CoordError`] when zero refuses.
    pub async fn ensure_uid_lease(&self, need: u64) -> Result<(), CoordError> {
        let have = self.query.allocator().leased_remaining();
        if have >= need {
            return Ok(());
        }
        let num = Num {
            val: need - have,
            ..Num::default()
        };
        let leased = self.zero.assign_ids(num).await?;
        self.query
            .allocator()
            .install_lease(leased.start_id, leased.end_id);
        self.max_uid.fetch_max(leased.end_id, Ordering::SeqCst);
        Ok(())
    }

    /// Highest UID safe to reference (leases + membership view).
    fn uid_ceiling(&self) -> u64 {
        let membership = self.groups.max_uid();
        self.max_uid
            .fetch_max(membership, Ordering::SeqCst)
            .max(membership)
    }

    /// Rejects explicit UIDs above the leased maximum (Go `verifyUid`).
    fn verify_uids(&self, edges: &[DirectedEdge]) -> Result<(), CoordError> {
        let ceiling = self.uid_ceiling();
        for edge in edges {
            for uid in [edge.entity, edge.value_id] {
                if uid > ceiling {
                    return Err(CoordError::Invalid(format!(
                        "UID {uid} exceeds the leased maximum {ceiling}"
                    )));
                }
            }
        }
        Ok(())
    }

    /// Runs a DQL query string at `read_ts` into JSON or RDF.
    ///
    /// When auth is configured, `identity` authorizes the query (queries
    /// degrade to their allowed predicates); `None` fails closed.
    ///
    /// # Errors
    ///
    /// Returns [`CoordError::Invalid`] on parse/execution failure, or
    /// auth errors when ACL is configured.
    pub fn execute_query(
        &self,
        namespace: u64,
        text: &str,
        vars: &HashMap<String, String>,
        read_ts: u64,
        rdf: bool,
        identity: Option<&datastreamdb_acl::jwt::Identity>,
    ) -> Result<QueryOutput, CoordError> {
        use std::time::Instant;
        let start = Instant::now();
        let mut parsed = datastreamdb_dql::parse(datastreamdb_dql::Request {
            text: text.to_owned(),
            variables: vars.clone(),
        })
        .map_err(|err| CoordError::Invalid(err.to_string()))?;
        let parsing_ns = nanos_since(start);

        // Internal callers (login, refresh, bootstrap) pass a guardian
        // identity instead of bypassing: same enforcement path throughout.
        if let Some(auth) = &self.auth {
            crate::auth::authorize_query(auth, identity, &mut parsed.query, namespace)?;
        }

        let start = Instant::now();
        let (result, _) = self
            .query
            .query_parsed(namespace, parsed.query, read_ts)
            .map_err(|err| CoordError::Invalid(err.to_string()))?;
        let processing_ns = nanos_since(start);

        // Counts come before rendering: renderers drain UID lists.
        let mut var_counts = HashMap::new();
        for subgraph in &result.subgraphs {
            collect_var_counts(&mut var_counts, subgraph);
        }

        let start = Instant::now();
        let mut subgraphs = result.subgraphs;
        let body = if rdf {
            datastreamdb_query::rdf::to_rdf(&subgraphs)
                .map_err(|err| CoordError::Invalid(err.to_string()))?
        } else {
            datastreamdb_query::output::to_json(&mut subgraphs)
                .map_err(|err| CoordError::Invalid(err.to_string()))?
        };
        let encoding_ns = nanos_since(start);
        Ok(QueryOutput {
            body,
            var_counts,
            metrics: result.metrics,
            latency_ns: (parsing_ns, processing_ns, encoding_ns),
        })
    }

    /// Runs [`execute_query`](Self::execute_query) off the runtime threads.
    ///
    /// Query execution blocks on timestamp waits and runs CPU-heavy task
    /// processing; awaiting it inline would stall async workers (and freeze
    /// single-threaded runtimes outright).
    ///
    /// # Errors
    ///
    /// Returns [`CoordError::Invalid`] on parse/execution failure, or
    /// [`CoordError::Internal`] when the blocking pool is gone.
    pub async fn execute_query_blocking(
        coordinator: Arc<Self>,
        namespace: u64,
        text: String,
        vars: HashMap<String, String>,
        read_ts: u64,
        rdf: bool,
        identity: Option<datastreamdb_acl::jwt::Identity>,
    ) -> Result<QueryOutput, CoordError> {
        tokio::task::spawn_blocking(move || {
            coordinator.execute_query(namespace, &text, &vars, read_ts, rdf, identity.as_ref())
        })
        .await
        .map_err(|err| CoordError::Internal(err.to_string()))?
    }

    /// Parses, stages, and (unless `commit_now`) holds API mutations.
    ///
    /// `counts` feeds upsert `cond` evaluation (empty without a query).
    ///
    /// # Errors
    ///
    /// Returns [`CoordError`] on parse failure, tablet gaps, conflicts,
    /// or group errors.
    pub async fn execute_mutations(
        &self,
        namespace: u64,
        mutations: &[Mutation],
        start_ts: u64,
        counts: &HashMap<String, usize>,
        identity: Option<&datastreamdb_acl::jwt::Identity>,
    ) -> Result<MutateOutput, CoordError> {
        // Parse every source into chunker quads first (shared blank scope).
        let (set_quads, del_quads, metadata) = parse_mutation_sources(mutations, counts)?;
        if let Some(auth) = &self.auth {
            crate::auth::authorize_mutation(auth, identity, &set_quads, &del_quads, namespace)?;
        }

        // Count set blanks across every source, then lease once (Go parity).
        let blanks = count_blanks(&set_quads);
        self.ensure_uid_lease(blanks).await?;

        let mut assigned = HashMap::new();
        let set_edges = datastreamdb_query::mutation::to_directed_edges_shared(
            &self.posting,
            self.query.allocator(),
            namespace,
            &set_quads,
            EdgeOp::Set,
            &mut assigned,
        )
        .map_err(|err| CoordError::Invalid(err.to_string()))?;
        let del_edges = datastreamdb_query::mutation::to_directed_edges_shared(
            &self.posting,
            self.query.allocator(),
            namespace,
            &del_quads,
            EdgeOp::Del,
            &mut assigned,
        )
        .map_err(|err| CoordError::Invalid(err.to_string()))?;

        // Split by group (Go `populateMutationMap`).
        let mut by_group: BTreeMap<u32, Mutations> = BTreeMap::new();
        for edges in [&set_edges, &del_edges] {
            for edge_list in edges.values() {
                for edge in edge_list {
                    let gid = self.group_for(&edge.attr).await?;
                    let entry = by_group.entry(gid).or_insert_with(|| Mutations {
                        group_id: gid,
                        start_ts,
                        ..Mutations::default()
                    });
                    entry.edges.push(edge.clone());
                }
            }
        }
        if let Some(entry) = by_group.values_mut().next() {
            entry.metadata = Some(metadata);
        }

        // Verify explicit UIDs, then propose per group and merge contexts.
        let all_edges: Vec<DirectedEdge> =
            by_group.values().flat_map(|mu| mu.edges.clone()).collect();
        self.verify_uids(&all_edges)?;
        let mut ctx = TxnContext {
            start_ts,
            ..TxnContext::default()
        };
        for mutations in by_group.values() {
            let group_ctx = self.propose_group(mutations.clone()).await?;
            ctx.keys.extend(group_ctx.keys);
            ctx.preds.extend(group_ctx.preds);
        }
        ctx.keys.sort();
        ctx.keys.dedup();
        ctx.preds.sort();
        ctx.preds.dedup();
        Ok(MutateOutput { ctx, assigned })
    }

    /// Proposes one group's mutations through the worker service.
    pub(crate) async fn propose_group(
        &self,
        mutations: Mutations,
    ) -> Result<TxnContext, CoordError> {
        use datastreamdb_proto::pb::worker_server::Worker as _;
        let response = self
            .worker
            .mutate(tonic::Request::new(mutations))
            .await
            .map_err(CoordError::from)?;
        Ok(response.into_inner())
    }

    /// Group serving `attr`, resolving unknown tablets through zero.
    pub(crate) async fn group_for(&self, attr: &str) -> Result<u32, CoordError> {
        if let Some(gid) = self.groups.belongs_to(attr) {
            return Ok(gid);
        }
        // Offer our own group (Go's `sendTablet`); zero assigns or answers.
        let tablet = self
            .zero
            .should_serve(datastreamdb_proto::pb::Tablet {
                predicate: attr.to_owned(),
                group_id: self.groups.group_id(),
                ..datastreamdb_proto::pb::Tablet::default()
            })
            .await?;
        let gid = tablet.group_id;
        self.groups.upsert_tablet(tablet);
        Ok(gid)
    }

    /// Commits (or aborts, when `aborted`) one transaction at zero.
    ///
    /// # Errors
    ///
    /// Returns [`CoordError::Aborted`] when zero aborts, or
    /// [`CoordError`] when zero is unreachable.
    pub async fn commit(&self, ctx: TxnContext) -> Result<TxnContext, CoordError> {
        let out = self.zero.commit_or_abort(ctx).await?;
        if out.aborted || out.commit_ts == 0 {
            return Err(CoordError::Aborted);
        }
        Ok(out)
    }

    /// Authenticates a login request and mints an access/refresh pair.
    ///
    /// Follows Go's `Server.Login`: refresh tokens re-authenticate by id,
    /// otherwise the userid/password pair is verified with `checkpwd`.
    /// The response JSON is the marshalled `api.Jwt`, like Go.
    ///
    /// # Errors
    ///
    /// Returns [`CoordError::Invalid`] when ACL is off or the request is
    /// malformed, [`CoordError::Unauthenticated`] on bad credentials.
    pub async fn login(
        &self,
        request: &datastreamdb_proto::api::LoginRequest,
    ) -> Result<datastreamdb_proto::api::Response, CoordError> {
        let auth = self
            .auth
            .as_ref()
            .ok_or_else(|| CoordError::Invalid("ACL is not enabled on this alpha".to_owned()))?;
        if !request.refresh_token.is_empty() {
            let identity = datastreamdb_acl::jwt::validate(auth.secrets(), &request.refresh_token)
                .map_err(|err| {
                    CoordError::Unauthenticated(format!(
                        "unable to authenticate the refresh token: {err}"
                    ))
                })?;
            let user = self
                .authorize_user(&identity.user_id, "", identity.namespace)
                .await?
                .ok_or_else(|| {
                    CoordError::Unauthenticated(
                        "unable to authenticate: invalid credentials".to_owned(),
                    )
                })?;
            return Self::mint_login_response(auth, &user, identity.namespace);
        }
        if request.userid.is_empty() {
            return Err(CoordError::Invalid(
                "the userid should not be empty".to_owned(),
            ));
        }
        if request.password.is_empty() {
            return Err(CoordError::Invalid(
                "the password should not be empty".to_owned(),
            ));
        }
        let user = self
            .authorize_user(&request.userid, &request.password, request.namespace)
            .await?
            .ok_or_else(|| {
                CoordError::Unauthenticated(
                    "unable to authenticate: invalid credentials".to_owned(),
                )
            })?;
        if !user.password_match {
            return Err(CoordError::Unauthenticated("invalid login".to_owned()));
        }
        Self::mint_login_response(auth, &user, request.namespace)
    }

    /// Aborts transactions that have not committed.
    ///
    /// # Errors
    ///
    /// Returns [`CoordError`] when zero is unreachable.
    pub async fn abort(&self, starts: Vec<u64>) -> Result<(), CoordError> {
        for start_ts in starts {
            self.zero
                .commit_or_abort(TxnContext {
                    start_ts,
                    aborted: true,
                    ..TxnContext::default()
                })
                .await?;
        }
        Ok(())
    }

    /// Mints and marshals the login token pair for `user`.
    fn mint_login_response(
        auth: &crate::auth::AuthContext,
        user: &datastreamdb_acl::entities::User,
        namespace: u64,
    ) -> Result<datastreamdb_proto::api::Response, CoordError> {
        use prost::Message as _;
        let groups = datastreamdb_acl::entities::group_ids(&user.groups);
        let access = datastreamdb_acl::jwt::mint_access(
            auth.secrets(),
            &user.user_id,
            &groups,
            namespace,
            auth.access_ttl_secs(),
        )
        .map_err(|err| CoordError::Internal(err.to_string()))?;
        let refresh = datastreamdb_acl::jwt::mint_refresh(
            auth.secrets(),
            &user.user_id,
            namespace,
            auth.refresh_ttl_secs(),
        )
        .map_err(|err| CoordError::Internal(err.to_string()))?;
        let jwt = datastreamdb_proto::api::Jwt {
            access_jwt: access,
            refresh_jwt: refresh,
        };
        Ok(datastreamdb_proto::api::Response {
            json: jwt.encode_to_vec(),
            ..datastreamdb_proto::api::Response::default()
        })
    }

    /// Queries one user with a `checkpwd` verdict (Go `authorizeUser`).
    ///
    /// Runs as a guardian read: the caller is not authenticated yet.
    async fn authorize_user(
        &self,
        userid: &str,
        password: &str,
        namespace: u64,
    ) -> Result<Option<datastreamdb_acl::entities::User>, CoordError> {
        let read_ts = self.begin(true).await?;
        let vars = HashMap::from([
            ("$userid".to_owned(), userid.to_owned()),
            ("$password".to_owned(), password.to_owned()),
        ]);
        let guardian = datastreamdb_acl::jwt::Identity::guardian();
        let out = Self::execute_query_blocking(
            self.clone_for_blocking(),
            namespace,
            QUERY_USER.to_owned(),
            vars,
            read_ts,
            false,
            Some(guardian),
        )
        .await?;
        datastreamdb_acl::entities::unmarshal_user(&out.body, "user")
            .map_err(|err| CoordError::Internal(err.to_string()))
    }

    /// Clones `self` into an `Arc` for blocking query calls.
    ///
    /// Internal callers hold `&self` while `execute_query_blocking` needs
    /// ownership; the coordinator is cheap to clone (all shared handles).
    fn clone_for_blocking(&self) -> Arc<Self> {
        Arc::new(Self {
            query: self.query.clone(),
            posting: self.posting.clone(),
            groups: self.groups.clone(),
            zero: self.zero.clone(),
            worker: self.worker.clone(),
            max_uid: AtomicU64::new(self.max_uid.load(Ordering::SeqCst)),
            auth: self.auth.clone(),
        })
    }

    /// Reloads group rules into the ACL cache unconditionally.
    ///
    /// # Errors
    ///
    /// Returns [`CoordError`] when the groups query fails.
    pub async fn refresh_acl_cache(&self, namespace: u64) -> Result<(), CoordError> {
        let Some(auth) = &self.auth else {
            return Ok(());
        };
        auth.cache().reset();
        self.ensure_acl_loaded(namespace).await
    }

    /// Loads group rules into the ACL cache when empty (Go `RefreshACLs`).
    ///
    /// Queries every user with their groups and rules (forward edges
    /// only), then inverts the map into group-indexed cache entries.
    ///
    /// # Errors
    ///
    /// Returns [`CoordError`] when the groups query fails.
    pub async fn ensure_acl_loaded(&self, namespace: u64) -> Result<(), CoordError> {
        let Some(auth) = &self.auth else {
            return Ok(());
        };
        if auth.cache().loaded() {
            return Ok(());
        }
        let read_ts = self.begin(true).await?;
        let guardian = datastreamdb_acl::jwt::Identity::guardian();
        let out = Self::execute_query_blocking(
            self.clone_for_blocking(),
            namespace,
            QUERY_GROUPS.to_owned(),
            HashMap::new(),
            read_ts,
            false,
            Some(guardian),
        )
        .await?;
        let users = datastreamdb_acl::entities::unmarshal_users(&out.body, "users")
            .map_err(|err| CoordError::Internal(err.to_string()))?;
        let mut by_group: HashMap<String, datastreamdb_acl::entities::Group> = HashMap::new();
        for user in &users {
            for group in &user.groups {
                let entry = by_group.entry(group.group_id.clone()).or_insert_with(|| {
                    datastreamdb_acl::entities::Group {
                        uid: group.uid.clone(),
                        group_id: group.group_id.clone(),
                        users: Vec::new(),
                        rules: group.rules.clone(),
                    }
                });
                if entry.rules.is_empty() {
                    entry.rules.clone_from(&group.rules);
                }
                if !entry.users.iter().any(|u| u.user_id == user.user_id) {
                    entry.users.push(datastreamdb_acl::entities::User {
                        uid: user.uid.clone(),
                        user_id: user.user_id.clone(),
                        ..datastreamdb_acl::entities::User::default()
                    });
                }
            }
        }
        auth.cache()
            .update(namespace, &by_group.into_values().collect::<Vec<_>>());
        Ok(())
    }

    /// Creates the guardians group and groot user when missing (Go
    /// `upsertGuardianAndGroot`), then refreshes the rule cache.
    ///
    /// Single-writer by deployment: only the bootstrap alpha runs this.
    /// Restarts are idempotent (presence is checked first).
    /// `groot_password` seeds groot's password (`"password"` when unset).
    ///
    /// # Errors
    ///
    /// Returns [`CoordError`] when the bootstrap queries or mutations fail.
    pub async fn bootstrap_acl(
        &self,
        namespace: u64,
        groot_password: Option<&str>,
    ) -> Result<(), CoordError> {
        use datastreamdb_proto::api::Mutation;
        if self.auth.is_none() {
            return Ok(());
        }
        let guardian = datastreamdb_acl::jwt::Identity::guardian();
        if !self
            .acl_entity_exists(namespace, "guardians", "datastreamdb.type.Group", &guardian)
            .await?
        {
            let start_ts = self.begin(false).await?;
            let out = self
                .execute_mutations(
                    namespace,
                    &[Mutation {
                        set: datastreamdb_acl::nquads::create_group_nquads(
                            datastreamdb_x::SUPER_ADMIN_ID,
                        ),
                        ..Mutation::default()
                    }],
                    start_ts,
                    &HashMap::new(),
                    Some(&guardian),
                )
                .await?;
            self.commit(out.ctx).await?;
        }
        if !self
            .acl_entity_exists(namespace, "groot", "datastreamdb.type.User", &guardian)
            .await?
        {
            let start_ts = self.begin(false).await?;
            let created = self
                .execute_mutations(
                    namespace,
                    &[Mutation {
                        set: datastreamdb_acl::nquads::create_user_nquads(
                            "groot",
                            groot_password.unwrap_or("password"),
                        ),
                        ..Mutation::default()
                    }],
                    start_ts,
                    &HashMap::new(),
                    Some(&guardian),
                )
                .await?;
            let user_uid = created.assigned.get("_:newuser").copied().unwrap_or(0);
            self.commit(created.ctx).await?;
            // Link groot into guardians once both exist.
            let read_ts = self.begin(true).await?;
            let groups = Self::execute_query_blocking(
                self.clone_for_blocking(),
                namespace,
                String::from(
                    "{ g(func: eq(datastreamdb.xid, \"guardians\")) @filter(type(datastreamdb.type.Group)) { uid } }",
                ),
                HashMap::new(),
                read_ts,
                false,
                Some(guardian.clone()),
            )
            .await?;
            if user_uid != 0 {
                if let Some(group_uid) = parse_single_uid(&groups.body) {
                    let link =
                        format!("<{user_uid:#x}> <datastreamdb.user.group> <{group_uid:#x}> .");
                    let start_ts = self.begin(false).await?;
                    let out = self
                        .execute_mutations(
                            namespace,
                            &[Mutation {
                                set_nquads: link.into_bytes(),
                                ..Mutation::default()
                            }],
                            start_ts,
                            &HashMap::new(),
                            Some(&guardian),
                        )
                        .await?;
                    self.commit(out.ctx).await?;
                }
            }
        }
        self.ensure_acl_loaded(namespace).await
    }

    /// Whether an `xid`-typed entity exists (bootstrap presence checks).
    async fn acl_entity_exists(
        &self,
        namespace: u64,
        label: &str,
        typ: &str,
        guardian: &datastreamdb_acl::jwt::Identity,
    ) -> Result<bool, CoordError> {
        let read_ts = self.begin(true).await?;
        let text = format!(
            "{{ q(func: eq(datastreamdb.xid, \"{label}\")) @filter(type({typ})) {{ uid }} }}"
        );
        let out = Self::execute_query_blocking(
            self.clone_for_blocking(),
            namespace,
            text,
            HashMap::new(),
            read_ts,
            false,
            Some(guardian.clone()),
        )
        .await?;
        Ok(parse_single_uid(&out.body).is_some())
    }

    /// Applies a schema/alter operation across groups.
    ///
    /// # Errors
    ///
    /// Returns [`CoordError`] on parse failure or group errors.
    pub async fn alter(
        &self,
        namespace: u64,
        op: &Operation,
        identity: Option<&datastreamdb_acl::jwt::Identity>,
    ) -> Result<(), CoordError> {
        use datastreamdb_proto::api::operation::DropOp as ApiDrop;
        use datastreamdb_proto::pb::mutations::DropOp as PbDrop;

        if op.schema.is_empty()
            && op.drop_attr.is_empty()
            && !op.drop_all
            && op.drop_op == ApiDrop::None as i32
        {
            return Err(CoordError::Invalid("empty alter operation".to_owned()));
        }
        // Schema/drop proposals carry no transaction; the applier ignores
        // this except for the non-zero edge-path guard, so floor at one.
        let start_ts = self.posting.oracle().max_assigned().max(1);

        // Drop-all (guardians only under ACL).
        if op.drop_all || op.drop_op == ApiDrop::All as i32 {
            if !op.drop_value.is_empty() {
                return Err(CoordError::Invalid(
                    "If DropOp is set to ALL, DropValue must be empty".to_owned(),
                ));
            }
            self.authorize_alter_op(&HashSet::new(), true, identity, namespace)?;
            return self
                .fan_out_drops(PbDrop::All as i32, String::new(), start_ts)
                .await;
        }
        if op.drop_op == ApiDrop::Data as i32 {
            if !op.drop_value.is_empty() {
                return Err(CoordError::Invalid(
                    "If DropOp is set to DATA, DropValue must be empty".to_owned(),
                ));
            }
            self.authorize_alter_op(&HashSet::new(), true, identity, namespace)?;
            return self
                .fan_out_drops(PbDrop::Data as i32, namespace.to_string(), start_ts)
                .await;
        }
        // Drop-attr travels as a `* P *` delete edge (Go parity).
        if !op.drop_attr.is_empty() || op.drop_op == ApiDrop::Attr as i32 {
            return self
                .alter_drop_attr(namespace, op, start_ts, identity)
                .await;
        }
        if op.drop_op == ApiDrop::Type as i32 {
            // Go checks no predicates for type drops (identity still required).
            self.authorize_alter_op(&HashSet::new(), false, identity, namespace)?;
            return self.alter_drop_type(namespace, op, start_ts).await;
        }

        // Plain schema/text alter.
        let parsed = datastreamdb_schema::parse_with_namespace(&op.schema, namespace)
            .map_err(|err| CoordError::Invalid(format!("bad schema: {err}")))?;
        let preds: HashSet<String> = parsed
            .preds
            .iter()
            .map(|update| datastreamdb_x::parse_attr(&update.predicate).to_owned())
            .collect();
        self.authorize_alter_op(&preds, false, identity, namespace)?;
        let mut by_group: BTreeMap<u32, Mutations> = BTreeMap::new();
        for update in &parsed.preds {
            let gid = self.group_for(&update.predicate).await?;
            by_group
                .entry(gid)
                .or_insert_with(|| Mutations {
                    group_id: gid,
                    start_ts,
                    ..Mutations::default()
                })
                .schema
                .push(update.clone());
        }
        for gid in self.groups.known_groups() {
            if gid == 0 {
                continue;
            }
            by_group
                .entry(gid)
                .or_insert_with(|| Mutations {
                    group_id: gid,
                    start_ts,
                    ..Mutations::default()
                })
                .types
                .extend(parsed.types.clone());
        }
        for mutations in by_group.into_values() {
            self.propose_group(mutations).await?;
        }
        Ok(())
    }

    /// Authorizes one alter's predicates (no-op without auth configured).
    fn authorize_alter_op(
        &self,
        preds: &HashSet<String>,
        drop_all_or_data: bool,
        identity: Option<&datastreamdb_acl::jwt::Identity>,
        namespace: u64,
    ) -> Result<(), CoordError> {
        if let Some(auth) = &self.auth {
            crate::auth::authorize_alter(auth, identity, preds, drop_all_or_data, namespace)?;
        }
        Ok(())
    }

    /// Drops one predicate via a `* P *` delete edge.
    async fn alter_drop_attr(
        &self,
        namespace: u64,
        op: &Operation,
        start_ts: u64,
        identity: Option<&datastreamdb_acl::jwt::Identity>,
    ) -> Result<(), CoordError> {
        use datastreamdb_proto::api::operation::DropOp as ApiDrop;
        if op.drop_op == ApiDrop::Attr as i32 && op.drop_value.is_empty() {
            return Err(CoordError::Invalid(
                "If DropOp is set to ATTR, DropValue must not be empty".to_owned(),
            ));
        }
        let name = if op.drop_attr.is_empty() {
            op.drop_value.clone()
        } else {
            op.drop_attr.clone()
        };
        self.authorize_alter_op(&HashSet::from([name.clone()]), false, identity, namespace)?;
        let attr = datastreamdb_x::namespace_attr(namespace, &name);
        if datastreamdb_x::is_pre_defined_predicate(&attr) {
            return Err(CoordError::Invalid(format!(
                "predicate {name} is pre-defined and is not allowed to be dropped"
            )));
        }
        // NOTE: Go strips the namespace here and re-derives it downstream;
        // our engine keys schema/data namespaced, so keep it attached.
        let nquad = datastreamdb_proto::api::NQuad {
            subject: datastreamdb_x::STAR.to_owned(),
            predicate: attr.clone(),
            object_value: Some(datastreamdb_proto::api::Value {
                val: Some(datastreamdb_proto::api::value::Val::StrVal("*".to_owned())),
            }),
            ..datastreamdb_proto::api::NQuad::default()
        };
        let wrapped = datastreamdb_dql::NQuad(crate::convert::api_nquad_to_chunker(&nquad));
        let edge = wrapped
            .to_delete_pred_edge()
            .map_err(|err| CoordError::Invalid(err.to_string()))?;
        let gid = self.group_for(&attr).await?;
        let mutations = Mutations {
            group_id: gid,
            start_ts,
            edges: vec![edge],
            ..Mutations::default()
        };
        self.propose_group(mutations).await?;
        Ok(())
    }

    /// Drops one type definition cluster-wide.
    async fn alter_drop_type(
        &self,
        namespace: u64,
        op: &Operation,
        start_ts: u64,
    ) -> Result<(), CoordError> {
        use datastreamdb_proto::pb::mutations::DropOp as PbDrop;
        if op.drop_value.is_empty() {
            return Err(CoordError::Invalid(
                "If DropOp is set to TYPE, DropValue must not be empty".to_owned(),
            ));
        }
        let drop_pred = datastreamdb_x::namespace_attr(namespace, &op.drop_value);
        if datastreamdb_x::is_pre_defined_type(&drop_pred) {
            return Err(CoordError::Invalid(format!(
                "type {} is pre-defined and is not allowed to be dropped",
                op.drop_value
            )));
        }
        let gid = self.groups.group_id();
        let mutations = Mutations {
            group_id: gid,
            start_ts,
            drop_op: PbDrop::Type as i32,
            drop_value: drop_pred,
            ..Mutations::default()
        };
        self.propose_group(mutations).await?;
        Ok(())
    }

    /// Fans a drop operation out to every known group.
    async fn fan_out_drops(
        &self,
        drop_op: i32,
        drop_value: String,
        start_ts: u64,
    ) -> Result<(), CoordError> {
        for gid in self.groups.known_groups() {
            if gid == 0 {
                continue;
            }
            let mutations = Mutations {
                group_id: gid,
                start_ts,
                drop_op,
                drop_value: drop_value.clone(),
                ..Mutations::default()
            };
            self.propose_group(mutations).await?;
        }
        Ok(())
    }
}

/// Parses every mutation source (protos, JSON, RDF) into chunker quads.
///
/// Mutations whose `cond` fails against `counts` are skipped. Returns set
/// quads, del quads, and merged type-hint metadata.
fn parse_mutation_sources(
    mutations: &[Mutation],
    counts: &HashMap<String, usize>,
) -> Result<
    (
        Vec<datastreamdb_chunker::NQuad>,
        Vec<datastreamdb_chunker::NQuad>,
        datastreamdb_proto::Metadata,
    ),
    CoordError,
> {
    let mut set_quads = Vec::new();
    let mut del_quads = Vec::new();
    let mut metadata = datastreamdb_proto::Metadata::default();
    for mutation in mutations {
        if !mutation.cond.is_empty()
            && !datastreamdb_graphql::rewrite_mutation::evaluate_condition(&mutation.cond, counts)
        {
            continue;
        }
        crate::convert::validate_nquads(&mutation.set, &mutation.del)
            .map_err(|err| CoordError::Invalid(err.to_string()))?;
        set_quads.extend(
            mutation
                .set
                .iter()
                .map(crate::convert::api_nquad_to_chunker),
        );
        del_quads.extend(
            mutation
                .del
                .iter()
                .map(crate::convert::api_nquad_to_chunker),
        );
        if !mutation.set_json.is_empty() {
            let (mut quads, meta) =
                datastreamdb_chunker::parse_json(&mutation.set_json, datastreamdb_chunker::Op::Set)
                    .map_err(|err| CoordError::Invalid(format!("bad set JSON: {err}")))?;
            set_quads.append(&mut quads);
            merge_metadata(&mut metadata, meta);
        }
        if !mutation.delete_json.is_empty() {
            let (mut quads, meta) = datastreamdb_chunker::parse_json(
                &mutation.delete_json,
                datastreamdb_chunker::Op::Delete,
            )
            .map_err(|err| CoordError::Invalid(format!("bad delete JSON: {err}")))?;
            del_quads.append(&mut quads);
            merge_metadata(&mut metadata, meta);
        }
        if !mutation.set_nquads.is_empty() {
            let (mut quads, meta) = datastreamdb_chunker::parse_rdfs(&mutation.set_nquads)
                .map_err(|err| CoordError::Invalid(format!("bad set N-Quads: {err}")))?;
            set_quads.append(&mut quads);
            merge_metadata(&mut metadata, meta);
        }
        if !mutation.del_nquads.is_empty() {
            let (mut quads, meta) = datastreamdb_chunker::parse_rdfs(&mutation.del_nquads)
                .map_err(|err| CoordError::Invalid(format!("bad del N-Quads: {err}")))?;
            del_quads.append(&mut quads);
            merge_metadata(&mut metadata, meta);
        }
    }
    Ok((set_quads, del_quads, metadata))
}

/// Counts matches per `params.var` across executed subgraphs.
///
/// A `v as <field>` child captures its parent's UID list (Go var
/// semantics); nodes carrying their own var name use their own list.
/// Captures apply after recursion so they win name collisions.
fn collect_var_counts(counts: &mut HashMap<String, usize>, node: &datastreamdb_query::SubGraph) {
    fn uids(node: &datastreamdb_query::SubGraph) -> usize {
        node.dest_uids.as_ref().map_or_else(
            || node.uid_matrix.iter().map(|list| list.uids.len()).sum(),
            |list| list.uids.len(),
        )
    }
    if !node.params.var.is_empty() {
        counts.insert(node.params.var.clone(), uids(node));
    }
    for child in &node.children {
        collect_var_counts(counts, child);
    }
    let here = uids(node);
    for child in &node.children {
        if !child.params.var.is_empty() {
            counts.insert(child.params.var.clone(), here);
        }
    }
    for filter in &node.filters {
        collect_var_counts(counts, filter);
    }
}

/// Counts distinct blank labels across set quads (lease sizing).
fn count_blanks(quads: &[datastreamdb_chunker::NQuad]) -> u64 {
    let mut labels = HashSet::new();
    for quad in quads {
        for id in [&quad.subject, &quad.object_id] {
            if id.starts_with("_:") {
                labels.insert(id.clone());
            }
        }
    }
    u64::try_from(labels.len()).unwrap_or(u64::MAX)
}

/// Merges chunker metadata hints into the request metadata.
fn merge_metadata(into: &mut datastreamdb_proto::Metadata, from: datastreamdb_proto::Metadata) {
    for (pred, hint) in from.pred_hints {
        into.pred_hints.entry(pred).or_insert(hint);
    }
}
