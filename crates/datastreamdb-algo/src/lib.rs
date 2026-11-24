//! UID-list algorithms: intersect, merge, sketches.
//!
//! Rust port of the upstream Go `algo` package: sorted-list set operations
//! ([`uidlist`]), packed-list variants ([`packed`]), the k-way merge heap
//! ([`heap`]), and the count-min sketch ([`sketch`]).
//!
//! Slice helpers ([`intersect_slices`], [`union_slices`], [`difference_slices`])
//! are the allocation-friendly entry points; `List`-based functions mirror the
//! Go API. Go's `IntersectWith(u, v, u)` in-place aliasing has no Rust
//! equivalent: callers pass a separate `out` buffer and swap it in.

#![warn(missing_docs)]

pub mod heap;
pub mod packed;
pub mod sketch;
pub mod uidlist;

/// Intersection of two sorted UID slices (duplicates intersect pairwise,
/// like [`uidlist::intersect_with_lin`]).
#[must_use]
pub fn intersect_slices(a: &[u64], b: &[u64]) -> Vec<u64> {
    let mut out = Vec::new();
    uidlist::intersect_with_lin(a, b, &mut out);
    out
}

/// Union of two sorted, deduplicated UID slices (sorted, deduplicated output).
#[must_use]
pub fn union_slices(a: &[u64], b: &[u64]) -> Vec<u64> {
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::with_capacity(a.len() + b.len());
    while let (Some(&x), Some(&y)) = (a.get(i), b.get(j)) {
        match x.cmp(&y) {
            std::cmp::Ordering::Less => {
                out.push(x);
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                out.push(y);
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                out.push(x);
                i += 1;
                j += 1;
            }
        }
    }
    out.extend_from_slice(&a[i.min(a.len())..]);
    out.extend_from_slice(&b[j.min(b.len())..]);
    out
}

/// Multiset difference of two sorted UID slices: each value of `b` cancels one
/// matching value of `a`. Port of Go's `Difference` walk.
#[must_use]
#[allow(clippy::many_single_char_names, reason = "Go algorithm identifiers")]
pub fn difference_slices(a: &[u64], b: &[u64]) -> Vec<u64> {
    let (n, m) = (a.len(), b.len());
    let mut out = Vec::with_capacity(n / 2);
    let (mut i, mut k) = (0, 0);
    while i < n && k < m {
        match a[i].cmp(&b[k]) {
            std::cmp::Ordering::Less => {
                while i < n && a[i] < b[k] {
                    out.push(a[i]);
                    i += 1;
                }
            }
            std::cmp::Ordering::Equal => {
                i += 1;
                k += 1;
            }
            std::cmp::Ordering::Greater => {
                k += 1;
                while k < m && b[k] < a[i] {
                    k += 1;
                }
            }
        }
    }
    while i < n && k >= m {
        out.push(a[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intersect_basic() {
        assert_eq!(intersect_slices(&[1, 2, 4, 8], &[2, 3, 4, 9]), vec![2, 4]);
        assert!(intersect_slices(&[1, 3], &[2, 4]).is_empty());
        assert!(intersect_slices(&[], &[1]).is_empty());
        assert!(intersect_slices(&[1], &[]).is_empty());
    }

    #[test]
    fn union_basic() {
        assert_eq!(union_slices(&[1, 3, 5], &[2, 3, 6]), vec![1, 2, 3, 5, 6]);
        assert_eq!(union_slices(&[], &[7]), vec![7]);
        assert_eq!(union_slices(&[7], &[]), vec![7]);
    }

    #[test]
    fn difference_basic() {
        assert_eq!(difference_slices(&[1, 2, 3, 4], &[2, 4, 6]), vec![1, 3]);
        assert!(difference_slices(&[1], &[1, 2]).is_empty());
        // Multiset semantics: each b value cancels one a value.
        assert_eq!(difference_slices(&[1, 1, 2], &[1]), vec![1, 2]);
        assert!(difference_slices(&[1, 1], &[1, 1]).is_empty());
    }

    #[test]
    fn ops_agree_on_inputs() {
        let a: Vec<u64> = (0..500).filter(|x| x % 3 != 0).collect();
        let b: Vec<u64> = (0..500).filter(|x| x % 5 != 0).collect();
        let inter = intersect_slices(&a, &b);
        let expected: Vec<u64> = (0..500).filter(|x| x % 3 != 0 && x % 5 != 0).collect();
        assert_eq!(inter, expected);
        assert_eq!(union_slices(&a, &b).len() + inter.len(), a.len() + b.len());
        assert_eq!(difference_slices(&a, &b).len() + inter.len(), a.len());
    }
}
