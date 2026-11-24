//! In-memory posting lists. Port of the `List` core of
//! `posting/list.go`: the immutable base plus [`MutableLayer`], mutation
//! ingestion, commit application, and UID warming.
//!
//! Locking: Go embeds a mutex in `List` and asserts recursive read locks in
//! helpers. Rust locks do not recurse, so [`List`] wraps a
//! [`ListInner`] in a [`parking_lot::RwLock`]; public methods lock
//! and delegate to lock-free inner methods. Every list also carries its
//! storage backend (Go uses the global `pstore`), so many engine instances
//! share one process.
//!
//! Read/iteration helpers live in [`crate::iter`], value accessors in
//! [`crate::read`], and rollup in [`crate::rollup`].

#![warn(missing_docs)]

use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

use datastreamdb_proto::{
    BackupPostingList, DirectedEdge, EdgeOp, Posting, PostingList, PostingType, ValType,
};
use datastreamdb_schema::SchemaState;
use datastreamdb_store::Backend;
use parking_lot::RwLock;
use prost::Message as _;

use crate::errors::{PostingError, Result};
use crate::layer::MutableLayer;

/// Set operation in the mutation layer (contributes +1 to length).
pub const OP_SET: u32 = 0x01;
/// Delete operation in the mutation layer (contributes -1 to length).
pub const OP_DEL: u32 = 0x02;
/// Overwrite operation in the mutation layer (contributes 0 to length).
pub const OP_OVR: u32 = 0x03;

/// Signals the value stores a schema or type.
pub const BIT_SCHEMA_POSTING: u8 = 0x01;
/// Signals the value stores a posting-list delta.
pub const BIT_DELTA_POSTING: u8 = 0x04;
/// Signals the value stores a complete posting list.
pub const BIT_COMPLETE_POSTING: u8 = 0x08;
/// Signals the value stores an empty posting list.
pub const BIT_EMPTY_POSTING: u8 = 0x10;

/// Star-all marker value (`_STAR_ALL`).
pub const STAR_ALL: &str = "_STAR_ALL";
/// Lists at or above this encoded size split during rollup.
pub const MAX_LIST_SIZE: usize = 512 * 1024;

/// Whether the posting is a delete-all marker.
#[must_use]
pub fn has_delete_all(mpost: &Posting) -> bool {
    mpost.op == OP_DEL && mpost.value == STAR_ALL.as_bytes() && mpost.lang_tag.is_empty()
}

/// Builds the delete-all marker posting.
#[must_use]
pub fn create_delete_all_posting() -> Posting {
    Posting {
        op: OP_DEL,
        value: STAR_ALL.as_bytes().to_vec(),
        uid: u64::MAX,
        ..Posting::default()
    }
}

/// Converts a directed edge into its posting representation.
///
/// # Errors
///
/// Returns [`PostingError::Corrupt`] on an unknown edge operation.
#[must_use = "builds the posting without applying it"]
pub fn new_posting(edge: &DirectedEdge) -> Result<Posting> {
    let op = match EdgeOp::try_from(edge.op)
        .map_err(|_| PostingError::Corrupt(format!("bad edge op {}", edge.op)))?
    {
        EdgeOp::Set => OP_SET,
        EdgeOp::Ovr => OP_OVR,
        EdgeOp::Del => OP_DEL,
    };
    let posting_type = if !edge.lang.is_empty() {
        PostingType::ValueLang
    } else if edge.value_id == 0 {
        PostingType::Value
    } else {
        PostingType::Ref
    };
    Ok(Posting {
        uid: edge.value_id,
        value: edge.value.clone(),
        val_type: edge.value_type,
        posting_type: posting_type as i32,
        lang_tag: edge.lang.as_bytes().to_vec(),
        op,
        facets: edge.facets.clone(),
        ..Posting::default()
    })
}

/// Stable 64-bit fingerprint. Delta from Go: Go uses `FarmHash64`
/// (`farm.Fingerprint64`); we use the verified FNV-1a in `datastreamdb-algo`
/// because our on-disk format is new and fingerprints never cross the
/// Go/Rust boundary.
#[must_use]
pub fn fingerprint64(data: &[u8]) -> u64 {
    datastreamdb_algo::sketch::fnv1_64(data)
}

