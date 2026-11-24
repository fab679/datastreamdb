//! Engine: one isolated posting instance. This is the anti-global hub
//! that Go spreads across package state (`pstore`, `MemLayerInstance`,
//! `Oracle()`, `IncrRollup`): storage, schema, oracle, shared cache, and
//! the commit lock live here, so many engines share one process.
//!
//! [`PostingEngine::commit`] runs the whole commit inline (harvest, conflict
//! check, chain append, cache update, oracle decision) where Go fans out
//! across Raft. [`PostingEngine::rollup_pending`] drains the synchronous
//! rollup queue that replaces Go's background `IncrRollup` loop.

#![warn(missing_docs)]

use std::sync::Arc;

use datastreamdb_proto::OracleDelta;
use datastreamdb_schema::SchemaState;
use datastreamdb_store::Backend;
use parking_lot::Mutex;

use crate::errors::{PostingError, Result};
use crate::list::List;
use crate::mem::MemoryLayer;
use crate::oracle::Oracle;
use crate::txn::Txn;
use crate::writer::TxnWriter;

/// One isolated posting engine.
#[derive(Debug)]
pub struct PostingEngine {
    /// Version-chain storage.
    store: Arc<dyn Backend>,
    /// Predicate schema.
    schema: Arc<SchemaState>,
    /// Timestamp oracle.
    oracle: Oracle,
    /// Shared list cache and disk reads.
    mem: MemoryLayer,
    /// Serializes commits and rollups.
    commit_lock: Mutex<()>,
    /// Chain writer.
    writer: TxnWriter,
}

impl PostingEngine {
    /// Opens an engine over `store` with `cache_entries` cached lists.
    #[must_use]
    pub fn open(
        store: Arc<dyn Backend>,
        schema: Arc<SchemaState>,
        cache_entries: usize,
    ) -> Arc<Self> {
        Arc::new(Self {
            writer: TxnWriter::new(store.clone()),
            store,
            schema,
            oracle: Oracle::new(),
            mem: MemoryLayer::new(cache_entries, false),
            commit_lock: Mutex::new(()),
        })
    }

    /// Storage backend.
    #[must_use]
    pub fn store(&self) -> &Arc<dyn Backend> {
        &self.store
    }

    /// Predicate schema.
    #[must_use]
    pub fn schema(&self) -> &Arc<SchemaState> {
        &self.schema
    }

    /// Timestamp oracle.
    #[must_use]
    pub fn oracle(&self) -> &Oracle {
        &self.oracle
    }

    /// Shared memory layer.
    #[must_use]
    pub fn memory(&self) -> &MemoryLayer {
        &self.mem
    }

    /// Current read timestamp (highest assigned).
    #[must_use]
    pub fn read_ts(&self) -> u64 {
        self.oracle.max_assigned()
    }

    /// Begins a transaction at a fresh timestamp.
    #[must_use]
    pub fn begin(self: &Arc<Self>) -> Txn {
        let start_ts = self.oracle.assign_ts();
        self.begin_at(start_ts)
    }

    /// Begins a transaction at an explicit timestamp (distribution layer).
    ///
    /// Raft-applied mutations carry zero-assigned timestamps; every group
    /// must run them under the same start timestamp.
    #[must_use]
    pub fn begin_at(self: &Arc<Self>, start_ts: u64) -> Txn {
        let pending = self.oracle.register_start_ts(start_ts);
        Txn::new(self.clone(), start_ts, pending)
    }

    /// Commits a transaction, returning its commit timestamp.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError::Conflict`] on write-write conflict or
    /// [`PostingError`] on storage failure.
    pub fn commit(&self, txn: &Txn) -> Result<u64> {
        self.commit_inner(txn, None)
    }

    /// Commits a transaction at an explicit timestamp (distribution layer).
    ///
    /// Raft-applied mutations commit at zero's timestamp on every group.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError::Conflict`] on write-write conflict or
    /// [`PostingError`] on storage failure.
    pub fn commit_at(&self, txn: &Txn, commit_ts: u64) -> Result<()> {
        self.commit_inner(txn, Some(commit_ts))?;
        Ok(())
    }

