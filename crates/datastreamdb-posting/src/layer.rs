//! The mutable layer of a posting list. Port of `MutableLayer`
//! (`posting/list.go`): committed deltas plus the current transaction's
//! entries, with the UID and delete-all caches the read path relies on.
//!
//! Delta from Go: the delete-all marker cache is an [`AtomicU64`] because
//! Go refreshes it under a read lock; atomics keep that race benign and
//! explicit. Cloning a layer deep-copies the committed maps where Go shares
//! them by reference (equivalent isolation: Go rebuilds the shared maps on
//! every cached commit).

#![warn(missing_docs)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use datastreamdb_proto::{Posting, PostingList};

use crate::list::has_delete_all;

/// Unset delete-all marker.
pub const NO_DELETE_ALL: u64 = u64::MAX;
/// Unset committed-UIDs timestamp.
pub const NO_COMMITTED_UIDS: u64 = u64::MAX;
/// Unset cached length.
pub const NO_LENGTH: i64 = i64::MAX;

/// Length contribution of one posting operation.
#[must_use]
pub fn length_delta(op: u32) -> i64 {
    match op {
        crate::list::OP_DEL => -1,
        crate::list::OP_SET => 1,
        _ => 0,
    }
}

/// Mutable layer: committed deltas plus the current transaction's entries.
#[derive(Debug, Default)]
pub struct MutableLayer {
    /// Deltas by commit timestamp (immutable once published).
    committed_entries: HashMap<u64, PostingList>,
    /// This transaction's entries.
    current_entries: Option<PostingList>,
    /// Read timestamp the current entries belong to.
    read_ts: u64,
    /// Latest delete-all marker (cached; refreshed under reads).
    delete_all_marker: AtomicU64,
    /// UID to posting map over the committed entries.
    committed_uids: HashMap<u64, Posting>,
    /// Latest commit timestamp in the committed entries.
    committed_uids_time: u64,
    /// Cached length over the committed entries.
    length: i64,
    /// Latest committed entry.
    last_entry: Option<PostingList>,
    /// UID to index map over the current entries.
    current_uids: Option<HashMap<u64, usize>>,
    /// Whether `calculated_uids` is populated.
    is_uids_calculated: bool,
    /// Materialized UID slice (warming optimization).
    calculated_uids: Vec<u64>,
}

impl Clone for MutableLayer {
    fn clone(&self) -> Self {
        Self {
            committed_entries: self.committed_entries.clone(),
            current_entries: self.current_entries.clone(),
            read_ts: self.read_ts,
            delete_all_marker: AtomicU64::new(self.delete_all_marker.load(Ordering::SeqCst)),
            committed_uids: self.committed_uids.clone(),
            committed_uids_time: self.committed_uids_time,
            length: self.length,
            last_entry: self.last_entry.clone(),
            current_uids: self.current_uids.clone(),
            is_uids_calculated: self.is_uids_calculated,
            calculated_uids: self.calculated_uids.clone(),
        }
    }
}

impl MutableLayer {
    /// Empty layer with unset caches.
    #[must_use]
    pub fn new() -> Self {
        Self {
            committed_entries: HashMap::new(),
            current_entries: None,
            read_ts: 0,
            delete_all_marker: AtomicU64::new(NO_DELETE_ALL),
            committed_uids: HashMap::new(),
            committed_uids_time: NO_COMMITTED_UIDS,
            length: NO_LENGTH,
            last_entry: None,
            current_uids: None,
            is_uids_calculated: false,
            calculated_uids: Vec::new(),
        }
    }

    /// Attaches the layer to a read timestamp.
    pub fn set_ts(&mut self, read_ts: u64) {
        self.read_ts = read_ts;
    }

    /// Current read timestamp.
    #[must_use]
    pub fn read_ts(&self) -> u64 {
        self.read_ts
    }

