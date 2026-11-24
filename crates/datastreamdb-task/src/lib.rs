//! Local query task processor (single-group worker/task port).
//!
//! Rust port of the local half of the upstream Go `worker/task.go`: task parsing,
//! UID/value posting handlers, scalar/regex/match/compare/geo/string/has
//! filters, and facet trees. Every task runs against one [`PostingEngine`](datastreamdb_posting::PostingEngine);
//! network, tablet, and checksum paths do not exist here.
//! See `README.md` / `TODO.md` for the port plan.

#![warn(missing_docs)]

pub mod context;
pub mod errors;
pub mod filter;
pub mod geo_string;
pub mod has;
pub mod parse;
pub mod process;
pub mod regex_match;
pub mod scalar;
pub mod uid;
pub mod value;

pub use context::{
    FuncType, FunctionContext, QueryState, needs_index, needs_intersect, parse_func_type,
};
pub use errors::{Result, TaskError};
pub use filter::{FacetsFunc, FacetsTree, apply_facets_tree, preprocess_filter};
pub use parse::{TYPE_FILTER_UID_LIMIT, parse_src_fn};
pub use process::process_task;
