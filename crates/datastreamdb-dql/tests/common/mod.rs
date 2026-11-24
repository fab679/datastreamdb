//! Shared helpers for the `datastreamdb-dql` parser integration tests.
//!
//! Included by each `parser_*.rs` target via `#[path]`; every helper is a
//! direct port of a helper in Go's `parser_test.go`.

#![allow(dead_code, reason = "shared across test targets; each uses a subset")]

use std::collections::HashMap;

use datastreamdb_dql::{GraphQuery, Request, parse};

/// Parses `query` with empty variables, expecting success.
pub fn parse_str(query: &str) -> datastreamdb_dql::ParseResult {
    parse(Request {
        text: query.to_owned(),
        variables: HashMap::new(),
    })
    .expect("parse")
}

/// Parses `query` with empty variables, expecting failure; returns the message.
pub fn parse_err(query: &str) -> String {
    parse(Request {
        text: query.to_owned(),
        variables: HashMap::new(),
    })
    .expect_err("must fail")
    .to_string()
}

/// `(query text, variables, expected uid count)` case for var-array tests.
pub type VarArrayCase<'a> = (&'a str, &'a [(&'a str, &'a str)], usize);

/// Attribute names of a block's children.
pub fn child_attrs(gq: &GraphQuery) -> Vec<&str> {
    gq.children.iter().map(|c| c.attr.as_str()).collect()
}

/// Renders the math expression at `query[block].children[child].children[grandchild]`.
pub fn math_debug(query: &str, block: usize, child: usize, grandchild: usize) -> String {
    parse_str(query).query[block].children[child].children[grandchild]
        .math_exp
        .as_ref()
        .expect("math")
        .debug_string()
}

/// Parses RDF quads (port of Go's test helper; stricter: reuses `parse_rdfs`).
pub fn parse_nquads(bytes: &[u8]) -> Vec<datastreamdb_chunker::NQuad> {
    datastreamdb_chunker::parse_rdfs(bytes).expect("nquads").0
}
