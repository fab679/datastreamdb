//! Sorted-list intersections, differences, and merges. Port of `algo/uidlist.go`.

#![warn(missing_docs)]

use std::collections::BinaryHeap;

use datastreamdb_codec::{Decoder, SeekPos, exact_len};
use datastreamdb_proto::{List, UidPack};

use super::heap::MergeEntry;

/// Jump size in [`intersect_with_jump`].
pub const JUMP: usize = 32;

/// Size ratio below which linear scan beats binary search.
pub const LIN_VS_BIN_RATIO: f64 = 10.0;

/// Threads × lists-per-thread before [`merge_sorted`] parallelizes.
const PARALLEL_LISTS: usize = 100;
/// Worker count for parallel merges.
const NUM_THREADS: usize = 10;

/// Retains UIDs satisfying `f(uid, index)` in place.
pub fn apply_filter(list: &mut List, mut f: impl FnMut(u64, usize) -> bool) {
    let mut out = 0usize;
    for i in 0..list.uids.len() {
        if f(list.uids[i], i) {
            list.uids[out] = list.uids[i];
            out += 1;
        }
    }
    list.uids.truncate(out);
}

/// Intersects packed `pack` (past `after_uid`) with sorted `v` into `out`.
pub fn intersect_compressed_with(pack: &UidPack, after_uid: u64, v: &[u64], out: &mut Vec<u64>) {
    let mut decoder = Decoder::new(pack);
    decoder.seek(after_uid, SeekPos::Start);
    let (mut n, mut m) = (decoder.approx_len(), v.len());
    if n > m {
        std::mem::swap(&mut n, &mut m);
    }
    out.clear();
    if n == 0 {
        n = 1;
    }
    #[allow(clippy::cast_precision_loss, reason = "Go float64() ratio")]
    let ratio = (m as f64) / (n as f64);
    if ratio < LIN_VS_BIN_RATIO {
        intersect_compressed_with_lin_jump(&mut decoder, v, out);
    } else {
        intersect_compressed_with_bin(&mut decoder, v, out);
    }
}

/// Linear compressed intersection; `decoder` must be seeked first.
pub fn intersect_compressed_with_lin_jump(
    decoder: &mut Decoder<'_>,
    v: &[u64],
    out: &mut Vec<u64>,
) {
    let m = v.len();
    let mut k = 0;
    let (_, off) = intersect_with_lin(decoder.uids(), &v[k..], out);
    k += off;
    while k < m {
        let block = decoder.linear_seek(v[k]).to_vec();
        if block.is_empty() {
            break;
        }
        let (_, off) = intersect_with_lin(&block, &v[k..], out);
        k += off.max(1); // v[k] missing from the block: move forward.
    }
}

/// Binary compressed intersection (2-approximation galloping); `decoder` must
/// be seeked first.
pub fn intersect_compressed_with_bin(decoder: &mut Decoder<'_>, q: &[u64], out: &mut Vec<u64>) {
    let ld = exact_len(decoder.pack());
    let lq = q.len();
    if lq == 0 {
        return;
    }
    if ld <= lq {
        let mut q = q;
        loop {
            let block = decoder.uids().to_vec();
            if block.is_empty() {
                break;
            }
            let (_, off) = intersect_with_jump(&block, q, out);
            q = &q[off..];
            if q.is_empty() {
                return;
            }
            decoder.next();
        }
        return;
    }
    let mut uids = decoder.uids().to_vec();
    let mut qidx = 0;
    loop {
        if qidx >= q.len() {
            return;
        }
        let u = q[qidx];
        if uids.is_empty() || u > uids[uids.len() - 1] {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "const 10.0 is exactly 10"
            )]
            let stride: usize = LIN_VS_BIN_RATIO as usize;
            uids = if lq.saturating_mul(stride) < ld {
                decoder.linear_seek(u).to_vec()
            } else {
                decoder.seek_to_block(u, SeekPos::Current).to_vec()
            };
            if uids.is_empty() {
                return;
            }
        }
        let (_, off) = intersect_with_jump(&uids, &q[qidx..], out);
        qidx += off.max(1);
    }
}

