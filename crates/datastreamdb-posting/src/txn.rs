//! Transactions and their local caches. Port of `Txn` plus `LocalCache`
//! from `posting/oracle.go` / `posting/lists.go`.
//!
//! Each transaction buffers the lists it touches in a [`LocalCache`];
//! [`Txn::commit_to_disk`] spills the harvested deltas into version chains.
//! Delta from Go: the conflict/predicate reporting of `FillContext` targets
//! Zero, which does not exist embedded, so it only stamps the start
//! timestamp; [`Txn::predicates`] exposes the predicate set for callers that
//! need it.

#![warn(missing_docs)]

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use datastreamdb_proto::{PostingList, TxnContext};
use parking_lot::Mutex;
use prost::Message as _;

use crate::engine::PostingEngine;
use crate::errors::Result;
use crate::layer::MutableLayer;
use crate::list::List;
use crate::oracle::PendingTxn;
use crate::store::read_chain;
use crate::writer::TxnWriter;

/// Per-transaction list and delta cache.
#[derive(Debug)]
pub struct LocalCache {
    /// Owning engine (storage, memory layer).
    engine: Arc<PostingEngine>,
    /// Transaction start timestamp.
    start_ts: u64,
    /// Commit timestamp (set when committing).
    commit_ts: Mutex<u64>,
    /// Harvested deltas by key.
    deltas: Mutex<HashMap<Vec<u8>, Vec<u8>>>,
    /// Highest committed version read per key.
    max_versions: Mutex<HashMap<Vec<u8>, u64>>,
    /// Buffered lists by key.
    plists: Mutex<HashMap<Vec<u8>, List>>,
    /// Whether `plists` caching is enabled (`NoCache` disables it).
    cache_lists: bool,
}

impl LocalCache {
    /// Caching local cache for a transaction.
    #[must_use]
    pub fn new(engine: Arc<PostingEngine>, start_ts: u64) -> Self {
        Self {
            engine,
            start_ts,
            commit_ts: Mutex::new(0),
            deltas: Mutex::new(HashMap::new()),
            max_versions: Mutex::new(HashMap::new()),
            plists: Mutex::new(HashMap::new()),
            cache_lists: true,
        }
    }

    /// Non-caching local cache (start-timestamp carrier only).
    #[must_use]
    pub fn no_cache(engine: Arc<PostingEngine>, start_ts: u64) -> Self {
        Self {
            engine,
            start_ts,
            commit_ts: Mutex::new(0),
            deltas: Mutex::new(HashMap::new()),
            max_versions: Mutex::new(HashMap::new()),
            plists: Mutex::new(HashMap::new()),
            cache_lists: false,
        }
    }

    /// Start timestamp.
    #[must_use]
    pub fn start_ts(&self) -> u64 {
        self.start_ts
    }

    /// Records the commit timestamp.
    pub fn update_commit_ts(&self, commit_ts: u64) {
        *self.commit_ts.lock() = commit_ts;
    }

    /// Inserts unless present, returning the resident list (prevents
    /// orphaned copies across threads sharing the transaction).
    pub fn set_if_absent(&self, key: &[u8], updated: List) -> List {
        let mut plists = self.plists.lock();
        if let Some(pl) = plists.get(key) {
            return pl.clone();
        }
        plists.insert(key.to_vec(), updated.clone());
        updated
    }

    /// Resolves a list, optionally from disk, applying harvested deltas.
    fn get_internal(&self, key: &[u8], read_from_disk: bool, read_uids: bool) -> Result<List> {
        if !self.cache_lists {
            return self.engine.memory().read_data(
                self.engine.store(),
                key,
                self.start_ts,
                read_uids,
            );
        }
        if let Some(list) = self.plists.lock().get(key) {
            return Ok(list.clone());
        }

        let list = if read_from_disk {
            self.engine
                .memory()
                .read_data(self.engine.store(), key, self.start_ts, read_uids)?
        } else {
            List::from_parts(
                key.to_vec(),
                PostingList::default(),
                MutableLayer::new(),
                0,
                0,
                self.engine.store().clone(),
            )
        };

        if let Some(delta) = self.deltas.lock().get(key) {
            if !delta.is_empty() {
                list.set_mutation(self.start_ts, delta)?;
            }
        }
        Ok(self.set_if_absent(key, list))
    }

