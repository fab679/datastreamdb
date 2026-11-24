//! Group membership and tablet view (`worker/groups.go`).
//!
//! [`GroupState`] is an alpha's cached picture of the cluster: which group
//! serves which predicate ([`Tablet`] map), group
//! members and leaders, and zero's lease counters. Zero pushes updates over
//! the membership stream; [`GroupState::apply_state`] installs them.

use std::collections::HashMap;
use std::sync::RwLock;

use datastreamdb_proto::pb::{Group, Member, MembershipState, Tablet};

/// Cached cluster picture for one alpha.
#[derive(Debug)]
pub struct GroupState {
    /// Latest membership state from zero.
    inner: RwLock<GroupView>,
}

/// The mutable half of [`GroupState`].
#[derive(Debug, Default)]
struct GroupView {
    /// This alpha's raft ID (zero-assigned; 0 until Connect).
    my_id: u64,
    /// This alpha's group ID (zero-assigned; 0 until Connect).
    group_id: u32,
    /// This alpha's address.
    addr: String,
    /// Groups by ID (members + tablets).
    groups: HashMap<u32, Group>,
    /// Zero members by raft ID.
    zeros: HashMap<u64, Member>,
    /// Highest UID leased.
    max_uid: u64,
    /// Highest transaction timestamp leased.
    max_txn_ts: u64,
    /// Highest namespace ID leased.
    max_ns_id: u64,
    /// Highest raft ID leased.
    max_raft_id: u64,
    /// Cluster ID.
    cid: String,
    /// State version; stale updates are dropped.
    counter: u64,
}

impl GroupState {
    /// Fresh state for alpha `my_id` in `group_id` at `addr`.
    ///
    /// IDs start at zero and are replaced by [`GroupState::set_ids`]
    /// after zero assigns them.
    ///
    /// # Panics
    ///
    /// Panics when the group lock is poisoned.
    #[must_use]
    pub fn new(my_id: u64, group_id: u32, addr: String) -> Self {
        let state = Self {
            inner: RwLock::new(GroupView::default()),
        };
        state.set_ids(my_id, group_id);
        {
            let mut view = state.inner.write().expect("group lock");
            view.addr = addr;
        }
        state
    }

    /// Records the zero-assigned IDs (replacing boot placeholders).
    ///
    /// # Panics
    ///
    /// Panics when the group lock is poisoned.
    pub fn set_ids(&self, my_id: u64, group_id: u32) {
        let mut view = self.inner.write().expect("group lock");
        view.my_id = my_id;
        view.group_id = group_id;
    }

    /// This alpha's raft ID.
    ///
    /// # Panics
    ///
    /// Panics when the group lock is poisoned.
    #[must_use]
    pub fn my_id(&self) -> u64 {
        self.inner.read().expect("group lock").my_id
    }

    /// This alpha's group ID.
    ///
    /// # Panics
    ///
    /// Panics when the group lock is poisoned.
    #[must_use]
    pub fn group_id(&self) -> u32 {
        self.inner.read().expect("group lock").group_id
    }

    /// This alpha's address.
    ///
    /// # Panics
    ///
    /// Panics when the group lock is poisoned.
    #[must_use]
    pub fn addr(&self) -> String {
        self.inner.read().expect("group lock").addr.clone()
    }

    /// Inserts or replaces one tablet (lazy `ShouldServe` resolution).
    /// # Panics
    ///
    /// Panics when the group lock is poisoned.
    pub fn upsert_tablet(&self, tablet: Tablet) {
        self.inner
            .write()
            .expect("group lock")
            .groups
            .entry(tablet.group_id)
            .or_default()
            .tablets
            .insert(tablet.predicate.clone(), tablet);
    }

    /// Installs a membership state from zero (stale counters are dropped).
    /// # Panics
    ///
    /// Panics when the group lock is poisoned.
    pub fn apply_state(&self, state: &MembershipState) {
        let mut view = self.inner.write().expect("group lock");
        if state.counter < view.counter {
            return;
        }
        view.groups.clone_from(&state.groups);
        view.zeros.clone_from(&state.zeros);
        view.max_uid = state.max_uid;
        view.max_txn_ts = state.max_txn_ts;
        view.max_ns_id = state.max_ns_id;
        view.max_raft_id = state.max_raft_id;
        view.cid.clone_from(&state.cid);
        view.counter = state.counter;
    }

    /// Whether this alpha belongs to `gid`.
    ///
    /// # Panics
    ///
    /// Panics when the group lock is poisoned.
    #[must_use]
    pub fn serves_group(&self, gid: u32) -> bool {
        gid == self.group_id()
    }

    /// Group serving `pred`, if any tablet claims it.
    #[must_use]
    /// # Panics
    ///
    /// Panics when the group lock is poisoned.
    pub fn belongs_to(&self, pred: &str) -> Option<u32> {
        let view = self.inner.read().expect("group lock");
        view.groups
            .iter()
            .find_map(|(gid, group)| group.tablets.contains_key(pred).then_some(*gid))
    }

    /// Whether this alpha's group serves `pred`.
    #[must_use]
    /// # Panics
    ///
    /// Panics when the group lock is poisoned.
    pub fn serves_tablet(&self, pred: &str) -> bool {
        self.belongs_to(pred) == Some(self.group_id())
    }

    /// Predicates assigned to `gid` (tablet keys, sorted).
    #[must_use]
    /// # Panics
    ///
    /// Panics when the group lock is poisoned.
    pub fn predicates_of(&self, gid: u32) -> Vec<String> {
        let view = self.inner.read().expect("group lock");
        let mut preds: Vec<String> = view
            .groups
            .get(&gid)
            .map(|group| group.tablets.keys().cloned().collect())
            .unwrap_or_default();
        preds.sort();
        preds
    }

