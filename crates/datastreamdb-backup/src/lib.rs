//! Online backup orchestration and restore.
//!
//! Rust port of the upstream Go backup surface (`worker/backup*.go`):
//! the [`manifest`] JSON contract, the Snappy-framed binary writer
//! ([`backup`]), the single-group restore driver ([`restore`]), and
//! the RDF/JSON/schema [`export`] engine. Multi-group fan-out and S3
//! destinations arrive with the later M6 epics.

#![warn(missing_docs)]

pub mod backup;
pub mod export;
pub mod manifest;
pub mod restore;
