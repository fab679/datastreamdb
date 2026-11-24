//! Mutation application. Port of `query/mutation.go` plus the edge
//! validation from `worker/mutation.go` (`ValidateAndConvert`).
//!
//! Deltas from Go: UIDs allocate from a store-backed local counter (no
//! Zero), and the ACL delete check is skipped (no ACL in M3).

#![warn(missing_docs)]

use std::collections::HashMap;
use std::hash::BuildHasher;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use datastreamdb_posting::PostingEngine;
use datastreamdb_proto::{DirectedEdge, EdgeOp};
use datastreamdb_types::{TypeId, Val, Value, convert, marshal};

use crate::errors::{QueryError, Result};

/// Metadata partition for the UID counter.
pub const META_PARTITION: &str = "query-meta";
/// Key of the highest allocated UID.
pub const MAX_UID_KEY: &[u8] = b"max-uid";

/// Store-backed UID allocator (replaces Zero leases).
#[derive(Debug)]
pub struct UidAllocator {
    /// Storage backend.
    store: Arc<dyn datastreamdb_store::Backend>,
    /// Next UID to hand out.
    next: AtomicU64,
    /// Zero-leased `[start, end]` range (cluster mode; local counter else).
    leased: Mutex<Option<(u64, u64)>>,
}

impl UidAllocator {
    /// Opens the allocator, loading (or scanning for) the high-water mark.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] on storage failure.
    pub fn open(store: Arc<dyn datastreamdb_store::Backend>) -> Result<Self> {
        if let Some(bytes) = store
            .get(META_PARTITION, MAX_UID_KEY)
            .map_err(datastreamdb_posting::PostingError::from)?
        {
            if bytes.len() >= 8 {
                let mut raw = [0u8; 8];
                raw.copy_from_slice(&bytes[..8]);
                let next = u64::from_be_bytes(raw).saturating_add(1).max(2);
                return Ok(Self {
                    store,
                    next: AtomicU64::new(next),
                    leased: Mutex::new(None),
                });
            }
        }
        // No marker yet: scan data keys once for the highest UID.
        let mut max = 1u64;
        for prefix in [0u64] {
            let keys = store
                .scan_prefix(
                    datastreamdb_posting::store::PARTITION,
                    &datastreamdb_x::data_prefix(prefix),
                )
                .map_err(datastreamdb_posting::PostingError::from)?;
            for (key, _) in &keys {
                if let Ok(parsed) = datastreamdb_x::parse(key) {
                    max = max.max(parsed.uid);
                }
            }
        }
        let alloc = Self {
            store,
            next: AtomicU64::new(max.saturating_add(1).max(2)),
            leased: Mutex::new(None),
        };
        alloc.persist()?;
        Ok(alloc)
    }

    /// Allocates one UID.
    ///
    /// Prefers the zero-leased range when one is installed (cluster mode);
    /// otherwise mints from the local counter (embedded mode).
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] on storage failure, UID exhaustion, or an
    /// exhausted lease (install a fresh one and retry).
    ///
    /// # Panics
    ///
    /// Panics when the lease lock is poisoned.
    pub fn alloc(&self) -> Result<u64> {
        let mut leased = self.leased.lock().expect("lease lock");
        if let Some((start, end)) = *leased {
            if start > end {
                return Err(QueryError::Invalid("UID lease exhausted".to_owned()));
            }
            *leased = Some((start + 1, end));
            return Ok(start);
        }
        drop(leased);
        let uid = self.next.fetch_add(1, Ordering::SeqCst);
        if uid == u64::MAX {
            return Err(QueryError::Invalid("UID space exhausted".to_owned()));
        }
        self.persist()?;
        Ok(uid)
    }

    /// Installs a zero-leased UID range (cluster mode).
    ///
    /// # Panics
    ///
    /// Panics when the lease lock is poisoned.
    pub fn install_lease(&self, start: u64, end: u64) {
        *self.leased.lock().expect("lease lock") = Some((start, end));
    }

    /// Leased UIDs remaining (`u64::MAX` when unleashed/local).
    ///
    /// # Panics
    ///
    /// Panics when the lease lock is poisoned.
    #[must_use]
    pub fn leased_remaining(&self) -> u64 {
        self.leased
            .lock()
            .expect("lease lock")
            .map_or(u64::MAX, |(start, end)| {
                end.saturating_sub(start).saturating_add(1)
            })
    }

    /// Next UID without allocating (tests).
    #[must_use]
    pub fn next(&self) -> u64 {
        self.next.load(Ordering::SeqCst)
    }

    /// Persists the high-water mark.
    fn persist(&self) -> Result<()> {
        let next = self.next.load(Ordering::SeqCst);
        self.store
            .put(
                META_PARTITION,
                MAX_UID_KEY,
                &next.saturating_sub(1).to_be_bytes(),
            )
            .map_err(datastreamdb_posting::PostingError::from)?;
        Ok(())
    }
}

