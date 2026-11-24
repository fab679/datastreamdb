//! Multi-segment write-ahead log (`raftwal/wal.go`).
//!
//! [`Wal`] owns the ordered segment list plus the segment currently being
//! written. Byte-compatible with Go: same rotation, truncation, and lookup
//! rules. Used behind [`crate::storage::DiskStorage`]'s lock.

use std::path::{Path, PathBuf};

use datastreamdb_proto::raftpb::{Entry, EntryType};
use prost::Message as _;

use crate::log::{LOG_FILE_OFFSET, MAX_NUM_ENTRIES, SlotPosition, get_log_files, open_log_file};
use crate::storage::StorageError;

/// 1 GiB segment cap (rotation trigger alongside [`MAX_NUM_ENTRIES`]).
const MAX_SEGMENT_BYTES: usize = 1 << 30;

/// Located slot: `None` file means the current (unlisted) segment.
type WalSlot = (Option<usize>, SlotPosition);

/// The entry log: ordered segments plus the segment being written.
#[derive(Debug)]
pub(crate) struct Wal {
    files: Vec<crate::log::LogFile>,
    current: crate::log::LogFile,
    next_entry_idx: usize,
    dir: PathBuf,
}

/// Opens (creating) the WAL in `dir`, dropping empty segments.
pub(crate) fn open_wal(dir: &Path) -> Result<Wal, StorageError> {
    let mut kept = Vec::new();
    let mut next_fid = 0;
    for file in get_log_files(dir)? {
        next_fid = next_fid.max(file.fid());
        if file.first_index() == 0 {
            file.remove()?;
        } else {
            kept.push(file);
        }
    }

    if let Some(current) = kept.pop() {
        let next_entry_idx = current.empty_slot();
        return Ok(Wal {
            files: kept,
            current,
            next_entry_idx,
            dir: dir.to_owned(),
        });
    }

    let current = open_log_file(dir, next_fid + 1)?;
    Ok(Wal {
        files: kept,
        current,
        next_entry_idx: 0,
        dir: dir.to_owned(),
    })
}

impl Wal {
    /// Segments below the current one (oldest first).
    pub(crate) fn sealed_count(&self) -> usize {
        self.files.len()
    }

    /// Entry headers in `[lo, hi)`, capped at `max_size` encoded bytes.
    ///
    /// Always returns at least one entry when any is available.
    pub(crate) fn all_entries(&self, lo: u64, hi: u64, max_size: u64) -> Vec<Entry> {
        let mut entries = Vec::new();
        let (mut file_idx, pos) = self.slot_ge(lo);
        let mut offset = match pos {
            SlotPosition::BeforeStart => 0,
            SlotPosition::At(slot) => slot,
            SlotPosition::PastEnd => MAX_NUM_ENTRIES,
        };
        let mut size = 0;
        let mut current_file = self.entry_file(file_idx);

        loop {
            if offset >= MAX_NUM_ENTRIES {
                let Some(idx) = file_idx else {
                    return entries;
                };
                let next = idx + 1;
                file_idx = if next >= self.files.len() {
                    None
                } else {
                    Some(next)
                };
                current_file = self.entry_file(file_idx);
                offset = 0;
            }

            let Some(file) = current_file else {
                return entries;
            };
            let entry = file.raft_entry(offset);
            if entry.index >= hi {
                return entries;
            }
            if entry.index == 0 {
                // Empty slot: skip to the next file.
                offset = MAX_NUM_ENTRIES;
                continue;
            }

            size += u64::try_from(entry.encoded_len()).unwrap_or(u64::MAX);
            if !entries.is_empty() && size > max_size {
                break;
            }
            entries.push(entry);
            offset += 1;
        }
        entries
    }

    /// Zeroes payloads of normal entries below `last_idx` (index GC).
    ///
    /// Only length prefixes are zeroed; offsets stay valid, so later
    /// overwrites still chain correctly.
    pub(crate) fn truncate_entries_until(&mut self, last_idx: u64) {
        for file in self
            .files
            .iter_mut()
            .chain(std::iter::once(&mut self.current))
        {
            for idx in 0..MAX_NUM_ENTRIES {
                let entry = file.raft_entry(idx);
                if entry.index == 0 {
                    // Empty slot. Go zeroes bytes `[0, 4)` here (the slot-0
                    // term prefix), a no-op for terms below 2^32; skipping it
                    // is behaviorally identical and cannot corrupt headers.
                    continue;
                }
                if entry.index >= last_idx {
                    return;
                }
                if entry.type_enum() == Some(EntryType::EntryNormal) {
                    let offset = file.slot_data_offset(idx);
                    file.zero_payload_len(offset);
                }
            }
        }
    }