/// Computes the mutation UID for a value edge (Go `fingerprintEdge`).
#[must_use]
pub fn fingerprint_edge(edge: &DirectedEdge, is_list: bool) -> u64 {
    if !edge.lang.is_empty() {
        return fingerprint64(edge.lang.as_bytes());
    }
    if is_list {
        return fingerprint64(&edge.value);
    }
    u64::MAX
}

/// Conflict key for an edge mutation (Go `GetConflictKey`).
#[must_use]
pub fn get_conflict_key(
    pk: &datastreamdb_x::ParsedKey,
    key: &[u8],
    edge: &DirectedEdge,
    schema: &SchemaState,
) -> u64 {
    let get_key = |key: &[u8], uid: u64| fingerprint64(key) ^ uid;
    if schema.has_no_conflict(&edge.attr) {
        0
    } else if schema.has_upsert(&edge.attr) {
        get_key(key, 0)
    } else if pk.is_data() && schema.is_list(&edge.attr) {
        get_key(key, edge.value_id)
    } else if pk.is_data() {
        get_key(key, 0)
    } else if pk.is_index() || pk.is_count_or_count_rev() {
        get_key(key, edge.value_id)
    } else {
        0
    }
}

/// Destination type of an edge.
#[must_use]
pub fn edge_type_id(edge: &DirectedEdge) -> datastreamdb_types::TypeId {
    if edge.value_id != 0 {
        datastreamdb_types::TypeId::Uid
    } else {
        datastreamdb_types::TypeId::try_from(edge.value_type)
            .unwrap_or(datastreamdb_types::TypeId::Default)
    }
}

/// UID-warm states (see `try_start_uid_warm`).
const UID_WARM_IDLE: i32 = 0;
const UID_WARM_RUNNING: i32 = 1;
const UID_WARM_ABANDONED: i32 = 2;

/// Lock-free list state. All methods require the caller to hold the
/// [`List`] lock appropriately (read for readers, write for writers).
#[derive(Clone, Debug)]
pub struct ListInner {
    /// Posting key (owned).
    pub key: Vec<u8>,
    /// Immutable rolled-up layer.
    pub plist: PostingList,
    /// Mutable layer (deltas + current entries).
    pub mutation_map: MutableLayer,
    /// Commit timestamp of the immutable layer; rejects older reads.
    pub min_ts: u64,
    /// Max commit timestamp seen for this list.
    pub max_ts: u64,
    /// One of the `UID_WARM_*` states.
    pub uid_warm_state: Arc<AtomicI32>,
    /// Cached scalar bytes (vector-index fast path).
    pub cache: Option<Vec<u8>>,
}

impl ListInner {
    /// Highest committed version.
    #[must_use]
    pub fn max_version(&self) -> u64 {
        self.max_ts
    }

    /// Short shape description (never values).
    #[must_use]
    pub fn debug_string(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut raw = Vec::with_capacity(self.key.len() * 2);
        for b in &self.key {
            raw.push(HEX[usize::from(b >> 4)]);
            raw.push(HEX[usize::from(b & 15)]);
        }
        let key = String::from_utf8(raw).unwrap_or_default();
        format!(
            "key: {key}, minTs: {}, maxTs: {}, splits: {:?}, mutationMap: {{{}}}",
            self.min_ts,
            self.max_ts,
            self.plist.splits,
            self.mutation_map.print_short()
        )
    }
}

/// In-memory posting list: immutable base plus mutation layer.
#[derive(Clone, Debug)]
pub struct List {
    pub(crate) store: Arc<dyn Backend>,
    pub(crate) inner: Arc<RwLock<ListInner>>,
}

