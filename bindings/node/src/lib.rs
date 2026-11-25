//! TypeScript bindings for datastreamdb (Node-API).
//!
//! Thin async-ready wrapper over [`datastreamdb_core::Instance`]: no business logic
//! lives here. Blocking engine calls move to `AsyncTask` in M1 (see
//! `bindings/node/TODO.md`).

#![warn(missing_docs)]
#![deny(clippy::unwrap_used)]

use std::sync::Arc;

use napi::bindgen_prelude::*;
use napi_derive::napi;

/// One isolated database instance.
#[napi]
pub struct Instance {
    inner: Arc<datastreamdb_core::Instance>,
}

fn core_error(err: impl std::fmt::Display) -> napi::Error {
    napi::Error::from_reason(err.to_string())
}

#[napi]
impl Instance {
    /// Opens an instance from JSON config (see `docs/EMBEDDING.md`).
    ///
    /// # Errors
    ///
    /// Returns a JS error when the config is invalid or the backend cannot open.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "Node-API takes owned strings"
    )]
    #[napi(constructor)]
    pub fn new(config_json: String) -> Result<Self> {
        let config =
            datastreamdb_core::InstanceConfig::from_json(&config_json).map_err(core_error)?;
        let inner = datastreamdb_core::Instance::open(config).map_err(core_error)?;
        Ok(Self { inner })
    }

    /// Unique id assigned at open.
    #[must_use]
    #[napi]
    pub fn id(&self) -> String {
        self.inner.id().to_string()
    }

    /// Storage backend name (`"memory"` or `"fjall"`).
    #[must_use]
    #[napi]
    pub fn backend(&self) -> String {
        self.inner.backend_name().to_owned()
    }

    /// Executes a JSON request, returns a JSON response.
    ///
    /// # Errors
    ///
    /// Returns a JS error on malformed requests or engine failure.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "Node-API takes owned strings"
    )]
    #[napi]
    pub fn execute(&self, request: String) -> Result<String> {
        self.inner.execute(&request).map_err(core_error)
    }

    /// Raw key write into the default partition.
    ///
    /// # Errors
    ///
    /// Returns a JS error on engine failure.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "Node-API takes owned strings"
    )]
    #[napi]
    pub fn put(&self, key: String, value: String) -> Result<()> {
        self.inner.put(&key, value.as_bytes()).map_err(core_error)
    }

    /// Raw key read from the default partition.
    ///
    /// # Errors
    ///
    /// Returns a JS error on engine failure.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "Node-API takes owned strings"
    )]
    #[napi]
    pub fn get(&self, key: String) -> Result<Option<String>> {
        let value = self.inner.get(&key).map_err(core_error)?;
        Ok(value.map(|bytes| String::from_utf8_lossy(&bytes).into_owned()))
    }

    /// Executes a DQL query and returns JSON.
    ///
    /// # Errors
    ///
    /// Returns a JS error on parse/execution failure.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "Node-API takes owned strings"
    )]
    #[napi]
    pub fn dql_query(&self, query: String) -> Result<String> {
        self.inner.dql_query_json(&query).map_err(core_error)
    }

    /// Applies a DQL mutation and returns assigned UIDs as JSON.
    ///
    /// # Errors
    ///
    /// Returns a JS error on parse/execution failure.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "Node-API takes owned strings"
    )]
    #[napi]
    pub fn dql_mutate(&self, mutation: String) -> Result<String> {
        self.inner.dql_mutate_json(&mutation).map_err(core_error)
    }

    /// Applies additive/updating DQL schema text.
    ///
    /// # Errors
    ///
    /// Returns a JS error on schema/index-rebuild failure.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "Node-API takes owned strings"
    )]
    #[napi]
    pub fn dql_alter(&self, schema: String) -> Result<()> {
        self.inner.dql_alter(&schema).map_err(core_error)
    }

    /// Installs/replaces the embedded GraphQL schema from original SDL.
    ///
    /// # Errors
    ///
    /// Returns a JS error when the SDL is invalid or the schema cannot build.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "Node-API takes owned strings"
    )]
    #[napi]
    pub fn set_graphql_schema(&self, sdl: String) -> Result<()> {
        self.inner.set_graphql_schema(&sdl).map_err(core_error)
    }

    /// Returns the installed GraphQL SDL, if any.
    #[must_use]
    #[napi]
    pub fn graphql_schema(&self) -> Option<String> {
        self.inner.graphql_schema()
    }

    /// Executes a GraphQL query/mutation and returns JSON.
    ///
    /// # Errors
    ///
    /// Returns a JS error on schema/execution failure.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "Node-API takes owned strings"
    )]
    #[napi]
    pub fn graphql(
        &self,
        query: String,
        variables_json: Option<String>,
        operation: Option<String>,
        headers_json: Option<String>,
    ) -> Result<String> {
        let variables = parse_json(variables_json, serde_json::json!({}))?;
        let headers = parse_headers(headers_json)?;
        let response = self
            .inner
            .graphql(&query, operation.as_deref(), variables, &headers)
            .map_err(core_error)?;
        serde_json::to_string(&response).map_err(core_error)
    }

    /// Async GraphQL query/mutation execution.
    ///
    /// # Errors
    ///
    /// Returns a JS error on schema/execution failure.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "Node-API takes owned strings"
    )]
    #[napi]
    pub async fn graphql_async(
        &self,
        query: String,
        variables_json: Option<String>,
        operation: Option<String>,
        headers_json: Option<String>,
    ) -> Result<String> {
        let variables = parse_json(variables_json, serde_json::json!({}))?;
        let headers = parse_headers(headers_json)?;
        let response = self
            .inner
            .graphql_async(&query, operation.as_deref(), variables, &headers)
            .await
            .map_err(core_error)?;
        serde_json::to_string(&response).map_err(core_error)
    }

    /// Creates a poll-based GraphQL subscription; returns its id.
    ///
    /// # Errors
    ///
    /// Returns a JS error when no schema is installed or the subscription is invalid.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "Node-API takes owned strings"
    )]
    #[napi]
    pub fn graphql_subscribe(
        &self,
        query: String,
        variables_json: Option<String>,
        headers_json: Option<String>,
        poll_ms: Option<u32>,
    ) -> Result<u32> {
        let variables = parse_json(variables_json, serde_json::json!({}))?;
        let headers = parse_headers(headers_json)?;
        let interval = std::time::Duration::from_millis(u64::from(poll_ms.unwrap_or(100)));
        let subscription = self
            .inner
            .graphql_subscribe(&query, variables, &headers, interval)
            .map_err(core_error)?;
        u32::try_from(subscription.into_id())
            .map_err(|_| napi::Error::from_reason("subscription id overflow"))
    }

    /// Polls a subscription and returns `{changed,closed,response?}` JSON.
    ///
    /// # Errors
    ///
    /// Returns a JS error for an unknown subscription or execution failure.
    #[napi]
    pub fn graphql_next(&self, id: u32, wait_ms: Option<u32>) -> Result<String> {
        let wait = std::time::Duration::from_millis(u64::from(wait_ms.unwrap_or(0)));
        self.inner
            .graphql_next_json(u64::from(id), wait)
            .map_err(core_error)
    }

    /// Cancels a subscription.
    #[must_use]
    #[napi]
    pub fn graphql_unsubscribe(&self, id: u32) -> bool {
        self.inner.graphql_unsubscribe(u64::from(id))
    }
}

fn parse_json(value: Option<String>, default: serde_json::Value) -> Result<serde_json::Value> {
    match value {
        Some(text) if !text.trim().is_empty() => serde_json::from_str(&text).map_err(core_error),
        _ => Ok(default),
    }
}

fn parse_headers(value: Option<String>) -> Result<std::collections::HashMap<String, String>> {
    match parse_json(value, serde_json::json!({}))? {
        serde_json::Value::Object(map) => Ok(map
            .into_iter()
            .filter_map(|(key, value)| value.as_str().map(|text| (key, text.to_owned())))
            .collect()),
        _ => Err(napi::Error::from_reason("headers must be a JSON object")),
    }
}

/// Binding version (tracks the workspace version).
#[must_use]
#[napi]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}
