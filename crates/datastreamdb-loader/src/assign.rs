//! Blank-node to UID assignment for loaders.
//!
//! Go resolves blank nodes through Zero UID leases; the embedded
//! loaders instead seed an [`XidMap`](datastreamdb_xidmap::XidMap) just
//! above the highest UID already stored ([`seed_above`]) and
//! resolve each batch through [`batch_uid_map`].

use std::collections::HashMap;
use std::sync::Arc;

use datastreamdb_posting::PostingEngine;

/// Loader assignment failures.
#[derive(Debug, thiserror::Error)]
pub enum AssignError {
    /// Underlying storage failure.
    #[error("uid scan failed: {0}")]
    Storage(String),
}

/// Highest data/reverse UID stored in `engine` (0 when empty).
///
/// # Errors
///
/// Returns [`AssignError::Storage`] on backend failure.
pub fn max_stored_uid(engine: &PostingEngine) -> Result<u64, AssignError> {
    let pairs = engine
        .store()
        .scan_prefix(datastreamdb_posting::store::PARTITION, b"")
        .map_err(|err| AssignError::Storage(err.to_string()))?;
    let mut max = 0;
    for (key_bytes, _) in &pairs {
        let Ok(key) = datastreamdb_x::keycodec::parse(key_bytes) else {
            continue;
        };
        if key.is_data() || key.is_reverse() {
            max = max.max(key.uid);
        }
    }
    Ok(max)
}

/// Fresh blank-node map seeded above the stored UIDs and `floor`.
///
/// `floor` carries the highest explicit UID in the input files, so
/// blanks never collide with explicit file UIDs (Go only bumps per
/// batch via `allocateUids`; the global pre-scan closes the
/// early-blank/late-explicit hole).
///
/// # Errors
///
/// Returns [`AssignError::Storage`] on backend failure.
pub fn seed_above(
    engine: &PostingEngine,
    floor: u64,
) -> Result<Arc<datastreamdb_xidmap::XidMap>, AssignError> {
    let max = max_stored_uid(engine)?.max(floor);
    Ok(Arc::new(datastreamdb_xidmap::XidMap::with_start(max + 1)))
}

/// Highest explicit (numeric) UID in `quads`.
#[must_use]
pub fn max_quad_uid(quads: &[datastreamdb_chunker::NQuad]) -> u64 {
    let mut max = 0;
    for quad in quads {
        for endpoint in [&quad.subject, &quad.object_id] {
            if let Ok(uid) = datastreamdb_types::strconv::parse_go_uint_base0(endpoint) {
                max = max.max(uid);
            }
        }
    }
    max
}

/// Advances the engine clock past every stored version.
///
/// Fresh engines start at timestamp 0; without this, a second load
/// into the same directory would reuse commit timestamps (Go avoids
/// this through Zero's timestamp leases).
///
/// # Errors
///
/// Returns [`AssignError::Storage`] on backend failure.
pub fn advance_oracle_to_stored(engine: &PostingEngine) -> Result<u64, AssignError> {
    let pairs = engine
        .store()
        .scan_prefix(datastreamdb_posting::store::PARTITION, b"")
        .map_err(|err| AssignError::Storage(err.to_string()))?;
    let mut max = 0;
    for (key_bytes, _) in &pairs {
        let chain = datastreamdb_posting::store::read_chain(engine.store().as_ref(), key_bytes)
            .map_err(|err| AssignError::Storage(err.to_string()))?;
        max = max.max(chain.map_or(0, |chain| chain.max_ts()));
    }
    engine.oracle().advance_to(max);
    Ok(max)
}

/// Resolves every non-numeric subject/object in `quads`, returning the
/// `xid -> UID` map that
/// [`to_edge_using`](datastreamdb_dql::NQuad::to_edge_using) needs.
///
/// Numeric subjects/objects pass through untouched; blank nodes (and
/// IRIs) allocate stable UIDs from `xids` on first sight.
pub fn batch_uid_map(
    xids: &datastreamdb_xidmap::XidMap,
    quads: &[datastreamdb_chunker::NQuad],
) -> HashMap<String, u64> {
    let mut map = HashMap::new();
    for quad in quads {
        for endpoint in [&quad.subject, &quad.object_id] {
            if endpoint.is_empty() || is_numeric_uid(endpoint) {
                continue;
            }
            map.entry(endpoint.clone())
                .or_insert_with(|| xids.get_or_assign(endpoint));
        }
    }
    map
}

/// Whether `text` parses as a Go base-0 unsigned integer.
fn is_numeric_uid(text: &str) -> bool {
    datastreamdb_types::strconv::parse_go_uint_base0(text).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oracle_advances_past_stored_versions() {
        let backend = Arc::new(datastreamdb_store::MemoryBackend::new());
        let schema = Arc::new(datastreamdb_schema::SchemaState::new());
        let engine = PostingEngine::open(backend.clone(), schema, 64);
        let txn = engine.begin();
        let mut edge = datastreamdb_proto::DirectedEdge {
            entity: 2,
            attr: "0-name".to_owned(),
            value: b"Alice".to_vec(),
            value_type: datastreamdb_proto::ValType::String as i32,
            op: datastreamdb_proto::EdgeOp::Set as i32,
            ..datastreamdb_proto::DirectedEdge::default()
        };
        let key = datastreamdb_x::data_key("0-name", 2);
        txn.get(&key)
            .expect("list")
            .add_mutation_with_index(&mut edge, &txn)
            .expect("mutate");
        engine.commit(&txn).expect("commit");
        let committed = engine.oracle().max_assigned();
        assert!(committed > 0);

        let fresh = PostingEngine::open(
            backend,
            Arc::new(datastreamdb_schema::SchemaState::new()),
            64,
        );
        assert_eq!(fresh.oracle().max_assigned(), 0);
        let max = advance_oracle_to_stored(&fresh).expect("advance");
        assert_eq!(max, committed);
        assert_eq!(fresh.oracle().max_assigned(), committed);
    }

    #[test]
    fn uid_map_assigns_blanks_stably() {
        let xids = datastreamdb_xidmap::XidMap::with_start(100);
        let quad = datastreamdb_chunker::NQuad {
            subject: "_:alice".to_owned(),
            predicate: "name".to_owned(),
            object_id: String::new(),
            object_value: Some(datastreamdb_types::ApiValue::Str("Alice".to_owned())),
            lang: String::new(),
            facets: Vec::new(),
            namespace: 0,
        };
        let first = batch_uid_map(&xids, std::slice::from_ref(&quad));
        let second = batch_uid_map(&xids, &[quad]);
        assert_eq!(first["_:alice"], 100);
        assert_eq!(second["_:alice"], 100);
        assert_eq!(xids.len(), 1);
    }

    #[test]
    fn uid_map_skips_numeric_and_empty() {
        let xids = datastreamdb_xidmap::XidMap::new();
        let quad = datastreamdb_chunker::NQuad {
            subject: "0x2".to_owned(),
            predicate: "friend".to_owned(),
            object_id: "0x3".to_owned(),
            object_value: None,
            lang: String::new(),
            facets: Vec::new(),
            namespace: 0,
        };
        assert!(batch_uid_map(&xids, &[quad]).is_empty());
        assert!(xids.is_empty());
    }
}
