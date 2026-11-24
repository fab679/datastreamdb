//! The parsed N-Quad model (mirrors `api.NQuad`).

#![warn(missing_docs)]

use datastreamdb_tok::facets::Facet;
use datastreamdb_types::ApiValue;

/// One parsed N-Quad (mirrors `api.NQuad`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NQuad {
    /// Subject UID, blank node, `uid(var)`, or `_STAR_ALL`.
    pub subject: String,
    /// Predicate (language tag split into [`NQuad::lang`]).
    pub predicate: String,
    /// Object UID / IRI / blank node (empty for literal objects).
    pub object_id: String,
    /// Literal object value (absent for UID objects).
    pub object_value: Option<ApiValue>,
    /// Language tag (`@en`), if any.
    pub lang: String,
    /// Facets on the edge.
    pub facets: Vec<Facet>,
    /// Namespace the quad belongs to.
    pub namespace: u64,
}
