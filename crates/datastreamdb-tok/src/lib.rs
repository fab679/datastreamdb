//! Tokenizers for scalar and full-text indexing.
//!
//! Rust port of the upstream Go `tok` package: the [`tok::Tokenizer`] registry
//! ([`tok`]), string tokenizers ([`text`]), analysis ([`analyzer`]),
//! stopwords ([`stopwords`]), stemming ([`stemmers`], [`porter`]), language
//! tags ([`lang`]), facets ([`facets`]), vector-index surface ([`index`]),
//! options ([`options`]), and the tantivy search surface ([`text_index`]).
//!
//! Tokens are byte strings: Go index keys carry arbitrary bytes, which Rust
//! `String` cannot hold. Text tokenizers emit UTF-8 bytes.

// Numeric `as` casts mirror Go's conversion semantics exactly (wrapping and
// truncation are specified, not accidental), with ported tests proving each.
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_sign_loss)]
#![warn(missing_docs)]

pub mod analyzer;
pub mod facets;
pub mod index;
pub mod lang;
pub mod lang_data;
pub mod options;
pub mod porter;
pub mod stemmers;
pub mod stopwords;
pub mod text;
pub mod text_index;
pub mod tok;
