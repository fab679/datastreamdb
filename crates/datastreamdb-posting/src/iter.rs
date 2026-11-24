//! Posting-list iteration. Port of `pIterator`, `List.iterate`,
//! `List.Uids`, and the length helpers from `posting/list.go`.
//!
//! The immutable layer merges the packed UID stream with the value
//! postings, UID by UID, against the sorted mutable postings. Delta from
//! Go: each split part's pack decodes eagerly when the iterator reaches the
//! part (Go decodes block by block); total work matches on full scans.

#![warn(missing_docs)]

use datastreamdb_codec::pack;
use datastreamdb_proto::{Posting, PostingType};
use datastreamdb_store::Backend;

use crate::errors::{PostingError, Result};
use crate::layer::MutableLayer;
use crate::list::{List, ListInner, OP_DEL};

/// Options for [`uids`](List::uids).
#[derive(Clone, Debug, Default)]
pub struct ListOptions {
    /// Read timestamp.
    pub read_ts: u64,
    /// Only UIDs after this value.
    pub after_uid: u64,
    /// Intersect results with these UIDs.
    pub intersect: Option<Vec<u64>>,
    /// Stop after collecting more than this many (0 = unbounded).
    pub first: i64,
}

/// One mutable posting selected for iteration.
#[derive(Clone, Copy, Debug)]
struct Picked {
    /// Posting UID.
    uid: u64,
    /// Effective timestamp (commit, or start when ours).
    eff: u64,
    /// Source timestamp key (`read_ts` for current entries).
    src_ts: u64,
    /// Index into the source posting array.
    idx: usize,
    /// Whether the source is the current entries.
    current: bool,
}

/// Collects the mutable postings visible at `read_ts`, sorted by UID
/// ascending then effective timestamp descending. Returns the delete-all
/// marker plus the picks.
fn pick_postings(layer: &MutableLayer, read_ts: u64) -> (u64, Vec<Picked>) {
    let effective = |start: u64, commit: u64| {
        if commit > 0 && commit <= read_ts {
            return commit;
        }
        if start == read_ts {
            return start;
        }
        0
    };

    let mut posts: Vec<Picked> = Vec::new();
    let delete_all_marker = layer.iterate(
        &mut |ts, plist| {
            let current = layer.is_current(plist);
            for (idx, mpost) in plist.postings.iter().enumerate() {
                posts.push(Picked {
                    uid: mpost.uid,
                    eff: effective(mpost.start_ts, mpost.commit_ts),
                    src_ts: ts,
                    idx,
                    current,
                });
            }
        },
        read_ts,
    );
    posts.sort_by(|a, b| a.uid.cmp(&b.uid).then_with(|| b.eff.cmp(&a.eff)));
    (delete_all_marker, posts)
}

/// Resolves a pick to its posting.
fn resolve_pick<'a>(layer: &'a MutableLayer, pick: &Picked) -> Option<&'a Posting> {
    if pick.current {
        layer.current_entries()?.postings.get(pick.idx)
    } else {
        layer
            .committed_entries()
            .get(&pick.src_ts)?
            .postings
            .get(pick.idx)
    }
}

/// Immutable-layer iterator over one list.
struct PIterator<'a> {
    /// Decoded UIDs of the current part past `after_uid`.
    uids: Vec<u64>,
    /// Offset into `uids`.
    uidx: usize,
    /// Postings of the current part (borrowed base or owned split part).
    postings: std::borrow::Cow<'a, [Posting]>,
    /// Offset into `postings`.
    pidx: usize,
    /// Current split index.
    split_idx: usize,
    /// Splits of the base list.
    splits: &'a [u64],
    /// Timestamp of a delete marker; skips the immutable layer when > 0.
    delete_below_ts: u64,
    /// Scratch posting for bare UIDs.
    uid_posting: Posting,
}

