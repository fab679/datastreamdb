//! Schema model, parser, and live schema state.
//!
//! Rust port of the upstream Go `schema` package (`parse.go`, `state.go` lexer,
//! in-memory `schema.go` state). [`parse`] parses schema text into
//! [`ParsedSchema`]; [`SchemaState`] holds the live predicate/type maps.
//!
//! Unlike Go's package-global state, [`SchemaState`] is an instance so many
//! isolated schemas can live in one process. Disk sync (`Init`, `Load`,
//! `Delete` with timestamps) waits for the store milestones (see `TODO.md`).
//!
//! # Port deltas
//!
//! - `context.Context` read/write views become a `write: bool` parameter.
//! - `InitialSchema`/`InitialTypes` take `acl_enabled` explicitly instead of
//!   reading Go's global `WorkerConfig`.

#![warn(missing_docs)]

mod lexer;
mod parse;
mod state;

pub use datastreamdb_proto::{Directive, OptionPair, SchemaUpdate, TypeUpdate, VectorIndexSpec};
pub use parse::{ParsedSchema, has_tokenizer_or_vector_spec, parse, parse_with_namespace};
pub use state::{
    SchemaState, check_and_modify_pre_def_predicate, complete_initial_schema,
    complete_initial_types, initial_schema, initial_types, is_pre_def_type_changed,
};

/// Schema error with Go's message text.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SchemaError {
    /// Anything else, with Go's message text.
    #[error("{0}")]
    Message(String),
}

impl SchemaError {
    /// Formats a Go-style message.
    pub fn message(text: impl Into<String>) -> Self {
        Self::Message(text.into())
    }
}

impl From<datastreamdb_lex::LexError> for SchemaError {
    fn from(err: datastreamdb_lex::LexError) -> Self {
        Self::Message(err.to_string())
    }
}
