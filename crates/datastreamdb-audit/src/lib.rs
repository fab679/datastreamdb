//! Audit logging pipeline and filters.
//!
//! Rust port of the upstream Go `audit` package: [`AuditEvent`] records,
//! skip tables for internal traffic, password [`mask`]ing, pluggable
//! [`sink`]s, [`config`] parsing, and the [`Auditor`] entry point.
//!
//! Divergence: Go keeps one global auditor; this crate is per-instance
//! (an [`Auditor`] per server) so embedded hosts can isolate audit
//! streams. Compression and encryption of rotated files arrive with
//! the encryption slice.

#![warn(missing_docs)]

pub mod auditor;
pub mod config;
pub mod event;
pub mod mask;
pub mod sink;

pub use auditor::{Auditor, Credential, GrpcCall, HttpCall, SharedAuditor};
pub use config::{AuditSettings, parse_audit_flag};
pub use event::AuditEvent;
pub use sink::{AuditSink, ChannelSender, ChannelSink, FileSink, FnSink, SharedSink, StdoutSink};