/// Validated edges keyed by namespaced predicate, plus blank-node assignments.
pub type DirectedEdgeMap = (HashMap<String, Vec<DirectedEdge>>, HashMap<String, u64>);

/// Converts chunker N-Quads into validated edges, assigning blank nodes.
/// Returns the edges plus the blank-node assignments.
///
/// # Errors
///
/// Returns [`QueryError`] on parse, UID, schema, or conversion failure.
pub fn to_directed_edges(
    engine: &Arc<PostingEngine>,
    allocator: &UidAllocator,
    namespace: u64,
    nquads: &[datastreamdb_chunker::NQuad],
    op: EdgeOp,
) -> Result<DirectedEdgeMap> {
    let mut assigned = HashMap::new();
    let edges = to_directed_edges_shared(engine, allocator, namespace, nquads, op, &mut assigned)?;
    Ok((edges, assigned))
}

/// Converts chunker N-Quads into validated edges with a shared blank map.
///
/// Blank labels allocate on first mention (set quads); del quads resolve
/// against labels allocated by earlier set quads (Go `AssignUids` scope).
/// Returns the edges keyed by namespaced predicate.
///
/// # Errors
///
/// Returns [`QueryError`] on parse, UID, schema, or conversion failure.
pub fn to_directed_edges_shared(
    engine: &Arc<PostingEngine>,
    allocator: &UidAllocator,
    namespace: u64,
    nquads: &[datastreamdb_chunker::NQuad],
    op: EdgeOp,
    assigned: &mut HashMap<String, u64, impl BuildHasher>,
) -> Result<HashMap<String, Vec<DirectedEdge>>> {
    // Pre-scan blank nodes so every mention shares one UID. Only set
    // quads allocate; del quads resolve against set labels (Go errors on
    // del-only blanks via `to_uid`).
    if op == EdgeOp::Set {
        for nquad in nquads {
            for id in [&nquad.subject, &nquad.object_id] {
                if is_blank_node(id) && !assigned.contains_key(id.as_str()) {
                    assigned.insert((*id).clone(), allocator.alloc()?);
                }
            }
        }
    }

    let mut edges: HashMap<String, Vec<DirectedEdge>> = HashMap::new();
    for nquad in nquads {
        let wrapped = datastreamdb_dql::NQuad(nquad.clone());
        let mut edge = if nquad.subject == datastreamdb_x::STAR {
            if op != EdgeOp::Del {
                return Err(QueryError::Invalid(
                    "star subject only allowed in deletions".to_owned(),
                ));
            }
            wrapped.to_delete_pred_edge().map_err(QueryError::from)?
        } else {
            wrapped.to_edge_using(assigned).map_err(QueryError::from)?
        };
        edge.op = op as i32;
        edge.attr = datastreamdb_x::namespace_attr(namespace, &edge.attr);
        validate_and_convert(engine, &mut edge)?;
        edges.entry(edge.attr.clone()).or_default().push(edge);
    }
    Ok(edges)
}

/// Whether the identifier is a blank node.
fn is_blank_node(id: &str) -> bool {
    id.starts_with("_:")
}

/// Applies validated edges through one posting transaction.
///
/// # Errors
///
/// Returns [`QueryError`] on storage or conflict failure.
pub fn apply_mutations<H: BuildHasher>(
    engine: &Arc<PostingEngine>,
    edges: &HashMap<String, Vec<DirectedEdge>, H>,
) -> Result<u64> {
    let txn = engine.begin();
    for edges in edges.values() {
        for edge in edges {
            expand_edges(&txn, edge)?;
        }
    }
    engine.commit(&txn).map_err(QueryError::from)
}

