//! Single WAL segment files (`raftwal/log.go`).
//!
//! Each `NNNNN.wal` file stores up to [`MAX_NUM_ENTRIES`] fixed-size entry
//! headers in its first [`LOG_FILE_OFFSET`] bytes; entry payloads live after
//! that offset as length-prefixed slices. Layout is byte-compatible with Go.

use std::io;
use std::path::{Path, PathBuf};

use datastreamdb_proto::raftpb::{Entry, EntryType};

use crate::mmap::{MmapFile, slice_size};
use crate::storage::StorageError;

/// Maximum entries per segment before rotation.
pub(crate) const MAX_NUM_ENTRIES: usize = 30_000;
/// Same bound as `u64` for index arithmetic.
const MAX_NUM_ENTRIES_U64: u64 = 30_000;
/// Payload region starts here; headers live in `[0, LOG_FILE_OFFSET)`.
pub(crate) const LOG_FILE_OFFSET: usize = 1 << 20;
/// Key ID (8 B) + base IV (8 B) live at the end of the header region.
const ENC_OFFSET: usize = LOG_FILE_OFFSET - 16;
/// Initial sparse size of a new segment.
const LOG_FILE_SIZE: u64 = 256 << 20;
/// One header: term, index, data offset, type (8 B each, big-endian).
const ENTRY_SIZE: usize = 32;
/// Segment file suffix.
const LOG_SUFFIX: &str = ".wal";

/// Fixed-size entry header view.
#[derive(Clone, Copy)]
struct RawEntry {
    term: u64,
    index: u64,
    data_offset: u64,
    typ: u64,
}

impl RawEntry {
    fn load(data: &[u8], idx: usize) -> Self {
        debug_assert!(idx < MAX_NUM_ENTRIES);
        let off = idx * ENTRY_SIZE;
        let word = |at: usize| {
            u64::from_be_bytes(
                data[off + at..off + at + 8]
                    .try_into()
                    .expect("entry header in bounds"),
            )
        };
        Self {
            term: word(0),
            index: word(8),
            data_offset: word(16),
            typ: word(24),
        }
    }

    fn store(data: &mut [u8], idx: usize, term: u64, index: u64, offset: u64, typ: u64) {
        debug_assert!(idx < MAX_NUM_ENTRIES);
        let off = idx * ENTRY_SIZE;
        for (slot, value) in [(0, term), (8, index), (16, offset), (24, typ)] {
            data[off + slot..off + slot + 8].copy_from_slice(&value.to_be_bytes());
        }
    }
}

/// Where a raft index lands inside one segment file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SlotPosition {
    /// Below this file's first index (look in an older file).
    BeforeStart,
    /// Slot `usize` holds it, follows it, or is empty past the tail.
    At(usize),
    /// Above this file's last index (look in a newer file).
    PastEnd,
}

/// One `NNNNN.wal` segment.
#[derive(Debug)]
pub(crate) struct LogFile {
    mmap: MmapFile,
    fid: i64,
}

/// Segment file name for `fid` inside `dir`.
fn log_fname(dir: &Path, fid: i64) -> PathBuf {
    dir.join(format!("{fid:05}{LOG_SUFFIX}"))
}

/// Opens the segment `fid` in `dir`, creating it when missing.
pub(crate) fn open_log_file(dir: &Path, fid: i64) -> Result<LogFile, StorageError> {
    let (mut mmap, is_new) = MmapFile::open(&log_fname(dir, fid), LOG_FILE_SIZE)?;
    if is_new {
        mmap.zero_out(0, LOG_FILE_OFFSET);
    } else {
        let data = mmap.data();
        let key_id = u64::from_be_bytes(
            data[ENC_OFFSET..ENC_OFFSET + 8]
                .try_into()
                .expect("key id in bounds"),
        );
        if key_id != 0 {
            return Err(StorageError::EncryptedWal);
        }
    }
    Ok(LogFile { mmap, fid })
}

impl LogFile {
    /// Segment id (file number).
    pub(crate) fn fid(&self) -> i64 {
        self.fid
    }

    fn raw_entry(&self, idx: usize) -> RawEntry {
        RawEntry::load(self.mmap.data(), idx)
    }

    /// Decodes the raft entry at slot `idx` (empty slots yield index 0).
    pub(crate) fn raft_entry(&self, idx: usize) -> Entry {
        let raw = self.raw_entry(idx);
        let entry_type = i32::try_from(raw.typ)
            .ok()
            .and_then(|typ| EntryType::try_from(typ).ok())
            .unwrap_or(EntryType::EntryNormal);
        let mut entry = Entry {
            term: raw.term,
            index: raw.index,
            entry_type: entry_type as i32,
            data: Vec::new(),
        };
        if raw.data_offset > 0 {
            let offset = usize::try_from(raw.data_offset).expect("64-bit address space");
            debug_assert!(offset < self.mmap.len());
            // Copy out so the mapping can be deleted later (matches Go).
            entry.data.extend_from_slice(self.mmap.slice(offset));
        }
        entry
    }

    /// Raw payload offset of slot `idx` (index GC needs it header-direct).
    pub(crate) fn slot_data_offset(&self, idx: usize) -> usize {
        usize::try_from(self.raw_entry(idx).data_offset).expect("64-bit address space")
    }

