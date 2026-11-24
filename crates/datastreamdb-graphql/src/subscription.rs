//! Subscription polling: re-executed queries with change detection. Port
//! of `dgraph-main/graphql/subscription/poller.go`.
//!
//! Deltas from Go: one poll task per subscription (no shared per-query
//! buckets); FNV-1a hashing (Go uses `FarmHash64`); epochs bump explicitly on
//! redeploy. WebSocket transport is server-milestone work; this module yields
//! a response stream.

#![warn(missing_docs)]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_graphql::Response as GqlResponse;
use futures::stream::{self, Stream};
use serde_json::Value as Json;

use crate::errors::GraphQlError;
use crate::op::{BoundOperation, OpKind};
use crate::resolve::GraphQlEngine;

/// Poll state shared across one hub generation.
pub struct SubscriptionHub {
    /// Execution engine (shared with poll tasks).
    engine: Arc<GraphQlEngine>,
    /// Schema epoch; subscriptions end when it changes.
    epoch: Arc<AtomicU64>,
    /// Delay between polls.
    poll_interval: Duration,
}

impl SubscriptionHub {
    /// Builds a hub over an engine with a poll interval.
    #[must_use]
    pub fn new(engine: GraphQlEngine, poll_interval: Duration) -> Self {
        Self {
            engine: Arc::new(engine),
            epoch: Arc::new(AtomicU64::new(0)),
            poll_interval,
        }
    }

    /// Bumps the schema epoch, ending all current subscriptions.
    pub fn bump_epoch(&self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
    }

    /// Current epoch (for tests).
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }

    /// Subscribes to a query, yielding the initial response plus updates.
    ///
    /// # Errors
    ///
    /// Returns [`GraphQlError`] on invalid subscriptions or auth failures.
    pub async fn subscribe(
        &self,
        query: &str,
        variables: Json,
        headers: &HashMap<String, String>,
    ) -> Result<impl Stream<Item = GqlResponse> + Send + 'static, GraphQlError> {
        let vars: HashMap<String, Json> = variables
            .as_object()
            .map(|map| {
                map.iter()
                    .map(|(key, val)| (key.clone(), val.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let operation = self
            .engine
            .shared()
            .bound
            .parse_operation(query, None, &vars)
            .map_err(|errs| GraphQlError::new(errs.to_string()))?;
        validate_subscription(&operation)?;
        // Subscription roots mirror query roots; execute as a query (the
        // padding preserves source positions).
        let executable = rewrite_subscription_keyword(query);
        let initial = self
            .engine
            .execute(&executable, variables.clone(), headers)
            .await;
        if !initial.errors.is_empty() {
            let messages: Vec<String> = initial
                .errors
                .iter()
                .map(|err| err.message.clone())
                .collect();
            return Err(GraphQlError::new(messages.join("; ")));
        }
        let expiry = expiry_of(&initial, headers, &self.engine);
        let prev_hash = response_hash(&initial);
        let state = PollState {
            engine: self.engine.clone(),
            query: executable,
            variables,
            headers: headers.clone(),
            expiry,
            local_epoch: self.epoch.load(Ordering::SeqCst),
            epoch: self.epoch.clone(),
            poll_interval: self.poll_interval,
        };
        let stream = stream::unfold((Some(initial), prev_hash, state), poll_next);
        Ok(futures::StreamExt::boxed(stream))
    }
}

/// One poll step: sleep, then re-execute until the response changes.
async fn poll_next(
    (pending, mut prev_hash, state): (Option<GqlResponse>, u64, PollState),
) -> Option<(GqlResponse, (Option<GqlResponse>, u64, PollState))> {
    if let Some(response) = pending {
        return Some((response, (None, prev_hash, state)));
    }
    loop {
        tokio::time::sleep(state.poll_interval).await;
        if state.epoch.load(Ordering::SeqCst) != state.local_epoch {
            return None;
        }
        if let Some(expiry) = state.expiry {
            if std::time::SystemTime::now() > expiry {
                return None;
            }
        }
        let response = state
            .engine
            .execute(&state.query, state.variables.clone(), &state.headers)
            .await;
        let hash = response_hash(&response);
        if hash != prev_hash {
            prev_hash = hash;
            return Some((response, (None, prev_hash, state)));
        }
    }
}

/// Rewrites the leading `subscription` keyword to `query` (padded).
fn rewrite_subscription_keyword(query: &str) -> String {
    let bytes = query.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'#' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if bytes[i].is_ascii_whitespace() || bytes[i] == b',' {
            i += 1;
            continue;
        }
        break;
    }
    if query[i..].starts_with("subscription")
        && query[i + "subscription".len()..]
            .starts_with(|ch: char| ch.is_whitespace() || ch == '{' || ch == '(' || ch == ',')
    {
        let mut out = query.to_owned();
        out.replace_range(i..i + "subscription".len(), "query      ");
        return out;
    }
    query.to_owned()
}

/// Validates that an operation is a single-root subscription.
fn validate_subscription(operation: &BoundOperation) -> Result<(), GraphQlError> {
    if operation.kind != OpKind::Subscription {
        return Err(GraphQlError::new(
            "subscription operations must use the `subscription` keyword",
        ));
    }
    if operation.roots.len() != 1 {
        return Err(GraphQlError::new(format!(
            "subscription must have exactly one root field, found {}",
            operation.roots.len()
        )));
    }
    Ok(())
}

/// Reads the JWT expiry for subscription termination, if any.
fn expiry_of(
    _initial: &GqlResponse,
    headers: &HashMap<String, String>,
    engine: &GraphQlEngine,
) -> Option<std::time::SystemTime> {
    let meta = engine.shared().bound.meta.auth_meta.as_ref()?;
    let header = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(meta.get_header()))
        .map(|(_, val)| val.as_str())?;
    let claims = crate::auth::extract_custom_claims(meta, Some(header)).ok()?;
    let exp = claims.get("exp")?.as_i64()?;
    let exp = u64::try_from(exp.max(0)).unwrap_or(0);
    std::time::UNIX_EPOCH.checked_add(Duration::from_secs(exp))
}

/// FNV-1a hash of a response body for change detection.
fn response_hash(response: &GqlResponse) -> u64 {
    let data = serde_json::to_value(&response.data).unwrap_or(Json::Null);
    let text = serde_json::to_string(&data).unwrap_or_default();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// Owned poll state for one subscription stream.
struct PollState {
    /// Execution engine.
    engine: Arc<GraphQlEngine>,
    /// Subscription query text.
    query: String,
    /// Request variables.
    variables: Json,
    /// Stored request headers (auth re-resolution per poll).
    headers: HashMap<String, String>,
    /// JWT expiry, if any.
    expiry: Option<std::time::SystemTime>,
    /// Epoch at subscribe time.
    local_epoch: u64,
    /// Shared epoch.
    epoch: Arc<AtomicU64>,
    /// Delay between polls.
    poll_interval: Duration,
}
