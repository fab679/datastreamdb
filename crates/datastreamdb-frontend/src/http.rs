//! Alpha HTTP routes (`dgraph/cmd/alpha/http.go`).
//!
//! [`router`] serves `/health`, `/state`, `/query`, `/mutate`, `/alter`,
//! `/commit`, `/abort`, `/login`, and the guardian-only `/admin`
//! GraphQL endpoint over a shared [`HttpState`]. Shutdown arrives
//! with the full admin epic.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response as AxumResponse};
use axum::routing::{get, post};
use datastreamdb_proto::api::{Mutation, Operation, TxnContext, operation::DropOp};
use serde::Serialize;

use crate::txn::{CoordError, TxnCoordinator};

/// Shared state behind every route.
#[derive(Clone)]
pub struct HttpState {
    /// Transaction coordinator.
    pub coordinator: Arc<TxnCoordinator>,
    /// Membership/tablet view.
    pub groups: Arc<datastreamdb_worker::GroupState>,
    /// Process start (uptime).
    pub started: Instant,
    /// Advertised address.
    pub addr: String,
    /// Audit sink (`None` disables auditing).
    pub auditor: Option<datastreamdb_audit::SharedAuditor>,
    /// Admin GraphQL schema ([`router`] builds it; `None` until then).
    pub admin: Option<async_graphql::dynamic::Schema>,
}

/// Maps coordinator failures to HTTP status codes.
fn status(err: CoordError) -> (StatusCode, String) {
    match err {
        CoordError::Invalid(message) => (StatusCode::BAD_REQUEST, message),
        CoordError::Unavailable(message) => (StatusCode::SERVICE_UNAVAILABLE, message),
        CoordError::Aborted => (StatusCode::CONFLICT, "transaction aborted".to_owned()),
        CoordError::Unauthenticated(message) => (StatusCode::UNAUTHORIZED, message),
        CoordError::PermissionDenied(message) => (StatusCode::FORBIDDEN, message),
        CoordError::Internal(message) => (StatusCode::INTERNAL_SERVER_ERROR, message),
    }
}

/// Health payload.
#[derive(Debug, Clone, Serialize)]
pub struct Health {
    /// Always `"ok"` when the router serves.
    pub status: &'static str,
    /// Crate version of the serving binary.
    pub version: &'static str,
    /// Advertised address.
    pub instance: String,
    /// Uptime seconds.
    pub uptime: u64,
}

async fn health(State(state): State<HttpState>) -> Json<Health> {
    Json(Health {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        instance: state.addr.clone(),
        uptime: state.started.elapsed().as_secs(),
    })
}

/// Membership snapshot as plain JSON (prost types have no serde).
async fn cluster_state(State(state): State<HttpState>) -> Json<serde_json::Value> {
    let view = state.groups.snapshot_state();
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
                        "groupId": member.group_id,
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
    Json(serde_json::json!({
        "counter": view.counter,
        "groups": groups,
        "maxUid": view.max_uid,
        "maxTxnTs": view.max_txn_ts,
        "cid": view.cid,
    }))
}

/// Query flags: `?ro=&be=` plus start timestamps.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueryFlags {
    /// Read-only lease.
    #[serde(default)]
    ro: bool,
    /// Best-effort read (no lease).
    #[serde(default)]
    be: bool,
    /// Start timestamp override.
    #[serde(default)]
    start_ts: Option<u64>,
}

/// Request variables from the `X-DataStreamDB-Vars` JSON header.
fn request_vars(headers: &HeaderMap) -> Result<HashMap<String, String>, String> {
    let Some(raw) = headers.get("x-datastreamdb-vars") else {
        return Ok(HashMap::new());
    };
    let text = raw
        .to_str()
        .map_err(|err| format!("bad X-DataStreamDB-Vars header: {err}"))?;
    serde_json::from_str(text).map_err(|err| format!("bad X-DataStreamDB-Vars JSON: {err}"))
}

