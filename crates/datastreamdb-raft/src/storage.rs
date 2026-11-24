//! Durable raft storage (`raftwal/storage.go`).
//!
//! [`DiskStorage`] pairs the [`crate::meta::MetaFile`] with the
//! [`crate::wal::Wal`], exposing the same surface Go's raft layer consumes:
//! term/index lookups, ranged entry reads, snapshots, and ordered saves.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use datastreamdb_proto::raftpb::{ConfState, Entry, HardState, Snapshot, SnapshotMetadata};

use crate::meta::{MetaInfo, open_meta_file};
use crate::wal::open_wal;

/// Storage failures, mirroring etcd raft's sentinel errors plus I/O.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// Entry was compacted away (`raft.ErrCompacted`).
    #[error("entry was compacted")]
    Compacted,
    /// Entry is beyond the stored tail (`raft.ErrUnavailable`).
    #[error("entry is unavailable")]
    Unavailable,
    /// Snapshot predates the first index (`raft.ErrSnapOutOfDate`).
    #[error("snapshot is out of date")]
    SnapOutOfDate,
    /// No entry at that exact index.
    #[error("unable to find raft entry")]
    NotFound,
    /// Snapshot/meta pointers disagree (corrupt WAL).
    #[error("corrupt WAL: {0}")]
    Corrupt(String),
    /// Segment file name is not a `NNNNN.wal` id.
    #[error("bad segment name {name:?}: {reason}")]
    BadSegmentName {
        /// Offending file name.
        name: String,
        /// Parse failure detail.
        reason: String,
    },
    /// Two segments share one id.
    #[error("duplicate segment id {0}")]
    DuplicateSegment(i64),
    /// Entry type discriminant is negative (unrepresentable on disk).
    #[error("bad entry type {0}")]
    BadEntryType(i32),
    /// A payload offset overflowed `u64`.
    #[error("payload offset overflow")]
    OffsetOverflow,
    /// The WAL is encrypted; M5 opens unencrypted WALs only (see M6 `enc`).
    #[error("WAL segment is encrypted; encrypted WALs need M6")]
    EncryptedWal,
    /// WAL encryption keys are not supported until M6.
    #[error("WAL encryption keys are not supported until M6")]
    EncryptionUnsupported,
    /// Protobuf decode failure.
    #[error("protobuf decode failed: {0}")]
    Proto(#[from] prost::DecodeError),
    /// Filesystem failure.
    #[error("I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

/// Lock poison carries no extra context; surface it as corruption.
fn poisoned<T>(_: std::sync::PoisonError<T>) -> StorageError {
    StorageError::Corrupt("storage lock poisoned".to_owned())
}

impl PartialEq for StorageError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Compacted, Self::Compacted)
            | (Self::Unavailable, Self::Unavailable)
            | (Self::SnapOutOfDate, Self::SnapOutOfDate)
            | (Self::NotFound, Self::NotFound)
            | (Self::OffsetOverflow, Self::OffsetOverflow)
            | (Self::EncryptedWal, Self::EncryptedWal)
            | (Self::EncryptionUnsupported, Self::EncryptionUnsupported) => true,
            (Self::Corrupt(a), Self::Corrupt(b)) => a == b,
            (
                Self::BadSegmentName {
                    name: an,
                    reason: ar,
                },
                Self::BadSegmentName {
                    name: bn,
                    reason: br,
                },
            ) => an == bn && ar == br,
            (Self::DuplicateSegment(a), Self::DuplicateSegment(b)) => a == b,
            (Self::BadEntryType(a), Self::BadEntryType(b)) => a == b,
            (Self::Proto(a), Self::Proto(b)) => a.to_string() == b.to_string(),
            (Self::Io(a), Self::Io(b)) => a.kind() == b.kind(),
            _ => false,
        }
    }
}

impl Eq for StorageError {}

type Wal = crate::wal::Wal;
type MetaFile = crate::meta::MetaFile;

