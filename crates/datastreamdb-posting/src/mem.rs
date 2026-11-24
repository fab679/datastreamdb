//! Shared list cache and disk reads. Port of `MemoryLayer`, the
//! ristretto cache wrapper, `ReadPostingList`, and `IterateDisk` from
//! `posting/mvcc.go`.
//!
//! Deltas from Go: the cache is a bounded `lru` map instead of ristretto
//! (count-based capacity, no `TinyLFU`), and reads filter the version chain
//! by timestamp instead of relying on Badger versions. Rollup triggers
//! record keys for the engine's synchronous rollup instead of feeding a
//! background batcher.

#![warn(missing_docs)]

use std::num::NonZeroUsize;
use std::sync::Arc;

use datastreamdb_proto::PostingList;
use datastreamdb_store::Backend;
use datastreamdb_x::ParsedKey;
use lru::LruCache;
use parking_lot::Mutex;
use prost::Message as _;

use crate::errors::{PostingError, Result};
use crate::layer::MutableLayer;
use crate::list::List;
use crate::stats::StatsHolder;
use crate::store::{PARTITION, StoredChain, read_chain};

/// Default list-cache capacity (entries).
pub const DEFAULT_CACHE_ENTRIES: usize = 10_000;

/// Chains with more deltas than this roll up eagerly.
pub const ROLLUP_DELTA_THRESHOLD: usize = 500;

/// Cached list plus its last-update timestamp.
#[derive(Clone, Debug)]
struct CacheEntry {
    /// Cached list snapshot.
    list: List,
    /// Last commit timestamp applied to the entry.
    last_update: u64,
}

/// Shared cache plus disk-read routines for one engine.
#[derive(Debug)]
pub struct MemoryLayer {
    /// Bounded list cache (`None` disables caching).
    cache: Option<Mutex<LruCache<Vec<u8>, CacheEntry>>>,
    /// Drop cache entries on commit instead of updating them.
    remove_on_update: bool,
    /// Predicate statistics.
    stats: StatsHolder,
    /// Keys flagged for rollup (drained by the engine).
    pending_rollups: Mutex<Vec<Vec<u8>>>,
}

impl MemoryLayer {
    /// Builds a layer with room for `cache_entries` lists.
    #[must_use]
    pub fn new(cache_entries: usize, remove_on_update: bool) -> Self {
        let cache = NonZeroUsize::new(cache_entries).map(|cap| Mutex::new(LruCache::new(cap)));
        Self {
            cache,
            remove_on_update,
            stats: StatsHolder::new(),
            pending_rollups: Mutex::new(Vec::new()),
        }
    }

    /// Flags a key for rollup.
    pub fn flag_rollup(&self, key: &[u8]) {
        self.pending_rollups.lock().push(key.to_vec());
    }

    /// Takes the flagged rollup keys.
    #[must_use]
    pub fn take_pending_rollups(&self) -> Vec<Vec<u8>> {
        std::mem::take(&mut *self.pending_rollups.lock())
    }

    /// Predicate statistics holder.
    #[must_use]
    pub fn stats(&self) -> &StatsHolder {
        &self.stats
    }

    /// Drops every cached list.
    pub fn clear(&self) {
        if let Some(cache) = &self.cache {
            cache.lock().clear();
        }
    }

    /// Drops the cached list for `key`.
    pub fn del(&self, key: &[u8]) {
        if let Some(cache) = &self.cache {
            cache.lock().pop(key);
        }
    }

    /// Reads a list through the cache, falling back to disk.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage failure or stale reads.
    pub fn read_data(
        &self,
        store: &Arc<dyn Backend>,
        key: &[u8],
        read_ts: u64,
        read_uids: bool,
    ) -> Result<List> {
        if let Some(list) = self.read_from_cache(key, read_ts, read_uids) {
            list.set_ts(read_ts);
            return Ok(list);
        }
        let list = self.read_from_disk(store, key, read_ts, read_uids)?;
        self.save_in_cache(key, &list);
        let min_ts = list_min_ts(&list);
        if min_ts == 0 || read_ts >= min_ts {
            list.set_ts(read_ts);
            return Ok(list);
        }
        // Go re-reads at `read_ts` for pre-rollup versions; our chains
        // compact eagerly, so anything older is simply too old.
        Err(PostingError::TsTooOld)
    }

