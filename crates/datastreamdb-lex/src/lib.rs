//! Low-level lexing primitives: a state-function lexer plus IRI scanning.
//!
//! Rust port of the upstream Go `lex` package (`lexer.go`, `iri.go`). Downstream
//! crates (`datastreamdb-chunker`, `datastreamdb-schema`, the query parsers) define their
//! own [`ItemType`]s and [`StateFn`] state functions on top of [`Lexer`].
//!
//! See `README.md` / `TODO.md` for the port plan and recorded deltas.

#![warn(missing_docs)]

mod iri;
mod lexer;

pub use iri::{has_uchars, has_xchars, iri_ref};
pub use lexer::{
    CheckRune, CheckRuneRec, EOF, ITEM_EOF, ITEM_ERROR, Item, ItemIterator, ItemType, Lexer,
    StateFn, is_end_of_line,
};
use thiserror::Error;

/// Lexing error: a preformatted message (Go returns plain `error`s here).
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{0}")]
pub struct LexError(pub String);

impl From<&str> for LexError {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl From<String> for LexError {
    fn from(value: String) -> Self {
        Self(value)
    }
}
