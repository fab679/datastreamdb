//! Zero transaction oracle (`dgraph/cmd/zero/oracle.go`).
//!
//! [`ZeroOracle`] is the global conflict arbiter: it records commit/abort
//! decisions (replicated through the zero raft group), detects write-write
//! conflicts across groups via conflict keys, and broadcasts [`OracleDelta`]
//! batches to subscribed alphas.
//!
//! Delta from Go: the commits map is never purged in M5 (production pruning
//! rides snapshots in Go); restarts rebuild it from the log suffix replay
//! while older transactions abort conservatively via the takeover floor.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use datastreamdb_proto::api::TxnContext;
use datastreamdb_proto::pb::{OracleDelta, TxnStatus};
use tokio::sync::broadcast;

/// Subscriber channel capacity (matches Go's buffered oracle stream).
const SUBSCRIBER_CAPACITY: usize = 1000;

/// Transaction decisions: start timestamp to commit (0 = aborted).
#[derive(Debug, Default)]
struct Decisions {
    /// Start timestamp to commit timestamp (0 = aborted).
    commits: HashMap<u64, u64>,
    /// Conflict key to latest committing timestamp.
    key_commit: HashMap<String, u64>,
}

/// Global conflict arbiter + decision broadcaster.
#[derive(Debug)]
pub struct ZeroOracle {
    /// Committed/aborted decisions and conflict keys.
    decisions: Mutex<Decisions>,
    /// Transactions starting below this abort (takeover/restore floor).
    start_floor: AtomicU64,
    /// Decision broadcast to oracle subscribers.
    subscribers: broadcast::Sender<OracleDelta>,
}

impl ZeroOracle {
    /// Empty oracle (takeover floor at zero; callers raise it on restore).
    #[must_use]
    pub fn new() -> Self {
        let (subscribers, _) = broadcast::channel(SUBSCRIBER_CAPACITY);
        Self {
            decisions: Mutex::new(Decisions::default()),
            start_floor: AtomicU64::new(0),
            subscribers,
        }
    }

    /// Whether `txn` conflicts (stale start or a newer key commit).
    #[must_use]
    /// # Panics
    ///
    /// Panics when the zero lock is poisoned.
    pub fn has_conflict(&self, txn: &TxnContext) -> bool {
        if txn.start_ts < self.start_floor.load(Ordering::SeqCst) {
            return true;
        }
        let decisions = self.decisions.lock().expect("oracle lock");
        txn.keys.iter().any(|key| {
            decisions
                .key_commit
                .get(key)
                .is_some_and(|commit| *commit > txn.start_ts)
        })
    }

    /// Records a decision and broadcasts it (raft apply path).
    ///
    /// `max_assigned` rides the delta so alphas advance their watermarks.
    /// Replays re-record idempotently; duplicate broadcasts are harmless
    /// downstream (finalize is idempotent).
    /// # Panics
    ///
    /// Panics when the zero lock is poisoned.
    pub fn record(&self, txn: &TxnContext, max_assigned: u64) {
        let commit_ts = if txn.aborted { 0 } else { txn.commit_ts };
        {
            let mut decisions = self.decisions.lock().expect("oracle lock");
            // First writer wins: replays must not overwrite newer fates.
            if let std::collections::hash_map::Entry::Vacant(slot) =
                decisions.commits.entry(txn.start_ts)
            {
                slot.insert(commit_ts);
                if commit_ts > 0 {
                    for key in &txn.keys {
                        decisions
                            .key_commit
                            .entry(key.clone())
                            .and_modify(|known| *known = (*known).max(commit_ts))
                            .or_insert(commit_ts);
                    }
                }
            }
        }
        let delta = OracleDelta {
            txns: vec![TxnStatus {
                start_ts: txn.start_ts,
                commit_ts,
            }],
            max_assigned,
            ..OracleDelta::default()
        };
        let _ = self.subscribers.send(delta);
    }

    /// Aborts undecided transactions, broadcasting their fates.
    /// # Panics
    ///
    /// Panics when the zero lock is poisoned.
    pub fn try_abort(&self, starts: &[u64], max_assigned: u64) -> OracleDelta {
        let mut txns = Vec::new();
        {
            let mut decisions = self.decisions.lock().expect("oracle lock");
            for start_ts in starts {
                if let std::collections::hash_map::Entry::Vacant(slot) =
                    decisions.commits.entry(*start_ts)
                {
                    slot.insert(0);
                    txns.push(TxnStatus {
                        start_ts: *start_ts,
                        commit_ts: 0,
                    });
                }
            }
        }
        let delta = OracleDelta {
            txns,
            max_assigned,
            ..OracleDelta::default()
        };
        if !delta.txns.is_empty() {
            let _ = self.subscribers.send(delta.clone());
        }
        delta
    }

