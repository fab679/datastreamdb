//! Standalone binaries: `alpha`, `zero`, `bulk`, `live`, and the `datastreamdb`
//! supervisor.
//!
//! Rust port of `dgraph/cmd`. Each binary drives the same engines that
//! embedded hosts use; only the front (network vs handle) differs.

#![warn(missing_docs)]

pub mod acl_admin;
pub mod zero_leases;
pub mod zero_oracle;
pub mod zero_raft;
pub mod zero_server;
pub mod zero_service;
pub mod zero_state;

pub use acl_admin::{AclAdmin, AdminError, GuardianCreds, parse_guardian_creds};
pub use zero_server::{ZeroBootError, ZeroConfig, ZeroServer};

/// Crate version shared by all binaries.
#[must_use]
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
