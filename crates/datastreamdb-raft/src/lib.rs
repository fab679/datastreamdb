//! Consensus, WAL, membership, and internode transport.
//!
//! Rust port of the upstream Go `raftwal` (durable log) and `conn` (cluster
//! transport) packages. The WAL segment format is byte-compatible with Go;
//! consensus runs on `openraft` with the WAL as its log store.

#![warn(missing_docs)]

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
