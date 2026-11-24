//! Embeddable multi-instance engine facade: open, query, mutate, close.
//!
//! [`Instance`] is the unit of embedding: a host opens any number of isolated
//! instances per process and drives each through typed helpers
//! ([`Instance::put`], [`Instance::get`], [`Instance::scan`], …) or the JSON
//! envelope ([`Instance::execute`]). All bindings (TypeScript, Python, C ABI)
//! funnel through this API. Dropping the last handle flushes and closes that
//! instance. Query planning/execution lands per `TODO.md`; M1 runs document
//! KV ops over [`datastreamdb_store::Backend`].

#![warn(missing_docs)]

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

#[cfg(feature = "fjall")]
use datastreamdb_store::FjallBackend;
use datastreamdb_store::{Backend, MemoryBackend};
use serde::{Deserialize, Serialize};

pub use datastreamdb_store::{KvPair, WriteBatch, WriteOp};

/// Default partition for instance key operations.
pub const DEFAULT_PARTITION: &str = "default";

/// Default cap for envelope scans when `limit` is omitted.
pub const DEFAULT_SCAN_LIMIT: usize = 1000;

/// Hard cap for envelope scans (hosts asking for more are clamped).
pub const MAX_SCAN_LIMIT: usize = 100_000;

/// Engine errors. Messages are host-safe (no key/value bytes, no paths).
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// Malformed config or request payload.
    #[error("bad request: {0}")]
    BadRequest(String),
    /// Storage failure.
    #[error("storage failure: {0}")]
    Store(#[from] datastreamdb_store::StoreError),
}

/// Configuration for one [`Instance`]. Unknown fields are rejected.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstanceConfig {
    /// Database directory, or `None` for a pure in-memory instance.
    /// The directory is locked while the instance is open.
    #[serde(default)]
    pub data_dir: Option<PathBuf>,
    /// Tenant label used in logs, metrics, and responses.
    #[serde(default = "default_tenant")]
    pub tenant: String,
}

fn default_tenant() -> String {
    "default".to_owned()
}

impl InstanceConfig {
    /// In-memory instance with the default tenant.
    #[must_use]
    pub fn memory() -> Self {
        Self {
            data_dir: None,
            tenant: default_tenant(),
        }
    }

    /// Parses JSON config, e.g. `{"data_dir": "/tmp/g", "tenant": "a"}`.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::BadRequest`] on malformed JSON or unknown fields.
    pub fn from_json(json: &str) -> Result<Self, CoreError> {
        serde_json::from_str(json).map_err(|err| CoreError::BadRequest(err.to_string()))
    }
}

/// One isolated database instance. Cheap to clone (`Arc` inside bindings).
#[derive(Debug)]
pub struct Instance {
    id: uuid::Uuid,
    tenant: String,
    store: Box<dyn Backend>,
    executed: AtomicU64,
}

impl Instance {
    /// Opens an instance: memory backend, or locked fjall directory when
    /// `data_dir` is set.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError`] when the directory cannot be opened or is locked.
    pub fn open(config: InstanceConfig) -> Result<Arc<Self>, CoreError> {
        let store: Box<dyn Backend> = match config.data_dir {
            None => Box::new(MemoryBackend::new()),
            Some(dir) => {
                #[cfg(feature = "fjall")]
                {
                    Box::new(FjallBackend::open(&dir)?)
                }
                #[cfg(not(feature = "fjall"))]
                {
                    let _ = dir;
                    return Err(datastreamdb_store::StoreError::BackendUnavailable.into());
                }
            }
        };
        let instance = Arc::new(Self {
            id: uuid::Uuid::new_v4(),
            tenant: config.tenant,
            store,
            executed: AtomicU64::new(0),
        });
        tracing::debug!(instance = %instance.id, backend = instance.store.name(), "opened");
        Ok(instance)
    }

    /// Unique id assigned at open.
    #[must_use]
    pub fn id(&self) -> uuid::Uuid {
        self.id
    }

    /// Tenant label from config.
    #[must_use]
    pub fn tenant(&self) -> &str {
        &self.tenant
    }