impl<'a> PIterator<'a> {
    /// Seeks the iterator past `after_uid`.
    fn seek(
        inner: &'a ListInner,
        store: &dyn Backend,
        after_uid: u64,
        delete_below_ts: u64,
    ) -> Result<Self> {
        if delete_below_ts > 0 && delete_below_ts < inner.min_ts {
            return Err(PostingError::Corrupt(format!(
                "deleteBelowTs ({delete_below_ts}) must be greater than the minTs in the list ({})",
                inner.min_ts
            )));
        }
        let mut it = Self {
            uids: Vec::new(),
            uidx: 0,
            postings: std::borrow::Cow::Borrowed(&[]),
            pidx: 0,
            split_idx: 0,
            splits: &inner.plist.splits,
            delete_below_ts,
            uid_posting: Posting::default(),
        };
        it.split_idx = it.select_initial_split(after_uid);
        it.load_part(inner, store, after_uid)?;
        if delete_below_ts > 0 {
            it.uids.clear();
            return Ok(it);
        }
        Ok(it)
    }

    /// Loads (and decodes) the current split part.
    fn load_part(
        &mut self,
        inner: &'a ListInner,
        store: &dyn Backend,
        after_uid: u64,
    ) -> Result<()> {
        if inner.plist.splits.is_empty() {
            self.postings = std::borrow::Cow::Borrowed(&inner.plist.postings);
            self.uids = inner
                .plist
                .pack
                .as_ref()
                .map(|p| pack::decode(p, after_uid))
                .unwrap_or_default();
        } else {
            let part = crate::mem::read_list_part_at(
                store,
                &inner.key,
                inner.plist.splits[self.split_idx],
                inner.min_ts,
            )?;
            self.uids = part
                .pack
                .as_ref()
                .map(|p| pack::decode(p, after_uid))
                .unwrap_or_default();
            self.postings = std::borrow::Cow::Owned(part.postings);
        }
        self.uids.retain(|uid| *uid > after_uid);
        self.uidx = 0;
        self.pidx = self.postings.partition_point(|p| p.uid <= after_uid);
        self.uid_posting = Posting::default();
        Ok(())
    }

    /// Selects the split containing `after_uid`.
    fn select_initial_split(&self, after_uid: u64) -> usize {
        if after_uid == 0 {
            return 0;
        }
        for (i, start_uid) in self.splits.iter().enumerate() {
            if *start_uid == after_uid {
                return i;
            }
            if *start_uid > after_uid {
                return i.saturating_sub(1);
            }
        }
        self.splits.len().saturating_sub(1)
    }

    /// Advances to the next split part with data.
    fn move_to_next_valid_part(
        &mut self,
        inner: &'a ListInner,
        store: &dyn Backend,
        after_uid: u64,
    ) -> Result<()> {
        if self.splits.is_empty() {
            return Ok(());
        }
        while self.uids.is_empty() && self.split_idx + 1 < self.splits.len() {
            self.split_idx += 1;
            self.load_part(inner, store, after_uid)?;
        }
        Ok(())
    }

    /// Advances past the current UID.
    fn next(&mut self, inner: &'a ListInner, store: &dyn Backend, after_uid: u64) -> Result<()> {
        if self.delete_below_ts > 0 {
            self.uids.clear();
            return Ok(());
        }
        self.uidx += 1;
        if self.uidx < self.uids.len() {
            return Ok(());
        }
        self.uidx = self.uids.len();
        self.uids.clear();
        self.move_to_next_valid_part(inner, store, after_uid)
    }

    /// Whether the iterator holds a UID (advancing over empty parts).
    fn valid(&mut self, inner: &'a ListInner, store: &dyn Backend, after_uid: u64) -> Result<bool> {
        if self.delete_below_ts > 0 {
            self.uids.clear();
            return Ok(false);
        }
        if self.uidx < self.uids.len() {
            return Ok(true);
        }
        self.move_to_next_valid_part(inner, store, after_uid)?;
        Ok(self.uidx < self.uids.len())
    }

