//! Local timestamp oracle. Port of `posting/oracle.go` minus Zero:
//! timestamp assignment, pending-transaction tracking, read waiters, and
//! commit/abort application all happen in-process.
//!
//! Conflict detection (which Zero performs in Go) runs locally: every
//! commit records its conflict fingerprints, and a transaction aborts when
//! any of its fingerprints committed after its start timestamp.

#![warn(missing_docs)]

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use datastreamdb_proto::OracleDelta;
use parking_lot::Mutex;

/// How long reads wait for a timestamp before giving up.
pub const WAIT_FOR_TS_TIMEOUT: Duration = Duration::from_secs(10);

/// Shared per-transaction oracle state.
#[derive(Debug)]
pub struct PendingTxn {
    /// Start timestamp.
    pub start_ts: u64,
    /// Whether the transaction must abort.
    pub should_abort: AtomicBool,
    /// Conflict fingerprints gathered by mutations.
    pub conflicts: Mutex<HashSet<u64>>,
    /// Last activity (stale-transaction sweeps).
    pub last_update: Mutex<Instant>,
}

impl PendingTxn {
    /// Fresh pending entry.
    #[must_use]
    pub fn new(start_ts: u64) -> Self {
        Self {
            start_ts,
            should_abort: AtomicBool::new(false),
            conflicts: Mutex::new(HashSet::new()),
            last_update: Mutex::new(Instant::now()),
        }
    }
}

/// Locked oracle state.
#[derive(Debug, Default)]
struct OracleInner {
    /// Start timestamp to pending transaction.
    pending: HashMap<u64, std::sync::Arc<PendingTxn>>,
    /// Read waiters by awaited timestamp.
    waiters: HashMap<u64, Vec<Sender<()>>>,
}

/// In-process timestamp oracle.
#[derive(Debug, Default)]
pub struct Oracle {
    /// Highest timestamp handed out (starts and commits share one counter).
    max_assigned: AtomicU64,
    /// Pending transactions and waiters.
    inner: Mutex<OracleInner>,
    /// Conflict fingerprint to the commit timestamp that wrote it.
    committed: Mutex<HashMap<u64, u64>>,
}

impl Oracle {
    /// Fresh oracle (no timestamps assigned).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Highest timestamp handed out so far.
    #[must_use]
    pub fn max_assigned(&self) -> u64 {
        self.max_assigned.load(Ordering::SeqCst)
    }

    /// Assigns the next timestamp (starts and commits alike).
    pub fn assign_ts(&self) -> u64 {
        self.max_assigned.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Raises the clock to at least `ts` (restore replays old versions).
    pub fn advance_to(&self, ts: u64) {
        let _ = self
            .max_assigned
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |cur| Some(cur.max(ts)));
    }

    /// Registers (or refreshes) a pending transaction.
    #[must_use]
    pub fn register_start_ts(&self, ts: u64) -> std::sync::Arc<PendingTxn> {
        let mut inner = self.inner.lock();
        if let Some(txn) = inner.pending.get(&ts) {
            *txn.last_update.lock() = Instant::now();
            return txn.clone();
        }
        let txn = std::sync::Arc::new(PendingTxn::new(ts));
        inner.pending.insert(ts, txn.clone());
        txn
    }

    /// Looks up a pending transaction.
    #[must_use]
    pub fn get_txn(&self, start_ts: u64) -> Option<std::sync::Arc<PendingTxn>> {
        self.inner.lock().pending.get(&start_ts).cloned()
    }

    /// Minimum pending start timestamp (`u64::MAX` when idle).
    #[must_use]
    pub fn min_pending_start_ts(&self) -> u64 {
        self.inner
            .lock()
            .pending
            .keys()
            .copied()
            .min()
            .unwrap_or(u64::MAX)
    }

    /// Number of pending transactions.
    #[must_use]
    pub fn num_pending(&self) -> usize {
        self.inner.lock().pending.len()
    }

    /// Start timestamps idle longer than `age`.
    #[must_use]
    pub fn txn_older_than(&self, age: Duration) -> Vec<u64> {
        let inner = self.inner.lock();
        inner
            .pending
            .iter()
            .filter(|(_, txn)| txn.last_update.lock().elapsed() > age)
            .map(|(start_ts, _)| *start_ts)
            .collect()
    }

    /// Drops every pending transaction (tests).
    pub fn reset_txns(&self) {
        self.inner.lock().pending.clear();
    }

    /// Blocks until `start_ts` is assigned (usually immediate).
    pub fn wait_for_ts(&self, start_ts: u64) {
        if start_ts <= self.max_assigned() {
            return;
        }
        let rx = self.add_waiter(start_ts);
        if let Some(rx) = rx {
            let _ = rx.recv_timeout(WAIT_FOR_TS_TIMEOUT);
        }
    }

