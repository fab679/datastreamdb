//! Consensus, WAL, membership, and internode transport.
//!
//! Rust port of the upstream Go `raftwal` (durable log) and `conn` (cluster
//! transport) packages. The WAL segment format is byte-compatible with Go;
//! consensus runs on `openraft` with the WAL as its log store.

#![warn(missing_docs)]
// openraft's async trait methods are implemented with immediate futures in a
// few places; the lint is newer than the workspace MSRV, so older clippy
// versions must not reject the allow itself.
#![allow(unknown_lints)]
#![allow(clippy::unused_async_trait_impl)]

mod log;
pub mod log_store;
mod meta;
mod mmap;
pub mod network;
pub mod node;
pub mod state_machine;
mod storage;
pub mod types;
mod wal;

pub use log_store::WalLogStore;
pub use meta::MetaInfo;
pub use network::{ChannelFactory, ChannelRegistry, TonicFactory, TransportServer};
pub use node::{Node, NodeError};
pub use state_machine::{Applier, SmStore};
pub use storage::{DiskStorage, StorageError};
pub use types::{DataStreamDBTypeConfig, Proposal};
