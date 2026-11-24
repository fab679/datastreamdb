//! Memory-mapped file wrapper backing the WAL and meta files.
//!
//! Port of the `z.MmapFile` usage in `raftwal`: fixed-size sparse mapping
//! that grows (and shrinks) on demand, with length-prefixed slice helpers.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use memmap2::{MmapMut, MmapOptions};

/// Length-prefix width for slices stored in mapped files.
const SLICE_LEN_WIDTH: usize = 4;

/// A memory-mapped file that can grow, shrink, sync, and be deleted.
pub(crate) struct MmapFile {
    path: PathBuf,
    file: File,
    mmap: MmapMut,
}

impl std::fmt::Debug for MmapFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MmapFile")
            .field("path", &self.path)
            .field("len", &self.mmap.len())
            .finish_non_exhaustive()
    }
}

impl MmapFile {
    /// Opens (creating) `path` with at least `size` bytes of mapping.
    ///
    /// Returns the mapping plus whether the file was newly created.
    pub(crate) fn open(path: &Path, size: u64) -> io::Result<(Self, bool)> {
        let is_new = !path.exists();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        if is_new || file.metadata()?.len() < size {
            file.set_len(size)?;
        }
        // SAFETY: `map_mut` borrows the file only to establish the mapping;
        // `MmapMut` owns the mapping afterwards, and `file` is kept alive in
        // the same struct so the mapping stays valid.
        let mmap = unsafe { MmapOptions::new().map_mut(&file)? };
        Ok((
            Self {
                path: path.to_owned(),
                file,
                mmap,
            },
            is_new,
        ))
    }

    /// Current mapping length in bytes.
    pub(crate) fn len(&self) -> usize {
        self.mmap.len()
    }

    /// Read view of the mapping.
    pub(crate) fn data(&self) -> &[u8] {
        &self.mmap
    }

    /// Mutable view of the mapping.
    pub(crate) fn data_mut(&mut self) -> &mut [u8] {
        &mut self.mmap
    }

    /// Zeroes `[start, end)` in the mapping.
    pub(crate) fn zero_out(&mut self, start: usize, end: usize) {
        self.mmap[start..end].fill(0);
    }

    /// Remaps the file after its length changed on disk.
    fn remap(&mut self) -> io::Result<()> {
        // SAFETY: same contract as [`MmapFile::open`]; the old mapping is
        // replaced (unmapped) only after the new one is established.
        let fresh = unsafe { MmapOptions::new().map_mut(&self.file)? };
        self.mmap = fresh;
        Ok(())
    }

    /// Grows the mapping so `len` bytes are addressable.
    pub(crate) fn ensure(&mut self, len: usize) -> io::Result<()> {
        if self.mmap.len() >= len {
            return Ok(());
        }
        let mut new_len = self.mmap.len().max(1);
        while new_len < len {
            new_len = new_len.saturating_mul(2);
        }
        self.file.set_len(new_len as u64)?;
        self.remap()
    }

    /// Resizes the file (grow or shrink) and remaps it.
    pub(crate) fn truncate(&mut self, len: u64) -> io::Result<()> {
        self.mmap.flush()?;
        self.file.set_len(len)?;
        self.remap()
    }

    /// Flushes the mapping and fsyncs the file.
    pub(crate) fn sync(&self) -> io::Result<()> {
        self.mmap.flush()?;
        self.file.sync_all()
    }

    /// Unmaps and deletes the file.
    pub(crate) fn delete(self) -> io::Result<()> {
        drop(self.mmap);
        drop(self.file);
        std::fs::remove_file(&self.path)
    }

    /// Reads the length-prefixed slice stored at `offset`.
    ///
    /// Layout: big-endian `u32` length followed by the payload bytes.
    pub(crate) fn slice(&self, offset: usize) -> &[u8] {
        let len = slice_len(&self.mmap, offset);
        &self.mmap[offset + SLICE_LEN_WIDTH..offset + SLICE_LEN_WIDTH + len]
    }

    /// Reserves room for a `data_len`-byte slice at `offset`.
    ///
    /// Writes the length prefix and returns `(data_start, next_offset)`.
    pub(crate) fn allocate_slice(
        &mut self,
        data_len: usize,
        offset: usize,
    ) -> io::Result<(usize, usize)> {
        let end = offset + SLICE_LEN_WIDTH + data_len;
        self.ensure(end)?;
        let prefix = u32::try_from(data_len)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "slice larger than 4 GiB"))?;
        self.mmap[offset..offset + SLICE_LEN_WIDTH].copy_from_slice(&prefix.to_be_bytes());
        Ok((offset + SLICE_LEN_WIDTH, end))
    }
}

/// Payload length of the slice stored at `offset`.
pub(crate) fn slice_len(data: &[u8], offset: usize) -> usize {
    let raw = u32::from_be_bytes(
        data[offset..offset + SLICE_LEN_WIDTH]
            .try_into()
            .expect("slice header in bounds"),
    );
    raw as usize
}

/// Total stored width (header + payload) of the slice at `offset`.
pub(crate) fn slice_size(data: &[u8], offset: usize) -> usize {
    SLICE_LEN_WIDTH + slice_len(data, offset)
}