    /// Serves a cached copy when it covers `read_ts`.
    fn read_from_cache(&self, key: &[u8], read_ts: u64, read_uids: bool) -> Option<List> {
        let cache = self.cache.as_ref()?;
        let (cached, needs_warm) = {
            let mut guard = cache.lock();
            let entry = guard.get(key)?;
            let list = entry.list.clone();
            drop(guard);
            let (min_ts, max_ts) = list_ts(&list);
            if min_ts > read_ts || max_ts < read_ts {
                return None;
            }
            let needs_warm = read_uids && list.needs_uid_warm();
            (list, needs_warm)
        };
        let copy = cached.snapshot();
        copy.set_ts(read_ts);
        if needs_warm {
            Self::warm_cached_uids(self, key, &cached, &copy);
        }
        if copy.inner.read().mutation_map.len() > ROLLUP_DELTA_THRESHOLD {
            self.flag_rollup(key);
        }
        Some(copy)
    }

    /// Builds a list from its version chain.
    fn read_from_disk(
        &self,
        store: &Arc<dyn Backend>,
        key: &[u8],
        read_ts: u64,
        read_uids: bool,
    ) -> Result<List> {
        let pk = datastreamdb_x::parse(key)
            .map_err(|err| PostingError::Corrupt(format!("while reading posting list: {err}")))?;
        if pk.has_start_uid {
            // Split parts resolve through their base key only.
            return Err(PostingError::InvalidKey);
        }

        let chain = read_chain(store.as_ref(), key)?.unwrap_or(StoredChain {
            version: crate::store::CHAIN_VERSION,
            base: None,
            deltas: Vec::new(),
        });

        let mut layer = MutableLayer::new();
        let mut min_ts = 0u64;
        if let Some(base) = &chain.base {
            if base.commit_ts <= read_ts {
                min_ts = base.commit_ts;
            } else if base.commit_ts > 0 {
                // Base is newer than the read: without retained older
                // versions (Go discards them on rollup) the read is stale.
                return Err(PostingError::TsTooOld);
            }
        }
        let plist = if min_ts > 0 {
            chain
                .base
                .as_ref()
                .and_then(|base| base.plist.clone())
                .unwrap_or_default()
        } else {
            PostingList::default()
        };

        let mut delta_count = 0usize;
        for delta in &chain.deltas {
            if delta.commit_ts > read_ts {
                continue;
            }
            if let Some(pl) = delta.plist.clone() {
                layer.insert_committed_postings(pl);
                delta_count += 1;
            }
        }

        let list = List::from_parts(
            key.to_vec(),
            plist,
            layer,
            min_ts,
            chain.max_ts(),
            store.clone(),
        );
        if delta_count >= ROLLUP_DELTA_THRESHOLD {
            self.flag_rollup(key);
        }
        if read_uids {
            if let Err(err) = list.calculate_uids() {
                tracing::warn!("uid warm failed: {err}");
            }
        }
        Ok(list)
    }

    /// Caches a snapshot of a freshly read list.
    fn save_in_cache(&self, key: &[u8], list: &List) {
        let Some(cache) = &self.cache else {
            return;
        };
        let copy = list.snapshot();
        let last_update = list_max_ts(&copy);
        cache.lock().put(
            key.to_vec(),
            CacheEntry {
                list: copy,
                last_update,
            },
        );
    }

    /// Materializes the warmed UID slice through a private copy.
    fn warm_cached_uids(layer: &Self, key: &[u8], cached: &List, copy: &List) {
        if !cached.try_start_uid_warm() {
            return;
        }
        match copy.calculate_uids() {
            Ok(true) => {
                let (ts, uids) = copy.warmed_uids();
                if cached.publish_calculated_uids(ts, uids) {
                    layer.reset_if_current(key, cached);
                } else {
                    cached.finish_uid_warm();
                }
            }
            Ok(false) => cached.finish_uid_warm(),
            Err(err) => {
                cached.abandon_uid_warm();
                tracing::warn!("giving up warming uids: {err}");
            }
        }
    }

    /// Refreshes a cache entry only while it is still current.
    fn reset_if_current(&self, key: &[u8], cached: &List) {
        let Some(cache) = &self.cache else {
            return;
        };
        let mut guard = cache.lock();
        let current = guard
            .get(key)
            .is_some_and(|entry| entry.list.same_inner(cached));
        if current {
            let last_update = list_max_ts(cached);
            guard.put(
                key.to_vec(),
                CacheEntry {
                    list: cached.snapshot(),
                    last_update,
                },
            );
        }
    }

