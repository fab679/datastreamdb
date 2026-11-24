//! Rollup: merging the mutable layer into the immutable base. Port of
//! `List.rollup`/`Rollup` plus the split machinery from `posting/list.go`.
//!
//! [`List::rollup`] computes the merged list; the engine writes the result
//! through [`crate::store`]. Split parts live under
//! [`split_key`](datastreamdb_x::split_key) keys with the same chain encoding.

#![warn(missing_docs)]

use std::collections::HashMap;

use datastreamdb_codec::pack;
use datastreamdb_proto::{BackupPostingList, PostingList, PostingType};
use prost::Message as _;

use crate::errors::{PostingError, Result};
use crate::iter::iterate;
use crate::list::{List, MAX_LIST_SIZE};

/// UID-block size used by rollup encoding.
pub const ROLLUP_BLOCK_SIZE: usize = 256;
/// Same size as encoded on the wire.
const ROLLUP_BLOCK_SIZE_U32: u32 = 256;

/// Merged rollup output: base list, split parts, and the new base timestamp.
#[derive(Clone, Debug, Default)]
pub struct RollupOutput {
    /// Merged base list.
    pub plist: PostingList,
    /// Split parts by start UID.
    pub parts: HashMap<u64, PostingList>,
    /// New base timestamp (max merged commit timestamp).
    pub new_min_ts: u64,
}

impl RollupOutput {
    /// Sorted start UIDs from the parts map (source of truth).
    #[must_use]
    pub fn splits(&self) -> Vec<u64> {
        let mut splits: Vec<u64> = self.parts.keys().copied().collect();
        splits.sort_unstable();
        splits
    }

    /// Refreshes the base split list from the parts map.
    fn update_splits(&mut self) {
        if !self.parts.is_empty() {
            self.plist = PostingList::default();
        }
        self.plist.splits = self.splits();
    }

    /// Splits oversized lists/parts until every part fits.
    fn recursive_split(&mut self) {
        self.split_up_list();
        loop {
            if !self.parts.values().any(should_split) {
                return;
            }
            self.split_up_list();
        }
    }

    /// Splits every oversized list once.
    fn split_up_list(&mut self) {
        let mut lists: Vec<PostingList> = Vec::new();
        if self.parts.is_empty() {
            lists.push(self.plist.clone());
        }
        for start_uid in self.splits() {
            if let Some(part) = self.parts.get(&start_uid) {
                lists.push(part.clone());
            }
        }

        for (i, list) in lists.iter().enumerate() {
            let mut start_uid = 1u64;
            if !self.parts.is_empty() {
                start_uid = self.plist.splits[i];
            }
            if should_split(list) {
                let (uids, parts) = bin_split(start_uid, list);
                for (i, uid) in uids.iter().enumerate() {
                    self.parts.insert(*uid, parts[i].clone());
                }
            }
        }
        self.update_splits();
    }

    /// Drops empty splits (except UID 1, which anchors multi-part lists).
    fn remove_empty_splits(&mut self) {
        let empties: Vec<u64> = self
            .parts
            .iter()
            .filter(|(uid, plist)| **uid != 1 && is_plist_empty(plist))
            .map(|(uid, _)| *uid)
            .collect();
        for uid in empties {
            self.parts.remove(&uid);
        }
        self.update_splits();

        if self.parts.len() == 1 {
            if let Some(only) = self.parts.get(&1) {
                if is_plist_empty(only) {
                    self.parts.remove(&1);
                    self.plist.splits = Vec::new();
                }
            }
        }
    }
}

/// Whether the list must split in two.
fn should_split(plist: &PostingList) -> bool {
    plist.encoded_len() >= MAX_LIST_SIZE && plist.pack.as_ref().is_some_and(|p| p.blocks.len() > 1)
}

/// Splits one list into low/high halves at the middle block.
fn bin_split(low_uid: u64, plist: &PostingList) -> (Vec<u64>, Vec<PostingList>) {
    let pack = plist.pack.as_ref().expect("split pack");
    let mid_block = pack.blocks.len() / 2;
    let mid_uid = pack.blocks[mid_block].base;

    let mut low = PostingList {
        pack: Some(datastreamdb_proto::UidPack {
            block_size: pack.block_size,
            blocks: pack.blocks[..mid_block].to_vec(),
            alloc_ref: pack.alloc_ref,
        }),
        ..PostingList::default()
    };
    let mut high = PostingList {
        pack: Some(datastreamdb_proto::UidPack {
            block_size: pack.block_size,
            blocks: pack.blocks[mid_block..].to_vec(),
            alloc_ref: pack.alloc_ref,
        }),
        ..PostingList::default()
    };

    let pidx = plist
        .postings
        .partition_point(|posting| posting.uid < mid_uid);
    low.postings = plist.postings[..pidx].to_vec();
    high.postings = plist.postings[pidx..].to_vec();
    (vec![low_uid, mid_uid], vec![low, high])
}

/// Whether the list holds no data (split parents count as non-empty).
fn is_plist_empty(plist: &PostingList) -> bool {
    if !plist.splits.is_empty() {
        return false;
    }
    plist.pack.as_ref().is_none_or(|p| p.blocks.is_empty())
}