/// Intersects sorted `u` with sorted `v` into `out` (cleared first),
/// picking the linear/jump/binary strategy by size ratio.
pub fn intersect_with(u: &[u64], v: &[u64], out: &mut Vec<u64>) {
    let (mut n, mut m) = (u.len(), v.len());
    if n > m {
        std::mem::swap(&mut n, &mut m);
    }
    out.clear();
    if n == 0 {
        n = 1;
    }
    #[allow(clippy::cast_precision_loss, reason = "Go float64() ratio")]
    let ratio = (m as f64) / (n as f64);
    match ratio {
        ratio if ratio < 100.0 => {
            intersect_with_lin(u, v, out);
        }
        ratio if ratio < 500.0 => {
            intersect_with_jump(u, v, out);
        }
        _ => {
            intersect_with_bin(u, v, out);
        }
    }
}

/// Linear intersection. Returns `(u consumed, v consumed)`.
#[allow(clippy::many_single_char_names, reason = "Go algorithm identifiers")]
pub fn intersect_with_lin(u: &[u64], v: &[u64], out: &mut Vec<u64>) -> (usize, usize) {
    let (n, m) = (u.len(), v.len());
    let (mut i, mut k) = (0, 0);
    while i < n && k < m {
        match u[i].cmp(&v[k]) {
            std::cmp::Ordering::Greater => {
                k += 1;
                while k < m && v[k] < u[i] {
                    k += 1;
                }
            }
            std::cmp::Ordering::Equal => {
                out.push(u[i]);
                k += 1;
                i += 1;
            }
            std::cmp::Ordering::Less => {
                i += 1;
                while i < n && u[i] < v[k] {
                    i += 1;
                }
            }
        }
    }
    (i, k)
}

/// Linear intersection with [`JUMP`]-step gallops. Returns `(u, v)` consumed.
#[allow(clippy::many_single_char_names, reason = "Go algorithm identifiers")]
pub fn intersect_with_jump(u: &[u64], v: &[u64], out: &mut Vec<u64>) -> (usize, usize) {
    let (n, m) = (u.len(), v.len());
    let (mut i, mut k) = (0, 0);
    while i < n && k < m {
        let (uid, vid) = (u[i], v[k]);
        if uid == vid {
            out.push(uid);
            k += 1;
            i += 1;
        } else if k + JUMP < m && uid > v[k + JUMP] {
            k += JUMP;
        } else if i + JUMP < n && vid > u[i + JUMP] {
            i += JUMP;
        } else if uid > vid {
            k += 1;
            while k < m && v[k] < uid {
                k += 1;
            }
        } else {
            i += 1;
            while i < n && u[i] < vid {
                i += 1;
            }
        }
    }
    (i, k)
}

/// Binary-search intersection for skewed sizes. Returns the `q` offset consumed.
pub fn intersect_with_bin(d: &[u64], q: &[u64], out: &mut Vec<u64>) -> usize {
    let (mut ld, mut lq) = (d.len(), q.len());
    let (mut d, mut q) = (d, q);
    if ld < lq {
        std::mem::swap(&mut ld, &mut lq);
        std::mem::swap(&mut d, &mut q);
    }
    if ld == 0 || lq == 0 || d[ld - 1] < q[0] || q[lq - 1] < d[0] {
        return 0;
    }
    let minq = q.partition_point(|&x| x < d[0]);
    let maxq = q.partition_point(|&x| x <= d[d.len() - 1]);
    bin_intersect(d, &q[minq..maxq], out);
    maxq
}

