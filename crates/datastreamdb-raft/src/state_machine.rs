//! openraft state machine over an application [`Applier`].
//!
//! [`SmStore`] tracks the applied log id and membership, delegates normal
//! entries to the [`Applier`], and persists snapshots as `snapshot.bin` plus
//! a `raft-snapshot.json` pointer next to the WAL segments.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use futures::StreamExt as _;
use openraft::OptionalSend;
use openraft::storage::{RaftSnapshotBuilder, RaftStateMachine, SnapshotMeta};
use serde::{Deserialize, Serialize};

use crate::types::{
    DataStreamDBTypeConfig, GroupEntry, GroupLogId, GroupMembership, GroupSnapshot,
    GroupSnapshotMeta, GroupStoredMembership,
};

/// Snapshot payload file (single latest snapshot).
const SNAPSHOT_NAME: &str = "snapshot.bin";
/// Snapshot pointer sidecar.
const SNAPSHOT_META_NAME: &str = "raft-snapshot.json";

/// Application logic behind a raft group: apply, snapshot, restore.
///
/// Implementations must be deterministic: every member applies the same
/// committed entries in the same order and must reach the same state.
pub trait Applier: Clone + Send + Sync + 'static {
    /// Apply failure.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Applies one committed proposal, returning its response bytes.
    fn apply(&self, data: &[u8]) -> impl Future<Output = Result<Vec<u8>, Self::Error>> + Send;

    /// Captures the full applier state as snapshot bytes.
    fn snapshot(&self) -> impl Future<Output = Result<Vec<u8>, Self::Error>> + Send;

    /// Replaces applier state from snapshot bytes.
    fn restore(&self, snapshot: &[u8]) -> impl Future<Output = Result<(), Self::Error>> + Send;
}

/// Maps failures into the `io::Error` openraft expects.
fn io_other(err: impl std::fmt::Display) -> io::Error {
    io::Error::other(err.to_string())
}

/// Snapshot pointer: what `raft-snapshot.json` holds.
///
/// [`SnapshotMeta`](openraft::storage::SnapshotMeta) is deserialize-only in
/// openraft 0.10, so the sidecar stores its two fields directly.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SnapshotPointer {
    /// Last log id covered by `snapshot.bin`, if any.
    last_log_id: Option<GroupLogId>,
    /// Membership as of the snapshot.
    last_membership: GroupStoredMembership,
}

/// Directories and files backing [`SmStore`].
#[derive(Debug, Clone)]
struct SnapshotFiles {
    /// Snapshot payload path.
    data: PathBuf,
    /// Pointer sidecar path.
    meta: PathBuf,
}

impl SnapshotFiles {
    fn new(dir: &Path) -> Self {
        Self {
            data: dir.join(SNAPSHOT_NAME),
            meta: dir.join(SNAPSHOT_META_NAME),
        }
    }

    fn load_pointer(&self) -> io::Result<SnapshotPointer> {
        match std::fs::read(&self.meta) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(SnapshotPointer::default()),
            Err(err) => Err(err),
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|err| io_other(format!("corrupt {SNAPSHOT_META_NAME}: {err}"))),
        }
    }

    fn store_pointer(&self, pointer: &SnapshotPointer) -> io::Result<()> {
        let bytes = serde_json::to_vec_pretty(pointer)
            .map_err(|err| io_other(format!("encode {SNAPSHOT_META_NAME}: {err}")))?;
        std::fs::write(&self.meta, bytes)
    }

    fn load_data(&self) -> io::Result<Option<Vec<u8>>> {
        match std::fs::read(&self.data) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err),
            Ok(bytes) => Ok(Some(bytes)),
        }
    }

    fn store_data(&self, bytes: &[u8]) -> io::Result<()> {
        std::fs::write(&self.data, bytes)
    }
}

/// In-memory applied state; snapshots make restarts converge.
#[derive(Debug)]
struct AppliedState {
    /// Last applied log id, if any entry was ever applied.
    last_applied: Option<GroupLogId>,
    /// Last applied membership.
    last_membership: GroupStoredMembership,
}

/// openraft state machine delegating to an [`Applier`].
pub struct SmStore<A: Applier> {
    /// Applied-state tracking (also the snapshot builder's view).
    state: Arc<Mutex<AppliedState>>,
    /// Application apply/snapshot/restore logic.
    applier: A,
    /// Snapshot persistence.
    files: SnapshotFiles,
}