/// Resolves the caller's identity from `X-DataStreamDB-AccessJWT` (Go
/// `AttachAccessJwt` + identity resolution, minus IP/auth-token plumbing).
fn http_identity(
    headers: &HeaderMap,
    coordinator: &TxnCoordinator,
) -> Result<Option<datastreamdb_acl::jwt::Identity>, String> {
    let Some(auth) = coordinator.auth() else {
        return Ok(None);
    };
    let token = headers
        .get(crate::auth::ACCESS_JWT_HEADER)
        .or_else(|| headers.get("X-DataStreamDB-AccessToken"))
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let Some(token) = token else {
        return Ok(None);
    };
    datastreamdb_acl::jwt::validate(auth.secrets(), &token)
        .map(Some)
        .map_err(|err| format!("unauthenticated: {err}"))
}

/// Loads ACL rules before an enforcing handler runs.
async fn ensure_loaded(coordinator: &TxnCoordinator) -> Result<(), AxumResponse> {
    coordinator
        .ensure_acl_loaded(datastreamdb_x::ROOT_NAMESPACE)
        .await
        .map_err(|err| {
            let (code, message) = status(err);
            (code, message).into_response()
        })
}

/// Transaction envelope for JSON responses.
#[derive(Debug, Clone, Serialize)]
struct TxnEnvelope {
    start_ts: u64,
    commit_ts: u64,
    aborted: bool,
}

impl From<&TxnContext> for TxnEnvelope {
    fn from(ctx: &TxnContext) -> Self {
        Self {
            start_ts: ctx.start_ts,
            commit_ts: ctx.commit_ts,
            aborted: ctx.aborted,
        }
    }
}

/// `POST /query`: DQL body, JSON response with extensions.
async fn query(
    State(state): State<HttpState>,
    Query(flags): Query<QueryFlags>,
    headers: HeaderMap,
    body: String,
) -> AxumResponse {
    let vars = match request_vars(&headers) {
        Ok(vars) => vars,
        Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
    };
    let identity = match http_identity(&headers, &state.coordinator) {
        Ok(identity) => identity,
        Err(message) => return (StatusCode::UNAUTHORIZED, message).into_response(),
    };
    if let Err(response) = ensure_loaded(&state.coordinator).await {
        return response;
    }
    let start_ts = match flags.start_ts {
        Some(ts) => ts,
        None if flags.be => state.coordinator.posting().oracle().max_assigned(),
        None => match state.coordinator.begin(flags.ro).await {
            Ok(ts) => ts,
            Err(err) => {
                let (code, message) = status(err);
                return (code, message).into_response();
            }
        },
    };
    let out = match TxnCoordinator::execute_query_blocking(
        Arc::clone(&state.coordinator),
        datastreamdb_x::ROOT_NAMESPACE,
        body.clone(),
        vars.clone(),
        start_ts,
        false,
        identity,
    )
    .await
    {
        Ok(out) => out,
        Err(err) => {
            let (code, message) = status(err);
            return (code, message).into_response();
        }
    };
    let data: serde_json::Value =
        serde_json::from_slice(&out.body).unwrap_or(serde_json::Value::Null);
    Json(serde_json::json!({
        "data": data,
        "extensions": {
            "txn": {"start_ts": start_ts, "commit_ts": 0, "aborted": false},
            "latency": {
                "parsing_ns": out.latency_ns.0,
                "processing_ns": out.latency_ns.1,
                "encoding_ns": out.latency_ns.2,
            },
            "metrics": {"num_uids": out.metrics},
        },
    }))
    .into_response()
}

/// Mutate flags: `?commitNow=` plus an optional `?cond=` upsert guard.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct MutateFlags {
    /// Commit in the same round trip.
    #[serde(default)]
    commit_now: bool,
    /// Upsert condition over query vars (empty without a query).
    #[serde(default)]
    cond: String,
    /// Start timestamp override.
    #[serde(default)]
    start_ts: Option<u64>,
}