    /// Cached list, reading through to disk.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`](crate::PostingError) on storage failure.
    pub fn get(&self, key: &[u8]) -> Result<List> {
        self.get_internal(key, true, false)
    }

    /// Cached list with warmed UIDs, reading through to disk.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`](crate::PostingError) on storage failure.
    pub fn get_uids(&self, key: &[u8]) -> Result<List> {
        self.get_internal(key, true, true)
    }

    /// Cached list from deltas only (mutation-only path, no disk read).
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`](crate::PostingError) on storage failure.
    pub fn get_from_delta(&self, key: &[u8]) -> Result<List> {
        self.get_internal(key, false, false)
    }

    /// Newest visible single-version entries for scalar fast paths.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`](crate::PostingError) on storage failure.
    pub fn get_single_posting(&self, key: &[u8]) -> Result<Option<PostingList>> {
        if let Some(delta) = self.deltas.lock().get(key) {
            if !delta.is_empty() {
                return Ok(Some(PostingList::decode(delta.as_slice())?));
            }
        }
        if let Some(list) = self.plists.lock().get(key) {
            return list.static_value(self.start_ts);
        }
        let Some(mut pl) = self.read_posting_list_at(key)? else {
            return Ok(None);
        };
        // Filter out STAR_ALL and OP_DELETE postings.
        if pl.postings.iter().any(crate::list::has_delete_all) {
            return Ok(None);
        }
        pl.postings.retain(|p| p.op != crate::list::OP_DEL);
        Ok(Some(pl))
    }

    /// Reads the newest chain entry visible at the start timestamp.
    fn read_posting_list_at(&self, key: &[u8]) -> Result<Option<PostingList>> {
        let Some(chain) = read_chain(self.engine.store().as_ref(), key)? else {
            return Ok(None);
        };
        for delta in &chain.deltas {
            if delta.commit_ts <= self.start_ts {
                return Ok(delta.plist.clone());
            }
        }
        Ok(chain.base.and_then(|base| {
            if base.commit_ts <= self.start_ts && !base.empty {
                base.plist
            } else {
                None
            }
        }))
    }

    /// Harvests mutations into deltas and drops buffered lists.
    pub fn update_deltas_and_discard_lists(&self) {
        let mut plists = self.plists.lock();
        if plists.is_empty() {
            return;
        }
        let mut deltas = self.deltas.lock();
        let mut max_versions = self.max_versions.lock();
        for (key, pl) in plists.iter() {
            let data = pl.get_mutation(self.start_ts);
            if !data.is_empty() {
                deltas.insert(key.clone(), data);
            }
            max_versions.insert(key.clone(), pl.max_version());
        }
        plists.clear();
    }
}

/// Transaction handle: oracle registration plus a local cache.
#[derive(Debug)]
pub struct Txn {
    /// Owning engine.
    engine: Arc<PostingEngine>,
    /// Start timestamp.
    start_ts: u64,
    /// Shared oracle-side state.
    pending: Arc<PendingTxn>,
    /// Local list/delta cache.
    cache: Arc<LocalCache>,
}

impl Txn {
    /// Builds a handle over a registered pending entry.
    #[must_use]
    pub(crate) fn new(engine: Arc<PostingEngine>, start_ts: u64, pending: Arc<PendingTxn>) -> Self {
        let cache = Arc::new(LocalCache::new(engine.clone(), start_ts));
        Self {
            engine,
            start_ts,
            pending,
            cache,
        }
    }

    /// Start timestamp.
    #[must_use]
    pub fn start_ts(&self) -> u64 {
        self.start_ts
    }

    /// Local cache.
    #[must_use]
    pub fn cache(&self) -> &Arc<LocalCache> {
        &self.cache
    }

    /// Owning engine.
    #[must_use]
    pub(crate) fn engine(&self) -> &Arc<crate::engine::PostingEngine> {
        &self.engine
    }

    /// Whether the transaction must abort.
    #[must_use]
    pub fn should_abort(&self) -> bool {
        self.pending.should_abort.load(Ordering::SeqCst)
    }

    /// Marks the transaction aborted.
    pub(crate) fn mark_aborted(&self) {
        self.pending.should_abort.store(true, Ordering::SeqCst);
    }