impl<A: Applier> SmStore<A> {
    /// Opens a state machine over `applier` with snapshots in `dir`.
    ///
    /// If a snapshot exists, the applier is restored from it and applied
    /// state resumes at its log id; the committed log suffix re-applies on
    /// top after restart.
    ///
    /// # Errors
    ///
    /// Returns [`io::Error`] when snapshot files cannot be read or the
    /// applier rejects the snapshot bytes.
    pub async fn open(applier: A, dir: &Path) -> io::Result<Self> {
        let files = SnapshotFiles::new(dir);
        let pointer = files.load_pointer()?;
        // A pointer without payload is an interrupted snapshot; ignore it.
        let state = match (pointer.last_log_id, files.load_data()?) {
            (Some(covered), Some(data)) => {
                applier
                    .restore(&data)
                    .await
                    .map_err(|err| io_other(format!("restore snapshot: {err}")))?;
                AppliedState {
                    last_applied: Some(covered),
                    last_membership: pointer.last_membership,
                }
            }
            _ => AppliedState {
                last_applied: None,
                last_membership: GroupStoredMembership::new(None, GroupMembership::default()),
            },
        };
        Ok(Self {
            state: Arc::new(Mutex::new(state)),
            applier,
            files,
        })
    }
}

impl<A: Applier> RaftStateMachine<DataStreamDBTypeConfig> for SmStore<A> {
    type SnapshotData = Vec<u8>;
    type SnapshotBuilder = SmSnapshotBuilder<A>;

    async fn applied_state(
        &mut self,
    ) -> Result<(Option<GroupLogId>, GroupStoredMembership), io::Error> {
        let state = self.state.lock().map_err(|_| io_other("sm lock"))?;
        Ok((state.last_applied, state.last_membership.clone()))
    }

    async fn apply<Strm>(&mut self, mut entries: Strm) -> Result<(), io::Error>
    where
        Strm: futures::Stream<
                Item = Result<
                    (
                        GroupEntry,
                        Option<openraft::storage::ApplyResponder<DataStreamDBTypeConfig>>,
                    ),
                    io::Error,
                >,
            > + Unpin
            + OptionalSend,
    {
        while let Some(item) = entries.next().await {
            let (entry, responder) = item?;
            let response = match &entry.payload {
                openraft::EntryPayload::Blank => Vec::new(),
                openraft::EntryPayload::Normal(data) => {
                    self.applier.apply(&data.0).await.map_err(io_other)?
                }
                openraft::EntryPayload::Membership(membership) => {
                    let mut state = self.state.lock().map_err(|_| io_other("sm lock"))?;
                    state.last_membership =
                        GroupStoredMembership::new(Some(entry.log_id), membership.clone());
                    Vec::new()
                }
            };
            {
                let mut state = self.state.lock().map_err(|_| io_other("sm lock"))?;
                state.last_applied = Some(entry.log_id);
            }
            if let Some(responder) = responder {
                responder.send(response);
            }
        }
        Ok(())
    }

    async fn get_snapshot_builder(&mut self) -> Self::SnapshotBuilder {
        SmSnapshotBuilder {
            state: Arc::clone(&self.state),
            applier: self.applier.clone(),
            files: self.files.clone(),
        }
    }

    async fn install_snapshot(
        &mut self,
        meta: &GroupSnapshotMeta,
        snapshot: Vec<u8>,
    ) -> Result<(), io::Error> {
        self.applier.restore(&snapshot).await.map_err(io_other)?;
        self.files.store_data(&snapshot)?;
        self.files.store_pointer(&SnapshotPointer {
            last_log_id: meta.last_log_id,
            last_membership: meta.last_membership.clone(),
        })?;
        let mut state = self.state.lock().map_err(|_| io_other("sm lock"))?;
        state.last_applied = meta.last_log_id;
        state.last_membership = meta.last_membership.clone();
        Ok(())
    }

    async fn get_current_snapshot(&mut self) -> Result<Option<GroupSnapshot>, io::Error> {
        let pointer = self.files.load_pointer()?;
        let Some(last_log_id) = pointer.last_log_id else {
            return Ok(None);
        };
        let Some(data) = self.files.load_data()? else {
            return Ok(None);
        };
        Ok(Some(GroupSnapshot {
            meta: SnapshotMeta {
                last_log_id: Some(last_log_id),
                last_membership: pointer.last_membership,
            },
            snapshot: data,
        }))
    }
}

/// Snapshot builder capturing the applier state at build time.
pub struct SmSnapshotBuilder<A: Applier> {
    /// Applied-state view (read at build time).
    state: Arc<Mutex<AppliedState>>,
    /// Application snapshot logic.
    applier: A,
    /// Snapshot persistence.
    files: SnapshotFiles,
}

impl<A: Applier> RaftSnapshotBuilder<DataStreamDBTypeConfig> for SmSnapshotBuilder<A> {
    type SnapshotData = Vec<u8>;

    async fn build_snapshot(&mut self) -> Result<GroupSnapshot, io::Error> {
        let data = self.applier.snapshot().await.map_err(io_other)?;
        let (last_applied, last_membership) = {
            let state = self.state.lock().map_err(|_| io_other("sm lock"))?;
            (state.last_applied, state.last_membership.clone())
        };
        self.files.store_data(&data)?;
        self.files.store_pointer(&SnapshotPointer {
            last_log_id: last_applied,
            last_membership: last_membership.clone(),
        })?;
        Ok(GroupSnapshot {
            meta: SnapshotMeta {
                last_log_id: last_applied,
                last_membership,
            },
            snapshot: data,
        })
    }
}
