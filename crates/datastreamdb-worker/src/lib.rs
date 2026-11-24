//! Tablet serving: predicates, mutations, backup, CDC.
//!
//! Rust port of the upstream Go `worker` package: group membership/tablets,
//! proposal application onto the embedded engine, the alpha→zero client,
//! and the `Worker` gRPC service.

#![warn(missing_docs)]

pub mod acl_cache;
pub mod applier;
pub mod backup;
pub mod cdc;
pub mod config;
pub mod groups;
pub mod predicate_move;
pub mod raft_service;
pub mod service;
pub mod zero;

pub use acl_cache::{ALL_PREDS, AclCache};
pub use applier::{AlphaApplier, ApplyError, conflict_key_string, parse_outcome, txn_pred_keys};
pub use config::{MutationsMode, Options, OptionsError};
pub use groups::GroupState;
pub use raft_service::RaftService;
pub use service::{WorkerService, schema_over_network};
pub use zero::ZeroConn;