    /// Appends entries, overwriting (and zeroing past) any index overlap.
    pub(crate) fn add_entries(&mut self, entries: &[Entry]) -> Result<(), StorageError> {
        if entries.is_empty() {
            return Ok(());
        }
        let (fidx, pos) = self.slot_ge(entries[0].index);
        if let SlotPosition::At(eidx) = pos {
            match fidx {
                None => {
                    if self.next_entry_idx > eidx {
                        self.current.clear_slots(eidx, self.next_entry_idx);
                    }
                }
                Some(fidx) => {
                    debug_assert!(fidx < self.files.len());
                    // Found in a sealed file: drop every newer file plus the
                    // old current, and promote the sealed file to current.
                    let mut tail = self.files.split_off(fidx);
                    let found = tail.remove(0);
                    let old_current = std::mem::replace(&mut self.current, found);
                    tail.push(old_current);
                    for file in tail {
                        file.remove()?;
                    }
                    // Go zeroes through `logFileOffset` here, which also wipes
                    // the key-ID/IV trailer on encrypted WALs (upstream bug:
                    // the file becomes unreadable after a reopen). Zeroing the
                    // header slots alone is byte-identical for unencrypted
                    // WALs, which are the only ones M5 opens.
                    self.current.clear_slots(eidx, MAX_NUM_ENTRIES);
                }
            }
            self.next_entry_idx = eidx;
        }

        let mut offset = if self.next_entry_idx == 0 {
            LOG_FILE_OFFSET
        } else {
            self.current.payload_end(self.next_entry_idx - 1)
        };

        for entry in entries {
            if self.next_entry_idx >= MAX_NUM_ENTRIES
                || offset + 4 + entry.data.len() > MAX_SEGMENT_BYTES
            {
                self.rotate(offset)?;
                self.next_entry_idx = 0;
                offset = LOG_FILE_OFFSET;
            }
            let typ = u64::try_from(entry.entry_type)
                .map_err(|_| StorageError::BadEntryType(entry.entry_type))?;
            offset = self.current.append_slot(
                self.next_entry_idx,
                entry.term,
                entry.index,
                typ,
                &entry.data,
                offset,
            )?;
            self.next_entry_idx += 1;
        }
        Ok(())
    }

    /// First available index (raft expects `>= 1`).
    pub(crate) fn first_index(&self) -> u64 {
        let first = if self.files.is_empty() {
            self.current.first_index()
        } else {
            self.files[0].first_index()
        };
        if first == 0 { 1 } else { first }
    }

    /// Last stored index (0 when empty).
    pub(crate) fn last_index(&self) -> u64 {
        if self.next_entry_idx > 0 {
            return self.current.raft_entry(self.next_entry_idx - 1).index;
        }
        for file in self.files.iter().rev() {
            let tail = file.tail_index();
            if tail > 0 {
                return tail;
            }
        }
        0
    }

    /// Term of entry `idx`.
    pub(crate) fn term(&self, idx: u64) -> Result<u64, StorageError> {
        Ok(self.seek_entry(idx)?.term)
    }

    /// Deletes segments strictly before the one holding `raft_index`.
    pub(crate) fn delete_before(&mut self, raft_index: u64) -> Result<(), StorageError> {
        let (fidx, pos) = self.slot_ge(raft_index);
        if matches!(pos, SlotPosition::BeforeStart) {
            return Ok(());
        }
        if let Some(idx) = fidx {
            if idx >= self.files.len() {
                // Go checks `fidx >= len(l.files)`; `None` (current) proceeds.
                return Ok(());
            }
            let before: Vec<_> = self.files.drain(..idx).collect();
            for file in before {
                file.remove()?;
            }
        } else {
            let before = std::mem::take(&mut self.files);
            for file in before {
                file.remove()?;
            }
        }
        Ok(())
    }

    /// Deletes entries above `last` (`None` clears everything).
    ///
    /// Removes slots from the tail backward, so the log never holds a hole.
    /// Stale payload bytes past the new tail are unreachable: appends chain
    /// their offset from the tail slot, like the overlap path in Go.
    pub(crate) fn truncate_after(&mut self, last: Option<u64>) -> Result<(), StorageError> {
        loop {
            if last.is_some_and(|keep| self.last_index() <= keep) {
                return Ok(());
            }
            if self.next_entry_idx == 0 {
                let Some(prev) = self.files.pop() else {
                    return Ok(());
                };
                let drained = std::mem::replace(&mut self.current, prev);
                drained.remove()?;
                self.next_entry_idx = self.current.empty_slot();
            } else {
                self.next_entry_idx -= 1;
                let tail = self.next_entry_idx;
                self.current.clear_slots(tail, tail + 1);
            }
        }
    }