/// Locked storage state: meta file plus entry log.
#[derive(Debug)]
struct Inner {
    meta: MetaFile,
    wal: Wal,
}

impl Inner {
    /// First index (snapshot-aware).
    fn first_index(&self) -> u64 {
        let snap = self.meta.uint(MetaInfo::SnapshotIndex);
        if snap > 0 {
            return snap + 1;
        }
        self.wal.first_index()
    }
}

/// Durable raft log + metadata for one node (`DiskStorage`).
#[derive(Debug)]
pub struct DiskStorage {
    /// Storage directory (mirrors Go's `DiskStorage.dir`).
    #[allow(dead_code, reason = "mirrors Go DiskStorage.dir")]
    dir: PathBuf,
    inner: Mutex<Inner>,
}

impl DiskStorage {
    /// Opens (creating) unencrypted storage in `dir`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the directory cannot be read, the WAL
    /// is encrypted or corrupt, or the snapshot pointer disagrees with the
    /// log.
    pub fn init(dir: &Path) -> Result<Self, StorageError> {
        Self::init_encrypted(dir, None)
    }

    /// Opens storage, rejecting encryption keys until M6.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::EncryptionUnsupported`] when `key` is
    /// provided, plus the [`DiskStorage::init`] failures.
    ///
    /// # Panics
    ///
    /// Panics when the fresh storage lock is poisoned (cannot happen).
    pub fn init_encrypted(dir: &Path, key: Option<&[u8]>) -> Result<Self, StorageError> {
        if key.is_some() {
            return Err(StorageError::EncryptionUnsupported);
        }
        let meta = open_meta_file(dir)?;
        let wal = open_wal(dir)?;
        let snap = meta.snapshot()?;

        let inner = Inner { meta, wal };
        let first = inner.first_index();
        if !snap.is_empty() {
            let snap_index = snap.metadata.as_ref().map_or(0, |m| m.index);
            if snap_index + 1 != first {
                return Err(StorageError::Corrupt(format!(
                    "snapshot index {snap_index} + 1 != first index {first}"
                )));
            }
        }

        let mut storage = Self {
            dir: dir.to_owned(),
            inner: Mutex::new(inner),
        };
        // Drop segments below the first index (unclean shutdown leftovers).
        storage
            .inner
            .get_mut()
            .expect("fresh lock")
            .wal
            .delete_before(first.saturating_sub(1))?;
        Ok(storage)
    }

    /// Writes a meta slot.
    ///
    /// # Panics
    ///
    /// Panics when the storage lock is poisoned.
    pub fn set_uint(&self, info: MetaInfo, id: u64) {
        self.inner
            .lock()
            .expect("storage lock")
            .meta
            .set_uint(info, id);
    }

    /// Reads a meta slot.
    ///
    /// # Panics
    ///
    /// Panics when the storage lock is poisoned.
    #[must_use]
    pub fn uint(&self, info: MetaInfo) -> u64 {
        self.inner.lock().expect("storage lock").meta.uint(info)
    }

    /// Reads the persisted hard state.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the stored bytes do not decode.
    pub fn hard_state(&self) -> Result<HardState, StorageError> {
        let inner = self.inner.lock().map_err(poisoned)?;
        inner.meta.hard_state()
    }

    /// Reads the checkpoint (applied) index.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] only when the lock is poisoned.
    pub fn checkpoint(&self) -> Result<u64, StorageError> {
        Ok(self
            .inner
            .lock()
            .map_err(poisoned)?
            .meta
            .uint(MetaInfo::CheckpointIndex))
    }

    /// Saved hard state plus the snapshot's membership.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when stored bytes do not decode.
    pub fn initial_state(&self) -> Result<(HardState, ConfState), StorageError> {
        let inner = self.inner.lock().map_err(poisoned)?;
        let hard_state = inner.meta.hard_state()?;
        let snap = inner.meta.snapshot()?;
        let conf = snap.metadata.and_then(|m| m.conf_state).unwrap_or_default();
        Ok((hard_state, conf))
    }