/// `POST /mutate`: RDF (default) or JSON (by content type) body.
async fn mutate(
    State(state): State<HttpState>,
    Query(flags): Query<MutateFlags>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> AxumResponse {
    let is_json = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|content| content.contains("json"));
    let bytes = body.to_vec();
    let mutations: Vec<Mutation> = if is_json {
        vec![Mutation {
            set_json: bytes,
            cond: flags.cond,
            commit_now: flags.commit_now,
            ..Mutation::default()
        }]
    } else {
        match decode_rdf_mutations(&bytes, &flags) {
            Ok(mutations) => mutations,
            Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
        }
    };
    let identity = match http_identity(&headers, &state.coordinator) {
        Ok(identity) => identity,
        Err(message) => return (StatusCode::UNAUTHORIZED, message).into_response(),
    };
    if let Err(response) = ensure_loaded(&state.coordinator).await {
        return response;
    }
    let start_ts = match flags.start_ts {
        Some(ts) => ts,
        None => match state.coordinator.begin(false).await {
            Ok(ts) => ts,
            Err(err) => {
                let (code, message) = status(err);
                return (code, message).into_response();
            }
        },
    };
    let out = match state
        .coordinator
        .execute_mutations(
            datastreamdb_x::ROOT_NAMESPACE,
            &mutations,
            start_ts,
            &HashMap::new(),
            identity.as_ref(),
        )
        .await
    {
        Ok(out) => out,
        Err(err) => {
            let (code, message) = status(err);
            return (code, message).into_response();
        }
    };
    let mut ctx = out.ctx;
    if flags.commit_now {
        match state.coordinator.commit(ctx.clone()).await {
            Ok(committed) => ctx = committed,
            Err(CoordError::Aborted) => {
                ctx.aborted = true;
                ctx.commit_ts = 0;
            }
            Err(err) => {
                let (code, message) = status(err);
                return (code, message).into_response();
            }
        }
    }
    Json(serde_json::json!({
        "data": {"uids": datastreamdb_query::request::uids_to_hex(&out.assigned)},
        "extensions": {"txn": TxnEnvelope::from(&ctx)},
    }))
    .into_response()
}

/// `POST /alter`: schema text, or a JSON drop operation.
async fn alter(State(state): State<HttpState>, headers: HeaderMap, body: String) -> AxumResponse {
    // NOTE: `api.Operation` has no serde impl (prost); JSON bodies decode
    // through a shape-compatible helper, anything else is schema text.
    let op = decode_alter_json(&body).unwrap_or(Operation {
        schema: body.clone(),
        ..Operation::default()
    });
    let identity = match http_identity(&headers, &state.coordinator) {
        Ok(identity) => identity,
        Err(message) => return (StatusCode::UNAUTHORIZED, message).into_response(),
    };
    if let Err(response) = ensure_loaded(&state.coordinator).await {
        return response;
    }
    match state
        .coordinator
        .alter(datastreamdb_x::ROOT_NAMESPACE, &op, identity.as_ref())
        .await
    {
        Ok(()) => (StatusCode::OK, "alter ok").into_response(),
        Err(err) => {
            let (code, message) = status(err);
            (code, message).into_response()
        }
    }
}

/// `POST /login`: JSON credentials in, token pair out (Go `loginHandler`).
async fn login(State(state): State<HttpState>, body: String) -> AxumResponse {
    use prost::Message as _;
    let request: LoginJson = match serde_json::from_str(&body) {
        Ok(request) => request,
        Err(err) => return (StatusCode::BAD_REQUEST, err.to_string()).into_response(),
    };
    let response = match state
        .coordinator
        .login(&datastreamdb_proto::api::LoginRequest {
            userid: request.userid,
            password: request.password,
            refresh_token: request.refresh_token,
            namespace: request.namespace,
        })
        .await
    {
        Ok(response) => response,
        Err(err) => {
            let (code, message) = status(err);
            return (code, message).into_response();
        }
    };
    let jwt = match datastreamdb_proto::api::Jwt::decode(response.json.as_slice()) {
        Ok(jwt) => jwt,
        Err(err) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
        }
    };
    Json(serde_json::json!({
        "data": {"accessJWT": jwt.access_jwt, "refreshJWT": jwt.refresh_jwt},
    }))
    .into_response()
}

