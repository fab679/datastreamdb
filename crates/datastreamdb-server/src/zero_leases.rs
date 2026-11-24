//! Zero lease manager (`dgraph/cmd/zero/assign.go`).
//!
//! [`ZeroLeases`] hands out UID, timestamp, and namespace ranges from an
//! in-memory window, renewing the window through zero raft proposals when
//! it runs low. Only the zero leader leases; followers reject (callers
//! retry across zeros) instead of Go's leader forwarding.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use datastreamdb_proto::pb::num::LeaseType;
use datastreamdb_proto::pb::{AssignedIds, Num, ZeroProposal};
use datastreamdb_raft::Node;
use prost::Message as _;
use tokio::sync::Mutex;

use crate::zero_state::{ZeroApplier, ZeroState};

/// Standard renewal bandwidth (matches Go's `leaseBandwidth`).
const LEASE_BANDWIDTH: u64 = 10_000;

/// Lease failures.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LeaseError {
    /// Nothing to lease (zero value, or bump below the ceiling).
    #[error("Nothing to be leased")]
    NothingToLease,
    /// This zero does not lead (retry another zero).
    #[error("Assigning IDs is only allowed on leader")]
    NotLeader,
    /// The ID space overflowed.
    #[error("Cannot lease {0:?} as the limit has reached")]
    Overflow(LeaseType),
    /// Unknown lease type discriminant.
    #[error("Unknown lease type: {0}")]
    UnknownType(i32),
    /// Raft proposal failed.
    #[error("lease proposal failed: {0}")]
    Propose(String),
}

/// In-memory lease windows over raft-agreed ceilings.
pub struct ZeroLeases {
    /// Next UID to hand out.
    next_uid: AtomicU64,
    /// Next transaction timestamp to hand out.
    next_txn_ts: AtomicU64,
    /// Next namespace ID to hand out.
    next_ns_id: AtomicU64,
    /// Last read-only timestamp served from memory.
    read_only_ts: AtomicU64,
    /// Serializes whole lease operations (like Go's `leaseLock`).
    lock: Mutex<()>,
    /// Raft term the windows were last resynced for (Go `updateLeases`
    /// runs on leadership; terms detect it without a watcher).
    synced_term: AtomicU64,
}

