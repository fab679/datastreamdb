//! openraft log store on top of the WAL.
//!
//! [`WalLogStore`] maps openraft entries to WAL segments: the log id's term
//! and index ride in the entry header, while the leader node ID and payload
//! are JSON-encoded in the entry data. Votes reuse the meta hard-state
//! words (`term`, `vote`, granted-flag-as-`commit`); purge/commit pointers
//! live in a `raft-log.json` sidecar next to the segments.
//!
//! Index base: openraft log indexes are 0-based, while the Go WAL reserves
//! index 0 as the empty-slot marker. Every conversion here adds one going
//! into the WAL and subtracts one coming out, so the WAL files stay
//! byte-compatible with Go while openraft sees its native 0-based log.

use std::io;
use std::ops::Bound;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use datastreamdb_proto::raftpb::{Entry as WalEntry, EntryType, HardState, Snapshot};
use openraft::OptionalSend;
use openraft::impls::Vote;
use openraft::impls::leader_id_adv::LeaderId;
use openraft::storage::{IOFlushed, LogState, RaftLogReader, RaftLogStorage};
use serde::{Deserialize, Serialize};

use crate::storage::DiskStorage;
use crate::types::{DataStreamDBTypeConfig, GroupEntry, GroupLogId, GroupPayload, GroupVote};

/// Sidecar file holding purge/commit pointers.
const LOG_STATE_NAME: &str = "raft-log.json";

/// JSON envelope stored in each WAL entry's data bytes.
///
/// The WAL header carries term + index only, so the committing leader's
/// node ID travels here alongside the openraft payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredPayload {
    /// Node that led the entry's term.
    leader_node: u64,
    /// Blank, normal (proposal bytes), or membership payload.
    payload: GroupPayload,
}

/// Durable log pointers that do not fit the Go WAL layout.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct LogSidecarState {
    /// Greatest purged log id, if any.
    last_purged: Option<GroupLogId>,
    /// Greatest committed log id, if any.
    committed: Option<GroupLogId>,
}

/// `raft-log.json` sidecar: load-modify-store per operation.
#[derive(Debug, Clone)]
struct LogSidecar {
    path: PathBuf,
}

impl LogSidecar {
    fn new(dir: &Path) -> Self {
        Self {
            path: dir.join(LOG_STATE_NAME),
        }
    }

    fn load(&self) -> io::Result<LogSidecarState> {
        match std::fs::read(&self.path) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(LogSidecarState::default()),
            Err(err) => Err(err),
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|err| io::Error::other(format!("corrupt {LOG_STATE_NAME}: {err}"))),
        }
    }

    fn store(&self, state: &LogSidecarState) -> io::Result<()> {
        let bytes = serde_json::to_vec_pretty(state)
            .map_err(|err| io::Error::other(format!("encode {LOG_STATE_NAME}: {err}")))?;
        std::fs::write(&self.path, bytes)
    }
}

/// Maps storage failures into the `io::Error` openraft expects.
fn io_other(err: impl std::fmt::Display) -> io::Error {
    io::Error::other(err.to_string())
}

/// openraft entry → WAL entry (0-based index shifted to 1-based).
fn to_wal(entry: &GroupEntry) -> io::Result<WalEntry> {
    let stored = StoredPayload {
        leader_node: entry.log_id.leader_id.node_id,
        payload: entry.payload.clone(),
    };
    let data =
        serde_json::to_vec(&stored).map_err(|err| io_other(format!("encode entry: {err}")))?;
    Ok(WalEntry {
        term: entry.log_id.leader_id.term,
        index: entry.log_id.index.saturating_add(1),
        entry_type: EntryType::EntryNormal as i32,
        data,
    })
}

/// WAL entry → openraft entry (1-based index shifted to 0-based).
fn from_wal(entry: &WalEntry) -> io::Result<GroupEntry> {
    let stored: StoredPayload = serde_json::from_slice(&entry.data)
        .map_err(|err| io_other(format!("decode entry: {err}")))?;
    Ok(GroupEntry {
        log_id: GroupLogId {
            leader_id: LeaderId {
                term: entry.term,
                node_id: stored.leader_node,
            },
            index: entry.index.saturating_sub(1),
        },
        payload: stored.payload,
    })
}

/// WAL-backed openraft log store.
pub struct WalLogStore {
    /// Durable segments + meta.
    storage: Arc<DiskStorage>,
    /// Purge/commit pointers.
    sidecar: LogSidecar,
}

impl WalLogStore {
    /// Opens a log store over `storage` with sidecars in `dir`.
    pub fn new(storage: Arc<DiskStorage>, dir: &Path) -> Self {
        Self {
            storage,
            sidecar: LogSidecar::new(dir),
        }
    }

    /// Decodes the tail entry, if the WAL holds any.
    fn last_entry(&self) -> io::Result<Option<GroupEntry>> {
        let last = self.storage.last_index().map_err(io_other)?;
        if last == 0 {
            return Ok(None);
        }
        let entries = self
            .storage
            .entries(last, last.saturating_add(1), u64::MAX)
            .map_err(io_other)?;
        entries.first().map(from_wal).transpose()
    }
}

impl RaftLogStorage<DataStreamDBTypeConfig> for WalLogStore {
    type LogReader = WalLogReader;

    async fn get_log_state(&mut self) -> Result<LogState<DataStreamDBTypeConfig>, io::Error> {
        let state = self.sidecar.load()?;
        let last_log_id = match self.last_entry()? {
            Some(entry) => Some(entry.log_id),
            None => state.last_purged,
        };
        Ok(LogState {
            last_purged_log_id: state.last_purged,
            last_log_id,
        })
    }

