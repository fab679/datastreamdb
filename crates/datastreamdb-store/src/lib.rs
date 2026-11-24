//! Pluggable LSM storage: memory + fjall backends.
//!
//! This crate replaces Badger. The engine talks only to [`Backend`]: point
//! reads/writes, prefix scans, atomic write batches, range deletes, and
//! explicit close/flush. Durability comes from [`FjallBackend`] (one locked
//! directory per instance); tests and ephemeral embeds use [`MemoryBackend`].
//! See `docs/STORAGE.md` for the mapping and crash model.

#![warn(missing_docs)]

use std::{collections::BTreeMap, path::Path};

use parking_lot::RwLock;

/// Storage errors. Variants never include key/value bytes or paths.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// I/O failure opening or syncing the database directory.
    #[error("storage I/O: {0}")]
    Io(#[from] std::io::Error),
    /// Backend-internal failure (message only).
    #[error("backend error: {0}")]
    Backend(String),
    /// The `fjall` backend was requested but compiled out.
    #[error("fjall backend not compiled in (enable the `fjall` feature)")]
    BackendUnavailable,
    /// fjall engine failure.
    #[cfg(feature = "fjall")]
    #[error(transparent)]
    Fjall(#[from] fjall::Error),
}

/// Owned key/value byte pair returned by scans.
pub type KvPair = (Vec<u8>, Vec<u8>);

/// Partition-scoped map key for [`MemoryBackend`].
type PartitionKey = (String, Vec<u8>);

/// One operation inside a [`WriteBatch`].
#[derive(Debug, Clone)]
pub enum WriteOp {
    /// Upsert `key` in `partition`.
    Put {
        /// Partition name.
        partition: String,
        /// Key bytes.
        key: Vec<u8>,
        /// Value bytes.
        value: Vec<u8>,
    },
    /// Delete `key` from `partition` (missing keys are a no-op).
    Delete {
        /// Partition name.
        partition: String,
        /// Key bytes.
        key: Vec<u8>,
    },
}

/// Atomic group of puts/deletes applied via [`Backend::write_batch`].
#[derive(Debug, Clone, Default)]
pub struct WriteBatch {
    ops: Vec<WriteOp>,
}

impl WriteBatch {
    /// Empty batch.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Stages a put. Returns `self` for chaining.
    pub fn put(&mut self, partition: &str, key: &[u8], value: &[u8]) -> &mut Self {
        self.ops.push(WriteOp::Put {
            partition: partition.to_owned(),
            key: key.to_vec(),
            value: value.to_vec(),
        });
        self
    }

    /// Stages a delete. Returns `self` for chaining.
    pub fn delete(&mut self, partition: &str, key: &[u8]) -> &mut Self {
        self.ops.push(WriteOp::Delete {
            partition: partition.to_owned(),
            key: key.to_vec(),
        });
        self
    }

    /// Staged operations in insertion order.
    #[must_use]
    pub fn ops(&self) -> &[WriteOp] {
        &self.ops
    }

    /// Number of staged operations.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    /// Whether the batch stages no operations.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
}

/// Key-value backend used by the engine. Sync, `Send + Sync`, partition-scoped.
pub trait Backend: Send + Sync + std::fmt::Debug + 'static {
    /// Backend name for logs and metrics (`"memory"`, `"fjall"`).
    fn name(&self) -> &'static str;

    /// Point read. Returns `None` for missing keys.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] on backend failure.
    fn get(&self, partition: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError>;

    /// Point write (upsert).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] on backend failure.
    fn put(&self, partition: &str, key: &[u8], value: &[u8]) -> Result<(), StoreError>;

    /// Point delete (missing keys are a no-op).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] on backend failure.
    fn delete(&self, partition: &str, key: &[u8]) -> Result<(), StoreError>;

    /// All entries in `partition` whose key starts with `prefix`, in key order.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] on backend failure.
    fn scan_prefix(&self, partition: &str, prefix: &[u8]) -> Result<Vec<KvPair>, StoreError>;

    /// Applies `batch` atomically: all ops land or none do.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] on backend failure.
    fn write_batch(&self, batch: &WriteBatch) -> Result<(), StoreError>;

    /// Deletes keys in `partition` over `start..end`. Returns the delete count.
    /// An empty or inverted range deletes nothing.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] on backend failure.
    fn delete_range(&self, partition: &str, start: &[u8], end: &[u8]) -> Result<u64, StoreError>;

    /// Makes all prior writes durable.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] on backend failure.
    fn flush(&self) -> Result<(), StoreError>;

    /// Flushes durable state as the last fallible shutdown step.
    /// Locks and file handles release on drop, so owners must `drop` the
    /// backend after `close` to free the directory for reopening. Dropping
    /// without `close` still flushes on durable backends but reports nothing.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] on backend failure.
    fn close(&self) -> Result<(), StoreError>;
}

/// In-memory backend: a locked ordered map. No durability, no I/O.
#[derive(Debug, Default)]
pub struct MemoryBackend {
    inner: RwLock<BTreeMap<PartitionKey, Vec<u8>>>,
}

impl MemoryBackend {
    /// Empty in-memory backend.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl Backend for MemoryBackend {
    fn name(&self) -> &'static str {
        "memory"
    }

    fn get(&self, partition: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self
            .inner
            .read()
            .get(&(partition.to_owned(), key.to_vec()))
            .cloned())
    }

    fn put(&self, partition: &str, key: &[u8], value: &[u8]) -> Result<(), StoreError> {
        self.inner
            .write()
            .insert((partition.to_owned(), key.to_vec()), value.to_vec());
        Ok(())
    }

    fn delete(&self, partition: &str, key: &[u8]) -> Result<(), StoreError> {
        self.inner
            .write()
            .remove(&(partition.to_owned(), key.to_vec()));
        Ok(())
    }

    fn scan_prefix(&self, partition: &str, prefix: &[u8]) -> Result<Vec<KvPair>, StoreError> {
        Ok(self
            .inner
            .read()
            .range((partition.to_owned(), prefix.to_vec())..)
            .take_while(|((p, k), _)| p == partition && k.starts_with(prefix))
            .map(|((_, k), v)| (k.clone(), v.clone()))
            .collect())
    }

    fn write_batch(&self, batch: &WriteBatch) -> Result<(), StoreError> {
        let mut guard = self.inner.write();
        for op in batch.ops() {
            match op {
                WriteOp::Put {
                    partition,
                    key,
                    value,
                } => {
                    guard.insert((partition.clone(), key.clone()), value.clone());
                }
                WriteOp::Delete { partition, key } => {
                    guard.remove(&(partition.clone(), key.clone()));
                }
            }
        }
        Ok(())
    }

    fn delete_range(&self, partition: &str, start: &[u8], end: &[u8]) -> Result<u64, StoreError> {
        if end < start {
            return Ok(0);
        }
        let mut guard = self.inner.write();
        let keys: Vec<PartitionKey> = guard
            .range((partition.to_owned(), start.to_vec())..(partition.to_owned(), end.to_vec()))
            .map(|(k, _)| k.clone())
            .collect();
        let count = keys.len() as u64;
        for key in keys {
            guard.remove(&key);
        }
        Ok(count)
    }

    fn flush(&self) -> Result<(), StoreError> {
        Ok(())
    }

    fn close(&self) -> Result<(), StoreError> {
        Ok(())
    }
}