    /// Drops all segments and zeroes the current header region (tests).
    #[cfg(test)]
    pub(crate) fn reset(&mut self) -> Result<(), StorageError> {
        for file in std::mem::take(&mut self.files) {
            file.remove()?;
        }
        self.current.clear_slots(0, MAX_NUM_ENTRIES);
        self.next_entry_idx = 0;
        Ok(())
    }

    /// Syncs the current segment to disk.
    pub(crate) fn sync_current(&self) -> Result<(), StorageError> {
        Ok(self.current.sync()?)
    }

    /// Segment for `None` (current) or a sealed index.
    fn entry_file(&self, fidx: Option<usize>) -> Option<&crate::log::LogFile> {
        match fidx {
            None => Some(&self.current),
            Some(idx) => self.files.get(idx),
        }
    }

    /// Locates the slot at or after `raft_index`.
    fn slot_ge(&self, raft_index: u64) -> WalSlot {
        match self.current.slot_position(raft_index) {
            SlotPosition::At(slot) => return (None, SlotPosition::At(slot)),
            SlotPosition::PastEnd => return (None, SlotPosition::PastEnd),
            SlotPosition::BeforeStart => {}
        }
        if self.files.is_empty() {
            return (None, SlotPosition::BeforeStart);
        }
        let mut file_idx = self.files.partition_point(|f| f.first_index() < raft_index);
        if file_idx < self.files.len() && self.files[file_idx].first_index() == raft_index {
            return (Some(file_idx), SlotPosition::At(0));
        }
        file_idx = file_idx.saturating_sub(1);
        let pos = self.files[file_idx].slot_position(raft_index);
        (Some(file_idx), pos)
    }

    /// Entry at exactly `raft_index`.
    pub(crate) fn seek_entry(&self, raft_index: u64) -> Result<Entry, StorageError> {
        if raft_index == 0 {
            return Ok(Entry::default());
        }
        let (fidx, pos) = self.slot_ge(raft_index);
        let slot = match pos {
            SlotPosition::BeforeStart => return Err(StorageError::Compacted),
            SlotPosition::PastEnd => return Err(StorageError::Unavailable),
            SlotPosition::At(slot) => slot,
        };
        let Some(file) = self.entry_file(fidx) else {
            return Err(StorageError::Unavailable);
        };
        let entry = file.raft_entry(slot);
        if entry.index == 0 {
            return Err(StorageError::Unavailable);
        }
        if entry.index != raft_index {
            return Err(StorageError::NotFound);
        }
        Ok(entry)
    }

    /// Seals the current segment and opens a fresh one.
    fn rotate(&mut self, offset: usize) -> Result<(), StorageError> {
        let mut next_fid = self.current.fid();
        debug_assert!(next_fid > 0);
        for file in &self.files {
            next_fid = next_fid.max(file.fid());
        }
        let len = u64::try_from(offset).map_err(|_| StorageError::OffsetOverflow)?;
        self.current.trim_to(len)?;
        let fresh = open_log_file(&self.dir, next_fid + 1)?;
        let old = std::mem::replace(&mut self.current, fresh);
        self.files.push(old);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use datastreamdb_proto::raftpb::Entry;

    use super::*;

    #[test]
    fn entry_file_vectors_match_go() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut wal = open_wal(dir.path()).expect("open");
        assert_eq!(wal.first_index(), 1);
        assert_eq!(wal.last_index(), 0);

        assert!(wal.seek_entry(2).is_err());

        wal.add_entries(&[Entry {
            index: 1,
            term: 1,
            entry_type: EntryType::EntryNormal as i32,
            data: b"abc".to_vec(),
        }])
        .expect("append");
        let entries = wal.all_entries(0, 100, 10_000);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].index, 1);
        assert_eq!(entries[0].term, 1);
        assert_eq!(entries[0].data, b"abc");
    }

    #[test]
    fn reopen_recovers_entries_and_next_slot() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let mut wal = open_wal(dir.path()).expect("open");
            wal.add_entries(&[
                Entry {
                    index: 1,
                    term: 1,
                    entry_type: EntryType::EntryNormal as i32,
                    data: b"a".to_vec(),
                },
                Entry {
                    index: 2,
                    term: 1,
                    entry_type: EntryType::EntryNormal as i32,
                    data: b"b".to_vec(),
                },
            ])
            .expect("append");
            wal.sync_current().expect("sync");
        }
        let mut wal = open_wal(dir.path()).expect("reopen");
        assert_eq!(wal.last_index(), 2);
        wal.add_entries(&[Entry {
            index: 3,
            term: 2,
            entry_type: EntryType::EntryNormal as i32,
            data: b"c".to_vec(),
        }])
        .expect("append");
        let entries = wal.all_entries(1, 4, u64::MAX);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[2].data, b"c");
        assert_eq!(entries[2].term, 2);
    }
}
