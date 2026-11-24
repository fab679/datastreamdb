//! DQL (GraphQL+-) lexer, parser, and AST.
//!
//! Rust port of the upstream Go `dql` package: [`parse`] turns query text into a
//! [`GraphQuery`] tree; [`parse_mutation`] and [`parse_dql`] handle mutation
//! and upsert blocks. The parser is an exact port of Go's `lex`-driven
//! recursive descent, so error positions and messages match.
//!
//! AST nodes own their children (`Vec`, `Option`, `Box`) where Go uses
//! pointers; traversal order and expansion semantics are unchanged.

#![warn(missing_docs)]

mod args;
mod ast;
mod directives;
mod facets;
mod filter;
mod funcs;
mod math;
mod mutation;
mod parse;
mod root;
mod state;
mod vars;

pub use ast::{
    ANY_VAR, Arg, FacetOrder, FilterTree, Function, GraphQuery, GroupByAttr, LIST_VAR, ParseResult,
    RecurseArgs, Request, ShortestPathArgs, UID_VAR, VALUE_VAR, VarContext, Vars, is_aggregator,
    is_expand_func, is_geo_func, is_inequality_fn, is_math_block,
};
pub use math::MathTree;
pub use mutation::{
    ApiMutation, ApiRequest, Mutation, NQuad, parse_dql, parse_mutation, parse_uid,
};
pub use parse::{parse, parse_with_need_vars};

/// DQL error with Go's message text.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DqlError {
    /// Anything else, with Go's message text.
    #[error("{0}")]
    Message(String),
}

impl DqlError {
    /// Formats a Go-style message.
    pub fn message(text: impl Into<String>) -> Self {
        Self::Message(text.into())
    }
}

impl From<datastreamdb_lex::LexError> for DqlError {
    fn from(err: datastreamdb_lex::LexError) -> Self {
        Self::Message(err.to_string())
    }
}