/// Applies one edge plus reverse/count/index maintenance.
fn expand_edges(txn: &datastreamdb_posting::Txn, edge: &DirectedEdge) -> Result<()> {
    stage_edge(txn, edge)
}

/// Stages one validated edge into a posting transaction.
///
/// Distribution layer entry point: the worker applier stages raft-committed
/// edges into explicit-timestamp transactions.
///
/// # Errors
///
/// Returns [`QueryError`] on storage failure.
pub fn stage_edge(txn: &datastreamdb_posting::Txn, edge: &DirectedEdge) -> Result<()> {
    let key = datastreamdb_x::data_key(&edge.attr, edge.entity);
    let list = txn.get(&key)?;
    list.add_mutation_with_index(&mut edge.clone(), txn)?;
    Ok(())
}

/// Validates an edge against the schema and converts its value to the
/// storage encoding (port of `ValidateAndConvert`).
///
/// # Errors
///
/// Returns [`QueryError`] on schema mismatch or conversion failure.
pub fn validate_and_convert(engine: &Arc<PostingEngine>, edge: &mut DirectedEdge) -> Result<()> {
    let schema = engine.schema().clone();
    if is_delete_predicate_edge(edge) {
        return Ok(());
    }
    let storage_type = datastreamdb_posting::list::edge_type_id(edge);
    if storage_type == TypeId::Default && is_star_all(&edge.value) {
        return Ok(());
    }

    if edge.attr.contains("__vector_") {
        return Err(QueryError::Invalid(format!(
            "Not allowed to insert mutations in vector index keys, edge: [{edge:?}]"
        )));
    }

    let mut schema_type = schema
        .get(&edge.attr, false)
        .map_or(TypeId::Default, |pred| {
            TypeId::try_from(pred.value_type).unwrap_or(TypeId::Default)
        });

    if !edge.lang.is_empty() && !schema.has_lang(&edge.attr) {
        return Err(QueryError::Invalid(format!(
            "Attr: [{}] should have @lang directive in schema to mutate edge: [{edge:?}]",
            datastreamdb_x::parse_attr(&edge.attr),
        )));
    }
    if !schema_type.is_scalar() && !storage_type.is_scalar() {
        return Ok(());
    }
    if !schema_type.is_scalar() && storage_type.is_scalar() {
        return Err(QueryError::Invalid(format!(
            "Input for predicate {:?} of type uid is scalar. Edge: {edge:?}",
            datastreamdb_x::parse_attr(&edge.attr),
        )));
    }
    if schema_type.is_scalar() && !storage_type.is_scalar() {
        return Err(QueryError::Invalid(format!(
            "Input for predicate {:?} of type scalar is uid. Edge: {edge:?}",
            datastreamdb_x::parse_attr(&edge.attr),
        )));
    }
    if schema_type == TypeId::VFloat {
        return Err(QueryError::Unsupported(
            "vector mutations (vector milestone)".to_owned(),
        ));
    }
    if storage_type == schema_type && schema_type != TypeId::Default {
        return Ok(());
    }
    if schema_type == TypeId::Default {
        schema_type = storage_type;
    }

    let src = Val {
        tid: storage_type,
        value: Value::Binary(edge.value.clone()),
    };
    let dst = convert(&src, schema_type)
        .map_err(|err| QueryError::Invalid(format!("convert {}: {err}", edge.attr)))?;
    let binary = marshal(&dst, TypeId::Binary)
        .map_err(|err| QueryError::Invalid(format!("marshal {}: {err}", edge.attr)))?;
    let Value::Binary(bytes) = binary.value else {
        return Err(QueryError::Invalid(format!(
            "marshal {} produced non-binary value",
            edge.attr
        )));
    };
    edge.value = bytes;
    edge.value_type = schema_type as i32;
    Ok(())
}

/// Whether the edge deletes a whole predicate (`* P *`).
fn is_delete_predicate_edge(edge: &DirectedEdge) -> bool {
    edge.entity == 0 && is_star_all(&edge.value)
}

/// Whether the value is the star marker.
fn is_star_all(value: &[u8]) -> bool {
    value == datastreamdb_x::STAR.as_bytes()
}