    /// Tablet record for `pred`, if any group serves it.
    #[must_use]
    /// # Panics
    ///
    /// Panics when the group lock is poisoned.
    pub fn tablet(&self, pred: &str) -> Option<Tablet> {
        let view = self.inner.read().expect("group lock");
        view.groups
            .values()
            .find_map(|group| group.tablets.get(pred).cloned())
    }

    /// Known group IDs, ascending.
    #[must_use]
    /// # Panics
    ///
    /// Panics when the group lock is poisoned.
    pub fn known_groups(&self) -> Vec<u32> {
        let view = self.inner.read().expect("group lock");
        let mut gids: Vec<u32> = view.groups.keys().copied().collect();
        gids.sort_unstable();
        gids
    }

    /// Members of `gid` by raft ID.
    #[must_use]
    /// # Panics
    ///
    /// Panics when the group lock is poisoned.
    pub fn members(&self, gid: u32) -> HashMap<u64, Member> {
        self.inner
            .read()
            .expect("group lock")
            .groups
            .get(&gid)
            .map_or_else(HashMap::new, |group| group.members.clone())
    }

    /// Leader member of `gid`, if one is flagged.
    #[must_use]
    /// # Panics
    ///
    /// Panics when the group lock is poisoned.
    pub fn leader_of(&self, gid: u32) -> Option<Member> {
        self.members(gid).into_values().find(|member| member.leader)
    }

    /// Zero addresses for (re)connection.
    #[must_use]
    /// # Panics
    ///
    /// Panics when the group lock is poisoned.
    pub fn zero_addrs(&self) -> Vec<String> {
        self.inner
            .read()
            .expect("group lock")
            .zeros
            .values()
            .map(|member| member.addr.clone())
            .collect()
    }

    /// Highest UID leased cluster-wide.
    #[must_use]
    /// # Panics
    ///
    /// Panics when the group lock is poisoned.
    pub fn max_uid(&self) -> u64 {
        self.inner.read().expect("group lock").max_uid
    }

    /// Highest transaction timestamp leased cluster-wide.
    #[must_use]
    /// # Panics
    ///
    /// Panics when the group lock is poisoned.
    pub fn max_txn_ts(&self) -> u64 {
        self.inner.read().expect("group lock").max_txn_ts
    }

    /// Cluster ID (empty until zero assigns one).
    #[must_use]
    /// # Panics
    ///
    /// Panics when the group lock is poisoned.
    pub fn cluster_id(&self) -> String {
        self.inner.read().expect("group lock").cid.clone()
    }

    /// Re-exports the cached view as a [`MembershipState`].
    #[must_use]
    /// # Panics
    ///
    /// Panics when the group lock is poisoned.
    pub fn snapshot_state(&self) -> MembershipState {
        let view = self.inner.read().expect("group lock");
        MembershipState {
            counter: view.counter,
            groups: view.groups.clone(),
            zeros: view.zeros.clone(),
            max_uid: view.max_uid,
            max_txn_ts: view.max_txn_ts,
            max_ns_id: view.max_ns_id,
            max_raft_id: view.max_raft_id,
            removed: Vec::new(),
            cid: view.cid.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> MembershipState {
        MembershipState {
            counter: 7,
            groups: [(
                1_u32,
                Group {
                    members: [(
                        2_u64,
                        Member {
                            id: 2,
                            group_id: 1,
                            addr: "alpha2:7080".to_owned(),
                            leader: true,
                            ..Default::default()
                        },
                    )]
                    .into_iter()
                    .collect(),
                    tablets: [(
                        "name".to_owned(),
                        Tablet {
                            group_id: 1,
                            predicate: "name".to_owned(),
                            ..Default::default()
                        },
                    )]
                    .into_iter()
                    .collect(),
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            zeros: [(
                1_u64,
                Member {
                    id: 1,
                    addr: "zero:5080".to_owned(),
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            max_uid: 100,
            max_txn_ts: 200,
            max_ns_id: 2,
            max_raft_id: 9,
            removed: Vec::new(),
            cid: "cluster-1".to_owned(),
        }
    }

    #[test]
    fn membership_lookups_match_go_semantics() {
        let groups = GroupState::new(2, 1, "alpha2:7080".to_owned());
        assert_eq!(groups.belongs_to("name"), None);
        groups.apply_state(&state());

        assert!(groups.serves_group(1));
        assert!(!groups.serves_group(2));
        assert_eq!(groups.belongs_to("name"), Some(1));
        assert_eq!(groups.belongs_to("missing"), None);
        assert!(groups.serves_tablet("name"));
        assert!(!groups.serves_tablet("missing"));
        assert_eq!(groups.tablet("name").expect("tablet").group_id, 1);
        assert_eq!(groups.known_groups(), vec![1]);
        assert_eq!(groups.leader_of(1).expect("leader").id, 2);
        assert_eq!(groups.zero_addrs(), vec!["zero:5080".to_owned()]);
        assert_eq!(groups.max_uid(), 100);
        assert_eq!(groups.max_txn_ts(), 200);
        assert_eq!(groups.cluster_id(), "cluster-1");
    }

    #[test]
    fn stale_states_are_dropped() {
        let groups = GroupState::new(2, 1, "alpha2:7080".to_owned());
        groups.apply_state(&state());
        let mut stale = state();
        stale.counter = 3;
        stale.max_uid = 999;
        groups.apply_state(&stale);
        assert_eq!(groups.max_uid(), 100);
        assert_eq!(groups.snapshot_state().counter, 7);
    }
}
