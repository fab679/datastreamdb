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
