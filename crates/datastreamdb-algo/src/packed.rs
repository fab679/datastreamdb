//! Packed-list set operations over [`UidPack`]s. Port of `algo/packed.go`.
//!
//! Deltas from Go: the `IntersectSortedPacked` loop accumulates correctly (Go
//! shadows `out` inside the loop, silently ignoring lists past the second),
//! block bytes are cloned instead of aliased (same values, borrow-safe), and
//! missing UIDs report `None` instead of `-1`.

#![warn(missing_docs)]

use std::collections::BinaryHeap;

#[cfg(test)]
use datastreamdb_codec::decode;
use datastreamdb_codec::{Decoder, Encoder, SeekPos, copy_uid_pack, exact_len};
use datastreamdb_proto::UidPack;

use super::heap::MergeEntry;

/// Filters packed UIDs by `f(uid, index)`, preserving order.
#[must_use]
pub fn apply_filter_packed(pack: &UidPack, mut f: impl FnMut(u64, usize) -> bool) -> UidPack {
    let mut decoder = Decoder::new(pack);
    let mut encoder = Encoder::new(pack.block_size as usize);
    let mut index = 0;
    while decoder.valid() {
        let block = decoder.uids().to_vec();
        for uid in block {
            if f(uid, index) {
                encoder.add(uid);
            }
            index += 1;
        }
        decoder.next();
    }
    encoder.finish()
}

/// Linear intersection of two packs, decoded block by block.
#[must_use]
#[allow(clippy::similar_names, reason = "Go u/v identifiers")]
pub fn intersect_with_lin_packed(u: &UidPack, v: &UidPack) -> UidPack {
    let mut u_dec = Decoder::new(u);
    let mut uuids = u_dec.uids().to_vec();
    let mut v_dec = Decoder::new(v);
    let mut vuids = v_dec.uids().to_vec();
    let (mut u_idx, mut v_idx) = (0, 0);
    let mut result = Encoder::new(u.block_size as usize);

    loop {
        if uuids.is_empty() || vuids.is_empty() {
            break;
        }
        if u_idx == uuids.len() {
            if !u_dec.valid() {
                break;
            }
            uuids = u_dec.next().to_vec();
            u_idx = 0;
        }
        if v_idx == vuids.len() {
            if !v_dec.valid() {
                break;
            }
            vuids = v_dec.next().to_vec();
            v_idx = 0;
        }
        let (u_len, v_len) = (uuids.len(), vuids.len());
        while u_idx < u_len && v_idx < v_len {
            match uuids[u_idx].cmp(&vuids[v_idx]) {
                std::cmp::Ordering::Greater => {
                    v_idx += 1;
                    while v_idx < v_len && vuids[v_idx] < uuids[u_idx] {
                        v_idx += 1;
                    }
                }
                std::cmp::Ordering::Equal => {
                    result.add(uuids[u_idx]);
                    v_idx += 1;
                    u_idx += 1;
                }
                std::cmp::Ordering::Less => {
                    u_idx += 1;
                    while u_idx < u_len && uuids[u_idx] < vuids[v_idx] {
                        u_idx += 1;
                    }
                }
            }
        }
    }
    result.finish()
}

/// Intersects all packs, smallest first.
#[must_use]
pub fn intersect_sorted_packed(lists: &[UidPack]) -> UidPack {
    if lists.is_empty() {
        return Encoder::new(10).finish();
    }
    let mut order: Vec<usize> = (0..lists.len()).collect();
    order.sort_by_key(|&i| exact_len(&lists[i]));
    if order.len() == 1 {
        return copy_uid_pack(&lists[order[0]]);
    }
    let mut out = intersect_with_lin_packed(&lists[order[0]], &lists[order[1]]);
    for &i in &order[2..] {
        out = intersect_with_lin_packed(&out, &lists[i]);
        if exact_len(&out) == 0 {
            break;
        }
    }
    out
}

/// Packed difference `u − v`. `None` inputs follow Go's nil rules.
#[must_use]
pub fn difference_packed(u: Option<&UidPack>, v: Option<&UidPack>) -> Option<UidPack> {
    match (u, v) {
        (None, _) => None,
        (Some(u), None) => Some(copy_uid_pack(u)),
        (Some(u), Some(v)) => Some(difference_packed_both(u, v)),
    }
}

