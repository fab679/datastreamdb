//! Durable chain writer. Port of `TxnWriter` (`posting/writer.go`):
//! version-chain appends and rollup application.
//!
//! Delta from Go: writes apply synchronously (no commit callbacks), and
//! callers hold the engine commit lock instead of relying on Badger
//! timestamps for isolation.

#![warn(missing_docs)]

use std::sync::Arc;

use datastreamdb_proto::PostingList;
use datastreamdb_store::Backend;
use prost::Message as _;

use crate::errors::Result;
use crate::store::{append_deltas, write_base};

/// Writes version chains. All methods require the engine commit lock.
#[derive(Clone, Debug)]
pub struct TxnWriter {
    /// Target backend.
    store: Arc<dyn Backend>,
}

impl TxnWriter {
    /// Writer over `store`.
    #[must_use]
    pub fn new(store: Arc<dyn Backend>) -> Self {
        Self { store }
    }

    /// Appends one marshaled delta at `commit_ts` (replay-safe).
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`](crate::PostingError) on backend or decode
    /// failure.
    pub fn append_delta(&self, key: &[u8], commit_ts: u64, data: &[u8]) -> Result<()> {
        if commit_ts == 0 || data.is_empty() {
            return Ok(());
        }
        let pl = PostingList::decode(data)?;
        append_deltas(self.store.as_ref(), key, &[(commit_ts, pl)])
    }

    /// Writes a rolled-up base, keeping newer deltas.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`](crate::PostingError) on backend failure.
    pub fn write_base(&self, key: &[u8], base_ts: u64, plist: Option<PostingList>) -> Result<()> {
        write_base(self.store.as_ref(), key, base_ts, plist)
    }
}