/// Recursive galloping intersection; callers keep `d.len() >= q.len()`.
#[allow(clippy::similar_names, reason = "Go d/q identifiers")]
fn bin_intersect(d: &[u64], q: &[u64], out: &mut Vec<u64>) {
    if d.is_empty() || q.is_empty() {
        return;
    }
    let midq = q.len() / 2;
    let qval = q[midq];
    let mut midd = d.partition_point(|&x| x < qval);

    let (dd, qq) = (&d[..midd], &q[..midq]);
    if dd.len() > qq.len() {
        bin_intersect(dd, qq, out);
    } else {
        bin_intersect(qq, dd, out);
    }

    if midd >= d.len() {
        return;
    }
    if d[midd] == qval {
        out.push(qval);
    } else {
        midd -= 1;
    }

    let (dd, qq) = (&d[midd + 1..], &q[midq + 1..]);
    if dd.len() > qq.len() {
        bin_intersect(dd, qq, out);
    } else {
        bin_intersect(qq, dd, out);
    }
}

/// Intersects all lists, smallest first. Empty input yields an empty list.
#[must_use]
pub fn intersect_sorted(lists: &[List]) -> List {
    if lists.is_empty() {
        return List { uids: Vec::new() };
    }
    let mut order: Vec<usize> = (0..lists.len()).collect();
    order.sort_by_key(|&i| lists[i].uids.len());
    if order.len() == 1 {
        return List {
            uids: lists[order[0]].uids.clone(),
        };
    }
    let mut out = Vec::with_capacity(lists[order[0]].uids.len());
    intersect_with(&lists[order[0]].uids, &lists[order[1]].uids, &mut out);
    for &i in &order[2..] {
        let mut next = Vec::new();
        intersect_with(&out, &lists[i].uids, &mut next);
        out = next;
        if out.is_empty() {
            break;
        }
    }
    List { uids: out }
}

/// Multiset difference `u − v` (each `v` value cancels one `u` value).
#[must_use]
pub fn difference(u: &List, v: &List) -> List {
    List {
        uids: super::difference_slices(&u.uids, &v.uids),
    }
}

/// Merges sorted lists (deduped); recursion + threads past 100 lists.
///
/// # Panics
///
/// Panics if a merge worker thread panics.
#[must_use]
pub fn merge_sorted_more_mem(lists: &[List]) -> List {
    const FANOUT: usize = 10;
    if lists.len() <= FANOUT * FANOUT {
        return internal_merge_sort(lists);
    }
    let mut merged: Vec<List> = Vec::with_capacity(FANOUT);
    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(FANOUT);
        for i in 0..FANOUT {
            let end = ((i + 1) * lists.len() / FANOUT).min(lists.len());
            let chunk = &lists[i * lists.len() / FANOUT..end];
            handles.push(scope.spawn(move || internal_merge_sort(chunk)));
        }
        for handle in handles {
            merged.push(handle.join().expect("merge worker"));
        }
    });
    merge_sorted_more_mem(&merged)
}

/// Merges sorted lists with one scratch buffer.
fn internal_merge_sort_with_buffer(lists: &[List], mut buffer: Vec<u64>) -> List {
    buffer.clear();
    let mut heap = BinaryHeap::with_capacity(lists.len());
    for (i, list) in lists.iter().enumerate() {
        if let Some(&first) = list.uids.first() {
            heap.push(MergeEntry {
                val: first,
                list_idx: i,
            });
        }
    }
    let mut idx = vec![0usize; lists.len()];
    let mut last = None;
    while let Some(mut top) = heap.peek_mut() {
        let (val, li) = (top.val, top.list_idx);
        if last != Some(val) {
            buffer.push(val);
            last = Some(val);
        }
        idx[li] += 1;
        if idx[li] < lists[li].uids.len() {
            top.val = lists[li].uids[idx[li]];
        } else {
            std::collections::binary_heap::PeekMut::pop(top);
        }
    }
    List { uids: buffer }
}

/// Merges sorted lists (deduped), single-threaded.
#[must_use]
pub fn internal_merge_sort(lists: &[List]) -> List {
    let size: usize = lists.iter().map(|l| l.uids.len()).sum();
    internal_merge_sort_with_buffer(lists, Vec::with_capacity(size))
}