/// JSON login shape (lowercase field names, like Go's struct tags).
#[derive(Debug, Default, serde::Deserialize)]
struct LoginJson {
    /// User id.
    #[serde(default)]
    userid: String,
    /// Plain-text password.
    #[serde(default)]
    password: String,
    /// Refresh token (takes precedence when set).
    #[serde(default)]
    refresh_token: String,
    /// Tenancy namespace.
    #[serde(default)]
    namespace: u64,
}

/// JSON alter shape (`{"drop_all":true}`, `{"drop_op":"ATTR",...}`, ...).
#[derive(Debug, Default, serde::Deserialize)]
struct AlterJson {
    #[serde(default)]
    schema: String,
    #[serde(default)]
    drop_attr: String,
    #[serde(default)]
    drop_all: bool,
    #[serde(default)]
    drop_op: String,
    #[serde(default)]
    drop_value: String,
}

/// Decodes an RDF mutate body: `{ set/delete }` blocks (Go
/// `ParseDQL`) or bare N-Quads (one set mutation). The `?cond=`
/// flag fills blocks without their own guard.
fn decode_rdf_mutations(body: &[u8], flags: &MutateFlags) -> Result<Vec<Mutation>, String> {
    let text = std::str::from_utf8(body).map_err(|err| format!("bad RDF body: {err}"))?;
    if !text.trim_start().starts_with('{') {
        return Ok(vec![Mutation {
            set_nquads: body.to_vec(),
            cond: flags.cond.clone(),
            commit_now: flags.commit_now,
            ..Mutation::default()
        }]);
    }
    let parsed =
        datastreamdb_dql::parse_mutation(text).map_err(|err| format!("bad mutation: {err}"))?;
    Ok(parsed
        .mutations
        .into_iter()
        .map(|block| Mutation {
            set_nquads: block.set_nquads,
            del_nquads: block.del_nquads,
            cond: if flags.cond.is_empty() {
                block.cond
            } else {
                flags.cond.clone()
            },
            commit_now: flags.commit_now,
            ..Mutation::default()
        })
        .collect())
}

/// Decodes a JSON alter body (`None` means schema text).
fn decode_alter_json(body: &str) -> Option<Operation> {
    let parsed: AlterJson = serde_json::from_str(body).ok()?;
    let looks_like_json_drop = !parsed.drop_attr.is_empty()
        || parsed.drop_all
        || !parsed.drop_op.is_empty()
        || (!parsed.schema.is_empty() && body.trim_start().starts_with('{'));
    if !looks_like_json_drop {
        return None;
    }
    let drop_op = match parsed.drop_op.to_ascii_uppercase().as_str() {
        "ALL" => DropOp::All as i32,
        "DATA" => DropOp::Data as i32,
        "ATTR" => DropOp::Attr as i32,
        "TYPE" => DropOp::Type as i32,
        _ if parsed.drop_all => DropOp::All as i32,
        _ if !parsed.drop_attr.is_empty() => DropOp::Attr as i32,
        _ => DropOp::None as i32,
    };
    let drop_value = if drop_op == DropOp::Attr as i32 && parsed.drop_value.is_empty() {
        parsed.drop_attr.clone()
    } else {
        parsed.drop_value
    };
    Some(Operation {
        schema: parsed.schema,
        drop_attr: parsed.drop_attr,
        drop_all: parsed.drop_all,
        drop_op,
        drop_value,
        ..Operation::default()
    })
}

