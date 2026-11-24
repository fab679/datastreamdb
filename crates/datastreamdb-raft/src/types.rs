//! openraft type configuration for `DataStreamDB` groups.
//!
//! One configuration serves every group (zero and alpha alike): proposals
//! and apply responses are opaque bytes, node IDs are raft IDs, and nodes
//! carry their transport address in [`BasicNode`].
//!
//! [`BasicNode`]: openraft::impls::BasicNode

openraft::declare_raft_types!(
    /// openraft types for a `DataStreamDB` raft group.
    pub DataStreamDBTypeConfig:
        D = Proposal,
        R = Vec<u8>,
);

/// Opaque proposal bytes (serialized `pb.Proposal` / `pb.ZeroProposal`).
///
/// A newtype (rather than bare `Vec<u8>`) because openraft's `AppData`
/// requires `Display`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Proposal(pub Vec<u8>);

impl std::fmt::Display for Proposal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "proposal({} bytes)", self.0.len())
    }
}

use openraft::type_config::alias::{
    EntryOf, EntryPayloadOf, LogIdOf, MembershipOf, SnapshotMetaOf, SnapshotOf, StoredMembershipOf,
    VoteOf,
};

/// Log entry: log id plus a blank/normal/membership payload.
pub type GroupEntry = EntryOf<DataStreamDBTypeConfig>;
/// Entry payload variants.
pub type GroupPayload = EntryPayloadOf<DataStreamDBTypeConfig>;
/// Log position: committing leader plus index.
pub type GroupLogId = LogIdOf<DataStreamDBTypeConfig>;
/// Persisted vote: candidate leader plus granted flag.
pub type GroupVote = VoteOf<DataStreamDBTypeConfig>;
/// Cluster membership with voter/learner sets.
pub type GroupMembership = MembershipOf<DataStreamDBTypeConfig>;
/// Membership plus the log id that stored it.
pub type GroupStoredMembership = StoredMembershipOf<DataStreamDBTypeConfig>;
/// Snapshot metadata: last log id, membership, snapshot id.
pub type GroupSnapshotMeta = SnapshotMetaOf<DataStreamDBTypeConfig>;
/// Full snapshot with byte payload.
pub type GroupSnapshot = SnapshotOf<DataStreamDBTypeConfig, Vec<u8>>;
/// Raft handle bound to our state machine (see [`crate::state_machine`]).
pub type GroupRaft<A> = openraft::Raft<DataStreamDBTypeConfig, crate::state_machine::SmStore<A>>;