    /// Overwrites the current entries (delete-all, single-UID update, replay).
    ///
    /// # Panics
    ///
    /// Panics when the layer is reused for another transaction (like Go).
    pub fn set_current_entries(&mut self, ts: u64, pl: PostingList) {
        if self.read_ts != 0 {
            assert_eq!(
                self.read_ts, ts,
                "List object reused for a different transaction"
            );
        }
        self.read_ts = ts;
        let mut map = HashMap::with_capacity(pl.postings.len());
        for (i, post) in pl.postings.iter().enumerate() {
            map.insert(post.uid, i);
        }
        self.current_entries = Some(pl);
        self.current_uids = Some(map);
        self.is_uids_calculated = false;
        self.calculated_uids.clear();
        self.delete_all_marker
            .store(NO_DELETE_ALL, Ordering::SeqCst);
    }

    /// Returns the entries for `ts`: current when it is ours, else committed.
    #[must_use]
    pub fn get(&self, ts: u64) -> Option<&PostingList> {
        if self.read_ts == ts {
            self.current_entries.as_ref()
        } else {
            self.committed_entries.get(&ts)
        }
    }

    /// Rough entry count (committed deltas plus one for current entries).
    #[must_use]
    pub fn len(&self) -> usize {
        self.committed_entries.len() + usize::from(self.current_entries.is_some())
    }

    /// Whether the layer holds no entries at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Length of the layer at `read_ts`, honoring delete-all markers.
    pub fn list_len(&self, read_ts: u64) -> i64 {
        let mut count = 0i64;
        if self.length == NO_LENGTH || read_ts < self.committed_uids_time {
            self.iterate(
                &mut |_, pl| {
                    for mpost in &pl.postings {
                        if has_delete_all(mpost) {
                            continue;
                        }
                        count += length_delta(mpost.op);
                    }
                },
                read_ts,
            );
            return count;
        }

        count = self.length;
        if self.current_entries.is_some() && read_ts == self.read_ts {
            if self.populate_delete_all(read_ts) == self.read_ts {
                count = 0;
            }
            if let Some(current) = &self.current_entries {
                for mpost in &current.postings {
                    if has_delete_all(mpost) {
                        continue;
                    }
                    count += length_delta(mpost.op);
                }
            }
        }
        count
    }

    /// Delete-all marker at or below `read_ts` (refreshing the cache).
    pub fn populate_delete_all(&self, read_ts: u64) -> u64 {
        let cached = self.delete_all_marker.load(Ordering::SeqCst);
        if cached != NO_DELETE_ALL && read_ts >= cached {
            return cached;
        }
        let mut marker = 0u64;
        let mut below_ts = 0u64;
        self.iterate_committed_entries(&mut |ts, pl| {
            for mpost in &pl.postings {
                if has_delete_all(mpost) {
                    marker = marker.max(ts);
                    if ts <= read_ts {
                        below_ts = below_ts.max(ts);
                    }
                }
            }
        });
        self.delete_all_marker.store(marker, Ordering::SeqCst);
        below_ts
    }

    /// Visits committed entries plus current entries (internal use only).
    fn iterate_committed_entries(&self, f: &mut dyn FnMut(u64, &PostingList)) {
        for (ts, pl) in &self.committed_entries {
            if pl.commit_ts == *ts || *ts == self.read_ts {
                f(*ts, pl);
            }
        }
        if let Some(current) = &self.current_entries {
            f(self.read_ts, current);
        }
    }

    /// Visits the entries visible at `read_ts` (at/above the delete-all
    /// marker). Returns the marker.
    pub fn iterate(&self, f: &mut dyn FnMut(u64, &PostingList), read_ts: u64) -> u64 {
        let delete_all_marker = self.populate_delete_all(read_ts);
        self.iterate_committed_entries(&mut |ts, pl| {
            if ts >= delete_all_marker && ts <= read_ts {
                f(ts, pl);
            }
        });
        delete_all_marker
    }

