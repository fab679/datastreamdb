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
}

/// Binding version (tracks the workspace version).
#[must_use]
#[napi]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}
