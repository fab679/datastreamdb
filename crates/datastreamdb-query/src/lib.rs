//! Embedded DQL query executor and mutation applier.
//!
//! Rust port of the upstream Go `query` package (executor half): the execution
//! graph, variable filling, aggregations, group-by, recurse, shortest path,
//! local sort, JSON/RDF output, and mutation application.
//! See `README.md` / `TODO.md` for the port plan.

#![warn(missing_docs)]

pub mod aggregator;
pub mod engine;
pub mod errors;
pub mod groupby;
pub mod math;
pub mod mutation;
pub mod output;
pub mod output_render;
pub mod process;
pub mod rdf;
pub mod recurse;
pub mod request;
pub mod shortest;
pub mod sort;
pub mod subgraph;
pub mod var_levels;
pub mod vars;

pub use engine::QueryEngine;
pub use errors::{QueryError, Result};
pub use process::{ExecCtx, process_graph};
pub use subgraph::{Params, SubGraph};
