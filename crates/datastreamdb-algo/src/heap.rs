//! Min-heap entries for k-way merges. Port of `algo/heap.go`.
//!
//! Go's `container/heap` with `heap.Fix` maps to [`std::collections::BinaryHeap`]
//! with [`std::collections::binary_heap::PeekMut`]: mutating through the peeked
//! guard re-heapifies on drop, exactly like `Fix`.

#![warn(missing_docs)]

/// One merge candidate: current value plus owning list index.
/// Ordered as a min-heap (smallest value pops first; ties break by list index
/// for determinism — Go leaves ties unspecified).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MergeEntry {
    /// Current head value of the list.
    pub val: u64,
    /// Owning list index (tie-break only).
    pub list_idx: usize,
}

impl Ord for MergeEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Reversed: BinaryHeap is a max-heap; we pop the minimum.
        other
            .val
            .cmp(&self.val)
            .then_with(|| other.list_idx.cmp(&self.list_idx))
    }
}

impl PartialOrd for MergeEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BinaryHeap;

    #[test]
    fn pops_minimum_first() {
        let mut heap = BinaryHeap::from([
            MergeEntry {
                val: 9,
                list_idx: 0,
            },
            MergeEntry {
                val: 1,
                list_idx: 1,
            },
            MergeEntry {
                val: 5,
                list_idx: 0,
            },
        ]);
        assert_eq!(heap.pop().map(|e| e.val), Some(1));
        assert_eq!(heap.pop().map(|e| e.val), Some(5));
        assert_eq!(heap.pop().map(|e| e.val), Some(9));
    }
}