    /// Zeroes the 4-byte length prefix at `offset` (index GC).
    pub(crate) fn zero_payload_len(&mut self, offset: usize) {
        self.mmap.zero_out(offset, offset + 4);
    }

    /// First index stored in this file (0 when empty).
    pub(crate) fn first_index(&self) -> u64 {
        self.raw_entry(0).index
    }

    /// First slot whose index is 0 (binary-searched; `== MAX` when full).
    fn first_empty_slot(&self) -> usize {
        let mut lo = 0;
        let mut hi = MAX_NUM_ENTRIES;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.raw_entry(mid).index == 0 {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        lo
    }

    /// Index of the last populated slot (0 when the file is empty).
    fn last_entry_index(&self) -> u64 {
        let pos = self.first_empty_slot();
        self.raw_entry(pos.saturating_sub(1)).index
    }

    /// Slot holding `raft_index`, or where it would land.
    fn slot_ge(&self, raft_index: u64) -> SlotPosition {
        let first = self.first_index();
        if first == 0 || raft_index < first {
            return SlotPosition::BeforeStart;
        }
        // Fast path: dense prefixes land exactly at `raft_index - first`.
        if let Some(diff) = raft_index
            .checked_sub(first)
            .filter(|diff| *diff < MAX_NUM_ENTRIES_U64)
            .and_then(|diff| usize::try_from(diff).ok())
        {
            if self.raw_entry(diff).index == raft_index {
                return SlotPosition::At(diff);
            }
        }
        // First slot that is empty or at/above `raft_index`.
        let mut lo = 0;
        let mut hi = MAX_NUM_ENTRIES;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let index = self.raw_entry(mid).index;
            if index == 0 || index >= raft_index {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        if lo >= MAX_NUM_ENTRIES {
            SlotPosition::PastEnd
        } else {
            SlotPosition::At(lo)
        }
    }

    /// Zeroes header slots `[from, until)`.
    fn zero_slots(&mut self, from: usize, until: usize) {
        self.mmap.zero_out(from * ENTRY_SIZE, until * ENTRY_SIZE);
    }

    /// Appends one payload, storing its header at slot `idx`.
    ///
    /// Returns the payload offset following this entry's data.
    fn write_entry(
        &mut self,
        idx: usize,
        term: u64,
        index: u64,
        typ: u64,
        data: &[u8],
        offset: usize,
    ) -> Result<usize, StorageError> {
        let (start, next) = self.mmap.allocate_slice(data.len(), offset)?;
        self.mmap.data_mut()[start..start + data.len()].copy_from_slice(data);
        let stored = u64::try_from(offset).map_err(|_| StorageError::OffsetOverflow)?;
        RawEntry::store(self.mmap.data_mut(), idx, term, index, stored, typ);
        Ok(next)
    }

    /// Payload end offset following slot `idx` (for chained appends).
    fn data_end_after(&self, idx: usize) -> usize {
        let raw = self.raw_entry(idx);
        let offset = usize::try_from(raw.data_offset).expect("64-bit address space");
        offset.saturating_add(slice_size(self.mmap.data(), offset))
    }

    /// Truncates the file to `len` bytes (rotation trims preallocated space).
    fn truncate(&mut self, len: u64) -> io::Result<()> {
        self.mmap.truncate(len)
    }

    /// Syncs mappings and file contents to disk.
    pub(crate) fn sync(&self) -> io::Result<()> {
        self.mmap.sync()
    }

    /// Unmaps and deletes the segment file.
    fn delete(self) -> io::Result<()> {
        self.mmap.delete()
    }
}

/// All segments in `dir`, ordered by first index ascending.
pub(crate) fn get_log_files(dir: &Path) -> Result<Vec<LogFile>, StorageError> {
    let mut files = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let entries = std::fs::read_dir(dir)?;
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(stem) = name.strip_suffix(LOG_SUFFIX) else {
            continue;
        };
        let fid: i64 =
            stem.parse().map_err(
                |err: std::num::ParseIntError| StorageError::BadSegmentName {
                    name: name.clone(),
                    reason: err.to_string(),
                },
            )?;
        if !seen.insert(fid) {
            return Err(StorageError::DuplicateSegment(fid));
        }
        files.push(open_log_file(dir, fid)?);
    }
    files.sort_by_key(LogFile::first_index);
    Ok(files)
}

/// Entry point for `wal.rs`: segment-level helpers needing cross-file logic.
impl LogFile {
    pub(crate) fn slot_position(&self, raft_index: u64) -> SlotPosition {
        self.slot_ge(raft_index)
    }

    pub(crate) fn empty_slot(&self) -> usize {
        self.first_empty_slot()
    }

    pub(crate) fn tail_index(&self) -> u64 {
        self.last_entry_index()
    }

    pub(crate) fn clear_slots(&mut self, from: usize, until: usize) {
        self.zero_slots(from, until);
    }

    pub(crate) fn append_slot(
        &mut self,
        idx: usize,
        term: u64,
        index: u64,
        typ: u64,
        data: &[u8],
        offset: usize,
    ) -> Result<usize, StorageError> {
        self.write_entry(idx, term, index, typ, data, offset)
    }

    pub(crate) fn payload_end(&self, idx: usize) -> usize {
        self.data_end_after(idx)
    }

    pub(crate) fn trim_to(&mut self, len: u64) -> io::Result<()> {
        self.truncate(len)
    }

    pub(crate) fn remove(self) -> io::Result<()> {
        self.delete()
    }
}