    /// Storage backend name (`"memory"` or `"fjall"`).
    #[must_use]
    pub fn backend_name(&self) -> &'static str {
        self.store.name()
    }

    /// Number of [`Instance::execute`] calls served.
    #[must_use]
    pub fn executed(&self) -> u64 {
        self.executed.load(Ordering::Relaxed)
    }

    /// Raw key write into [`DEFAULT_PARTITION`].
    ///
    /// # Errors
    ///
    /// Returns [`CoreError`] on storage failure.
    pub fn put(&self, key: &str, value: &[u8]) -> Result<(), CoreError> {
        self.store.put(DEFAULT_PARTITION, key.as_bytes(), value)?;
        Ok(())
    }

    /// Raw key read from [`DEFAULT_PARTITION`].
    ///
    /// # Errors
    ///
    /// Returns [`CoreError`] on storage failure.
    pub fn get(&self, key: &str) -> Result<Option<Vec<u8>>, CoreError> {
        Ok(self.store.get(DEFAULT_PARTITION, key.as_bytes())?)
    }

    /// Raw key delete from [`DEFAULT_PARTITION`] (missing keys are a no-op).
    ///
    /// # Errors
    ///
    /// Returns [`CoreError`] on storage failure.
    pub fn delete(&self, key: &str) -> Result<(), CoreError> {
        self.store.delete(DEFAULT_PARTITION, key.as_bytes())?;
        Ok(())
    }

    /// Prefix scan over [`DEFAULT_PARTITION`] in key order, capped at `limit`
    /// pairs. Keys are lossy UTF-8 (raw bytes stay available via `get`).
    ///
    /// # Errors
    ///
    /// Returns [`CoreError`] on storage failure.
    pub fn scan(&self, prefix: &str, limit: usize) -> Result<Vec<(String, Vec<u8>)>, CoreError> {
        let pairs = self
            .store
            .scan_prefix(DEFAULT_PARTITION, prefix.as_bytes())?;
        Ok(pairs
            .into_iter()
            .take(limit.min(MAX_SCAN_LIMIT))
            .map(|(k, v)| (String::from_utf8_lossy(&k).into_owned(), v))
            .collect())
    }

    /// Applies an atomic [`WriteBatch`] (any partition).
    ///
    /// # Errors
    ///
    /// Returns [`CoreError`] on storage failure.
    pub fn write_batch(&self, batch: &WriteBatch) -> Result<(), CoreError> {
        self.store.write_batch(batch)?;
        Ok(())
    }

    /// Deletes keys over `start..end` in [`DEFAULT_PARTITION`].
    /// Returns the delete count.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError`] on storage failure.
    pub fn delete_range(&self, start: &str, end: &str) -> Result<u64, CoreError> {
        Ok(self
            .store
            .delete_range(DEFAULT_PARTITION, start.as_bytes(), end.as_bytes())?)
    }

    /// Makes all prior writes durable.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError`] on storage failure.
    pub fn flush(&self) -> Result<(), CoreError> {
        self.store.flush()?;
        Ok(())
    }

    /// Executes a JSON request and returns a JSON response.
    ///
    /// M1 ops: `ping`, `put`, `get`, `delete`, `scan`, `batch`, `flush`.
    /// DQL/GraphQL ops land in M3/M4.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError`] on malformed requests or storage failure.
    pub fn execute(&self, request: &str) -> Result<String, CoreError> {
        self.executed.fetch_add(1, Ordering::Relaxed);
        let req: WireRequest =
            serde_json::from_str(request).map_err(|err| CoreError::BadRequest(err.to_string()))?;
        let response = match req {
            WireRequest::Ping => serde_json::json!({
                "ok": true,
                "instance": self.id.to_string(),
                "tenant": self.tenant,
                "backend": self.store.name(),
            }),
            WireRequest::Put { key, value } => {
                self.put(&key, value.as_bytes())?;
                serde_json::json!({ "ok": true })
            }
            WireRequest::Get { key } => {
                let value = self
                    .get(&key)?
                    .map(|b| String::from_utf8_lossy(&b).into_owned());
                serde_json::json!({ "ok": true, "value": value })
            }
            WireRequest::Delete { key } => {
                self.delete(&key)?;
                serde_json::json!({ "ok": true })
            }
            WireRequest::Scan { prefix, limit } => {
                let pairs = self.scan(&prefix, limit.unwrap_or(DEFAULT_SCAN_LIMIT))?;
                let pairs: Vec<serde_json::Value> = pairs
                    .into_iter()
                    .map(|(k, v)| {
                        serde_json::json!({
                            "key": k,
                            "value": String::from_utf8_lossy(&v).into_owned(),
                        })
                    })
                    .collect();
                serde_json::json!({ "ok": true, "pairs": pairs })
            }
            WireRequest::Batch { puts, deletes } => {
                let mut batch = WriteBatch::new();
                for entry in &puts {
                    batch.put(
                        DEFAULT_PARTITION,
                        entry.key.as_bytes(),
                        entry.value.as_bytes(),
                    );
                }
                for key in &deletes {
                    batch.delete(DEFAULT_PARTITION, key.as_bytes());
                }
                self.write_batch(&batch)?;
                serde_json::json!({ "ok": true, "applied": batch.len() })
            }
            WireRequest::Flush => {
                self.flush()?;
                serde_json::json!({ "ok": true })
            }
        };
        serde_json::to_string(&response).map_err(|err| CoreError::BadRequest(err.to_string()))
    }
}

