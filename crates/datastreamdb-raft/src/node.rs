//! One raft group member: openraft core over the WAL log store.
//!
//! [`Node`] is generic over the [`Applier`] that gives the group its
//! meaning (zero group vs alpha group). Proposals are opaque bytes;
//! leadership, replication, and snapshots are openraft's job.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use openraft::error::{ClientWriteError, RaftError};
use openraft::impls::BasicNode;
use openraft::network::RaftNetworkFactory;
use openraft::{Config, Membership};

use crate::log_store::WalLogStore;
use crate::state_machine::{Applier, SmStore};
use crate::storage::{DiskStorage, StorageError};
use crate::types::{DataStreamDBTypeConfig, GroupRaft, Proposal};

/// Node failures: routing, timeouts, and fatal raft/storage errors.
#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    /// This node is not the leader; retry on `leader` when known.
    #[error("not the leader{}", .leader.map_or(String::new(), |id| format!("; leader is {id}")))]
    NotLeader {
        /// Known leader, if any.
        leader: Option<u64>,
    },
    /// The group was already initialized (restart with stale bootstrap).
    #[error("group already initialized")]
    AlreadyInitialized,
    /// The operation timed out.
    #[error("timed out after {0:?}")]
    Timeout(Duration),
    /// Raft core reported a fatal error.
    #[error("raft fatal: {0}")]
    Fatal(String),
    /// Storage failure.
    #[error("storage failed: {0}")]
    Storage(#[from] StorageError),
    /// I/O failure (sidecars, snapshots).
    #[error("I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

/// Maps a client-write failure to [`NodeError`].
fn write_error(
    err: RaftError<DataStreamDBTypeConfig, ClientWriteError<DataStreamDBTypeConfig>>,
) -> NodeError {
    match err {
        RaftError::APIError(ClientWriteError::ForwardToLeader(forward)) => NodeError::NotLeader {
            leader: forward.leader_id,
        },
        other => NodeError::Fatal(other.to_string()),
    }
}

/// One member of a raft group.
pub struct Node<A: Applier> {
    /// This member's raft ID.
    id: u64,
    /// Storage directory (for diagnostics).
    dir: PathBuf,
    /// openraft handle (cloneable; drives replication tasks).
    raft: GroupRaft<A>,
}

impl<A: Applier> Clone for Node<A> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            dir: self.dir.clone(),
            raft: self.raft.clone(),
        }
    }
}

impl<A: Applier> Node<A> {
    /// Opens (creating) the member's storage in `dir` and starts raft.
    ///
    /// The node stays a follower until [`Node::initialize`] seeds the
    /// cluster membership (single call on one member bootstraps the group).
    ///
    /// # Errors
    ///
    /// Returns [`NodeError`] when storage cannot be opened or raft fails
    /// to start.
    pub async fn open<N>(
        id: u64,
        dir: &Path,
        config: Arc<Config>,
        applier: A,
        network: N,
    ) -> Result<Self, NodeError>
    where
        N: RaftNetworkFactory<
                DataStreamDBTypeConfig,
                Network: openraft::network::NetSnapshot<
                    DataStreamDBTypeConfig,
                    SnapshotData = Vec<u8>,
                >,
            >,
    {
        let storage = Arc::new(DiskStorage::init(dir)?);
        let log_store = WalLogStore::new(Arc::clone(&storage), dir);
        let state_machine = SmStore::open(applier, dir).await?;
        let raft = GroupRaft::new(id, config, network, log_store, state_machine)
            .await
            .map_err(|err| NodeError::Fatal(err.to_string()))?;
        Ok(Self {
            id,
            dir: dir.to_owned(),
            raft,
        })
    }

    /// This member's raft ID.
    #[must_use]
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Storage directory.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Clones the inner openraft handle (for transports and tests).
    #[must_use]
    pub fn handle(&self) -> GroupRaft<A> {
        self.raft.clone()
    }