/// Lock file name inside a database directory (one live backend per directory).
#[cfg(feature = "fjall")]
const LOCK_FILE: &str = "LOCK";

/// Durable fjall-backed database: one locked directory per engine instance.
#[cfg(feature = "fjall")]
pub struct FjallBackend {
    db: fjall::Database,
    partitions: RwLock<std::collections::HashMap<String, fjall::Keyspace>>,
    /// Held open for the backend lifetime; the OS lock dies with this handle.
    _lock: std::fs::File,
}

#[cfg(feature = "fjall")]
impl std::fmt::Debug for FjallBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FjallBackend").finish_non_exhaustive()
    }
}

#[cfg(feature = "fjall")]
impl FjallBackend {
    /// Opens (creating) the database rooted at `dir`.
    ///
    /// Takes an exclusive `LOCK` file: a second live backend on the same
    /// directory fails with [`StoreError::Backend`].
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the directory cannot be opened or is locked.
    pub fn open(dir: &Path) -> Result<Self, StoreError> {
        use fs2::FileExt as _;

        std::fs::create_dir_all(dir)?;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(LOCK_FILE))?;
        lock.try_lock_exclusive().map_err(|err| {
            if err.kind() == std::io::ErrorKind::WouldBlock {
                StoreError::Backend("database directory is locked by another instance".to_owned())
            } else {
                StoreError::Io(err)
            }
        })?;
        let db = fjall::Database::builder(dir).open()?;
        Ok(Self {
            db,
            partitions: RwLock::new(std::collections::HashMap::new()),
            _lock: lock,
        })
    }

    fn keyspace(&self, partition: &str) -> Result<fjall::Keyspace, StoreError> {
        if let Some(ks) = self.partitions.read().get(partition) {
            return Ok(ks.clone());
        }
        let mut guard = self.partitions.write();
        if let Some(ks) = guard.get(partition) {
            return Ok(ks.clone());
        }
        let ks = self
            .db
            .keyspace(partition, fjall::KeyspaceCreateOptions::default)?;
        guard.insert(partition.to_owned(), ks.clone());
        Ok(ks)
    }

    fn persist(&self) -> Result<(), StoreError> {
        self.db.persist(fjall::PersistMode::SyncAll)?;
        Ok(())
    }
}