/// Commit/abort flags: repeated `keys`/`preds` plus `startTs`.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommitFlags {
    /// Start timestamp to decide.
    start_ts: u64,
    /// Conflict keys (repeated).
    #[serde(default)]
    keys: Vec<String>,
    /// Predicates (repeated).
    #[serde(default)]
    preds: Vec<String>,
    /// Abort instead of committing.
    #[serde(default)]
    abort: bool,
}

/// `GET /commit`: decide one transaction.
async fn commit(State(state): State<HttpState>, Query(flags): Query<CommitFlags>) -> AxumResponse {
    decide(state, flags, false).await
}

/// `GET /abort`: abort one transaction.
async fn abort(State(state): State<HttpState>, Query(flags): Query<CommitFlags>) -> AxumResponse {
    decide(state, flags, true).await
}

/// Shared commit/abort implementation.
async fn decide(state: HttpState, flags: CommitFlags, abort: bool) -> AxumResponse {
    let ctx = TxnContext {
        start_ts: flags.start_ts,
        keys: flags.keys,
        preds: flags.preds,
        aborted: abort || flags.abort,
        ..TxnContext::default()
    };
    if ctx.aborted {
        match state.coordinator.abort(vec![ctx.start_ts]).await {
            Ok(()) => Json(TxnEnvelope::from(&ctx)).into_response(),
            Err(err) => {
                let (code, message) = status(err);
                (code, message).into_response()
            }
        }
    } else {
        match state.coordinator.commit(ctx).await {
            Ok(committed) => Json(TxnEnvelope::from(&committed)).into_response(),
            Err(CoordError::Aborted) => Json(serde_json::json!({
                "start_ts": flags.start_ts, "commit_ts": 0, "aborted": true,
            }))
            .into_response(),
            Err(err) => {
                let (code, message) = status(err);
                (code, message).into_response()
            }
        }
    }
}

/// GraphQL-over-HTTP request envelope.
#[derive(Debug, Default, serde::Deserialize)]
struct GraphqlBody {
    /// Query or mutation text.
    #[serde(default)]
    query: String,
    /// Operation name for multi-operation documents.
    #[serde(default)]
    operation_name: Option<String>,
    /// Variables object.
    #[serde(default)]
    variables: serde_json::Value,
}

/// `POST /admin`: guardian-only admin GraphQL (Go's admin server).
async fn admin(State(state): State<HttpState>, headers: HeaderMap, body: String) -> AxumResponse {
    if let Err(response) = require_guardian(&headers, &state.coordinator) {
        return *response;
    }
    let Some(schema) = &state.admin else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "admin schema missing").into_response();
    };
    let envelope: GraphqlBody = match serde_json::from_str(&body) {
        Ok(envelope) => envelope,
        Err(err) => return (StatusCode::BAD_REQUEST, err.to_string()).into_response(),
    };
    if envelope.query.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "missing query").into_response();
    }
    let mut request = async_graphql::Request::new(envelope.query);
    if let Some(name) = envelope.operation_name {
        request = request.operation_name(name);
    }
    if let Some(vars) = envelope.variables.as_object() {
        request = request.variables(async_graphql::Variables::from_json(
            serde_json::Value::Object(vars.clone()),
        ));
    }
    Json(schema.execute(request).await).into_response()
}

/// Rejects non-guardians when ACL is configured (open without auth,
/// like every other ACL-off handler).
fn require_guardian(
    headers: &HeaderMap,
    coordinator: &TxnCoordinator,
) -> Result<(), Box<AxumResponse>> {
    if coordinator.auth().is_none() {
        return Ok(());
    }
    let identity = match http_identity(headers, coordinator) {
        Ok(identity) => identity,
        Err(message) => {
            return Err(Box::new(
                (StatusCode::UNAUTHORIZED, message).into_response(),
            ));
        }
    };
    let guardian = identity
        .as_ref()
        .is_some_and(|identity| identity.groups.iter().any(|group| group == "guardians"));
    if guardian {
        Ok(())
    } else {
        Err(Box::new(
            (StatusCode::FORBIDDEN, "admin requires guardian").into_response(),
        ))
    }
}