    /// Counts stored entries by scanning (`NumEntries`).
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] only when the lock is poisoned.
    pub fn num_entries(&self) -> Result<usize, StorageError> {
        let inner = self.inner.lock().map_err(poisoned)?;
        let mut start = inner.first_index();
        let mut count = 0;
        loop {
            let entries = inner.wal.all_entries(start, u64::MAX, 64 << 20);
            if entries.is_empty() {
                return Ok(count);
            }
            count += entries.len();
            start = entries[entries.len() - 1].index + 1;
        }
    }

    /// Entries in `[lo, hi)`, capped at `max_size` encoded bytes.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Compacted`] when `lo` precedes the first
    /// index and [`StorageError::Unavailable`] when `hi` overshoots the tail.
    pub fn entries(&self, lo: u64, hi: u64, max_size: u64) -> Result<Vec<Entry>, StorageError> {
        let inner = self.inner.lock().map_err(poisoned)?;
        let first = inner.first_index();
        if lo < first {
            return Err(StorageError::Compacted);
        }
        let last = inner.wal.last_index();
        if hi > last + 1 {
            return Err(StorageError::Unavailable);
        }
        Ok(inner.wal.all_entries(lo, hi, max_size))
    }

    /// Term of entry `idx`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Compacted`] below the snapshot,
    /// [`StorageError::Unavailable`] past the tail, or
    /// [`StorageError::NotFound`] for gaps.
    pub fn term(&self, idx: u64) -> Result<u64, StorageError> {
        let inner = self.inner.lock().map_err(poisoned)?;
        let snap_index = inner.meta.uint(MetaInfo::SnapshotIndex);
        if idx < snap_index {
            return Err(StorageError::Compacted);
        }
        if idx == snap_index {
            return Ok(inner.meta.uint(MetaInfo::SnapshotTerm));
        }
        inner.wal.term(idx)
    }

    /// Highest stored index (snapshot-aware).
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] only when the lock is poisoned.
    pub fn last_index(&self) -> Result<u64, StorageError> {
        let inner = self.inner.lock().map_err(poisoned)?;
        let last = inner.wal.last_index();
        let snap = inner.meta.uint(MetaInfo::SnapshotIndex);
        Ok(last.max(snap))
    }

    /// First available index (snapshot-aware).
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] only when the lock is poisoned.
    pub fn first_index(&self) -> Result<u64, StorageError> {
        Ok(self.inner.lock().map_err(poisoned)?.first_index())
    }

    /// Most recent snapshot (empty when none was stored).
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the stored bytes do not decode.
    pub fn snapshot(&self) -> Result<Snapshot, StorageError> {
        let inner = self.inner.lock().map_err(poisoned)?;
        inner.meta.snapshot()
    }

    /// Stores a snapshot at `index` and drops older segments.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::SnapOutOfDate`] below the first index,
    /// [`StorageError::Unavailable`] past the tail, or I/O errors.
    pub fn create_snapshot(
        &self,
        index: u64,
        conf: &ConfState,
        data: &[u8],
    ) -> Result<(), StorageError> {
        let mut inner = self.inner.lock().map_err(poisoned)?;
        let first = inner.first_index();
        if index < first {
            return Err(StorageError::SnapOutOfDate);
        }
        let entry = inner.wal.seek_entry(index)?;
        let snap = Snapshot {
            data: data.to_vec(),
            metadata: Some(SnapshotMetadata {
                conf_state: Some(conf.clone()),
                index,
                term: entry.term,
            }),
        };
        inner.meta.store_snapshot(&snap)?;
        inner.wal.delete_before(index)?;
        Ok(())
    }

    /// Persists entries, then hard state, then snapshot, in order.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] on I/O or decode failures.
    pub fn save(
        &self,
        hard_state: &HardState,
        entries: &[Entry],
        snap: &Snapshot,
    ) -> Result<(), StorageError> {
        let mut inner = self.inner.lock().map_err(poisoned)?;
        inner.wal.add_entries(entries)?;
        inner.meta.store_hard_state(hard_state)?;
        inner.meta.store_snapshot(snap)?;
        Ok(())
    }

    /// Appends entries, trimming any compacted prefix first.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] on I/O failures.
    pub fn add_entries(&self, entries: &[Entry]) -> Result<(), StorageError> {
        if entries.is_empty() {
            return Ok(());
        }
        let mut inner = self.inner.lock().map_err(poisoned)?;
        let first = inner.first_index();
        let first_new = entries[0].index;
        let incoming = u64::try_from(entries.len()).unwrap_or(u64::MAX);
        if first_new + incoming - 1 < first {
            return Ok(());
        }
        if first > first_new {
            let skip = usize::try_from(first - first_new).unwrap_or(usize::MAX);
            inner.wal.add_entries(&entries[skip.min(entries.len())..])?;
        } else {
            inner.wal.add_entries(entries)?;
        }
        Ok(())
    }

    /// Deletes entries above `last` (`None` clears everything).
    pub(crate) fn truncate_after(&self, last: Option<u64>) -> Result<(), StorageError> {
        self.inner
            .lock()
            .map_err(poisoned)?
            .wal
            .truncate_after(last)
    }

    /// Deletes segments strictly before the one holding `index`.
    pub(crate) fn delete_before(&self, index: u64) -> Result<(), StorageError> {
        self.inner
            .lock()
            .map_err(poisoned)?
            .wal
            .delete_before(index)
    }

    /// Zeroes payloads of normal entries below `last_idx` (index GC).
    ///
    /// # Panics
    ///
    /// Panics when the storage lock is poisoned.
    pub fn truncate_entries_until(&self, last_idx: u64) {
        self.inner
            .lock()
            .expect("storage lock")
            .wal
            .truncate_entries_until(last_idx);
    }

    /// Count of sealed (rotated) segments.
    ///
    /// # Panics
    ///
    /// Panics when the storage lock is poisoned.
    #[must_use]
    pub fn num_log_files(&self) -> usize {
        self.inner.lock().expect("storage lock").wal.sealed_count()
    }

    /// Syncs meta and current segment files to disk.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] on I/O failures.
    pub fn sync(&self) -> Result<(), StorageError> {
        let inner = self.inner.lock().map_err(poisoned)?;
        inner.meta.sync()?;
        inner.wal.sync_current()?;
        Ok(())
    }

    /// Flushes and releases storage (`Close`).
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] on I/O failures.
    pub fn close(&self) -> Result<(), StorageError> {
        self.sync()
    }

    /// Clears the log and writes `entries` (test helper; mirrors Go `reset`).
    #[cfg(test)]
    pub(crate) fn reset(&self, entries: &[Entry]) -> Result<(), StorageError> {
        let mut inner = self.inner.lock().map_err(poisoned)?;
        inner.wal.reset()?;
        inner.wal.add_entries(entries)
    }

    /// Raw WAL scan bypassing range checks (test helper).
    #[cfg(test)]
    pub(crate) fn wal_all_entries(&self, lo: u64, hi: u64, max: u64) -> Vec<Entry> {
        self.inner
            .lock()
            .expect("storage lock")
            .wal
            .all_entries(lo, hi, max)
    }
}