#[cfg(feature = "fjall")]
impl Backend for FjallBackend {
    fn name(&self) -> &'static str {
        "fjall"
    }

    fn get(&self, partition: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self.keyspace(partition)?.get(key)?.map(|s| s.to_vec()))
    }

    fn put(&self, partition: &str, key: &[u8], value: &[u8]) -> Result<(), StoreError> {
        self.keyspace(partition)?.insert(key, value)?;
        Ok(())
    }

    fn delete(&self, partition: &str, key: &[u8]) -> Result<(), StoreError> {
        self.keyspace(partition)?.remove(key)?;
        Ok(())
    }

    fn scan_prefix(&self, partition: &str, prefix: &[u8]) -> Result<Vec<KvPair>, StoreError> {
        let mut out = Vec::new();
        for guard in self.keyspace(partition)?.prefix(prefix) {
            let (k, v) = guard.into_inner()?;
            out.push((k.to_vec(), v.to_vec()));
        }
        Ok(out)
    }

    fn write_batch(&self, batch: &WriteBatch) -> Result<(), StoreError> {
        let mut wb = self.db.batch();
        for op in batch.ops() {
            match op {
                WriteOp::Put {
                    partition,
                    key,
                    value,
                } => {
                    wb.insert(&self.keyspace(partition)?, key.as_slice(), value.as_slice());
                }
                WriteOp::Delete { partition, key } => {
                    wb.remove(&self.keyspace(partition)?, key.as_slice());
                }
            }
        }
        wb.commit()?;
        Ok(())
    }

    fn delete_range(&self, partition: &str, start: &[u8], end: &[u8]) -> Result<u64, StoreError> {
        if end < start {
            return Ok(0);
        }
        let ks = self.keyspace(partition)?;
        let keys: Vec<Vec<u8>> = ks
            .range(start..end)
            .map(|guard| guard.into_inner().map(|(k, _)| k.to_vec()))
            .collect::<Result<_, _>>()?;
        let count = keys.len() as u64;
        let mut wb = self.db.batch();
        for key in &keys {
            wb.remove(&ks, key.as_slice());
        }
        wb.commit()?;
        Ok(count)
    }

    fn flush(&self) -> Result<(), StoreError> {
        self.persist()
    }

    fn close(&self) -> Result<(), StoreError> {
        self.persist()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exercise(backend: &dyn Backend) {
        assert_eq!(backend.get("p", b"k").expect("get"), None);
        backend.put("p", b"k", b"v").expect("put");
        backend.put("p", b"k2", b"v2").expect("put");
        backend.put("other", b"k", b"nope").expect("put");
        assert_eq!(backend.get("p", b"k").expect("get"), Some(b"v".to_vec()));
        assert_eq!(
            backend.scan_prefix("p", b"k").expect("scan"),
            [
                (b"k".to_vec(), b"v".to_vec()),
                (b"k2".to_vec(), b"v2".to_vec())
            ]
        );
        assert!(backend.scan_prefix("p", b"zzz").expect("scan").is_empty());
        backend.delete("p", b"k").expect("delete");
        assert_eq!(backend.get("p", b"k").expect("get"), None);
        backend.flush().expect("flush");
        backend.close().expect("close");
    }

    fn exercise_batch_and_range(backend: &dyn Backend) {
        let mut batch = WriteBatch::new();
        assert!(batch.is_empty());
        batch
            .put("p", b"a", b"1")
            .put("p", b"b", b"2")
            .put("p", b"c", b"3")
            .put("q", b"a", b"9")
            .delete("p", b"gone");
        assert_eq!(batch.len(), 5);
        backend.write_batch(&batch).expect("batch");
        assert_eq!(backend.get("p", b"b").expect("get"), Some(b"2".to_vec()));
        assert_eq!(backend.get("q", b"a").expect("get"), Some(b"9".to_vec()));

        // Range delete is [start, end): removes a, b; keeps c.
        assert_eq!(backend.delete_range("p", b"a", b"c").expect("range"), 2);
        assert_eq!(backend.get("p", b"a").expect("get"), None);
        assert_eq!(backend.get("p", b"c").expect("get"), Some(b"3".to_vec()));
        // Inverted and empty ranges delete nothing.
        assert_eq!(backend.delete_range("p", b"z", b"a").expect("range"), 0);
        assert_eq!(backend.delete_range("p", b"c", b"c").expect("range"), 0);
        // Partitions are independent.
        assert_eq!(backend.get("q", b"a").expect("get"), Some(b"9".to_vec()));

        let mut cleanup = WriteBatch::new();
        cleanup.delete("p", b"c").delete("q", b"a");
        backend.write_batch(&cleanup).expect("cleanup");
        assert!(backend.scan_prefix("p", b"").expect("scan").is_empty());
    }

    #[test]
    fn memory_backend_conforms() {
        exercise(&MemoryBackend::new());
        exercise_batch_and_range(&MemoryBackend::new());
    }

    #[test]
    fn batch_builder_reports_shape() {
        let mut batch = WriteBatch::new();
        batch.put("p", b"k", b"v");
        assert_eq!(batch.len(), 1);
        assert!(matches!(batch.ops()[0], WriteOp::Put { .. }));
    }

    #[cfg(feature = "fjall")]
    #[test]
    fn fjall_backend_conforms_and_reopens() {
        let dir = tempfile::tempdir().expect("tempdir");
        exercise(&FjallBackend::open(dir.path()).expect("open"));
        // Data survives close + reopen.
        let backend = FjallBackend::open(dir.path()).expect("reopen");
        assert_eq!(
            backend.get("other", b"k").expect("get"),
            Some(b"nope".to_vec())
        );
        assert_eq!(backend.get("p", b"k2").expect("get"), Some(b"v2".to_vec()));
        backend.close().expect("close");

        let batch_dir = tempfile::tempdir().expect("tempdir");
        let batch_backend = FjallBackend::open(batch_dir.path()).expect("open");
        exercise_batch_and_range(&batch_backend);
        // Batches survive close + reopen (drop releases the directory lock).
        batch_backend.put("p", b"dur", b"able").expect("put");
        batch_backend.close().expect("close");
        drop(batch_backend);
        let reopened = FjallBackend::open(batch_dir.path()).expect("reopen");
        assert_eq!(
            reopened.get("p", b"dur").expect("get"),
            Some(b"able".to_vec())
        );
    }

    #[cfg(feature = "fjall")]
    #[test]
    fn fjall_backend_locks_its_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = FjallBackend::open(dir.path()).expect("first open");
        let err = FjallBackend::open(dir.path()).expect_err("second open must fail");
        assert!(matches!(err, StoreError::Backend(_)), "{err:?}");
        drop(first);
        FjallBackend::open(dir.path()).expect("open after close");
    }

    #[cfg(feature = "fjall")]
    #[test]
    fn fjall_unflushed_drop_still_recovers() {
        // No explicit flush/close: drop must persist (fjall persists on drop).
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let backend = FjallBackend::open(dir.path()).expect("open");
            backend.put("p", b"k", b"v").expect("put");
        }
        let backend = FjallBackend::open(dir.path()).expect("reopen");
        assert_eq!(backend.get("p", b"k").expect("get"), Some(b"v".to_vec()));
    }

    #[test]
    fn backends_handle_concurrent_access() {
        let backend = std::sync::Arc::new(MemoryBackend::new());
        let mut handles = Vec::new();
        for worker in 0..8u32 {
            let backend = backend.clone();
            handles.push(std::thread::spawn(move || {
                for n in 0..250 {
                    let key = format!("w{worker}-{n:04}");
                    backend
                        .put("p", key.as_bytes(), &worker.to_be_bytes())
                        .expect("put");
                    let mut batch = WriteBatch::new();
                    batch.put("p", format!("b{worker}-{n:04}").as_bytes(), b"x");
                    backend.write_batch(&batch).expect("batch");
                }
            }));
        }
        for handle in handles {
            handle.join().expect("join");
        }
        assert_eq!(backend.scan_prefix("p", b"w").expect("scan").len(), 8 * 250);
        assert_eq!(backend.scan_prefix("p", b"b").expect("scan").len(), 8 * 250);
    }

    #[test]
    fn backends_are_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<MemoryBackend>();
        #[cfg(feature = "fjall")]
        assert_send_sync::<FjallBackend>();
    }
}