#[allow(clippy::similar_names, reason = "Go u/v identifiers")]
fn difference_packed_both(u: &UidPack, v: &UidPack) -> UidPack {
    let mut result = Encoder::new(u.block_size as usize);
    let mut u_dec = Decoder::new(u);
    let mut uuids = u_dec.uids().to_vec();
    let mut v_dec = Decoder::new(v);
    let mut vuids = v_dec.uids().to_vec();
    let (mut u_idx, mut v_idx) = (0, 0);

    loop {
        if uuids.is_empty() || vuids.is_empty() {
            break;
        }
        if u_idx == uuids.len() {
            if !u_dec.valid() {
                break;
            }
            uuids = u_dec.next().to_vec();
            u_idx = 0;
        }
        if v_idx == vuids.len() {
            if !v_dec.valid() {
                break;
            }
            vuids = v_dec.next().to_vec();
            v_idx = 0;
        }
        let (u_len, v_len) = (uuids.len(), vuids.len());
        while u_idx < u_len && v_idx < v_len {
            match uuids[u_idx].cmp(&vuids[v_idx]) {
                std::cmp::Ordering::Less => {
                    while u_idx < u_len && uuids[u_idx] < vuids[v_idx] {
                        result.add(uuids[u_idx]);
                        u_idx += 1;
                    }
                }
                std::cmp::Ordering::Equal => {
                    u_idx += 1;
                    v_idx += 1;
                }
                std::cmp::Ordering::Greater => {
                    v_idx += 1;
                    while v_idx < v_len && vuids[v_idx] < uuids[u_idx] {
                        v_idx += 1;
                    }
                }
            }
        }
        while u_idx < u_len && v_idx >= v_len {
            result.add(uuids[u_idx]);
            u_idx += 1;
        }
    }
    result.finish()
}

/// One packed merge candidate: heap entry plus its decoding cursor.
struct PackedEntry<'a> {
    entry: MergeEntry,
    decoder: Decoder<'a>,
    block_pos: usize,
    block: Vec<u64>,
}

impl PartialEq for PackedEntry<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.entry == other.entry
    }
}

impl Eq for PackedEntry<'_> {}

impl PartialOrd for PackedEntry<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PackedEntry<'_> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.entry.cmp(&other.entry)
    }
}

/// Merges sorted packs (deduped) with a k-way heap merge.
#[must_use]
pub fn merge_sorted_packed(lists: &[UidPack]) -> UidPack {
    if lists.is_empty() {
        return Encoder::new(0).finish();
    }
    let mut heap = BinaryHeap::with_capacity(lists.len());
    let mut block_size = 0;
    for (i, list) in lists.iter().enumerate() {
        if block_size == 0 {
            block_size = list.block_size;
        }
        let decoder = Decoder::new(list);
        let block = decoder.uids().to_vec();
        if block.is_empty() || exact_len(list) == 0 {
            continue;
        }
        heap.push(PackedEntry {
            entry: MergeEntry {
                val: block[0],
                list_idx: i,
            },
            decoder,
            block_pos: 0,
            block,
        });
    }

    let mut result = Encoder::new(block_size as usize);
    let mut empty = true;
    let mut last = 0u64;
    while let Some(mut top) = heap.peek_mut() {
        if empty || top.entry.val != last {
            result.add(top.entry.val);
            last = top.entry.val;
            empty = false;
        }
        let on_last_block = top.decoder.block_idx() + 1 >= top.decoder.pack().blocks.len();
        if top.block_pos + 1 >= top.block.len() && on_last_block {
            std::collections::binary_heap::PeekMut::pop(top);
            continue;
        }
        top.block_pos += 1;
        if top.block_pos >= top.block.len() {
            top.block = top.decoder.next().to_vec();
            top.block_pos = 0;
        }
        if top.block.is_empty() {
            // Corrupt/ragged input: Go would index out of bounds; drop the list.
            std::collections::binary_heap::PeekMut::pop(top);
            continue;
        }
        top.entry.val = top.block[top.block_pos];
    }
    result.finish()
}