    /// Inserts old committed postings while building the list. Callers feed
    /// deltas newest-first, like the Badger version walk.
    pub fn insert_committed_postings(&mut self, mut pl: PostingList) {
        if self.committed_uids_time == NO_COMMITTED_UIDS {
            self.committed_uids_time = 0;
        }
        if self.length == NO_LENGTH {
            self.length = 0;
        }
        if self.delete_all_marker.load(Ordering::SeqCst) == NO_DELETE_ALL {
            self.delete_all_marker.store(0, Ordering::SeqCst);
        }

        if pl.commit_ts > self.committed_uids_time {
            self.last_entry = Some(pl.clone());
        }
        self.committed_uids_time = self.committed_uids_time.max(pl.commit_ts);

        for mpost in &mut pl.postings {
            mpost.commit_ts = pl.commit_ts;
            if has_delete_all(mpost) {
                let marker = self.delete_all_marker.load(Ordering::SeqCst);
                if mpost.commit_ts > marker {
                    self.delete_all_marker
                        .store(mpost.commit_ts, Ordering::SeqCst);
                }
                continue;
            }
            if mpost.commit_ts >= self.delete_all_marker.load(Ordering::SeqCst) {
                self.length += length_delta(mpost.op);
            }
            // Reverse insertion order: keep the first update per UID.
            self.committed_uids
                .entry(mpost.uid)
                .or_insert_with(|| mpost.clone());
        }
        self.committed_entries.insert(pl.commit_ts, pl);
    }

    /// Inserts a posting into the current entries.
    ///
    /// # Panics
    ///
    /// Panics on timestamp mismatch with the attached transaction (like Go).
    pub fn insert_posting(&mut self, mpost: Posting, has_count_index: bool) {
        if self.read_ts != 0 {
            assert_eq!(
                mpost.start_ts, self.read_ts,
                "different read ts and start ts"
            );
        }
        self.read_ts = mpost.start_ts;

        if has_delete_all(&mpost) {
            let marker = self.delete_all_marker.load(Ordering::SeqCst);
            if mpost.commit_ts > marker {
                self.delete_all_marker
                    .store(mpost.commit_ts, Ordering::SeqCst);
            }
        }

        if mpost.uid != 0 {
            if self.current_entries.is_none() {
                self.current_entries = Some(PostingList::default());
            }
            if self.current_uids.is_none() {
                let mut map = HashMap::new();
                if let Some(current) = &self.current_entries {
                    map.reserve(current.postings.len());
                    for (i, post) in current.postings.iter().enumerate() {
                        map.insert(post.uid, i);
                    }
                }
                self.current_uids = Some(map);
            }
            let post_index = self
                .current_uids
                .as_ref()
                .expect("uid map")
                .get(&mpost.uid)
                .copied();
            let current = self.current_entries.as_mut().expect("entries");
            if let Some(post_index) = post_index {
                if has_count_index && mpost.op == crate::list::OP_DEL {
                    if current.postings[post_index].op == crate::list::OP_DEL {
                        current.postings[post_index] = mpost;
                        return;
                    }
                    current.postings.remove(post_index);
                    self.current_uids = None;
                    return;
                }
                current.postings[post_index] = mpost;
            } else {
                let uid = mpost.uid;
                current.postings.push(mpost);
                let idx = current.postings.len() - 1;
                self.current_uids
                    .as_mut()
                    .expect("uid map")
                    .insert(uid, idx);
            }
            return;
        }

        self.current_entries
            .get_or_insert_with(PostingList::default)
            .postings
            .push(mpost);
    }

    /// Looks up one UID in the mutable layer. Returns whether the immutable
    /// layer still needs searching, plus the posting when found.
    pub fn find_posting(&self, read_ts: u64, uid: u64) -> (bool, Option<Posting>) {
        let delete_all_marker = self.populate_delete_all(read_ts);

        let cached = || -> Option<(Posting, u64)> {
            if read_ts == self.read_ts {
                if let Some(uids) = &self.current_uids {
                    if let Some(&pos) = uids.get(&uid) {
                        let posting = self.current_entries.as_ref()?.postings[pos].clone();
                        return Some((posting, self.read_ts));
                    }
                }
            }
            self.committed_uids
                .get(&uid)
                .map(|posting| (posting.clone(), posting.commit_ts))
        };

        let posting = if read_ts >= self.committed_uids_time {
            cached().and_then(|(posting, ts)| {
                if ts < delete_all_marker {
                    None
                } else {
                    Some(posting)
                }
            })
        } else {
            let mut found: Option<(Posting, u64)> = None;
            self.iterate(
                &mut |ts, pl| {
                    for mpost in &pl.postings {
                        if mpost.uid != uid {
                            continue;
                        }
                        let replace = found.as_ref().is_none_or(|(_, t)| *t <= ts);
                        if replace {
                            found = Some((mpost.clone(), ts));
                        }
                    }
                },
                read_ts,
            );
            found.map(|(posting, _)| posting)
        };

        match posting {
            Some(posting) if posting.op != crate::list::OP_DEL => (false, Some(posting)),
            Some(_) => (false, None),
            None if delete_all_marker > 0 => (false, None),
            None => (true, None),
        }
    }

