//! Posting lists: MVCC lists over the storage backend.
//!
//! Rust port of the upstream Go `posting` package. Every engine instance owns a
//! [`engine::PostingEngine`]; nothing here is global, so many
//! isolated instances share one process. See `README.md` / `TODO.md` for the
//! port plan and the documented deltas from Go (version chains instead of
//! Badger versions, synchronous rollup, local oracle).

#![warn(missing_docs)]

pub mod engine;
pub mod errors;
pub mod index;
pub mod iter;
pub mod layer;
pub mod list;
pub mod mem;
pub mod oracle;
pub mod read;
pub mod rebuild;
pub mod rollup;
pub mod size;
pub mod stats;
pub mod store;
pub mod txn;
pub mod writer;

pub use engine::PostingEngine;
pub use errors::{PostingError, Result};
pub use iter::ListOptions;
pub use layer::MutableLayer;
pub use list::List;
pub use mem::MemoryLayer;
pub use oracle::{Oracle, PendingTxn};
pub use read::filter_facets;
pub use rebuild::IndexRebuild;
pub use stats::StatsHolder;
pub use txn::{LocalCache, Txn};
pub use writer::TxnWriter;