impl List {
    /// Merges layers at `read_ts` into a rollup output (`split` enables
    /// multi-part splitting). Returns `None` when the base already covers
    /// `read_ts`.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage failure.
    pub fn rollup(&self, read_ts: u64, split: bool) -> Result<Option<RollupOutput>> {
        let inner = self.inner.read();
        if inner.min_ts > read_ts {
            return Ok(None);
        }

        let mut out = RollupOutput {
            plist: PostingList {
                splits: inner.plist.splits.clone(),
                ..PostingList::default()
            },
            parts: HashMap::new(),
            new_min_ts: 0,
        };

        if !out.plist.splits.is_empty() || !inner.mutation_map.is_empty() {
            encode(&inner, self.store.as_ref(), &mut out, read_ts, split)?;
        } else {
            out.plist = inner.plist.clone();
        }

        let mut max_commit_ts = inner.min_ts;
        let (delete_below_ts, mposts) = {
            // Pick committed postings without the index wrapper: reuse the
            // layer iterator directly for the timestamp scan.
            let mut posts = Vec::new();
            let marker = inner.mutation_map.iterate(
                &mut |_, plist| {
                    posts.extend(plist.postings.iter().cloned());
                },
                read_ts,
            );
            (marker, posts)
        };
        max_commit_ts = max_commit_ts.max(delete_below_ts);
        for mp in &mposts {
            max_commit_ts = max_commit_ts.max(mp.commit_ts);
        }

        out.new_min_ts = max_commit_ts;
        if split {
            out.recursive_split();
            out.remove_empty_splits();
        } else {
            out.plist.splits.clear();
        }
        Ok(Some(out))
    }

    /// Builds the backup-format list (single part, no splits).
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage failure.
    ///
    /// # Panics
    ///
    /// Never in practice (a max-timestamp rollup always yields output).
    pub fn to_backup_posting_list(&self) -> Result<BackupPostingList> {
        let out = self
            .rollup(u64::MAX, false)?
            .expect("max-ts rollup always produces output");
        let ol = out.plist;
        let uid_bytes = ol
            .pack
            .as_ref()
            .map(pack::decode_to_uvarints)
            .unwrap_or_default();
        Ok(BackupPostingList {
            uids: Vec::new(),
            postings: ol.postings,
            commit_ts: ol.commit_ts,
            splits: ol.splits,
            uid_bytes,
        })
    }
}

/// Encodes the merged layers into `out`, honoring split boundaries.
fn encode(
    inner: &crate::list::ListInner,
    store: &dyn datastreamdb_store::Backend,
    out: &mut RollupOutput,
    read_ts: u64,
    split: bool,
) -> Result<()> {
    let mut state = if inner.plist.splits.is_empty() || !split {
        SplitState {
            plist: PostingList::default(),
            start_uid: 0,
            end_uid: u64::MAX,
            split_idx: 0,
            enc: pack::Encoder::new(ROLLUP_BLOCK_SIZE),
        }
    } else {
        SplitState {
            plist: PostingList::default(),
            start_uid: inner.plist.splits[0],
            end_uid: if inner.plist.splits.len() > 1 {
                inner.plist.splits[1] - 1
            } else {
                u64::MAX
            },
            split_idx: 0,
            enc: pack::Encoder::new(ROLLUP_BLOCK_SIZE),
        }
    };
    let single_part = inner.plist.splits.is_empty() || !split;

    iterate(inner, store, read_ts, 0, &mut |p| {
        if p.uid > state.end_uid && split {
            finish_part(&mut state, out)?;
            state.split_idx += 1;
            let splits = &inner.plist.splits;
            state.start_uid = splits[state.split_idx];
            state.end_uid = if state.split_idx + 1 == splits.len() {
                u64::MAX
            } else {
                splits[state.split_idx + 1] - 1
            };
            state.plist = PostingList::default();
            state.enc = pack::Encoder::new(ROLLUP_BLOCK_SIZE);
        }
        state.enc.add(p.uid);
        if !p.facets.is_empty() || p.posting_type != PostingType::Ref as i32 {
            state.plist.postings.push(p.clone());
        }
        Ok(())
    })?;

    if single_part {
        let pack = state.enc.finish();
        if !pack.blocks.is_empty() {
            if pack.block_size != ROLLUP_BLOCK_SIZE_U32 {
                return Err(PostingError::Corrupt(
                    "rollup block size mismatch".to_owned(),
                ));
            }
            state.plist.pack = Some(pack);
        }
        state.plist.splits.clone_from(&out.plist.splits);
        out.plist = state.plist;
    } else {
        finish_part(&mut state, out)?;
    }
    Ok(())
}

/// Finishes the encoder into the current split part.
fn finish_part(state: &mut SplitState, out: &mut RollupOutput) -> Result<()> {
    let fresh = pack::Encoder::new(ROLLUP_BLOCK_SIZE);
    let enc = std::mem::replace(&mut state.enc, fresh);
    let pack = enc.finish();
    if !pack.blocks.is_empty() {
        if pack.block_size != ROLLUP_BLOCK_SIZE_U32 {
            return Err(PostingError::Corrupt(
                "rollup block size mismatch".to_owned(),
            ));
        }
        state.plist.pack = Some(pack);
    }
    let mut plist = PostingList::default();
    std::mem::swap(&mut state.plist, &mut plist);
    out.parts.insert(state.start_uid, plist);
    Ok(())
}

/// Split-encoding state shared by [`encode`] and [`finish_part`].
struct SplitState {
    /// Part list under construction.
    plist: PostingList,
    /// Start UID of the current part.
    start_uid: u64,
    /// Inclusive end UID of the current part.
    end_uid: u64,
    /// Index into the base split list.
    split_idx: usize,
    /// UID encoder for the current part.
    enc: pack::Encoder,
}
