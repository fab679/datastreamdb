//! Python bindings for datastreamdb (pyo3/maturin).
//!
//! Thin wrapper over [`datastreamdb_core::Instance`]: no business logic lives here.
//! The GIL is released around every engine call. Asyncio helpers live in the
//! pure-Python `datastreamdb` package.

#![warn(missing_docs)]

use std::sync::Arc;

use pyo3::prelude::*;

fn core_error(err: impl std::fmt::Display) -> PyErr {
    pyo3::exceptions::PyRuntimeError::new_err(err.to_string())
}

/// One isolated database instance.
#[pyclass]
pub struct Instance {
    inner: Arc<datastreamdb_core::Instance>,
}

#[pymethods]
impl Instance {
    /// Opens an instance from JSON config, e.g. `'{}'` for in-memory.
    ///
    /// # Errors
    ///
    /// Raises `RuntimeError` when the config is invalid or the backend cannot open.
    #[new]
    fn new(config_json: &str) -> PyResult<Self> {
        let config =
            datastreamdb_core::InstanceConfig::from_json(config_json).map_err(core_error)?;
        let inner = datastreamdb_core::Instance::open(config).map_err(core_error)?;
        Ok(Self { inner })
    }

    /// Unique id assigned at open.
    #[must_use]
    fn id(&self) -> String {
        self.inner.id().to_string()
    }

    /// Storage backend name (`"memory"` or `"fjall"`).
    #[must_use]
    fn backend(&self) -> &str {
        self.inner.backend_name()
    }

    /// Executes a JSON request, returns a JSON response (GIL released).
    ///
    /// # Errors
    ///
    /// Raises `RuntimeError` on malformed requests or engine failure.
    fn execute(&self, py: Python<'_>, request: &str) -> PyResult<String> {
        let inner = self.inner.clone();
        let request = request.to_owned();
        py.detach(|| inner.execute(&request)).map_err(core_error)
    }

    /// Raw key write into the default partition (GIL released).
    ///
    /// # Errors
    ///
    /// Raises `RuntimeError` on engine failure.
    fn put(&self, py: Python<'_>, key: &str, value: &str) -> PyResult<()> {
        let inner = self.inner.clone();
        let (key, value) = (key.to_owned(), value.as_bytes().to_vec());
        py.detach(|| inner.put(&key, &value)).map_err(core_error)
    }

    /// Raw key read from the default partition (GIL released).
    ///
    /// # Errors
    ///
    /// Raises `RuntimeError` on engine failure.
    fn get(&self, py: Python<'_>, key: &str) -> PyResult<Option<String>> {
        let inner = self.inner.clone();
        let key = key.to_owned();
        let value = py.detach(|| inner.get(&key)).map_err(core_error)?;
        Ok(value.map(|bytes| String::from_utf8_lossy(&bytes).into_owned()))
    }

    /// Executes a DQL query and returns JSON (GIL released).
    fn dql_query(&self, py: Python<'_>, query: &str) -> PyResult<String> {
        let inner = self.inner.clone();
        let query = query.to_owned();
        py.detach(|| inner.dql_query_json(&query))
            .map_err(core_error)
    }

    /// Applies a DQL mutation and returns assigned UIDs as JSON.
    fn dql_mutate(&self, py: Python<'_>, mutation: &str) -> PyResult<String> {
        let inner = self.inner.clone();
        let mutation = mutation.to_owned();
        py.detach(|| inner.dql_mutate_json(&mutation))
            .map_err(core_error)
    }

    /// Applies additive/updating DQL schema text.
    fn dql_alter(&self, py: Python<'_>, schema: &str) -> PyResult<()> {
        let inner = self.inner.clone();
        let schema = schema.to_owned();
        py.detach(|| inner.dql_alter(&schema)).map_err(core_error)
    }

    /// Installs/replaces the embedded GraphQL schema from original SDL.
    fn set_graphql_schema(&self, py: Python<'_>, sdl: &str) -> PyResult<()> {
        let inner = self.inner.clone();
        let sdl = sdl.to_owned();
        py.detach(|| inner.set_graphql_schema(&sdl))
            .map_err(core_error)
    }

    /// Returns the installed GraphQL SDL, if any.
    fn graphql_schema(&self) -> Option<String> {
        self.inner.graphql_schema()
    }

    /// Executes a GraphQL query/mutation and returns JSON.
    #[pyo3(signature = (query, variables_json=None, operation=None, headers_json=None))]
    fn graphql(
        &self,
        py: Python<'_>,
        query: &str,
        variables_json: Option<&str>,
        operation: Option<&str>,
        headers_json: Option<&str>,
    ) -> PyResult<String> {
        let variables = parse_json(variables_json)?;
        let headers = parse_headers(headers_json)?;
        let inner = self.inner.clone();
        let query = query.to_owned();
        let operation = operation.map(str::to_owned);
        py.detach(move || {
            let response = inner
                .graphql(&query, operation.as_deref(), variables, &headers)
                .map_err(core_error)?;
            serde_json::to_string(&response).map_err(core_error)
        })
    }

    /// Creates a poll-based GraphQL subscription; returns its id.
    #[pyo3(signature = (query, variables_json=None, headers_json=None, poll_ms=100))]
    fn graphql_subscribe(
        &self,
        py: Python<'_>,
        query: &str,
        variables_json: Option<&str>,
        headers_json: Option<&str>,
        poll_ms: u64,
    ) -> PyResult<u64> {
        let variables = parse_json(variables_json)?;
        let headers = parse_headers(headers_json)?;
        let inner = self.inner.clone();
        let query = query.to_owned();
        py.detach(move || {
            let interval = std::time::Duration::from_millis(poll_ms);
            let subscription = inner
                .graphql_subscribe(&query, variables, &headers, interval)
                .map_err(core_error)?;
            Ok(subscription.into_id())
        })
    }

    /// Polls a subscription and returns `{changed,closed,response?}` JSON.
    #[pyo3(signature = (subscription_id, wait_ms=0))]
    fn graphql_next(&self, py: Python<'_>, subscription_id: u64, wait_ms: u64) -> PyResult<String> {
        let inner = self.inner.clone();
        py.detach(move || {
            inner
                .graphql_next_json(subscription_id, std::time::Duration::from_millis(wait_ms))
                .map_err(core_error)
        })
    }

    /// Cancels a subscription.
    fn graphql_unsubscribe(&self, subscription_id: u64) -> bool {
        self.inner.graphql_unsubscribe(subscription_id)
    }
}

fn parse_json(value: Option<&str>) -> PyResult<serde_json::Value> {
    match value {
        Some(text) if !text.trim().is_empty() => serde_json::from_str(text).map_err(core_error),
        _ => Ok(serde_json::json!({})),
    }
}

fn parse_headers(value: Option<&str>) -> PyResult<std::collections::HashMap<String, String>> {
    match parse_json(value)? {
        serde_json::Value::Object(map) => Ok(map
            .into_iter()
            .filter_map(|(key, value)| value.as_str().map(|text| (key, text.to_owned())))
            .collect()),
        _ => Err(pyo3::exceptions::PyValueError::new_err(
            "headers must be a JSON object",
        )),
    }
}

/// Binding version (tracks the workspace version).
#[must_use]
#[pyfunction]
fn version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}

/// Native extension module (`datastreamdb._native`).
#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Instance>()?;
    m.add_function(wrap_pyfunction!(version, m)?)?;
    Ok(())
}