#[cfg(test)]
mod tests {
    use datastreamdb_proto::raftpb::{ConfState, EntryType, HardState, Snapshot, SnapshotMetadata};
    use prost::Message as _;

    use super::*;

    fn entry(index: u64, term: u64) -> Entry {
        Entry {
            index,
            term,
            entry_type: EntryType::EntryNormal as i32,
            data: Vec::new(),
        }
    }

    fn is_compacted(result: &Result<u64, StorageError>) -> bool {
        matches!(result, Err(StorageError::Compacted))
    }

    fn is_unavailable<T>(result: &Result<T, StorageError>) -> bool {
        matches!(result, Err(StorageError::Unavailable))
    }

    #[test]
    fn term_vectors_match_go() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ds = DiskStorage::init(dir.path()).expect("init");
        ds.reset(&[entry(3, 3), entry(4, 4), entry(5, 5)])
            .expect("reset");

        assert!(is_compacted(&ds.term(2)));
        assert_eq!(ds.term(3).as_ref(), Ok(&3));
        assert_eq!(ds.term(4).as_ref(), Ok(&4));
        assert_eq!(ds.term(5).as_ref(), Ok(&5));
        assert!(is_unavailable(&ds.term(6)));
    }

    #[test]
    fn entries_vectors_match_go() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ds = DiskStorage::init(dir.path()).expect("init");
        let ents = [entry(3, 3), entry(4, 4), entry(5, 5), entry(6, 6)];
        let size = |e: &Entry| u64::try_from(e.encoded_len()).unwrap_or(u64::MAX);

        // lo below first compacts.
        ds.reset(&ents).expect("reset");
        assert!(matches!(
            ds.entries(2, 6, u64::MAX),
            Err(StorageError::Compacted)
        ));

        // Exact ranges.
        for (lo, hi, want) in [
            (4, 5, vec![entry(4, 4)]),
            (4, 6, vec![entry(4, 4), entry(5, 5)]),
            (4, 7, vec![entry(4, 4), entry(5, 5), entry(6, 6)]),
        ] {
            ds.reset(&ents).expect("reset");
            assert_eq!(ds.entries(lo, hi, u64::MAX).as_ref(), Ok(&want));
        }

        // Even with max_size zero, the first entry is returned.
        ds.reset(&ents).expect("reset");
        assert_eq!(ds.entries(4, 7, 0).as_ref(), Ok(&vec![entry(4, 4)]));

        // Size-capped ranges (mirrors the Go byte-budget cases).
        let two = size(&ents[1]) + size(&ents[2]);
        for max in [two, two + size(&ents[3]) / 2, two + size(&ents[3]) - 1] {
            ds.reset(&ents).expect("reset");
            assert_eq!(
                ds.entries(4, 7, max).as_ref(),
                Ok(&vec![entry(4, 4), entry(5, 5)])
            );
        }
        ds.reset(&ents).expect("reset");
        assert_eq!(
            ds.entries(4, 7, two + size(&ents[3])).as_ref(),
            Ok(&vec![entry(4, 4), entry(5, 5), entry(6, 6)])
        );
    }

    #[test]
    fn last_and_first_index_match_go() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ds = DiskStorage::init(dir.path()).expect("init");
        ds.reset(&[entry(3, 3), entry(4, 4), entry(5, 5)])
            .expect("reset");
        assert_eq!(ds.last_index().as_ref(), Ok(&5));
        assert_eq!(ds.first_index().as_ref(), Ok(&3));

        ds.reset(&[entry(6, 5)]).expect("reset");
        assert_eq!(ds.last_index().as_ref(), Ok(&6));
    }

    #[test]
    fn create_snapshot_vectors_match_go() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ds = DiskStorage::init(dir.path()).expect("init");
        let ents = [entry(3, 3), entry(4, 4), entry(5, 5)];
        let conf = ConfState {
            voters: vec![1, 2, 3],
            ..Default::default()
        };
        for index in [4, 5] {
            ds.reset(&ents).expect("reset");
            ds.create_snapshot(index, &conf, b"data").expect("snapshot");
            let snap = ds.snapshot().expect("read snapshot");
            assert_eq!(
                snap,
                Snapshot {
                    data: b"data".to_vec(),
                    metadata: Some(SnapshotMetadata {
                        conf_state: Some(conf.clone()),
                        index,
                        term: index,
                    }),
                }
            );
        }
    }

    #[test]
    fn append_vectors_match_go() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ds = DiskStorage::init(dir.path()).expect("init");
        let base = [entry(3, 3), entry(4, 4), entry(5, 5)];
        let cases: Vec<(Vec<Entry>, Vec<Entry>)> = vec![
            (
                vec![entry(3, 3), entry(4, 4), entry(5, 5)],
                vec![entry(3, 3), entry(4, 4), entry(5, 5)],
            ),
            (
                vec![entry(3, 3), entry(4, 6), entry(5, 6)],
                vec![entry(3, 3), entry(4, 6), entry(5, 6)],
            ),
            (
                vec![entry(3, 3), entry(4, 4), entry(5, 5), entry(6, 5)],
                vec![entry(3, 3), entry(4, 4), entry(5, 5), entry(6, 5)],
            ),
            // Truncate incoming entries, truncate existing, append.
            (
                vec![entry(2, 3), entry(3, 3), entry(4, 5)],
                vec![entry(3, 3), entry(4, 5)],
            ),
            // Truncate existing and append.
            (vec![entry(4, 5)], vec![entry(3, 3), entry(4, 5)]),
            // Direct append.
            (
                vec![entry(6, 5)],
                vec![entry(3, 3), entry(4, 4), entry(5, 5), entry(6, 5)],
            ),
        ];
        for (incoming, want) in cases {
            ds.reset(&base).expect("reset");
            ds.add_entries(&incoming).expect("append");
            assert_eq!(ds.wal_all_entries(0, u64::MAX, u64::MAX), want);
        }
    }

    #[test]
    fn truncate_storage_matches_go() {
        const NUM_ENTRIES: u64 = 30_000 * 3 / 2;
        const NUM_TRUNCATED: u64 = NUM_ENTRIES / 2;
        const NUM_NEW: u64 = NUM_ENTRIES + 30_000;

        let dir = tempfile::tempdir().expect("tempdir");
        let ds = DiskStorage::init(dir.path()).expect("init");

        for i in 0..NUM_ENTRIES {
            let typ = if i % 3 == 0 {
                EntryType::EntryConfChange
            } else {
                EntryType::EntryNormal
            };
            let idx = i + 1;
            ds.add_entries(&[Entry {
                index: idx,
                term: idx,
                entry_type: typ as i32,
                data: format!("entry {idx}").into_bytes(),
            }])
            .expect("append");
        }

        let entries = ds.wal_all_entries(0, u64::MAX, u64::MAX);
        assert_eq!(entries.len(), usize::try_from(NUM_ENTRIES).expect("small"));

        ds.truncate_entries_until(NUM_TRUNCATED);

        let entries = ds.wal_all_entries(0, u64::MAX, u64::MAX);
        assert_eq!(entries.len(), usize::try_from(NUM_ENTRIES).expect("small"));
        for (i, stored) in entries.iter().enumerate() {
            let idx = u64::try_from(i).expect("small") + 1;
            if idx < NUM_TRUNCATED && i % 3 != 0 {
                assert!(stored.data.is_empty(), "entry {idx} keeps data");
            } else {
                assert_eq!(stored.data, format!("entry {idx}").into_bytes());
            }
        }

        for i in NUM_ENTRIES..NUM_NEW {
            let idx = i + 1;
            ds.add_entries(&[Entry {
                index: idx,
                term: idx,
                entry_type: EntryType::EntryNormal as i32,
                data: format!("entry {idx}").into_bytes(),
            }])
            .expect("append");
        }
        let entries = ds.wal_all_entries(0, u64::MAX, u64::MAX);
        assert_eq!(entries.len(), usize::try_from(NUM_NEW).expect("small"));
    }

    #[test]
    fn snapshot_only_storage_matches_go() {
        const N: u64 = 1000;

        let dir = tempfile::tempdir().expect("tempdir");
        let ds = DiskStorage::init(dir.path()).expect("init");
        let snap = Snapshot {
            data: vec![7; 128],
            metadata: Some(SnapshotMetadata {
                conf_state: Some(ConfState::default()),
                index: N,
                term: 0,
            }),
        };
        ds.save(&HardState::default(), &[], &snap).expect("save");

        let out = ds.snapshot().expect("snapshot");
        assert_eq!(out.metadata.as_ref().expect("meta").index, N);
        assert_eq!(ds.first_index().as_ref(), Ok(&(N + 1)));
        assert_eq!(ds.last_index().as_ref(), Ok(&N));
    }

    #[test]
    fn big_storage_matches_go() {
        const N: u64 = 100_000;

        let dir = tempfile::tempdir().expect("tempdir");
        let ds = DiskStorage::init(dir.path()).expect("init");
        for idx in 1..=N {
            ds.add_entries(&[entry(idx, 1)]).expect("append");
            assert_eq!(ds.last_index().as_ref(), Ok(&idx));
        }
        assert_eq!(
            ds.num_entries().as_ref(),
            Ok(&usize::try_from(N).expect("small"))
        );

        let check = |ds: &DiskStorage, start: u64, end: u64| {
            let ents = ds.entries(start, end, u64::MAX).expect("range");
            assert_eq!(u64::try_from(ents.len()).expect("small"), end - start);
            for (i, e) in ents.iter().enumerate() {
                assert_eq!(e.index, start + u64::try_from(i).expect("small"));
            }
        };
        assert!(matches!(
            ds.entries(0, 1, u64::MAX),
            Err(StorageError::Compacted)
        ));
        check(&ds, 3, N);
        check(&ds, 10_000, 20_000);
        check(&ds, 20_000, 33_000);
        check(&ds, 33_000, 45_000);
        check(&ds, 45_000, N);
        // Around file boundaries.
        check(&ds, 1, N);
        check(&ds, 30_000, N);
        check(&ds, 30_001, N);
        check(&ds, 60_000, N);
        check(&ds, 60_001, N);
        check(&ds, 60_000, 90_000);
        check(&ds, N, N + 1);
        assert!(is_unavailable(&ds.entries(N + 1, N + 10, u64::MAX)));

        // Jump back a few files (cross-file overwrite).
        for idx in N / 3..=N {
            ds.add_entries(&[entry(idx, 1)]).expect("append");
        }
        check(&ds, 3, N);
        check(&ds, 10_000, 20_000);
        check(&ds, 20_000, 33_000);
        check(&ds, 33_000, 45_000);
        check(&ds, 45_000, N);
        check(&ds, N, N + 1);

        ds.create_snapshot(N - 100, &ConfState::default(), &[9; 128])
            .expect("snapshot");
        assert_eq!(ds.first_index().as_ref(), Ok(&(N - 100 + 1)));
        let snap = ds.snapshot().expect("snapshot");
        assert_eq!(snap.metadata.as_ref().expect("meta").index, N - 100);
        assert_eq!(snap.data, vec![9; 128]);
        assert_eq!(ds.num_log_files(), 0);

        let on_disk = std::fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".wal"))
            .count();
        assert_eq!(on_disk, 1);

        // Jumping back.
        ds.add_entries(&[entry(N - 50, 1)]).expect("append");
        let start = N - 100 + 1;
        let ents = ds.wal_all_entries(start, u64::MAX, u64::MAX);
        assert_eq!(ents.len(), 50);
        for (i, e) in ents.iter().enumerate() {
            assert_eq!(e.index, start + u64::try_from(i).expect("small"));
        }

        ds.add_entries(&[entry(N, 1)]).expect("append");
        let ents = ds.wal_all_entries(start, u64::MAX, u64::MAX);
        assert_eq!(ents.len(), 51);

        ds.sync().expect("sync");
        drop(ds);
        let reopened = DiskStorage::init(dir.path()).expect("reopen");
        let ents = reopened.wal_all_entries(start, u64::MAX, u64::MAX);
        assert_eq!(ents.len(), 51);
        for (i, e) in ents.iter().enumerate().take(50) {
            assert_eq!(e.index, start + u64::try_from(i).expect("small"));
        }
        assert_eq!(ents[50].index, N);
    }

    #[test]
    fn encrypted_open_is_rejected_until_m6() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(matches!(
            DiskStorage::init_encrypted(dir.path(), Some(b"badger16byteskey")),
            Err(StorageError::EncryptionUnsupported)
        ));
    }
}