impl List {
    /// Builds a list over an immutable layer with timestamp `min_ts`.
    #[must_use]
    pub fn new(key: Vec<u8>, plist: PostingList, min_ts: u64, store: Arc<dyn Backend>) -> Self {
        Self {
            store,
            inner: Arc::new(RwLock::new(ListInner {
                key,
                plist,
                mutation_map: MutableLayer::new(),
                min_ts,
                max_ts: 0,
                uid_warm_state: Arc::new(AtomicI32::new(UID_WARM_IDLE)),
                cache: None,
            })),
        }
    }

    /// Rebuilds a list from parts (disk reads, cache copies).
    #[must_use]
    pub fn from_parts(
        key: Vec<u8>,
        plist: PostingList,
        mutation_map: MutableLayer,
        min_ts: u64,
        max_ts: u64,
        store: Arc<dyn Backend>,
    ) -> Self {
        Self {
            store,
            inner: Arc::new(RwLock::new(ListInner {
                key,
                plist,
                mutation_map,
                min_ts,
                max_ts,
                uid_warm_state: Arc::new(AtomicI32::new(UID_WARM_IDLE)),
                cache: None,
            })),
        }
    }

    /// Storage backend backing this list's split parts.
    #[must_use]
    pub fn store(&self) -> &Arc<dyn Backend> {
        &self.store
    }

    /// Posting key.
    #[must_use]
    pub fn key(&self) -> Vec<u8> {
        self.inner.read().key.clone()
    }

    /// Highest committed version.
    #[must_use]
    pub fn max_version(&self) -> u64 {
        self.inner.read().max_version()
    }

    /// Attaches the mutation layer to a read timestamp.
    pub fn set_ts(&self, read_ts: u64) {
        self.inner.write().mutation_map.set_ts(read_ts);
    }

    /// Adds an edge mutation. Returns the conflict key for the transaction
    /// (0 when the edge carries none). Callers check `should_abort` first.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on key/schema failures.
    pub fn add_mutation(
        &self,
        start_ts: u64,
        edge: &mut DirectedEdge,
        schema: &SchemaState,
    ) -> Result<u64> {
        let mut inner = self.inner.write();
        Self::add_mutation_inner(&mut inner, start_ts, edge, schema)
    }

    /// Adds an edge mutation with the write lock held.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on key/schema failures.
    pub(crate) fn add_mutation_inner(
        inner: &mut ListInner,
        start_ts: u64,
        edge: &mut DirectedEdge,
        schema: &SchemaState,
    ) -> Result<u64> {
        let mpost = {
            let mut mpost = new_posting(edge)?;
            mpost.start_ts = start_ts;
            if mpost.posting_type != PostingType::Ref as i32 {
                edge.value_id = fingerprint_edge(edge, schema.is_list(&edge.attr));
                mpost.uid = edge.value_id;
            }
            mpost
        };

        let pk = datastreamdb_x::parse(&inner.key)?;
        // Mutations resolve the committed schema (Go passes a read context).
        let pred = schema.get(&edge.attr, false);
        let is_single_uid_update = pred.as_ref().is_some_and(|pred| {
            !pred.list
                && pred.value_type == ValType::Uid as i32
                && pk.is_data()
                && mpost.op != OP_DEL
                && mpost.posting_type == PostingType::Ref as i32
        });
        let has_count_index =
            pred.as_ref().is_some_and(|pred| pred.count) && (pk.is_data() || pk.is_reverse());
        Self::update_mutation_layer_inner(inner, mpost, is_single_uid_update, has_count_index);
        inner.cache = None;
        Ok(get_conflict_key(&pk, &inner.key, edge, schema))
    }