/// Builds the alpha HTTP router.
///
/// # Panics
///
/// Panics when the static admin schema fails to build (a programming
/// error: the schema shape is fixed at compile time).
pub fn router(state: HttpState) -> axum::Router {
    let backend: Arc<dyn datastreamdb_graphql::admin::AdminBackend> =
        Arc::new(crate::admin::FrontendAdmin::new(
            state.coordinator.clone(),
            state.addr.clone(),
            state.started,
        ));
    let schema =
        datastreamdb_graphql::admin::build_admin_schema(backend).expect("admin schema builds");
    let state = HttpState {
        admin: Some(schema),
        ..state
    };
    axum::Router::new()
        .route("/health", get(health))
        .route("/state", get(cluster_state))
        .route("/query", post(query))
        .route("/mutate", post(mutate))
        .route("/alter", post(alter))
        .route("/login", post(login))
        .route("/commit", get(commit))
        .route("/abort", get(abort))
        .route("/admin", post(admin))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            audit_middleware,
        ))
        .with_state(state)
}

/// Audits one HTTP exchange after the handler runs (Go `AuditRequestHttp`).
async fn audit_middleware(
    State(state): State<HttpState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> AxumResponse {
    use axum::body::Body;
    let Some(auditor) = &state.auditor else {
        return next.run(request).await;
    };
    let path = request.uri().path().to_owned();
    if datastreamdb_audit::event::skip_path(&path) {
        return next.run(request).await;
    }
    let client = request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|info| info.0.to_string())
        .unwrap_or_default();
    let credential = http_credential(&state, request.headers());
    let query_params = parse_query_params(request.uri().query().unwrap_or(""));
    let (mut parts, body) = request.into_parts();
    let bytes = axum::body::to_bytes(body, 64 * 1024)
        .await
        .unwrap_or_default();
    let body_text = String::from_utf8_lossy(&bytes).into_owned();
    parts.headers.insert(
        axum::http::header::CONTENT_LENGTH,
        axum::http::HeaderValue::from(bytes.len()),
    );
    let request = axum::extract::Request::from_parts(parts, Body::from(bytes));
    let response = next.run(request).await;
    let status = response
        .status()
        .canonical_reason()
        .unwrap_or("Unknown")
        .to_owned();
    auditor.http(&datastreamdb_audit::HttpCall {
        path: &path,
        client_host: &client,
        credential: &credential,
        namespace: datastreamdb_x::ROOT_NAMESPACE,
        body: &body_text,
        status: &status,
        query_params,
    });
    response
}

/// Resolves the audit credential from access/auth headers.
fn http_credential(state: &HttpState, headers: &HeaderMap) -> datastreamdb_audit::Credential {
    use datastreamdb_audit::Credential;
    let token = headers
        .get(crate::auth::ACCESS_JWT_HEADER)
        .or_else(|| headers.get("X-DataStreamDB-AccessToken"))
        .and_then(|value| value.to_str().ok());
    match (state.coordinator.auth(), token) {
        (Some(auth), Some(token)) => match datastreamdb_acl::jwt::validate(auth.secrets(), token) {
            Ok(identity) => Credential::Identity {
                user: identity.user_id,
            },
            Err(_) => Credential::InvalidToken,
        },
        (_, None) if headers.contains_key("X-DataStreamDB-AuthToken") => Credential::PoorMan,
        _ => Credential::None,
    }
}

/// Parses `a=1&b=2` into multi-value pairs.
fn parse_query_params(query: &str) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        out.entry(key.to_owned())
            .or_default()
            .push(value.to_owned());
    }
    out
}

/// Default HTTP read timeout for handlers (reserved for M6 draining).
pub const HANDLER_TIMEOUT: Duration = Duration::from_secs(120);