    /// Shared commit path: conflict check, chain append, oracle decision.
    ///
    /// `None` assigns a fresh timestamp after the conflict check, exactly
    /// like [`PostingEngine::commit`] always has (aborts consume nothing).
    fn commit_inner(&self, txn: &Txn, commit_ts: Option<u64>) -> Result<u64> {
        let _guard = self.commit_lock.lock();
        if txn.should_abort() {
            self.oracle.apply_decision(txn.start_ts(), 0);
            return Err(PostingError::Conflict);
        }
        txn.update();
        let conflicts = txn.conflicts();
        if self.oracle.has_conflict(txn.start_ts(), &conflicts) {
            txn.mark_aborted();
            self.oracle.apply_decision(txn.start_ts(), 0);
            return Err(PostingError::Conflict);
        }
        let commit_ts = commit_ts.unwrap_or_else(|| self.oracle.assign_ts());
        txn.cache().update_commit_ts(commit_ts);
        txn.commit_to_disk(&self.writer, commit_ts)?;
        self.oracle.record_commit(commit_ts, &conflicts);
        txn.update_cached_keys(commit_ts)?;
        self.oracle.apply_decision(txn.start_ts(), commit_ts);
        Ok(commit_ts)
    }

    /// Aborts a transaction.
    pub fn abort(&self, txn: &Txn) {
        txn.mark_aborted();
        self.oracle.apply_decision(txn.start_ts(), 0);
    }

    /// Applies an oracle delta (multi-engine progress sharing).
    pub fn process_delta(&self, delta: &OracleDelta) {
        self.oracle.process_delta(delta);
    }

    /// Reads a list without involving any transaction cache.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage failure or stale reads.
    pub fn get_no_store(&self, key: &[u8], read_ts: u64) -> Result<List> {
        self.mem.read_data(&self.store, key, read_ts, false)
    }

    /// Reads a list with warmed UIDs.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage failure or stale reads.
    pub fn get_list(&self, key: &[u8], read_ts: u64) -> Result<List> {
        self.mem.read_data(&self.store, key, read_ts, true)
    }

    /// Rolls one key up at a fresh timestamp.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage failure.
    pub fn rollup_key(&self, key: &[u8]) -> Result<()> {
        let _guard = self.commit_lock.lock();
        let read_ts = self.oracle.assign_ts();
        self.rollup_key_at(key, read_ts)
    }

    /// Rolls every flagged key up at one shared timestamp.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage failure.
    pub fn rollup_pending(&self) -> Result<()> {
        let keys = self.mem.take_pending_rollups();
        if keys.is_empty() {
            return Ok(());
        }
        let _guard = self.commit_lock.lock();
        let read_ts = self.oracle.assign_ts();
        let mut seen = std::collections::HashSet::new();
        for key in &keys {
            if seen.insert(key.clone()) {
                self.rollup_key_at(key, read_ts)?;
            }
        }
        Ok(())
    }

    /// Rolls one key up at `read_ts` (commit lock held).
    fn rollup_key_at(&self, key: &[u8], read_ts: u64) -> Result<()> {
        self.oracle.wait_for_ts(read_ts);
        let list = self.mem.read_data(&self.store, key, read_ts, false)?;
        let Some(out) = list.rollup(read_ts, true)? else {
            return Ok(());
        };
        let version = out.new_min_ts + u64::from(read_ts != u64::MAX);
        let empty = out.plist.pack.as_ref().is_none_or(|p| p.blocks.is_empty())
            && out.plist.postings.is_empty()
            && out.parts.is_empty();
        self.writer.write_base(
            key,
            version,
            if empty { None } else { Some(out.plist.clone()) },
        )?;
        for (start_uid, part) in &out.parts {
            let part_key = datastreamdb_x::split_key(key, *start_uid)?;
            let part_empty =
                part.pack.as_ref().is_none_or(|p| p.blocks.is_empty()) && part.postings.is_empty();
            self.writer.write_base(
                &part_key,
                out.new_min_ts,
                if part_empty { None } else { Some(part.clone()) },
            )?;
        }
        // Tombstone removed splits at the parent version so readers at the
        // old base still resolve them.
        for split in list.part_splits() {
            if out.parts.contains_key(&split) {
                continue;
            }
            let part_key = datastreamdb_x::split_key(key, split)?;
            self.writer.write_base(&part_key, version, None)?;
        }
        self.mem.del(key);
        Ok(())
    }
}