    /// Whether warming the UID slice would be useful.
    #[must_use]
    pub fn needs_uid_warm(&self) -> bool {
        !self.is_uids_calculated && self.current_entries.is_none()
    }

    /// Current entries (for commit/delta extraction).
    #[must_use]
    pub fn current_entries(&self) -> Option<&PostingList> {
        self.current_entries.as_ref()
    }

    /// Whether `pl` is this layer's current entries.
    #[must_use]
    pub fn is_current(&self, pl: &PostingList) -> bool {
        self.current_entries
            .as_ref()
            .is_some_and(|current| std::ptr::eq(current, pl))
    }

    /// Committed entries (for rollup/tests).
    #[must_use]
    pub fn committed_entries(&self) -> &HashMap<u64, PostingList> {
        &self.committed_entries
    }

    /// Latest commit timestamp in the committed entries.
    #[must_use]
    pub fn committed_uids_time(&self) -> u64 {
        self.committed_uids_time
    }

    /// Latest committed entry, if any.
    #[must_use]
    pub fn last_entry(&self) -> Option<&PostingList> {
        self.last_entry.as_ref()
    }

    /// Materialized UID slice plus its validity flag.
    #[must_use]
    pub fn calculated_uids(&self) -> (&[u64], bool) {
        (&self.calculated_uids, self.is_uids_calculated)
    }

    /// Installs a materialized UID slice.
    pub fn set_calculated_uids(&mut self, uids: Vec<u64>) {
        self.calculated_uids = uids;
        self.is_uids_calculated = true;
    }

    /// Drops the warmed UID slice after the list changes.
    pub fn clear_warm(&mut self) {
        self.is_uids_calculated = false;
        self.calculated_uids.clear();
    }

    /// Drops the current entries after a commit.
    pub fn clear_current(&mut self) {
        self.current_entries = None;
        self.read_ts = 0;
        self.current_uids = None;
        self.is_uids_calculated = false;
        self.calculated_uids.clear();
    }

    /// Commits `pl` at `commit_ts` into this layer (cached-commit path).
    pub fn commit_entries(&mut self, commit_ts: u64, mut pl: PostingList) {
        pl.commit_ts = commit_ts;
        for posting in &mut pl.postings {
            posting.commit_ts = commit_ts;
        }
        if self.committed_uids_time == NO_COMMITTED_UIDS {
            self.committed_uids_time = 0;
        }
        if pl.commit_ts > self.committed_uids_time {
            self.last_entry = Some(pl.clone());
        }
        self.committed_uids_time = self.committed_uids_time.max(commit_ts);
        for mpost in &pl.postings {
            if has_delete_all(mpost) {
                self.delete_all_marker.store(commit_ts, Ordering::SeqCst);
                self.length = 0;
                continue;
            }
            self.committed_uids.insert(mpost.uid, mpost.clone());
            if self.length == NO_LENGTH {
                self.length = 0;
            }
            self.length += length_delta(mpost.op);
        }
        self.committed_entries.insert(commit_ts, pl);
        self.clear_current();
    }

    /// Short debug shape (entry counts, never values).
    #[must_use]
    pub fn print_short(&self) -> String {
        format!(
            "committed: {}, proposed: {}, deleteAllMarker: {}",
            self.committed_entries.len(),
            self.current_entries.is_some(),
            self.delete_all_marker.load(Ordering::SeqCst)
        )
    }
}