    /// Records a conflict fingerprint.
    pub fn add_conflict_key(&self, conflict_key: u64) {
        if conflict_key > 0 {
            self.pending.conflicts.lock().insert(conflict_key);
        }
    }

    /// Conflict fingerprints gathered so far.
    #[must_use]
    pub fn conflicts(&self) -> HashSet<u64> {
        self.pending.conflicts.lock().clone()
    }

    /// Cached list for `key`.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`](crate::PostingError) on storage failure.
    pub fn get(&self, key: &[u8]) -> Result<List> {
        self.cache.get(key)
    }

    /// Delta-only list for `key`.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`](crate::PostingError) on storage failure.
    pub fn get_from_delta(&self, key: &[u8]) -> Result<List> {
        self.cache.get_from_delta(key)
    }

    /// Scalar list, hydrating an untouched list from disk.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`](crate::PostingError) on storage failure.
    pub fn get_scalar_list(&self, key: &[u8]) -> Result<List> {
        let list = self.cache.get_from_delta(key)?;
        let needs_hydration = {
            let inner = list.inner.read();
            inner.mutation_map.is_empty() && inner.plist.postings.is_empty()
        };
        if needs_hydration {
            if let Some(pl) = self.cache.read_posting_list_at(key)? {
                if pl.pack.is_some() {
                    list.inner.write().plist = pl;
                } else if pl.commit_ts == 0 {
                    list.inner
                        .write()
                        .mutation_map
                        .set_current_entries(self.start_ts, pl);
                } else {
                    list.inner
                        .write()
                        .mutation_map
                        .insert_committed_postings(pl);
                }
            }
        }
        Ok(list)
    }

    /// Harvests mutations into deltas and drops buffered lists.
    pub fn update(&self) {
        self.cache.update_deltas_and_discard_lists();
    }

    /// Stores a list in the transaction cache (tests).
    #[must_use]
    pub fn store(&self, list: &List) -> List {
        let key = list.key();
        self.cache.set_if_absent(&key, list.clone())
    }

    /// Stamps the transaction context (embedded: start timestamp only).
    pub fn fill_context(&self, ctx: &mut TxnContext) {
        ctx.start_ts = self.start_ts;
    }

    /// Predicates touched by this transaction's deltas.
    #[must_use]
    pub fn predicates(&self) -> Vec<String> {
        let deltas = self.cache.deltas.lock();
        let mut preds: Vec<String> = deltas
            .keys()
            .filter_map(|key| datastreamdb_x::parse(key).ok())
            .map(|pk| pk.attr)
            .filter(|attr| !attr.is_empty())
            .collect();
        preds.sort();
        preds.dedup();
        preds
    }

    /// Writes harvested deltas at `commit_ts` (no-op for timestamp 0).
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`](crate::PostingError) on storage failure.
    pub fn commit_to_disk(&self, writer: &TxnWriter, commit_ts: u64) -> Result<()> {
        if commit_ts == 0 {
            return Ok(());
        }
        let keys: Vec<Vec<u8>> = self.cache.deltas.lock().keys().cloned().collect();
        for key in &keys {
            let data = self
                .cache
                .deltas
                .lock()
                .get(key)
                .cloned()
                .unwrap_or_default();
            if data.is_empty() {
                continue;
            }
            if self
                .cache
                .max_versions
                .lock()
                .get(key)
                .is_some_and(|ts| *ts >= commit_ts)
            {
                continue;
            }
            writer.append_delta(key, commit_ts, &data)?;
        }
        for key in &keys {
            self.engine.memory().flag_rollup(key);
        }
        Ok(())
    }

    /// Applies this transaction's deltas to the shared cache.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`](crate::PostingError) on decode failure.
    pub fn update_cached_keys(&self, commit_ts: u64) -> Result<()> {
        let deltas: Vec<(Vec<u8>, Vec<u8>)> = self
            .cache
            .deltas
            .lock()
            .iter()
            .map(|(key, delta)| (key.clone(), delta.clone()))
            .collect();
        for (key, delta) in &deltas {
            self.engine
                .memory()
                .update_item_in_cache(key, delta, self.start_ts, commit_ts)?;
        }
        Ok(())
    }
}