    /// Posting for the current UID (merged with value postings).
    fn posting(&mut self) -> &Posting {
        let uid = self.uids[self.uidx];
        while self.pidx < self.postings.len() {
            let p = &self.postings[self.pidx];
            if p.uid > uid {
                break;
            }
            if p.uid == uid {
                return p;
            }
            self.pidx += 1;
        }
        self.uid_posting.uid = uid;
        self.uid_posting.val_type = datastreamdb_proto::ValType::Uid as i32;
        &self.uid_posting
    }
}

/// Iterates the merged mutable + immutable layers (lock held by caller).
pub(crate) fn iterate(
    inner: &ListInner,
    store: &dyn Backend,
    read_ts: u64,
    after_uid: u64,
    f: &mut dyn FnMut(&Posting) -> Result<()>,
) -> Result<()> {
    let (delete_below_ts, mposts) = pick_postings(&inner.mutation_map, read_ts);
    if read_ts < inner.min_ts {
        return Err(PostingError::TsTooOld);
    }

    let mut midx = 0usize;
    if after_uid > 0 {
        midx = mposts.partition_point(|mp| mp.uid <= after_uid);
    }

    let mut pitr = PIterator::seek(inner, store, after_uid, delete_below_ts)?;
    let mut prev_uid = 0u64;

    loop {
        let mp = mposts.get(midx);
        let valid = pitr.valid(inner, store, after_uid)?;
        let pp_uid = if valid { pitr.uids[pitr.uidx] } else { 0 };
        let mp_uid = mp.map_or(0, |m| m.uid);

        if mp_uid > 0 && mp_uid == prev_uid {
            midx += 1;
            continue;
        }
        if pp_uid == 0 && mp_uid == 0 {
            return Ok(());
        }
        if mp_uid == 0 || (pp_uid > 0 && pp_uid < mp_uid) {
            let pp = pitr.posting().clone();
            match f(&pp) {
                Ok(()) => {}
                Err(PostingError::StopIteration) => return Ok(()),
                Err(err) => return Err(err),
            }
            pitr.next(inner, store, after_uid)?;
            continue;
        }
        if pp_uid == 0 || (mp_uid > 0 && mp_uid < pp_uid) {
            let pick = mp.expect("mutable pick");
            if let Some(posting) = resolve_pick(&inner.mutation_map, pick) {
                if posting.op != OP_DEL {
                    let owned = posting.clone();
                    match f(&owned) {
                        Ok(()) => {}
                        Err(PostingError::StopIteration) => return Ok(()),
                        Err(err) => return Err(err),
                    }
                }
            }
            prev_uid = mp_uid;
            midx += 1;
            continue;
        }
        if pp_uid == mp_uid {
            let pick = mp.expect("mutable pick");
            if let Some(posting) = resolve_pick(&inner.mutation_map, pick) {
                if posting.op != OP_DEL {
                    let owned = posting.clone();
                    match f(&owned) {
                        Ok(()) => {}
                        Err(PostingError::StopIteration) => return Ok(()),
                        Err(err) => return Err(err),
                    }
                }
            }
            prev_uid = mp_uid;
            pitr.next(inner, store, after_uid)?;
            midx += 1;
            continue;
        }
        unreachable!("unhandled case during iteration of posting list");
    }
}

impl List {
    /// Iterates postings after `after_uid` at `read_ts`.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage failure or when the callback fails
    /// with anything but [`PostingError::StopIteration`].
    pub fn iterate(
        &self,
        read_ts: u64,
        after_uid: u64,
        f: &mut dyn FnMut(&Posting) -> Result<()>,
    ) -> Result<()> {
        // Lock discipline: the callback must not touch this list (same rule
        // as Go, which holds its read lock across the walk).
        let inner = self.inner.read();
        iterate(&inner, self.store.as_ref(), read_ts, after_uid, f)
    }

    /// Whether the list holds no UIDs at `read_ts` past `after_uid`.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage failure.
    pub fn is_empty(&self, read_ts: u64, after_uid: u64) -> Result<bool> {
        let mut count = 0;
        self.iterate(read_ts, after_uid, &mut |_| {
            count += 1;
            Err(PostingError::StopIteration)
        })?;
        Ok(count == 0)
    }

