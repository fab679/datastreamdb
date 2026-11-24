//! Predicate statistics for query planning. Port of `posting/stats.go`:
//! per-predicate count-min sketches estimating `eq` fan-out.

#![warn(missing_docs)]

use std::collections::HashMap;

use datastreamdb_algo::sketch::CountMinSketch;
use parking_lot::RwLock;

/// Estimated-count container for one predicate.
#[derive(Debug)]
pub struct EqContainer {
    inner: RwLock<CountMinSketch>,
}

impl EqContainer {
    /// Container with Go's sketch parameters (0.001, 0.99).
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(CountMinSketch::new(0.001, 0.99)),
        }
    }

    /// Estimated count for `key`.
    #[must_use]
    pub fn estimate(&self, key: &[u8]) -> u64 {
        self.inner.read().count(key)
    }

    /// Records `count` occurrences of `key`.
    pub fn insert_record(&self, key: &[u8], count: u64) {
        self.inner.write().add_int(key, count);
    }
}

impl Default for EqContainer {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-predicate statistic containers.
#[derive(Debug, Default)]
pub struct StatsHolder {
    pred_stats: RwLock<HashMap<String, EqContainer>>,
}

impl StatsHolder {
    /// Empty holder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records `count` occurrences of `key` under `pred` (no-op when the
    /// predicate has no container yet).
    pub fn insert_record(&self, pred: &str, key: &[u8], count: u64) {
        if let Some(container) = self.pred_stats.read().get(pred) {
            container.insert_record(key, count);
        }
    }

    /// Estimated `eq` fan-out, creating the predicate container on first use
    /// (which reports "everything", like Go).
    #[must_use]
    pub fn process_eq_predicate(&self, pred: &str, key: &[u8]) -> u64 {
        if let Some(container) = self.pred_stats.read().get(pred) {
            return container.estimate(key);
        }
        self.pred_stats
            .write()
            .insert(pred.to_owned(), EqContainer::new());
        u64::MAX
    }
}