/// Index of `uid` in the pack, or `None` when absent.
#[must_use]
pub fn index_of_packed(pack: &UidPack, uid: u64) -> Option<usize> {
    let mut decoder = Decoder::new(pack);
    decoder.seek(uid, SeekPos::Start);
    // Re-unpack: seek may have truncated the block, and the offset math below
    // needs the full block.
    decoder.unpack_block();
    let uids = decoder.uids().to_vec();
    if uids.is_empty() {
        return None;
    }
    let pos = uids.partition_point(|&u| u < uid);
    if pos >= uids.len() || uids[pos] != uid {
        return None;
    }
    let mut index = pos;
    for block in pack.blocks.iter().take(decoder.block_idx()) {
        index += block.num_uids as usize;
    }
    Some(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use datastreamdb_codec::encode;

    fn pack(uids: &[u64]) -> UidPack {
        // Small blocks, like Go's newUidPack, to force multi-block paths.
        encode(uids, 5)
    }

    fn decoded(pack: &UidPack) -> Vec<u64> {
        decode(pack, 0)
    }

    #[test]
    fn merge_packed_vectors() {
        // Ports of TestMergeSorted{1..10}Packed.
        assert_eq!(decoded(&merge_sorted_packed(&[pack(&[55])])), [55]);
        assert_eq!(
            decoded(&merge_sorted_packed(&[
                pack(&[1, 3, 6, 8, 10]),
                pack(&[2, 4, 5, 7, 15])
            ])),
            [1, 2, 3, 4, 5, 6, 7, 8, 10, 15]
        );
        assert_eq!(
            decoded(&merge_sorted_packed(&[pack(&[1, 3, 6, 8, 10]), pack(&[])])),
            [1, 3, 6, 8, 10]
        );
        assert_eq!(
            decoded(&merge_sorted_packed(&[pack(&[]), pack(&[1, 3, 6, 8, 10])])),
            [1, 3, 6, 8, 10]
        );
        assert!(decoded(&merge_sorted_packed(&[pack(&[]), pack(&[])])).is_empty());
        assert_eq!(
            decoded(&merge_sorted_packed(&[
                pack(&[11, 13, 16, 18, 20]),
                pack(&[12, 14, 15, 15, 16, 16, 17, 25]),
                pack(&[1, 2]),
            ])),
            [1, 2, 11, 12, 13, 14, 15, 16, 17, 18, 20, 25]
        );
        assert_eq!(
            decoded(&merge_sorted_packed(&[
                pack(&[5, 6, 7]),
                pack(&[3, 4]),
                pack(&[1, 2]),
                pack(&[]),
            ])),
            [1, 2, 3, 4, 5, 6, 7]
        );
        assert!(decoded(&merge_sorted_packed(&[])).is_empty());
        assert_eq!(decoded(&merge_sorted_packed(&[pack(&[1, 1, 1])])), [1]);
        assert_eq!(
            decoded(&merge_sorted_packed(&[
                pack(&[1, 2, 3, 3, 6]),
                pack(&[4, 8, 9])
            ])),
            [1, 2, 3, 4, 6, 8, 9]
        );
    }

    #[test]
    fn intersect_packed_vectors() {
        // Ports of TestUIDListIntersect{1..6}Packed + dup cases.
        let case = |u: &[u64], v: &[u64]| decoded(&intersect_with_lin_packed(&pack(u), &pack(v)));
        assert!(case(&[1, 2, 3], &[]).is_empty());
        assert_eq!(case(&[1, 2, 3], &[1, 2, 3, 4, 5]), [1, 2, 3]);
        assert_eq!(case(&[1, 2, 3], &[2]), [2]);
        assert!(case(&[1, 2, 3], &[0, 5]).is_empty());
        assert_eq!(case(&[1, 2, 3], &[3, 5]), [3]);
        assert_eq!(
            case(&[1, 2, 3, 4, 5, 6, 7, 9], &[1, 3, 5, 7, 8, 9]),
            [1, 3, 5, 7, 9]
        );
        assert_eq!(case(&[1, 1, 2, 3], &[1, 2]), [1, 2]);
        assert_eq!(case(&[1, 1, 2, 3, 5], &[1, 1, 2, 4]), [1, 1, 2]);
        assert_eq!(case(&[1, 2, 3, 5], &[1, 1, 2, 4]), [1, 2]);
    }

    #[test]
    fn intersect_sorted_packed_vectors() {
        // Ports of TestIntersectSorted{1..7}Packed.
        let run = |lists: &[&[u64]]| {
            decoded(&intersect_sorted_packed(
                &lists.iter().map(|l| pack(l)).collect::<Vec<_>>(),
            ))
        };
        assert_eq!(run(&[&[1, 2, 3], &[2, 3, 4, 5]]), [2, 3]);
        assert_eq!(run(&[&[1, 2, 3]]), [1, 2, 3]);
        assert!(run(&[]).is_empty());
        assert_eq!(run(&[&[100, 101]]), [100, 101]);
        assert!(run(&[&[1, 2, 3], &[2, 3, 4, 5], &[4, 5, 6]]).is_empty());
        assert!(run(&[&[10, 12, 13], &[2, 3, 4, 13], &[4, 5, 6]]).is_empty());
        assert_eq!(
            run(&[
                &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
                &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10],
                &[1, 2, 3, 4, 5, 6, 7, 8, 9],
                &[1, 2, 3, 4, 5, 6, 7, 8],
                &[1, 2, 3, 4, 5, 6, 7],
                &[1, 2, 3, 4, 5, 6],
                &[1, 2, 3, 4, 5],
                &[1, 2, 3, 4],
                &[1, 2, 3],
                &[1, 2],
                &[1],
            ]),
            [1]
        );
    }

    #[test]
    fn difference_packed_vectors() {
        // Ports of TestDiffSorted{1..5}Packed + TestSubSorted{1,6}Packed.
        let some = |u: &[u64]| Some(pack(u));
        let run = |u: &[u64], v: &[u64]| {
            decoded(&difference_packed(Some(&pack(u)), Some(&pack(v))).expect("some"))
        };
        assert_eq!(run(&[1, 2, 3], &[1]), [2, 3]);
        assert_eq!(run(&[1, 2, 3], &[2]), [1, 3]);
        assert_eq!(run(&[1, 2, 3], &[3]), [1, 2]);
        // Go's empty pack is nil: `u − nil` copies u (DiffSorted4Packed).
        assert_eq!(
            decoded(&difference_packed(some(&[1, 2, 3]).as_ref(), None).expect("copy")),
            [1, 2, 3]
        );
        assert!(run(&[], &[1, 2]).is_empty());
        assert_eq!(run(&[1, 2, 3], &[2, 3, 4, 5]), [1]);
        assert_eq!(run(&[10, 12, 13], &[2, 3, 4, 13]), [10, 12]);
        assert!(difference_packed(None, None).is_none());
        assert!(difference_packed(None, some(&[1]).as_ref()).is_none());
        assert_eq!(
            decoded(&difference_packed(some(&[1, 2]).as_ref(), None).expect("copy")),
            [1, 2]
        );
    }

    #[test]
    fn index_of_packed_vectors() {
        // Ports of TestIndexOfPacked{1,2}.
        let pack = encode(&(0..1000u64).collect::<Vec<_>>(), 10);
        for i in 0..1000u64 {
            assert_eq!(
                index_of_packed(&pack, i),
                Some(usize::try_from(i).expect("test index fits"))
            );
        }
        assert_eq!(index_of_packed(&pack, 1000), None);
        let pack = encode(&(0..100u64).collect::<Vec<_>>(), 10);
        for missing in [100, 101, 1000, u64::MAX] {
            assert_eq!(index_of_packed(&pack, missing), None);
        }
    }

    #[test]
    fn apply_filter_packed_keeps_matching() {
        let l = [1, 2, 3, 4, 5, 6, 7, 8, 9];
        let out = apply_filter_packed(&pack(&l), |_, idx| l[idx] % 2 == 1);
        assert_eq!(decoded(&out), [1, 3, 5, 7, 9]);
    }
}