    /// Counts postings at `read_ts` past `after_uid` (-1 on error, like Go).
    #[must_use]
    pub fn length(&self, read_ts: u64, after_uid: u64) -> i64 {
        let mut count = 0i64;
        let result = self.iterate(read_ts, after_uid, &mut |_| {
            count += 1;
            Ok(())
        });
        if result.is_err() {
            return -1;
        }
        count
    }

    /// Length via caches plus the immutable layer.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage failure. Delta from Go: errors
    /// instead of panicking.
    pub fn get_length(&self, read_ts: u64) -> Result<i64> {
        let inner = self.inner.read();
        get_length_inner(&inner, self.store.as_ref(), read_ts)
    }

    /// Posting plus length without sorting (fast path when idle).
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage failure.
    pub fn get_posting_and_length_no_sort(
        &self,
        read_ts: u64,
        after_uid: u64,
        uid: u64,
        schema: &datastreamdb_schema::SchemaState,
    ) -> Result<(i64, bool, Option<Posting>)> {
        let inner = self.inner.read();
        get_posting_and_length_no_sort_inner(
            &inner,
            self.store.as_ref(),
            schema,
            read_ts,
            after_uid,
            uid,
        )
    }

    /// Posting plus length via full iteration.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage failure.
    pub fn get_posting_and_length(
        &self,
        read_ts: u64,
        after_uid: u64,
        uid: u64,
    ) -> Result<(i64, bool, Option<Posting>)> {
        let mut count = 0i64;
        let mut found = false;
        let mut post = None;
        self.iterate(read_ts, after_uid, &mut |p| {
            if p.uid == uid {
                post = Some(p.clone());
                found = true;
            }
            count += 1;
            Ok(())
        })?;
        Ok((count, found, post))
    }

    /// UIDs selected by `opt`.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage failure or stale reads.
    pub fn uids(&self, opt: &ListOptions) -> Result<Vec<u64>> {
        let mut opt = opt.clone();
        let requested_first = opt.first;
        let bounded = requested_first > 0 && requested_first < i64::from(i32::MAX);
        if opt.first == 0 {
            opt.first = i64::from(i32::MAX);
        }

        if opt.intersect.is_none() && !bounded && self.can_use_calculated_uids(opt.read_ts) {
            let inner = self.inner.read();
            let (uids, _) = inner.mutation_map.calculated_uids();
            let after_idx = if opt.after_uid != 0 {
                uids.partition_point(|uid| *uid <= opt.after_uid)
            } else {
                0
            };
            if after_idx >= uids.len() {
                return Ok(Vec::new());
            }
            return Ok(uids[after_idx..].to_vec());
        }

        // Compressed fast path: no deltas, intersect given, single part.
        {
            let inner = self.inner.read();
            if inner.mutation_map.is_empty()
                && opt.intersect.is_some()
                && inner.plist.splits.is_empty()
            {
                if opt.read_ts < inner.min_ts {
                    return Err(PostingError::TsTooOld);
                }
                let decoded = inner
                    .plist
                    .pack
                    .as_ref()
                    .map(|p| pack::decode(p, opt.after_uid))
                    .unwrap_or_default();
                let intersect = opt.intersect.as_deref().unwrap_or(&[]);
                return Ok(datastreamdb_algo::intersect_slices(&decoded, intersect));
            }
        }

        // Small-intersect path: probe each UID.
        let approx = self.approx_len();
        if let Some(intersect) = &opt.intersect {
            if intersect.len() < approx {
                let mut res = Vec::with_capacity(intersect.len());
                for uid in intersect {
                    let (ok, _) = self.find_posting(opt.read_ts, *uid)?;
                    if ok {
                        res.push(*uid);
                    }
                }
                return Ok(res);
            }
        }

        let (uid_min, uid_max) = opt.intersect.as_ref().map_or((0, 0), |list| {
            (
                list.first().copied().unwrap_or(0),
                list.last().copied().unwrap_or(0),
            )
        });
        let mut cap = approx;
        let bounded_cap = usize::try_from(requested_first + 1).unwrap_or(usize::MAX);
        if bounded && bounded_cap < cap {
            cap = bounded_cap;
        }

        let mut res = Vec::with_capacity(cap);
        let first = opt.first;
        self.iterate(opt.read_ts, opt.after_uid, &mut |p| {
            if p.posting_type == PostingType::Ref as i32 {
                if p.uid < uid_min {
                    return Ok(());
                }
                if p.uid > uid_max && uid_max > 0 {
                    return Err(PostingError::StopIteration);
                }
                res.push(p.uid);
                if first > 0 && res.len() > usize::try_from(first).unwrap_or(usize::MAX) {
                    return Err(PostingError::StopIteration);
                }
            }
            Ok(())
        })?;

        let mut out = res;
        if let Some(intersect) = &opt.intersect {
            out = datastreamdb_algo::intersect_slices(&out, intersect);
        }
        // Positive `first` trims here (paired with the early stop above);
        // negative counts are applied by the query layer over filtered rows.
        if opt.first > 0 && out.len() > usize::try_from(opt.first).unwrap_or(usize::MAX) {
            out.truncate(usize::try_from(opt.first).unwrap_or(usize::MAX));
        }
        Ok(out)
    }

