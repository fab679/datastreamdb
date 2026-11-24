//! Zero cluster state and raft applier (`dgraph/cmd/zero/raft.go`).
//!
//! [`ZeroState`] is the replicated [`MembershipState`]: groups, members,
//! tablets, and lease counters. [`ZeroApplier`] applies `ZeroProposal`
//! bytes to it (plus oracle decisions); snapshots are the prost-encoded
//! state itself, so restarts resume exactly.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use datastreamdb_proto::pb::{Group, Member, MembershipState, Tablet, ZeroProposal};
use prost::Message as _;

use crate::zero_oracle::ZeroOracle;

/// Tablet application failures (mirrors Go's sentinel errors).
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TabletError {
    /// Tablet carries group ID zero.
    #[error("tablet group id is zero")]
    ZeroGroup,
    /// Another group already serves the predicate (no force flag).
    #[error("tablet already served elsewhere")]
    AlreadyServed,
}

/// Replicated zero state: the membership state behind a lock.
#[derive(Debug, Default)]
pub struct ZeroState {
    /// Groups, members, tablets, counters, cluster ID.
    inner: RwLock<MembershipState>,
}

impl ZeroState {
    /// Empty state (bootstrapped by the first proposals).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Clones the full membership state.
    #[must_use]
    /// # Panics
    ///
    /// Panics when the zero lock is poisoned.
    pub fn snapshot_state(&self) -> MembershipState {
        self.inner.read().expect("zero lock").clone()
    }

    /// Installs a member (alpha groups) or zero (group ID zero).
    ///
    /// `AmDead` members remove instead (Go `RemoveNode` proposals).
    /// # Panics
    ///
    /// Panics when the zero lock is poisoned.
    pub fn apply_member(&self, member: &Member) {
        if member.am_dead {
            self.remove_member(member.group_id, member.id);
            return;
        }
        let mut state = self.inner.write().expect("zero lock");
        state.counter += 1;
        if member.group_id == 0 {
            state.zeros.insert(member.id, member.clone());
            return;
        }
        state
            .groups
            .entry(member.group_id)
            .or_default()
            .members
            .insert(member.id, member.clone());
    }

    /// Removes a member, recording it in `removed`.
    /// # Panics
    ///
    /// Panics when the zero lock is poisoned.
    pub fn remove_member(&self, group_id: u32, raft_id: u64) -> Option<Member> {
        let mut state = self.inner.write().expect("zero lock");
        let removed = if group_id == 0 {
            state.zeros.remove(&raft_id)
        } else {
            state
                .groups
                .get_mut(&group_id)
                .and_then(|group| group.members.remove(&raft_id))
        };
        if let Some(member) = removed.clone() {
            state.removed.push(member);
        }
        if removed.is_some() {
            state.counter += 1;
        }
        removed
    }

    /// Installs, moves (force), or removes a tablet (Go `handleTablet`).
    ///
    /// # Errors
    ///
    /// Returns [`TabletError`] on zero groups or unforced conflicts.
    ///
    /// # Panics
    ///
    /// Panics when the zero lock is poisoned.
    pub fn apply_tablet(&self, tablet: &Tablet) -> Result<(), TabletError> {
        let mut state = self.inner.write().expect("zero lock");
        if tablet.group_id == 0 {
            return Err(TabletError::ZeroGroup);
        }
        if tablet.remove {
            if let Some(group) = state.groups.get_mut(&tablet.group_id) {
                if group.tablets.remove(&tablet.predicate).is_some() {
                    state.counter += 1;
                }
            }
            return Ok(());
        }
        if let Some(prev) = serving_tablet_locked(&state.groups, &tablet.predicate).cloned() {
            if tablet.force {
                if let Some(original) = state.groups.get_mut(&prev.group_id) {
                    original.tablets.remove(&tablet.predicate);
                }
            } else if prev.group_id != tablet.group_id {
                return Err(TabletError::AlreadyServed);
            }
        }
        let mut stored = tablet.clone();
        stored.force = false;
        state
            .groups
            .entry(tablet.group_id)
            .or_default()
            .tablets
            .insert(stored.predicate.clone(), stored);
        state.counter += 1;
        Ok(())
    }

    /// Drops every tablet of one namespace (Go `deleteNamespace`).
    /// # Panics
    ///
    /// Panics when the zero lock is poisoned.
    pub fn delete_namespace(&self, ns: u64) {
        let mut state = self.inner.write().expect("zero lock");
        let mut dropped = 0;
        for group in state.groups.values_mut() {
            let before = group.tablets.len();
            group
                .tablets
                .retain(|pred, _| datastreamdb_x::parse_namespace(pred) != ns);
            dropped += before - group.tablets.len();
        }
        if dropped > 0 {
            state.counter += 1;
        }
    }

    /// Raises a lease counter (max wins); returns the new maximum.
    /// # Panics
    ///
    /// Panics when the zero lock is poisoned.
    pub fn raise_max_uid(&self, max: u64) -> u64 {
        let mut state = self.inner.write().expect("zero lock");
        state.max_uid = state.max_uid.max(max);
        state.max_uid
    }

    /// Raises the timestamp ceiling (max wins); returns the new maximum.
    /// # Panics
    ///
    /// Panics when the zero lock is poisoned.
    pub fn raise_max_txn_ts(&self, max: u64) -> u64 {
        let mut state = self.inner.write().expect("zero lock");
        state.max_txn_ts = state.max_txn_ts.max(max);
        state.max_txn_ts
    }

