//! Request orchestration: HTTP/gRPC serving, auth, admin.
//!
//! Rust port of the upstream Go `edgraph` package plus the alpha HTTP surface.
//! [`TxnCoordinator`] runs queries, mutations, and alters; [`DataStreamDBService`]
//! serves the `api.DataStreamDB` gRPC API; [`http`] serves the REST surface.

#![warn(missing_docs)]

pub mod admin;
pub mod auth;
pub mod convert;
pub mod http;
pub mod server;
pub mod service;
pub mod tenancy;
pub mod txn;

pub use admin::FrontendAdmin;
pub use server::{AlphaConfig, AlphaServer};
pub use service::DataStreamDBService;
pub use txn::{CoordError, MutateOutput, QueryOutput, TxnCoordinator};