impl ZeroLeases {
    /// Fresh windows; the first lease of each kind renews from raft state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            next_uid: AtomicU64::new(1),
            next_txn_ts: AtomicU64::new(1),
            next_ns_id: AtomicU64::new(1),
            read_only_ts: AtomicU64::new(0),
            lock: Mutex::new(()),
            synced_term: AtomicU64::new(0),
        }
    }

    /// Ceiling for `typ` from raft state.
    fn max_lease(state: &ZeroState, typ: LeaseType) -> u64 {
        let snapshot = state.snapshot_state();
        match typ {
            LeaseType::Uid => snapshot.max_uid,
            LeaseType::TxnTs => snapshot.max_txn_ts,
            LeaseType::NsId => snapshot.max_ns_id,
        }
    }

    /// Next counter for `typ`.
    fn next(&self, typ: LeaseType) -> &AtomicU64 {
        match typ {
            LeaseType::Uid => &self.next_uid,
            LeaseType::TxnTs => &self.next_txn_ts,
            LeaseType::NsId => &self.next_ns_id,
        }
    }

    /// Leases IDs or timestamps (Go `Server.lease`).
    ///
    /// # Errors
    ///
    /// Returns [`LeaseError`] when not leading, when there is nothing to
    /// lease, on overflow, or when the renewal proposal fails.
    pub async fn lease(
        &self,
        num: &Num,
        state: &Arc<ZeroState>,
        node: &Node<ZeroApplier>,
    ) -> Result<AssignedIds, LeaseError> {
        let typ =
            LeaseType::try_from(num.r#type).map_err(|_| LeaseError::UnknownType(num.r#type))?;
        if !node.is_leader().await {
            return Err(LeaseError::NotLeader);
        }
        // New term, (possibly) new leader: resync windows just past the
        // replicated ceilings so IDs handed out by a previous leader are
        // never reused (Go `updateLeases` on leadership).
        let term = node.current_term();
        if self.synced_term.swap(term, Ordering::SeqCst) != term {
            self.next_uid.store(
                Self::max_lease(state, LeaseType::Uid).saturating_add(1),
                Ordering::SeqCst,
            );
            self.next_txn_ts.store(
                Self::max_lease(state, LeaseType::TxnTs).saturating_add(1),
                Ordering::SeqCst,
            );
            self.next_ns_id.store(
                Self::max_lease(state, LeaseType::NsId).saturating_add(1),
                Ordering::SeqCst,
            );
        }
        if num.val == 0 && !num.read_only {
            return Err(LeaseError::NothingToLease);
        }
        let _guard = self.lock.lock().await;

        // Read-only fast path: serve the last timestamp from memory.
        if typ == LeaseType::TxnTs && num.val == 0 && num.read_only {
            let read_only = self.read_only_ts.load(Ordering::SeqCst);
            let next = self.next_txn_ts.load(Ordering::SeqCst);
            if read_only > 0 && read_only == next.saturating_sub(1) {
                return Ok(AssignedIds {
                    read_only,
                    ..AssignedIds::default()
                });
            }
        }

        let max_lease = Self::max_lease(state, typ);
        let next = self.next(typ).load(Ordering::SeqCst);
        let available = max_lease.saturating_sub(next).saturating_add(1);
        if available < num.val.saturating_add(1) {
            let mut how_many = LEASE_BANDWIDTH;
            if num.val > LEASE_BANDWIDTH {
                how_many = num.val.saturating_add(LEASE_BANDWIDTH);
            }
            let new_max = max_lease
                .checked_add(how_many)
                .ok_or(LeaseError::Overflow(typ))?;
            if how_many < num.val {
                return Err(LeaseError::Overflow(typ));
            }
            let mut proposal = ZeroProposal::default();
            match typ {
                LeaseType::TxnTs => proposal.max_txn_ts = new_max,
                LeaseType::Uid => proposal.max_uid = new_max,
                LeaseType::NsId => proposal.max_ns_id = new_max,
            }
            node.propose(proposal.encode_to_vec())
                .await
                .map_err(|err| LeaseError::Propose(err.to_string()))?;
        }

        let mut out = AssignedIds::default();
        if typ == LeaseType::TxnTs {
            if num.val > 0 {
                let start = self.next_txn_ts.load(Ordering::SeqCst);
                out.start_id = start;
                out.end_id = start.saturating_add(num.val).saturating_sub(1);
                self.next_txn_ts
                    .store(out.end_id.saturating_add(1), Ordering::SeqCst);
            }
            if num.read_only {
                let read_only = self.next_txn_ts.load(Ordering::SeqCst);
                self.read_only_ts.store(read_only, Ordering::SeqCst);
                self.next_txn_ts
                    .store(read_only.saturating_add(1), Ordering::SeqCst);
                out.read_only = read_only;
            }
        } else {
            let start = self.next(typ).load(Ordering::SeqCst);
            out.start_id = start;
            out.end_id = start.saturating_add(num.val).saturating_sub(1);
            self.next(typ)
                .store(out.end_id.saturating_add(1), Ordering::SeqCst);
        }
        Ok(out)
    }

    /// Bumps a ceiling to at least `val`, leasing the difference.
    ///
    /// Used at alpha boot (local high-water marks) and by explicit bump
    /// requests. Returns the leased range, or [`LeaseError::NothingToLease`]
    /// when already past `val`.
    ///
    /// # Errors
    ///
    /// Returns [`LeaseError`] like [`ZeroLeases::lease`].
    pub async fn bump(
        &self,
        typ: LeaseType,
        val: u64,
        state: &Arc<ZeroState>,
        node: &Node<ZeroApplier>,
    ) -> Result<AssignedIds, LeaseError> {
        let cur = self.next(typ).load(Ordering::SeqCst).saturating_sub(1);
        if cur >= val {
            return Err(LeaseError::NothingToLease);
        }
        let num = Num {
            val: val.saturating_sub(cur),
            r#type: typ as i32,
            ..Num::default()
        };
        self.lease(&num, state, node).await
    }
}

impl Default for ZeroLeases {
    fn default() -> Self {
        Self::new()
    }
}
