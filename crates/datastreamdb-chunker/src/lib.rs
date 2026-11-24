//! Bulk ingest: RDF N-Quads and JSON chunkers.
//!
//! Rust port of the upstream Go `chunker` package (`chunk.go`, `rdf_parser.go`,
//! `rdf_state.go`, `json_parser.go`). [`parse_rdf`] parses one N-Quad line,
//! [`parse_json`] parses a JSON mutation buffer, and [`Chunker`] splits input
//! streams into parseable [`Chunk`]s.
//!
//! [`NQuad`] mirrors `api.NQuad` with [`datastreamdb_types::ApiValue`] values and
//! [`datastreamdb_tok::facets::Facet`] facets; wire-compatible `api` prost types
//! land with the serving milestones (see `TODO.md`).
//!
//! # Port deltas
//!
//! - One JSON parser (serde with literal-preserving numbers) replaces both
//!   Go's `encoding/json` path and the simdjson fast path; it implements the
//!   reference semantics every shared test vector pins.
//! - [`NQuadBuffer`] batches are drained with [`NQuadBuffer::take_batches`]
//!   instead of a Go channel.
//! - JSON object iteration is sorted (serde maps), where Go randomizes it;
//!   single-predicate assertions are unaffected.
//! - `FileReader`/`StreamReader` (gzip + encryption) wait for the loader
//!   milestones; chunking works over in-memory bytes.
//! - [`sane`] uses `char::is_alphabetic`/`is_numeric` instead of Go's exact
//!   letter/digit tables (differs only on exotic numeric/letter classes).

#![warn(missing_docs)]

mod chunk;
mod json_parser;
mod model;
mod rdf_parser;
mod rdf_state;

pub use chunk::{Chunk, Chunker, Cursor, InputFormat, data_format, is_json_data, new_chunker};
pub use json_parser::{NQuadBuffer, Op, parse_json};
pub use model::NQuad;
pub use rdf_parser::{parse_rdf, parse_rdfs, sane};

/// Chunker error. Most variants carry Go's formatted message text.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ChunkError {
    /// Harmless input (blank line or comment), like Go's `ErrEmpty`.
    #[error("RDF: harmless error, e.g. comment line")]
    Empty,
    /// Delete without a UID, like Go's `errEmptyUID`.
    #[error("UID must be present and non-zero while deleting edges")]
    EmptyUid,
    /// Anything else, with Go's message text.
    #[error("{0}")]
    Message(String),
}

impl ChunkError {
    /// Formats a Go-style message.
    pub fn message(text: impl Into<String>) -> Self {
        Self::Message(text.into())
    }
}

/// Go `unicode.IsSpace`: tab, LF, VT, FF, CR, space, NEL, NBSP.
pub(crate) fn is_go_space(c: char) -> bool {
    matches!(c, '\u{09}'..='\u{0D}' | '\u{20}' | '\u{85}' | '\u{A0}')
}

/// Splits `name@lang` into predicate + lang in place, like Go's
/// `PredicateLang` assignment (a leading `@` does not split).
pub(crate) fn split_lang(nq: &mut NQuad) {
    if let Some(i) = nq.predicate.rfind('@') {
        if i > 0 {
            nq.lang = nq.predicate.split_off(i + 1);
            nq.predicate.truncate(i);
        }
    }
}