    /// Raises the namespace ceiling (max wins); returns the new maximum.
    /// # Panics
    ///
    /// Panics when the zero lock is poisoned.
    pub fn raise_max_ns_id(&self, max: u64) -> u64 {
        let mut state = self.inner.write().expect("zero lock");
        state.max_ns_id = state.max_ns_id.max(max);
        state.max_ns_id
    }

    /// Raises the raft-ID ceiling (max wins); returns the new maximum.
    /// # Panics
    ///
    /// Panics when the zero lock is poisoned.
    pub fn raise_max_raft_id(&self, max: u64) -> u64 {
        let mut state = self.inner.write().expect("zero lock");
        state.max_raft_id = state.max_raft_id.max(max);
        state.max_raft_id
    }

    /// Sets the cluster ID once (first writer wins).
    ///
    /// # Panics
    ///
    /// Panics when the zero lock is poisoned.
    pub fn set_cid(&self, cid: &str) {
        let mut state = self.inner.write().expect("zero lock");
        if state.cid.is_empty() {
            cid.clone_into(&mut state.cid);
            state.counter += 1;
        }
    }

    /// Tablet serving `pred`, if any group claims it.
    #[must_use]
    /// # Panics
    ///
    /// Panics when the zero lock is poisoned.
    pub fn serving_tablet(&self, pred: &str) -> Option<Tablet> {
        let state = self.inner.read().expect("zero lock");
        serving_tablet_locked(&state.groups, pred).cloned()
    }

    /// Encodes the state for snapshots.
    #[must_use]
    /// # Panics
    ///
    /// Panics when the zero lock is poisoned.
    pub fn encode_snapshot(&self) -> Vec<u8> {
        self.inner.read().expect("zero lock").encode_to_vec()
    }

    /// Decodes a snapshot, replacing the state.
    ///
    /// # Errors
    ///
    /// Returns [`prost::DecodeError`] on corrupt bytes.
    /// # Panics
    ///
    /// Panics when the zero lock is poisoned.
    pub fn decode_snapshot(&self, bytes: &[u8]) -> Result<(), prost::DecodeError> {
        let state = MembershipState::decode(bytes)?;
        *self.inner.write().expect("zero lock") = state;
        Ok(())
    }
}

/// Tablet serving `pred` across groups (caller holds the lock).
fn serving_tablet_locked<'a>(groups: &'a HashMap<u32, Group>, pred: &str) -> Option<&'a Tablet> {
    groups.values().find_map(|group| group.tablets.get(pred))
}

/// Zero applier: `ZeroProposal` bytes onto [`ZeroState`] + [`ZeroOracle`].
#[derive(Clone)]
pub struct ZeroApplier {
    /// Replicated cluster state.
    state: Arc<ZeroState>,
    /// Transaction oracle (decisions + subscriber broadcast).
    oracle: Arc<ZeroOracle>,
}

impl ZeroApplier {
    /// Binds state and oracle for one zero group.
    pub fn new(state: Arc<ZeroState>, oracle: Arc<ZeroOracle>) -> Self {
        Self { state, oracle }
    }
}

/// Applier failures: only corruption is fatal.
#[derive(Debug, thiserror::Error)]
pub enum ZeroApplyError {
    /// Proposal bytes do not decode.
    #[error("bad zero proposal: {0}")]
    Decode(String),
    /// Snapshot bytes do not decode.
    #[error("bad zero snapshot: {0}")]
    Snapshot(String),
}

impl datastreamdb_raft::Applier for ZeroApplier {
    type Error = ZeroApplyError;

    async fn apply(&self, data: &[u8]) -> Result<Vec<u8>, Self::Error> {
        let proposal =
            ZeroProposal::decode(data).map_err(|err| ZeroApplyError::Decode(err.to_string()))?;
        // Member joins/updates; the response echoes the stored member so
        // Connect learns the assigned raft ID without a second lookup.
        if let Some(member) = &proposal.member {
            self.state.apply_member(member);
            return Ok(member.encode_to_vec());
        }
        if let Some(tablet) = &proposal.tablet {
            // Already-served races resolve benignly (first writer wins).
            let _ = self.state.apply_tablet(tablet);
        }
        if proposal.max_uid > 0 {
            self.state.raise_max_uid(proposal.max_uid);
        }
        if proposal.max_txn_ts > 0 {
            self.state.raise_max_txn_ts(proposal.max_txn_ts);
        }
        if proposal.max_ns_id > 0 {
            self.state.raise_max_ns_id(proposal.max_ns_id);
        }
        if proposal.max_raft_id > 0 {
            self.state.raise_max_raft_id(proposal.max_raft_id);
        }
        if !proposal.cid.is_empty() {
            self.state.set_cid(&proposal.cid);
        }
        if let Some(delete_ns) = &proposal.delete_ns {
            self.state.delete_namespace(delete_ns.namespace);
        }
        if let Some(txn) = &proposal.txn {
            self.oracle
                .record(txn, self.state.snapshot_state().max_txn_ts);
        }
        if proposal.snapshot.is_some() {
            // Forced snapshots are policy-driven under openraft; the
            // proposal only marks the log position (Go parity).
        }
        Ok(Vec::new())
    }

    async fn snapshot(&self) -> Result<Vec<u8>, Self::Error> {
        Ok(self.state.encode_snapshot())
    }

    async fn restore(&self, snapshot: &[u8]) -> Result<(), Self::Error> {
        self.state
            .decode_snapshot(snapshot)
            .map_err(|err| ZeroApplyError::Snapshot(err.to_string()))?;
        // Decisions rebuild from the log suffix replayed after this point;
        // anything older aborts conservatively via the takeover floor.
        let max = self.state.snapshot_state().max_txn_ts;
        self.oracle.reset_for_restore(max);
        Ok(())
    }
}
