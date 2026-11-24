//! Encryption at rest helpers (AES-CTR streams, key rotation).
//!
//! Rust port of the upstream Go `enc` package (`enc/util.go`): AES-CTR stream
//! encryption for backups/exports with a random IV prefix, plus key
//! loading and file re-encryption for rotation. Go uses AES-CTR (not
//! GCM) here, so this crate mirrors that exactly for wire compat.
//!
//! - [`keys`]: [`DataKey`](keys::DataKey) loading and validation.
//! - [`stream`]: [`encrypting_writer`](stream::encrypting_writer) and
//!   [`decrypting_reader`](stream::decrypting_reader).
//! - [`rotate`]: [`rotate_file`](rotate::rotate_file) re-encryption.

#![warn(missing_docs)]

pub mod keys;
pub mod rotate;
pub mod stream;