    async fn get_log_reader(&mut self) -> Self::LogReader {
        WalLogReader {
            storage: Arc::clone(&self.storage),
            sidecar: self.sidecar.clone(),
        }
    }

    async fn save_vote(&mut self, vote: &GroupVote) -> Result<(), io::Error> {
        let hard_state = HardState {
            term: vote.leader_id.term,
            vote: vote.leader_id.node_id,
            commit: u64::from(vote.committed),
        };
        self.storage
            .save(&hard_state, &[], &Snapshot::default())
            .map_err(io_other)
    }

    async fn save_committed(&mut self, committed: Option<GroupLogId>) -> Result<(), io::Error> {
        let mut state = self.sidecar.load()?;
        state.committed = committed;
        self.sidecar.store(&state)
    }

    async fn read_committed(&mut self) -> Result<Option<GroupLogId>, io::Error> {
        Ok(self.sidecar.load()?.committed)
    }

    async fn append<I>(
        &mut self,
        entries: I,
        callback: IOFlushed<DataStreamDBTypeConfig>,
    ) -> Result<(), io::Error>
    where
        I: IntoIterator<Item = GroupEntry> + OptionalSend,
        I::IntoIter: OptionalSend,
    {
        let mut wal_entries = Vec::new();
        for entry in entries {
            wal_entries.push(to_wal(&entry)?);
        }
        self.storage.add_entries(&wal_entries).map_err(io_other)?;
        // fsync before acknowledging: a committed entry must survive a crash.
        // (Throughput knob for later: group-commit across append batches.)
        self.storage.sync().map_err(io_other)?;
        callback.io_completed(Ok(()));
        Ok(())
    }

    async fn truncate_after(&mut self, last_log_id: Option<GroupLogId>) -> Result<(), io::Error> {
        self.storage
            .truncate_after(last_log_id.map(|id| id.index.saturating_add(1)))
            .map_err(io_other)
    }

    async fn purge(&mut self, log_id: GroupLogId) -> Result<(), io::Error> {
        let mut state = self.sidecar.load()?;
        let keep = match state.last_purged {
            Some(prev) if prev.index >= log_id.index => prev,
            _ => log_id,
        };
        state.last_purged = Some(keep);
        self.sidecar.store(&state)?;
        // Purge is inclusive: drop segments holding 0-based indexes up to
        // `log_id.index` (WAL indexes up to `index + 1`).
        self.storage
            .delete_before(log_id.index.saturating_add(2))
            .map_err(io_other)
    }
}

/// WAL-backed openraft log reader (shares the store's handle).
pub struct WalLogReader {
    /// Durable segments + meta.
    storage: Arc<DiskStorage>,
    /// Purge/commit pointers.
    sidecar: LogSidecar,
}

impl WalLogReader {
    /// Reads the persisted vote, if any node ever voted here.
    fn vote(&self) -> io::Result<Option<GroupVote>> {
        let hard_state = self.storage.hard_state().map_err(io_other)?;
        if hard_state.is_empty() {
            return Ok(None);
        }
        Ok(Some(Vote {
            leader_id: LeaderId {
                term: hard_state.term,
                node_id: hard_state.vote,
            },
            committed: hard_state.commit != 0,
        }))
    }
}

impl RaftLogReader<DataStreamDBTypeConfig> for WalLogReader {
    async fn try_get_log_entries<RB>(&mut self, range: RB) -> Result<Vec<GroupEntry>, io::Error>
    where
        RB: std::ops::RangeBounds<u64> + Clone + std::fmt::Debug + OptionalSend,
    {
        let start = match range.start_bound() {
            Bound::Included(&bound) => bound,
            Bound::Excluded(&bound) => bound.saturating_add(1),
            Bound::Unbounded => 0,
        };
        let end = match range.end_bound() {
            Bound::Included(&bound) => bound.saturating_add(1),
            Bound::Excluded(&bound) => bound,
            Bound::Unbounded => u64::MAX,
        };

        // Bounds below are 0-based openraft indexes; the WAL read shifts
        // them into the 1-based WAL space.
        let purged_next = self
            .sidecar
            .load()?
            .last_purged
            .map_or(0, |id| id.index.saturating_add(1));
        let wal_first = self.storage.first_index().map_err(io_other)?;
        let wal_last = self.storage.last_index().map_err(io_other)?;
        if wal_last == 0 {
            return Ok(Vec::new());
        }

        let lo = start.max(purged_next).max(wal_first.saturating_sub(1));
        // 0-based last present is `wal_last - 1`, so the exclusive cap is `wal_last`.
        let hi = end.min(wal_last);
        if lo >= hi {
            return Ok(Vec::new());
        }
        let entries = self
            .storage
            .entries(lo.saturating_add(1), hi.saturating_add(1), u64::MAX)
            .map_err(io_other)?;
        let mut out = Vec::with_capacity(entries.len());
        for (offset, entry) in entries.iter().enumerate() {
            let step = u64::try_from(offset).unwrap_or(u64::MAX);
            let want = lo.saturating_add(1).saturating_add(step);
            if entry.index != want {
                return Err(io_other(format!(
                    "hole in WAL: want index {want}, found {}",
                    entry.index
                )));
            }
            out.push(from_wal(entry)?);
        }
        Ok(out)
    }

    async fn read_vote(&mut self) -> Result<Option<GroupVote>, io::Error> {
        self.vote()
    }
}