/// Merges sorted lists (deduped), parallel past a threshold list count.
///
/// # Panics
///
/// Panics if a merge worker thread panics.
#[must_use]
pub fn merge_sorted(lists: &[List]) -> List {
    if lists.len() < PARALLEL_LISTS {
        return internal_merge_sort(lists);
    }
    let chunk = lists.len().div_ceil(NUM_THREADS);
    let mut merged: Vec<List> = Vec::with_capacity(NUM_THREADS);
    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(NUM_THREADS);
        for i in 0..NUM_THREADS {
            let start = i * chunk;
            if start >= lists.len() {
                break;
            }
            let end = ((i + 1) * chunk).min(lists.len());
            let slice = &lists[start..end];
            handles.push(scope.spawn(move || internal_merge_sort(slice)));
        }
        for handle in handles {
            let part = handle.join().expect("merge worker");
            if !part.uids.is_empty() {
                merged.push(part);
            }
        }
    });
    internal_merge_sort(&merged)
}

/// Binary-searches `uid` in `list`, returning its index or `None`.
#[must_use]
pub fn index_of(list: &List, uid: u64) -> Option<usize> {
    let i = list.uids.partition_point(|&x| x < uid);
    (i < list.uids.len() && list.uids[i] == uid).then_some(i)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(uids: &[u64]) -> List {
        List {
            uids: uids.to_vec(),
        }
    }

    #[test]
    fn merge_vectors() {
        // Ports of TestMergeSorted1..10.
        assert_eq!(merge_sorted(&[list(&[55])]).uids, [55]);
        assert_eq!(
            merge_sorted(&[list(&[1, 3, 6, 8, 10]), list(&[2, 4, 5, 7, 15])]).uids,
            [1, 2, 3, 4, 5, 6, 7, 8, 10, 15]
        );
        assert_eq!(
            merge_sorted(&[list(&[1, 3, 6, 8, 10]), list(&[])]).uids,
            [1, 3, 6, 8, 10]
        );
        assert_eq!(
            merge_sorted(&[list(&[]), list(&[1, 3, 6, 8, 10])]).uids,
            [1, 3, 6, 8, 10]
        );
        assert!(merge_sorted(&[list(&[]), list(&[])]).uids.is_empty());
        assert_eq!(
            merge_sorted(&[
                list(&[11, 13, 16, 18, 20]),
                list(&[12, 14, 15, 15, 16, 16, 17, 25]),
                list(&[1, 2]),
            ])
            .uids,
            [1, 2, 11, 12, 13, 14, 15, 16, 17, 18, 20, 25]
        );
        assert_eq!(
            merge_sorted(&[list(&[5, 6, 7]), list(&[3, 4]), list(&[1, 2]), list(&[])]).uids,
            [1, 2, 3, 4, 5, 6, 7]
        );
        assert!(merge_sorted(&[]).uids.is_empty());
        assert_eq!(merge_sorted(&[list(&[1, 1, 1])]).uids, [1]);
        assert_eq!(
            merge_sorted(&[list(&[1, 2, 3, 3, 6]), list(&[4, 8, 9])]).uids,
            [1, 2, 3, 4, 6, 8, 9]
        );
    }

    #[test]
    fn intersect_sorted_vectors() {
        // Ports of TestIntersectSorted1..6.
        assert_eq!(
            intersect_sorted(&[list(&[1, 2, 3]), list(&[2, 3, 4, 5])]).uids,
            [2, 3]
        );
        assert_eq!(intersect_sorted(&[list(&[1, 2, 3])]).uids, [1, 2, 3]);
        assert!(intersect_sorted(&[]).uids.is_empty());
        assert_eq!(intersect_sorted(&[list(&[100, 101])]).uids, [100, 101]);
        assert!(
            intersect_sorted(&[list(&[1, 2, 3]), list(&[2, 3, 4, 5]), list(&[4, 5, 6])])
                .uids
                .is_empty()
        );
        assert!(
            intersect_sorted(&[list(&[10, 12, 13]), list(&[2, 3, 4, 13]), list(&[4, 5, 6])])
                .uids
                .is_empty()
        );
    }

    #[test]
    fn difference_vectors() {
        // Ports of TestDiffSorted1..5 + TestSubSorted1/6.
        assert_eq!(difference(&list(&[1, 2, 3]), &list(&[1])).uids, [2, 3]);
        assert_eq!(difference(&list(&[1, 2, 3]), &list(&[2])).uids, [1, 3]);
        assert_eq!(difference(&list(&[1, 2, 3]), &list(&[3])).uids, [1, 2]);
        assert_eq!(difference(&list(&[1, 2, 3]), &list(&[])).uids, [1, 2, 3]);
        assert!(difference(&list(&[]), &list(&[1, 2])).uids.is_empty());
        assert_eq!(
            difference(&list(&[1, 2, 3]), &list(&[2, 3, 4, 5])).uids,
            [1]
        );
        assert_eq!(
            difference(&list(&[10, 12, 13]), &list(&[2, 3, 4, 13])).uids,
            [10, 12]
        );
    }

    #[test]
    fn intersect_with_vectors() {
        // Ports of TestUIDListIntersect1..8 + dup cases (aliasing spelled
        // with an explicit out buffer).
        let mut out = Vec::new();
        intersect_with(&[1, 2, 3], &[], &mut out);
        assert!(out.is_empty());
        intersect_with(&[1, 2, 3], &[1, 2, 3, 4, 5], &mut out);
        assert_eq!(out, [1, 2, 3]);
        intersect_with(&[1, 2, 3], &[2], &mut out);
        assert_eq!(out, [2]);
        intersect_with(&[1, 2, 3], &[0, 5], &mut out);
        assert!(out.is_empty());
        intersect_with(&[1, 2, 3], &[3, 5], &mut out);
        assert_eq!(out, [3]);
        intersect_with(&[1, 1, 2, 3], &[1, 2], &mut out);
        assert_eq!(out, [1, 2]);
        intersect_with(&[1, 1, 2, 3, 5], &[1, 1, 2, 4], &mut out);
        assert_eq!(out, [1, 1, 2]);
        intersect_with(&[1, 2, 3, 5], &[1, 1, 2, 4], &mut out);
        assert_eq!(out, [1, 2]);
    }

    #[test]
    fn intersect_scales_to_bin_and_jump_paths() {
        // fillNums-style skewed inputs exercising the jump/bin strategies.
        let common: Vec<u64> = (0..10).map(|i| i * 1000).collect();
        let mut other: Vec<u64> = (0..20000).collect();
        other.extend_from_slice(&common);
        other.sort_unstable();
        other.dedup();
        let mut out = Vec::new();
        intersect_with(&common, &other, &mut out);
        assert_eq!(out, common);
        out.clear();
        intersect_with_bin(&other, &common, &mut out);
        // Bin path is exact on duplicate-free inputs.
        assert_eq!(out, common);
    }

    #[test]
    fn apply_filter_keeps_matching() {
        let mut l = list(&[1, 2, 3, 4, 5]);
        apply_filter(&mut l, |uid, _| uid % 2 == 1);
        assert_eq!(l.uids, [1, 3, 5]);
    }

    #[test]
    fn index_of_finds_or_not() {
        let l = list(&[10, 20, 30]);
        assert_eq!(index_of(&l, 20), Some(1));
        assert_eq!(index_of(&l, 25), None);
    }

    #[test]
    fn parallel_merge_matches_serial() {
        let lists: Vec<List> = (0..120)
            .map(|i| list(&(i * 10..i * 10 + 25).collect::<Vec<_>>()))
            .collect();
        assert_eq!(merge_sorted(&lists).uids, internal_merge_sort(&lists).uids);
        assert_eq!(
            merge_sorted_more_mem(&lists).uids,
            internal_merge_sort(&lists).uids
        );
    }
}