    /// Calls `post_fn` with the `REF` postings selected by `opt`.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage or callback failure.
    pub fn postings(
        &self,
        opt: &ListOptions,
        post_fn: &mut dyn FnMut(&Posting) -> Result<()>,
    ) -> Result<()> {
        self.iterate(opt.read_ts, opt.after_uid, &mut |p| {
            if p.posting_type != PostingType::Ref as i32 {
                return Ok(());
            }
            post_fn(p)
        })
    }

    /// Approximate UID count (deltas plus packed estimate).
    #[must_use]
    pub fn approx_len(&self) -> usize {
        let inner = self.inner.read();
        inner.mutation_map.len() + inner.plist.pack.as_ref().map_or(0, pack::approx_len)
    }
}

/// Length via caches plus the immutable layer (lock held).
pub(crate) fn get_length_inner(
    inner: &ListInner,
    store: &dyn Backend,
    read_ts: u64,
) -> Result<i64> {
    let mut length = inner.mutation_map.list_len(read_ts);
    if inner.mutation_map.populate_delete_all(read_ts) == 0 {
        length += immutable_layer_len(inner, store)?;
    }
    Ok(length)
}

/// Posting plus length without sorting (lock held).
pub(crate) fn get_posting_and_length_no_sort_inner(
    inner: &ListInner,
    store: &dyn Backend,
    schema: &datastreamdb_schema::SchemaState,
    read_ts: u64,
    after_uid: u64,
    uid: u64,
) -> Result<(i64, bool, Option<Posting>)> {
    if schema.indexing_in_progress() {
        let mut count = 0i64;
        let mut found = false;
        let mut post = None;
        iterate(inner, store, read_ts, after_uid, &mut |p| {
            if p.uid == uid {
                post = Some(p.clone());
                found = true;
            }
            count += 1;
            Ok(())
        })?;
        return Ok((count, found, post));
    }
    let length = get_length_inner(inner, store, read_ts)?;
    let (found, posting) = crate::read::find_posting(inner, store, read_ts, uid)?;
    Ok((length, found, posting))
}

/// Immutable-layer UID count across splits.
///
/// # Errors
///
/// Returns [`PostingError`] on storage failure.
fn immutable_layer_len(inner: &ListInner, store: &dyn Backend) -> Result<i64> {
    if !inner.plist.splits.is_empty() {
        let mut length = 0i64;
        for split in &inner.plist.splits {
            let part = crate::mem::read_list_part_at(store, &inner.key, *split, inner.min_ts)?;
            length +=
                i64::try_from(part.pack.as_ref().map_or(0, pack::exact_len)).unwrap_or(i64::MAX);
        }
        return Ok(length);
    }
    Ok(i64::try_from(inner.plist.pack.as_ref().map_or(0, pack::exact_len)).unwrap_or(i64::MAX))
}