    /// Registers a waiter unless the timestamp already passed.
    fn add_waiter(&self, start_ts: u64) -> Option<Receiver<()>> {
        if start_ts <= self.max_assigned() {
            return None;
        }
        let mut inner = self.inner.lock();
        if start_ts <= self.max_assigned() {
            return None;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        inner.waiters.entry(start_ts).or_default().push(tx);
        Some(rx)
    }

    /// Applies one commit/abort decision: drops the pending entry and wakes
    /// every waiter at or below the new maximum.
    pub fn apply_decision(&self, start_ts: u64, commit_ts: u64) {
        let mut inner = self.inner.lock();
        inner.pending.remove(&start_ts);
        let current_max = self.max_assigned();
        let new_max = current_max.max(commit_ts).max(start_ts);
        let mut due: Vec<u64> = inner
            .waiters
            .keys()
            .copied()
            .filter(|ts| *ts <= new_max)
            .collect();
        due.sort_unstable();
        for ts in due {
            if let Some(waiters) = inner.waiters.remove(&ts) {
                for waiter in waiters {
                    let _ = waiter.send(());
                }
            }
        }
        // `fetch_max`: concurrent deltas (raft apply + direct watermarks)
        // must not lose the higher advance to a failed CAS.
        self.max_assigned.fetch_max(new_max, Ordering::SeqCst);
    }

    /// Applies an oracle delta (commit/abort batch plus a new maximum).
    pub fn process_delta(&self, delta: &OracleDelta) {
        for status in &delta.txns {
            self.apply_decision(status.start_ts, status.commit_ts);
        }
        let current_max = self.max_assigned();
        if delta.max_assigned > current_max {
            let mut inner = self.inner.lock();
            let mut due: Vec<u64> = inner
                .waiters
                .keys()
                .copied()
                .filter(|ts| *ts <= delta.max_assigned)
                .collect();
            due.sort_unstable();
            for ts in due {
                if let Some(waiters) = inner.waiters.remove(&ts) {
                    for waiter in waiters {
                        let _ = waiter.send(());
                    }
                }
            }
            // `fetch_max`: concurrent deltas (raft apply + direct watermarks)
            // must not lose the higher advance to a failed CAS.
            self.max_assigned
                .fetch_max(delta.max_assigned, Ordering::SeqCst);
        }
    }

    /// Whether any conflict fingerprint committed after `start_ts`.
    #[must_use]
    pub fn has_conflict(&self, start_ts: u64, conflicts: &HashSet<u64>) -> bool {
        let committed = self.committed.lock();
        conflicts
            .iter()
            .any(|fp| committed.get(fp).is_some_and(|ts| *ts > start_ts))
    }

    /// Records a commit's conflict fingerprints.
    pub fn record_commit(&self, commit_ts: u64, conflicts: &HashSet<u64>) {
        let mut committed = self.committed.lock();
        for fp in conflicts {
            committed.insert(*fp, commit_ts);
        }
    }

    /// Exports conflict fingerprints for snapshots (distribution layer).
    #[must_use]
    pub fn export_committed(&self) -> HashMap<u64, u64> {
        self.committed.lock().clone()
    }

    /// Imports conflict fingerprints from a snapshot (distribution layer).
    ///
    /// Entries already present keep the newer commit timestamp.
    pub fn import_committed(&self, committed: &HashMap<u64, u64>) {
        let mut current = self.committed.lock();
        for (fp, ts) in committed {
            current
                .entry(*fp)
                .and_modify(|old| *old = (*old).max(*ts))
                .or_insert(*ts);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assign_and_wait() {
        let oracle = Oracle::new();
        assert_eq!(oracle.max_assigned(), 0);
        assert_eq!(oracle.assign_ts(), 1);
        assert_eq!(oracle.assign_ts(), 2);
        oracle.wait_for_ts(2);
        assert_eq!(oracle.min_pending_start_ts(), u64::MAX);
    }

    #[test]
    fn waiter_wakes_on_decision() {
        let oracle = Oracle::new();
        let pending = oracle.register_start_ts(1);
        assert_eq!(oracle.num_pending(), 1);
        assert!(oracle.get_txn(1).is_some());
        let _ = &pending;
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(50));
                oracle.apply_decision(1, 3);
            });
            oracle.wait_for_ts(3);
        });
        assert_eq!(oracle.max_assigned(), 3);
        assert_eq!(oracle.num_pending(), 0);
    }

    #[test]
    fn conflicts_detected() {
        let oracle = Oracle::new();
        let mut mine = HashSet::new();
        mine.insert(7);
        assert!(!oracle.has_conflict(1, &mine));
        oracle.record_commit(2, &mine);
        assert!(oracle.has_conflict(1, &mine));
        assert!(!oracle.has_conflict(2, &mine));
        assert!(!oracle.has_conflict(3, &mine));
    }

    #[test]
    fn stale_detection_and_reset() {
        let oracle = Oracle::new();
        let _ = oracle.register_start_ts(1);
        assert!(oracle.txn_older_than(Duration::from_millis(0)).contains(&1));
        assert!(oracle.txn_older_than(Duration::from_secs(3600)).is_empty());
        oracle.reset_txns();
        assert_eq!(oracle.num_pending(), 0);
    }
}