impl Drop for Instance {
    /// Graceful close: flushes the backend. Failures are logged; use
    /// [`Instance::flush`] beforehand when the result matters.
    fn drop(&mut self) {
        if let Err(err) = self.store.close() {
            tracing::warn!(instance = %self.id, error = %err, "close flush failed");
        } else {
            tracing::debug!(instance = %self.id, "closed");
        }
    }
}

/// One staged put inside a `batch` envelope request.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PutEntry {
    /// Key bytes (UTF-8).
    key: String,
    /// Value bytes (UTF-8).
    value: String,
}

/// M1 request envelope (DQL/GraphQL envelopes land in M3/M4).
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase", deny_unknown_fields)]
enum WireRequest {
    /// Liveness probe.
    Ping,
    /// Raw key write.
    Put {
        /// Key bytes (UTF-8).
        key: String,
        /// Value bytes (UTF-8).
        value: String,
    },
    /// Raw key read.
    Get {
        /// Key bytes (UTF-8).
        key: String,
    },
    /// Raw key delete.
    Delete {
        /// Key bytes (UTF-8).
        key: String,
    },
    /// Prefix scan.
    Scan {
        /// Key prefix (UTF-8).
        #[serde(default)]
        prefix: String,
        /// Max pairs (defaults to [`DEFAULT_SCAN_LIMIT`], clamped).
        #[serde(default)]
        limit: Option<usize>,
    },
    /// Atomic multi-write.
    Batch {
        /// Puts to apply.
        #[serde(default)]
        puts: Vec<PutEntry>,
        /// Keys to delete.
        #[serde(default)]
        deletes: Vec<String>,
    },
    /// Durability fence.
    Flush,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_to_memory() {
        let cfg = InstanceConfig::from_json("{}").expect("parse");
        assert!(cfg.data_dir.is_none());
        assert_eq!(cfg.tenant, "default");
        assert!(InstanceConfig::from_json("{\"nope\": 1}").is_err());
    }

    #[test]
    fn ping_reports_identity() {
        let db = Instance::open(InstanceConfig::memory()).expect("open");
        let res: serde_json::Value =
            serde_json::from_str(&db.execute("{\"op\":\"ping\"}").expect("exec")).expect("json");
        assert_eq!(res["ok"], true);
        assert_eq!(res["instance"], db.id().to_string());
        assert_eq!(res["backend"], "memory");
        assert_eq!(db.executed(), 1);
    }

    #[test]
    fn typed_crud_and_scan() {
        let db = Instance::open(InstanceConfig::memory()).expect("open");
        db.put("user:1", b"Ada").expect("put");
        db.put("user:2", b"Bob").expect("put");
        db.put("post:1", b"hi").expect("put");
        let users = db.scan("user:", 10).expect("scan");
        assert_eq!(users.len(), 2);
        assert_eq!(users[0].0, "user:1");
        assert_eq!(db.scan("user:", 1).expect("scan").len(), 1);
        db.delete("user:1").expect("delete");
        assert_eq!(db.scan("user:", 10).expect("scan").len(), 1);
        assert_eq!(db.delete_range("user:", "user;").expect("range"), 1);
        assert!(
            db.scan("", 10)
                .expect("scan")
                .iter()
                .all(|(k, _)| k == "post:1")
        );
        db.flush().expect("flush");
    }

    #[test]
    fn envelope_batch_is_atomic() {
        let db = Instance::open(InstanceConfig::memory()).expect("open");
        let res: serde_json::Value = serde_json::from_str(
            &db.execute(
                "{\"op\":\"batch\",\"puts\":[{\"key\":\"a\",\"value\":\"1\"},{\"key\":\"b\",\"value\":\"2\"}],\"deletes\":[\"zz\"]}",
            )
            .expect("exec"),
        )
        .expect("json");
        assert_eq!(res["applied"], 3);
        let scan: serde_json::Value = serde_json::from_str(
            &db.execute("{\"op\":\"scan\",\"prefix\":\"\"}")
                .expect("exec"),
        )
        .expect("json");
        assert_eq!(scan["pairs"].as_array().expect("array").len(), 2);
    }
}
