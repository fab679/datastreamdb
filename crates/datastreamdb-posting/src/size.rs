//! List memory accounting. Port of `posting/size.go`: rough
//! (`approximate_size`, for cache costing) and deep sizes.
//!
//! Delta from Go: sizes derive from `len`/`capacity` instead of pointer
//! arithmetic over Go maps, so absolute numbers differ while relative
//! ordering (what eviction needs) is preserved.

#![warn(missing_docs)]

use datastreamdb_proto::{Posting, PostingList, UidBlock, UidPack, api::Facet};

use crate::layer::MutableLayer;
use crate::list::List;

impl List {
    /// Rough memory footprint (cache costing).
    #[must_use]
    pub fn approximate_size(&self) -> u64 {
        let inner = self.inner.read();
        #[allow(clippy::cast_possible_truncation, reason = "sizes fit u64 in practice")]
        let mut size: u64 = 14 * 8;
        size += inner.key.capacity() as u64;
        size += inner.cache.as_ref().map_or(0, |c| c.capacity() as u64);
        size += approx_posting_list_size(&inner.plist);
        size += inner.mutation_map.approximate_size();
        size
    }

    /// Deep memory footprint.
    #[must_use]
    pub fn deep_size(&self) -> u64 {
        let inner = self.inner.read();
        #[allow(clippy::cast_possible_truncation, reason = "sizes fit u64 in practice")]
        let mut size: u64 = 11 * 8;
        size += inner.key.capacity() as u64;
        size += posting_list_size(&inner.plist);
        size += 6 * 8; // map header
        for pl in inner.mutation_map.committed_entries().values() {
            size += posting_list_size(pl);
        }
        if let Some(current) = inner.mutation_map.current_entries() {
            size += posting_list_size(current);
        }
        size
    }
}

/// Rough size of a posting list (samples one posting, like Go).
fn approx_posting_list_size(list: &PostingList) -> u64 {
    #[allow(clippy::cast_possible_truncation, reason = "sizes fit u64 in practice")]
    let mut size: u64 = 8 * 8;
    if let Some(pack) = &list.pack {
        size += pack_size(pack);
    }
    size += list.postings.capacity() as u64 * 8;
    if let Some(first) = list.postings.first() {
        size += posting_size(first) * list.postings.len() as u64;
    }
    size += list.splits.capacity() as u64 * 8;
    size
}

impl MutableLayer {
    /// Rough memory footprint of the layer.
    #[must_use]
    pub fn approximate_size(&self) -> u64 {
        #[allow(clippy::cast_possible_truncation, reason = "sizes fit u64 in practice")]
        let mut size: u64 = 11 * 8;
        size += self.committed_entries().len() as u64 * 8;
        if let Some(first) = self.committed_entries().values().next() {
            size += posting_list_size(first);
        }
        if let Some(current) = self.current_entries() {
            size += approx_posting_list_size(current);
        }
        if let Some(last) = self.last_entry() {
            size += approx_posting_list_size(last);
        }
        size
    }
}

/// Deep size of a posting list.
fn posting_list_size(list: &PostingList) -> u64 {
    #[allow(clippy::cast_possible_truncation, reason = "sizes fit u64 in practice")]
    let mut size: u64 = 8 * 8;
    if let Some(pack) = &list.pack {
        size += pack_size(pack);
    }
    size += list.postings.capacity() as u64 * 8;
    for posting in &list.postings {
        size += posting_size(posting);
    }
    size += list.splits.capacity() as u64 * 8;
    size
}

/// Size of one posting.
fn posting_size(posting: &Posting) -> u64 {
    #[allow(clippy::cast_possible_truncation, reason = "sizes fit u64 in practice")]
    let mut size: u64 = 19 * 8;
    size += posting.value.capacity() as u64;
    size += posting.lang_tag.capacity() as u64;
    for facet in &posting.facets {
        size += facet_size(facet);
    }
    size
}

/// Size of a UID pack.
fn pack_size(pack: &UidPack) -> u64 {
    #[allow(clippy::cast_possible_truncation, reason = "sizes fit u64 in practice")]
    let mut size: u64 = 6 * 8;
    size += pack.blocks.capacity() as u64 * 8;
    for block in &pack.blocks {
        size += uid_block_size(block);
    }
    size
}

/// Size of a UID block.
fn uid_block_size(block: &UidBlock) -> u64 {
    #[allow(clippy::cast_possible_truncation, reason = "sizes fit u64 in practice")]
    let mut size: u64 = 6 * 8;
    size += block.deltas.capacity() as u64;
    size
}

/// Size of a facet.
fn facet_size(facet: &Facet) -> u64 {
    #[allow(clippy::cast_possible_truncation, reason = "sizes fit u64 in practice")]
    let mut size: u64 = 16 * 8;
    size += facet.key.len() as u64;
    size += facet.value.capacity() as u64;
    for token in &facet.tokens {
        size += token.len() as u64;
    }
    size += facet.alias.len() as u64;
    size
}