    /// Merges one posting into the mutation layer (write lock held).
    fn update_mutation_layer_inner(
        inner: &mut ListInner,
        mpost: Posting,
        single_uid_update: bool,
        has_count_index: bool,
    ) {
        assert!(
            mpost.op == OP_SET || mpost.op == OP_DEL || mpost.op == OP_OVR,
            "bad op"
        );
        if has_delete_all(&mpost) {
            let plist = PostingList {
                postings: vec![mpost.clone()],
                ..PostingList::default()
            };
            inner
                .mutation_map
                .set_current_entries(mpost.start_ts, plist);
            return;
        }
        if inner.mutation_map.current_entries().is_none() {
            inner
                .mutation_map
                .set_current_entries(mpost.start_ts, PostingList::default());
        }
        // Clear any warmed UID slice: the list just changed.
        inner.mutation_map.clear_warm();

        if single_uid_update {
            let mut new_plist = PostingList::default();
            if mpost.op != OP_DEL {
                new_plist.postings.push(create_delete_all_posting());
            }
            let start_ts = mpost.start_ts;
            new_plist.postings.push(mpost);
            inner.mutation_map.set_current_entries(start_ts, new_plist);
            return;
        }
        inner.mutation_map.insert_posting(mpost, has_count_index);
    }

    /// Applies a commit to a cached list. Delta from Go: no `refresh` flag
    /// (our clones own their maps, so no shared-map rebuild is needed).
    pub fn set_mutation_after_commit(&self, start_ts: u64, commit_ts: u64, pl: PostingList) {
        let _ = start_ts;
        let mut inner = self.inner.write();
        inner.mutation_map.commit_entries(commit_ts, pl);
        inner
            .uid_warm_state
            .compare_exchange(
                UID_WARM_ABANDONED,
                UID_WARM_IDLE,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .ok();
        inner.max_ts = inner.max_ts.max(commit_ts);
    }

    /// Installs marshaled delta bytes as the current entries.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] when the bytes do not decode.
    pub fn set_mutation(&self, start_ts: u64, data: &[u8]) -> Result<()> {
        let pl = PostingList::decode(data)?;
        let mut inner = self.inner.write();
        let commit_ts = pl.commit_ts;
        inner.mutation_map.set_current_entries(start_ts, pl);
        if commit_ts != 0 {
            inner.max_ts = inner.max_ts.max(commit_ts);
        }
        Ok(())
    }

    /// Returns the committed/current entries for `start_ts`, if present.
    #[must_use]
    pub fn get_posting(&self, start_ts: u64) -> Option<PostingList> {
        self.inner.read().mutation_map.get(start_ts).cloned()
    }

    /// Marshals this transaction's mutation for the delta cache.
    ///
    /// # Panics
    ///
    /// Never in practice (encoding into a `Vec` is infallible).
    #[must_use]
    pub fn get_mutation(&self, start_ts: u64) -> Vec<u8> {
        let inner = self.inner.read();
        inner
            .mutation_map
            .get(start_ts)
            .map(|pl| {
                let mut buf = Vec::with_capacity(pl.encoded_len());
                pl.encode(&mut buf).expect("prost encode");
                buf
            })
            .unwrap_or_default()
    }

    /// Split start UIDs (empty for single-part lists).
    #[must_use]
    pub fn part_splits(&self) -> Vec<u64> {
        self.inner.read().plist.splits.clone()
    }

    /// Short shape description (never values).
    #[must_use]
    pub fn debug_string(&self) -> String {
        self.inner.read().debug_string()
    }

    /// Reads one split part from storage at `min_ts`.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on key, storage, or decode failures.
    pub fn read_list_part(&self, start_uid: u64) -> Result<PostingList> {
        let (key, min_ts, store) = {
            let inner = self.inner.read();
            (inner.key.clone(), inner.min_ts, self.store.clone())
        };
        crate::mem::read_list_part_at(store.as_ref(), &key, start_uid, min_ts)
    }

    /// Elects a single UID warmer for this list.
    pub(crate) fn try_start_uid_warm(&self) -> bool {
        self.inner
            .read()
            .uid_warm_state
            .compare_exchange(
                UID_WARM_IDLE,
                UID_WARM_RUNNING,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
    }

    /// Releases a won warm election.
    pub(crate) fn finish_uid_warm(&self) {
        self.inner
            .read()
            .uid_warm_state
            .compare_exchange(
                UID_WARM_RUNNING,
                UID_WARM_IDLE,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .ok();
    }

    /// Gives up warming this list until it next changes.
    pub(crate) fn abandon_uid_warm(&self) {
        self.inner
            .read()
            .uid_warm_state
            .store(UID_WARM_ABANDONED, Ordering::SeqCst);
    }

    /// Whether the warmed UID slice can serve `read_ts`.
    #[must_use]
    pub fn can_use_calculated_uids(&self, read_ts: u64) -> bool {
        let inner = self.inner.read();
        let (uids, calculated) = inner.mutation_map.calculated_uids();
        let _ = uids;
        if !calculated || inner.mutation_map.current_entries().is_some() {
            return false;
        }
        inner.mutation_map.committed_uids_time() == u64::MAX
            || read_ts >= inner.mutation_map.committed_uids_time()
    }

    /// Deep snapshot for cache handoff (fresh warm state, like Go's copy).
    #[must_use]
    pub(crate) fn snapshot(&self) -> List {
        let inner = self.inner.read();
        List {
            store: self.store.clone(),
            inner: Arc::new(RwLock::new(ListInner {
                key: inner.key.clone(),
                plist: inner.plist.clone(),
                mutation_map: inner.mutation_map.clone(),
                min_ts: inner.min_ts,
                max_ts: inner.max_ts,
                uid_warm_state: Arc::new(AtomicI32::new(UID_WARM_IDLE)),
                cache: inner.cache.clone(),
            })),
        }
    }

    /// Materializes the UID slice onto this list's own layer.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage failure.
    pub(crate) fn calculate_uids(&self) -> Result<bool> {
        // Read-guarded walk plus a short write for the install (Go holds a
        // write lock across the walk; the result is identical either way).
        let read_ts = {
            let inner = self.inner.read();
            if !inner.mutation_map.needs_uid_warm() {
                return Ok(false);
            }
            inner.mutation_map.committed_uids_time()
        };
        let mut res = Vec::new();
        {
            let inner = self.inner.read();
            crate::iter::iterate(&inner, self.store.as_ref(), read_ts, 0, &mut |p| {
                if p.posting_type == PostingType::Ref as i32 {
                    res.push(p.uid);
                }
                Ok(())
            })?;
        }
        self.inner.write().mutation_map.set_calculated_uids(res);
        Ok(true)
    }

    /// Whether warming the UID slice would be useful.
    #[must_use]
    pub(crate) fn needs_uid_warm(&self) -> bool {
        self.inner.read().mutation_map.needs_uid_warm()
    }

    /// Installs a warmed UID slice when the layer has not moved on.
    pub(crate) fn publish_calculated_uids(&self, committed_uids_time: u64, uids: Vec<u64>) -> bool {
        let mut inner = self.inner.write();
        if !inner.mutation_map.needs_uid_warm()
            || inner.mutation_map.committed_uids_time() != committed_uids_time
        {
            return false;
        }
        inner.mutation_map.set_calculated_uids(uids);
        true
    }

    /// Whether two handles share the same inner list.
    #[must_use]
    pub(crate) fn same_inner(&self, other: &List) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    /// Warmed UID slice plus its timestamp.
    #[must_use]
    pub(crate) fn warmed_uids(&self) -> (u64, Vec<u64>) {
        let inner = self.inner.read();
        let (uids, _) = inner.mutation_map.calculated_uids();
        (inner.mutation_map.committed_uids_time(), uids.to_vec())
    }

    /// Converts a backup-format list into a normal posting list.
    #[must_use]
    pub fn from_backup_posting_list(bl: &BackupPostingList) -> PostingList {
        const BLOCK_SIZE: usize = 256;
        let pack = if !bl.uids.is_empty() {
            Some(datastreamdb_codec::pack::encode(&bl.uids, BLOCK_SIZE))
        } else if !bl.uid_bytes.is_empty() {
            Some(datastreamdb_codec::pack::encode_from_buffer(
                &bl.uid_bytes,
                BLOCK_SIZE,
            ))
        } else {
            None
        };
        PostingList {
            pack,
            postings: bl.postings.clone(),
            commit_ts: bl.commit_ts,
            splits: bl.splits.clone(),
        }
    }
}