    /// Current full state for new subscribers (Go's initial oracle push).
    #[must_use]
    /// # Panics
    ///
    /// Panics when the zero lock is poisoned.
    pub fn current_state(&self, max_assigned: u64) -> OracleDelta {
        let decisions = self.decisions.lock().expect("oracle lock");
        OracleDelta {
            txns: decisions
                .commits
                .iter()
                .map(|(start_ts, commit_ts)| TxnStatus {
                    start_ts: *start_ts,
                    commit_ts: *commit_ts,
                })
                .collect(),
            max_assigned,
            ..OracleDelta::default()
        }
    }

    /// Subscribes to decision broadcasts (lagged receivers resubscribe).
    pub fn subscribe(&self) -> broadcast::Receiver<OracleDelta> {
        self.subscribers.subscribe()
    }

    /// Broadcasts a watermark advance with no decisions (Go `storePending`).
    ///
    /// Timestamp leases call this so alphas learn `max_assigned` without
    /// waiting for the next commit; without it, first-write waits always
    /// hit their timeout (alphas sit at zero until a commit delta lands).
    /// Empty deltas are idempotent downstream (watermark-only application).
    pub fn broadcast_max(&self, max_assigned: u64) {
        let delta = OracleDelta {
            max_assigned,
            ..OracleDelta::default()
        };
        let _ = self.subscribers.send(delta);
    }

    /// Highest decided timestamp (connection `MaxPending` hint).
    #[must_use]
    /// # Panics
    ///
    /// Panics when the zero lock is poisoned.
    pub fn max_decided(&self) -> u64 {
        self.decisions
            .lock()
            .expect("oracle lock")
            .commits
            .values()
            .copied()
            .max()
            .unwrap_or(0)
    }

    /// Resets decisions for a snapshot restore (replay rebuilds the suffix).
    /// # Panics
    ///
    /// Panics when the zero lock is poisoned.
    pub fn reset_for_restore(&self, max_assigned: u64) {
        *self.decisions.lock().expect("oracle lock") = Decisions::default();
        self.start_floor.store(max_assigned, Ordering::SeqCst);
    }

    /// Raises the takeover floor (new leaders abort older transactions).
    pub fn set_takeover_floor(&self, floor: u64) {
        self.start_floor.fetch_max(floor, Ordering::SeqCst);
    }
}

impl Default for ZeroOracle {
    fn default() -> Self {
        Self::new()
    }
}

/// Predicate keys carried in `TxnContext.preds` (`{gid}-{predicate}`).
#[must_use]
pub fn pred_group(pred: &str) -> Option<u32> {
    pred.split_once('-')?.0.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn txn(start: u64, keys: &[&str]) -> TxnContext {
        TxnContext {
            start_ts: start,
            keys: keys.iter().map(ToString::to_string).collect(),
            ..TxnContext::default()
        }
    }

    #[test]
    fn conflicts_match_go() {
        let oracle = ZeroOracle::new();
        let mut first = txn(5, &["a", "b"]);
        assert!(!oracle.has_conflict(&first));
        first.commit_ts = 10;
        oracle.record(&first, 10);

        // Overlapping key committed after start: conflict.
        assert!(oracle.has_conflict(&txn(6, &["b"])));
        // Disjoint keys: no conflict.
        assert!(!oracle.has_conflict(&txn(6, &["c"])));
        // Older commit: no conflict.
        assert!(!oracle.has_conflict(&txn(11, &["a"])));
    }

    #[test]
    fn takeover_floor_aborts_stale() {
        let oracle = ZeroOracle::new();
        oracle.set_takeover_floor(100);
        assert!(oracle.has_conflict(&txn(99, &[])));
        assert!(!oracle.has_conflict(&txn(100, &[])));
    }

    #[test]
    fn aborts_broadcast_once() {
        let oracle = ZeroOracle::new();
        let mut rx = oracle.subscribe();
        let delta = oracle.try_abort(&[7, 8], 50);
        assert_eq!(delta.txns.len(), 2);
        assert_eq!(delta.max_assigned, 50);
        let seen = rx.try_recv().expect("broadcast");
        assert_eq!(seen.txns.len(), 2);
        // Second abort of the same transactions is a no-op.
        let delta = oracle.try_abort(&[7], 51);
        assert!(delta.txns.is_empty());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn pred_group_splits_gid() {
        assert_eq!(pred_group("1-0-name"), Some(1));
        assert_eq!(pred_group("12-0-x"), Some(12));
        assert_eq!(pred_group("name"), None);
    }
}