    /// Seeds cluster membership (call once, on one member, to bootstrap).
    ///
    /// `voters` maps raft IDs to transport addresses.
    ///
    /// # Errors
    ///
    /// Returns [`NodeError`] when the member is not alone, when this node
    /// is not in `voters`, or when the write is lost or rejected.
    /// [`NodeError::AlreadyInitialized`] on restarts is benign.
    pub async fn initialize(&self, voters: BTreeMap<u64, String>) -> Result<(), NodeError> {
        let nodes: BTreeMap<u64, BasicNode> = voters
            .into_iter()
            .map(|(id, addr)| (id, BasicNode { addr }))
            .collect();
        let voter_ids: BTreeSet<u64> = nodes.keys().copied().collect();
        let membership = Membership::new(vec![voter_ids], nodes)
            .map_err(|err| NodeError::Fatal(err.to_string()))?;
        self.raft
            .initialize(membership)
            .await
            .map_err(|err| match err {
                RaftError::APIError(openraft::error::InitializeError::NotAllowed(_)) => {
                    NodeError::AlreadyInitialized
                }
                other => NodeError::Fatal(other.to_string()),
            })
    }

    /// Proposes bytes and waits for the apply response.
    ///
    /// # Errors
    ///
    /// Returns [`NodeError::NotLeader`] when another member (or none)
    /// leads, or [`NodeError::Fatal`] when the write is lost.
    pub async fn propose(&self, data: Vec<u8>) -> Result<Vec<u8>, NodeError> {
        let response = self
            .raft
            .client_write(Proposal(data))
            .await
            .map_err(write_error)?;
        Ok(response.data)
    }

    /// Adds a voter (address included) to the cluster.
    ///
    /// # Errors
    ///
    /// Returns [`NodeError`] when this node does not lead or the change
    /// is rejected.
    pub async fn add_voter(&self, id: u64, addr: String) -> Result<(), NodeError> {
        let edit =
            openraft::ChangeMembers::AddVoters([(id, BasicNode { addr })].into_iter().collect());
        self.raft
            .change_membership(edit, false)
            .await
            .map_err(write_error)?;
        Ok(())
    }

    /// Removes a voter from the cluster (it stays a learner when `retain`).
    ///
    /// # Errors
    ///
    /// Returns [`NodeError`] when this node does not lead or the change
    /// is rejected.
    pub async fn remove_voter(&self, id: u64, retain: bool) -> Result<(), NodeError> {
        let edit = openraft::ChangeMembers::RemoveVoters([id].into_iter().collect());
        self.raft
            .change_membership(edit, retain)
            .await
            .map_err(write_error)?;
        Ok(())
    }

    /// Current leader as seen by this member, if any is established.
    pub async fn current_leader(&self) -> Option<u64> {
        self.raft.current_leader().await
    }

    /// Current raft term as seen by this member.
    #[must_use]
    pub fn current_term(&self) -> u64 {
        use openraft::async_runtime::WatchReceiver as _;
        self.raft.metrics().borrow_watched().current_term
    }

    /// Whether this member currently leads the cluster.
    pub async fn is_leader(&self) -> bool {
        self.current_leader().await == Some(self.id)
    }

    /// Waits for any member to establish leadership.
    ///
    /// # Errors
    ///
    /// Returns [`NodeError::Timeout`] when no leader appears in time.
    pub async fn wait_for_leader(&self, timeout: Duration) -> Result<u64, NodeError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Some(leader) = self.current_leader().await {
                return Ok(leader);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(NodeError::Timeout(timeout));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Forces a snapshot of the state machine (testing and tooling).
    ///
    /// # Errors
    ///
    /// Returns [`NodeError::Fatal`] when the snapshot cannot complete.
    pub async fn trigger_snapshot(&self) -> Result<(), NodeError> {
        self.raft
            .trigger()
            .snapshot()
            .await
            .map_err(|err| NodeError::Fatal(err.to_string()))
    }

    /// Shuts the member down, stopping replication tasks.
    pub async fn shutdown(self) {
        let _ = self.raft.shutdown().await;
    }
}
