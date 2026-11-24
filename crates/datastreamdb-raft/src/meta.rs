//! `wal.meta` metadata file (`raftwal/meta.go`).
//!
//! Layout (byte-compatible with Go):
//!
//! ```text
//! 00-08  Raft ID
//! 08-16  Group ID
//! 16-24  Checkpoint index
//! 512    Hard state (length-prefixed marshalled proto)
//! 1024-1032 Snapshot index
//! 1032-1040 Snapshot term
//! 1040   Snapshot (length-prefixed marshalled proto)
//! ```

use std::io;
use std::path::Path;

use datastreamdb_proto::raftpb::{HardState, Snapshot};
use prost::Message as _;

use crate::mmap::MmapFile;
use crate::storage::StorageError;

/// Metadata file name.
const META_NAME: &str = "wal.meta";
/// Initial sparse size of the meta file.
const META_FILE_SIZE: u64 = 1 << 20;
/// Hard-state slice offset.
const HARD_STATE_OFFSET: usize = 512;
/// Snapshot index/term words start here.
const SNAPSHOT_INDEX: usize = 1024;
/// Snapshot slice offset.
const SNAPSHOT_OFFSET: usize = SNAPSHOT_INDEX + 16;

/// Addressable `u64` slots in the meta file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetaInfo {
    /// This node's raft ID.
    RaftId,
    /// This node's group ID.
    GroupId,
    /// Checkpoint (applied) index.
    CheckpointIndex,
    /// Snapshot index.
    SnapshotIndex,
    /// Snapshot term.
    SnapshotTerm,
}

/// Byte offset of a [`MetaInfo`] slot.
const fn info_offset(info: MetaInfo) -> usize {
    match info {
        MetaInfo::RaftId => 0,
        MetaInfo::GroupId => 8,
        MetaInfo::CheckpointIndex => 16,
        MetaInfo::SnapshotIndex => SNAPSHOT_INDEX,
        MetaInfo::SnapshotTerm => SNAPSHOT_INDEX + 8,
    }
}

/// The `wal.meta` file: IDs, hard state, and snapshot pointer.
#[derive(Debug)]
pub(crate) struct MetaFile {
    mmap: MmapFile,
}

/// Opens (creating) the meta file in `dir`.
pub(crate) fn open_meta_file(dir: &Path) -> Result<MetaFile, StorageError> {
    let (mut mmap, is_new) = MmapFile::open(&dir.join(META_NAME), META_FILE_SIZE)?;
    if is_new {
        mmap.zero_out(0, SNAPSHOT_OFFSET + 4);
    }
    Ok(MetaFile { mmap })
}

impl MetaFile {
    /// Reads a `u64` slot.
    pub(crate) fn uint(&self, info: MetaInfo) -> u64 {
        let at = info_offset(info);
        u64::from_be_bytes(
            self.mmap.data()[at..at + 8]
                .try_into()
                .expect("meta slot in bounds"),
        )
    }

    /// Writes a `u64` slot.
    pub(crate) fn set_uint(&mut self, info: MetaInfo, value: u64) {
        let at = info_offset(info);
        self.mmap.data_mut()[at..at + 8].copy_from_slice(&value.to_be_bytes());
    }

    /// Persists the hard state (skips empty states, like Go).
    pub(crate) fn store_hard_state(&mut self, state: &HardState) -> Result<(), StorageError> {
        if state.is_empty() {
            return Ok(());
        }
        let buf = state.encode_to_vec();
        debug_assert!(buf.len() < SNAPSHOT_INDEX - HARD_STATE_OFFSET);
        let end = HARD_STATE_OFFSET + 4 + buf.len();
        self.mmap.ensure(end)?;
        let (start, _) = self.mmap.allocate_slice(buf.len(), HARD_STATE_OFFSET)?;
        self.mmap.data_mut()[start..start + buf.len()].copy_from_slice(&buf);
        Ok(())
    }

    /// Reads the persisted hard state (empty when never stored).
    pub(crate) fn hard_state(&self) -> Result<HardState, StorageError> {
        let raw = self.mmap.slice(HARD_STATE_OFFSET);
        if raw.is_empty() {
            return Ok(HardState::default());
        }
        Ok(HardState::decode(raw)?)
    }

    /// Persists a snapshot (skips empty snapshots, like Go).
    pub(crate) fn store_snapshot(&mut self, snap: &Snapshot) -> Result<(), StorageError> {
        if snap.is_empty() {
            return Ok(());
        }
        if let Some(meta) = &snap.metadata {
            self.set_uint(MetaInfo::SnapshotIndex, meta.index);
            self.set_uint(MetaInfo::SnapshotTerm, meta.term);
        }
        let buf = snap.encode_to_vec();
        self.mmap.ensure(SNAPSHOT_OFFSET + 4 + buf.len())?;
        let (start, _) = self.mmap.allocate_slice(buf.len(), SNAPSHOT_OFFSET)?;
        self.mmap.data_mut()[start..start + buf.len()].copy_from_slice(&buf);
        Ok(())
    }

    /// Reads the persisted snapshot (empty when never stored).
    pub(crate) fn snapshot(&self) -> Result<Snapshot, StorageError> {
        let raw = self.mmap.slice(SNAPSHOT_OFFSET);
        if raw.is_empty() {
            return Ok(Snapshot::default());
        }
        Ok(Snapshot::decode(raw)?)
    }

    /// Syncs the meta file to disk.
    pub(crate) fn sync(&self) -> io::Result<()> {
        self.mmap.sync()
    }
}

#[cfg(test)]
mod tests {
    use datastreamdb_proto::raftpb::{HardState, Snapshot, SnapshotMetadata};

    use super::*;

    #[test]
    fn meta_file_vectors_match_go() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut mf = open_meta_file(dir.path()).expect("open");
        assert_eq!(mf.uint(MetaInfo::RaftId), 0);

        mf.set_uint(MetaInfo::RaftId, 10);
        assert_eq!(mf.uint(MetaInfo::RaftId), 10);

        assert_eq!(mf.hard_state().expect("hs"), HardState::default());

        let hs = HardState {
            term: 10,
            vote: 20,
            commit: 30,
        };
        mf.store_hard_state(&hs).expect("store hs");
        assert_eq!(mf.hard_state().expect("hs"), hs);

        assert_eq!(mf.snapshot().expect("snap"), Snapshot::default());

        let snap = Snapshot {
            data: b"foo".to_vec(),
            metadata: Some(SnapshotMetadata {
                conf_state: None,
                index: 12,
                term: 200,
            }),
        };
        mf.store_snapshot(&snap).expect("store snap");
        assert_eq!(mf.snapshot().expect("snap"), snap);
        assert_eq!(mf.uint(MetaInfo::SnapshotIndex), 12);
        assert_eq!(mf.uint(MetaInfo::SnapshotTerm), 200);
    }
}
