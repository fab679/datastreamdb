//! etcd raft wire mirrors (`raftpb`) for the write-ahead log.
//!
//! Hand-authored prost types (not codegen): upstream `raft.proto` uses
//! `proto2` syntax, which prost cannot compile. The layout below matches the
//! Go-marshalled wire bytes exactly for all values the WAL stores:
//! proto2 `optional` scalars marshal identically to proto3 scalars whenever
//! set, and Go always sets `Term`/`Index`/`Type` on persisted entries.
//! Golden vectors in `tests` pin the encoding.

/// etcd raft entry types (`raftpb.EntryType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, prost::Enumeration)]
#[repr(i32)]
pub enum EntryType {
    /// Application payload (proposal data).
    EntryNormal = 0,
    /// Membership change (`ConfChange` payload).
    EntryConfChange = 1,
    /// Joint-consensus membership change (`ConfChangeV2` payload).
    EntryConfChangeV2 = 2,
}

/// One log entry (`raftpb.Entry`: `Type = 1, Term = 2, Index = 3, Data = 4`).
#[derive(Clone, PartialEq, prost::Message)]
pub struct Entry {
    /// Entry term.
    #[prost(uint64, tag = "2")]
    pub term: u64,
    /// Log index (1-based; 0 marks an empty slot).
    #[prost(uint64, tag = "3")]
    pub index: u64,
    /// Entry kind ([`EntryType`] discriminant).
    #[prost(enumeration = "EntryType", tag = "1")]
    pub entry_type: i32,
    /// Opaque payload.
    #[prost(bytes, tag = "4")]
    pub data: Vec<u8>,
}

impl Entry {
    /// Typed view of [`Entry::entry_type`] (unknown discriminants err).
    #[must_use]
    pub fn type_enum(&self) -> Option<EntryType> {
        EntryType::try_from(self.entry_type).ok()
    }
}

/// Voter/learner sets (`raftpb.ConfState`).
#[derive(Clone, PartialEq, prost::Message)]
pub struct ConfState {
    /// Voting members.
    #[prost(uint64, repeated, tag = "1")]
    pub voters: Vec<u64>,
    /// Non-voting members.
    #[prost(uint64, repeated, tag = "2")]
    pub learners: Vec<u64>,
    /// Outgoing voters during joint consensus.
    #[prost(uint64, repeated, tag = "3")]
    pub voters_outgoing: Vec<u64>,
    /// Incoming learners during joint consensus.
    #[prost(uint64, repeated, tag = "4")]
    pub learners_next: Vec<u64>,
    /// Auto-leave joint consensus once applied.
    #[prost(bool, tag = "5")]
    pub auto_leave: bool,
}

/// Snapshot pointer (`raftpb.SnapshotMetadata`).
#[derive(Clone, PartialEq, prost::Message)]
pub struct SnapshotMetadata {
    /// Membership at the snapshot.
    #[prost(message, optional, tag = "1")]
    pub conf_state: Option<ConfState>,
    /// Last included log index.
    #[prost(uint64, tag = "2")]
    pub index: u64,
    /// Last included log term.
    #[prost(uint64, tag = "3")]
    pub term: u64,
}

/// Persisted snapshot (`raftpb.Snapshot`).
#[derive(Clone, PartialEq, prost::Message)]
pub struct Snapshot {
    /// State-machine snapshot bytes.
    #[prost(bytes, tag = "1")]
    pub data: Vec<u8>,
    /// Snapshot pointer.
    #[prost(message, optional, tag = "2")]
    pub metadata: Option<SnapshotMetadata>,
}

impl Snapshot {
    /// An empty snapshot carries no metadata (matches Go `IsEmptySnap`).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.metadata.as_ref().is_none_or(|m| m.index == 0)
    }
}

/// Persisted voter state (`raftpb.HardState`).
#[derive(Clone, Copy, PartialEq, prost::Message)]
pub struct HardState {
    /// Current term.
    #[prost(uint64, tag = "1")]
    pub term: u64,
    /// Candidate voted for this term (0 = none).
    #[prost(uint64, tag = "2")]
    pub vote: u64,
    /// Highest committed log index.
    #[prost(uint64, tag = "3")]
    pub commit: u64,
}

impl HardState {
    /// An empty hard state votes nothing (matches Go `IsEmptyHardState`).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.term == 0 && self.vote == 0 && self.commit == 0
    }
}

#[cfg(test)]
mod tests {
    use prost::Message as _;

    use super::*;

    #[test]
    fn entry_encoding_matches_raftpb() {
        // Hand-computed per the protobuf spec for
        // Entry{Type: EntryNormal(0), Term: 7, Index: 42, Data: "hi"}.
        // Field 1 (enum 0) is omitted as the zero value.
        let entry = Entry {
            term: 7,
            index: 42,
            entry_type: EntryType::EntryNormal as i32,
            data: b"hi".to_vec(),
        };
        assert_eq!(
            entry.encode_to_vec(),
            [0x10, 0x07, 0x18, 0x2A, 0x22, 0x02, b'h', b'i']
        );
        let back = Entry::decode(entry.encode_to_vec().as_slice()).expect("decode");
        assert_eq!(back, entry);
        assert_eq!(back.type_enum(), Some(EntryType::EntryNormal));
    }

    #[test]
    fn hard_state_encoding_matches_raftpb() {
        let hs = HardState {
            term: 3,
            vote: 9,
            commit: 128,
        };
        assert_eq!(
            hs.encode_to_vec(),
            [0x08, 0x03, 0x10, 0x09, 0x18, 0x80, 0x01]
        );
        assert!(!hs.is_empty());
        assert!(HardState::default().is_empty());
    }

    #[test]
    fn snapshot_roundtrips_with_conf_state() {
        let snap = Snapshot {
            data: vec![1, 2, 3],
            metadata: Some(SnapshotMetadata {
                conf_state: Some(ConfState {
                    voters: vec![1, 2, 3],
                    learners: Vec::new(),
                    voters_outgoing: Vec::new(),
                    learners_next: Vec::new(),
                    auto_leave: false,
                }),
                index: 100,
                term: 5,
            }),
        };
        let bytes = snap.encode_to_vec();
        let back = Snapshot::decode(bytes.as_slice()).expect("decode");
        assert_eq!(back, snap);
        assert!(!back.is_empty());
        assert!(Snapshot::default().is_empty());
    }
}