    /// Applies a commit to the cached list, or drops the entry.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] when the delta does not decode.
    pub fn update_item_in_cache(
        &self,
        key: &[u8],
        delta: &[u8],
        start_ts: u64,
        commit_ts: u64,
    ) -> Result<()> {
        if commit_ts == 0 {
            return Ok(());
        }
        if self.remove_on_update {
            self.del(key);
            return Ok(());
        }
        let Some(cache) = &self.cache else {
            return Ok(());
        };
        let entry = cache.lock().get(key).cloned();
        let Some(mut entry) = entry else {
            return Ok(());
        };
        entry.last_update = commit_ts;
        let pl = PostingList::decode(delta)?;
        if pl.pack.is_none() {
            entry
                .list
                .set_mutation_after_commit(start_ts, commit_ts, pl);
            if entry.list.inner.read().mutation_map.len() > ROLLUP_DELTA_THRESHOLD {
                self.flag_rollup(key);
            }
            cache.lock().put(key.to_vec(), entry);
        } else {
            self.del(key);
        }
        Ok(())
    }
}

/// Min timestamp of a list (test/cache helper).
fn list_min_ts(list: &List) -> u64 {
    list.inner.read().min_ts
}

/// Max timestamp of a list (test/cache helper).
fn list_max_ts(list: &List) -> u64 {
    list.inner.read().max_ts
}

/// Min/max timestamps of a list.
fn list_ts(list: &List) -> (u64, u64) {
    let inner = list.inner.read();
    (inner.min_ts, inner.max_ts)
}

/// Reads one split part at `min_ts`.
///
/// # Errors
///
/// Returns [`PostingError`] on key, storage, or decode failures.
pub fn read_list_part_at(
    store: &dyn Backend,
    base_key: &[u8],
    start_uid: u64,
    min_ts: u64,
) -> Result<PostingList> {
    let key = datastreamdb_x::split_key(base_key, start_uid)?;
    let chain = read_chain(store, &key)?
        .ok_or_else(|| PostingError::Corrupt("missing list part".to_owned()))?;
    let Some(base) = chain.base else {
        return Err(PostingError::Corrupt("list part without base".to_owned()));
    };
    if base.commit_ts > min_ts {
        return Err(PostingError::Corrupt(
            "list part newer than parent".to_owned(),
        ));
    }
    Ok(base.plist.unwrap_or_default())
}

/// Disk-iteration arguments (port of `IterateDiskArgs`).
pub struct IterateDiskArgs<'a> {
    /// Key prefix to scan.
    pub prefix: Vec<u8>,
    /// Read timestamp.
    pub read_ts: u64,
    /// Iterate in reverse key order.
    pub reverse: bool,
    /// First key to visit (inclusive bound).
    pub start_key: Vec<u8>,
    /// UID pre-filter (`Ok(false)` skips the key).
    pub check_inclusion: Option<&'a dyn Fn(u64) -> Result<bool>>,
    /// Visitor; [`PostingError::StopIteration`] ends the scan.
    pub function: &'a mut dyn FnMut(&List, &ParsedKey) -> Result<()>,
}

impl MemoryLayer {
    /// Visits every non-empty list under `prefix` (fresh lists, no caching).
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage failure or visitor error.
    pub fn iterate_disk(
        &self,
        store: &Arc<dyn Backend>,
        args: &mut IterateDiskArgs<'_>,
    ) -> Result<()> {
        let mut pairs = store.scan_prefix(PARTITION, &args.prefix)?;
        if args.reverse {
            pairs.reverse();
        }
        for (key, _) in &pairs {
            let in_range = if args.reverse {
                args.start_key.is_empty() || key.as_slice() <= args.start_key.as_slice()
            } else {
                args.start_key.is_empty() || key.as_slice() >= args.start_key.as_slice()
            };
            if !in_range {
                continue;
            }
            let Ok(pk) = datastreamdb_x::parse(key) else {
                continue;
            };
            if pk.has_start_uid {
                continue;
            }
            if chain_is_empty(store.as_ref(), key, args.read_ts)? {
                continue;
            }
            if let Some(check) = args.check_inclusion {
                if !check(pk.uid)? {
                    continue;
                }
            }
            let list = self.read_from_disk(store, key, args.read_ts, false)?;
            if list.is_empty(args.read_ts, 0)? {
                continue;
            }
            match (args.function)(&list, &pk) {
                Ok(()) => {}
                Err(PostingError::StopIteration) => return Ok(()),
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }
}

/// Whether the newest entry visible at `read_ts` is an empty base.
fn chain_is_empty(store: &dyn Backend, key: &[u8], read_ts: u64) -> Result<bool> {
    let Some(chain) = read_chain(store, key)? else {
        return Ok(true);
    };
    if chain.deltas.iter().any(|d| d.commit_ts <= read_ts) {
        return Ok(false);
    }
    Ok(chain
        .base
        .as_ref()
        .is_none_or(|base| base.empty || base.commit_ts > read_ts))
}
