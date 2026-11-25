//! Consensus tests: single node, channel clusters, restarts, snapshots.

#![allow(unknown_lints)]
#![allow(clippy::unused_async_trait_impl)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use datastreamdb_raft::{Applier, ChannelFactory, ChannelRegistry, Node, NodeError};

/// Applier recording proposals in memory; snapshots as JSON.
#[derive(Debug, Clone, Default)]
struct VecApplier {
    inner: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl VecApplier {
    fn applied(&self) -> Vec<Vec<u8>> {
        self.inner.lock().expect("applier lock").clone()
    }
}

impl Applier for VecApplier {
    type Error = std::convert::Infallible;

    async fn apply(&self, data: &[u8]) -> Result<Vec<u8>, Self::Error> {
        self.inner.lock().expect("applier lock").push(data.to_vec());
        Ok(data.to_vec())
    }

    async fn snapshot(&self) -> Result<Vec<u8>, Self::Error> {
        Ok(serde_json::to_vec(&*self.inner.lock().expect("applier lock")).expect("json"))
    }

    async fn restore(&self, snapshot: &[u8]) -> Result<(), Self::Error> {
        *self.inner.lock().expect("applier lock") = serde_json::from_slice(snapshot).expect("json");
        Ok(())
    }
}

fn test_config() -> Arc<openraft::Config> {
    Arc::new(openraft::Config {
        election_timeout_min: 150,
        election_timeout_max: 300,
        heartbeat_interval: 50,
        ..Default::default()
    })
}

fn voters(ids: &[u64]) -> BTreeMap<u64, String> {
    ids.iter()
        .map(|id| (*id, format!("channel:{id}")))
        .collect()
}

async fn leader_of(nodes: &[Node<VecApplier>]) -> Node<VecApplier> {
    let leader = nodes[0]
        .wait_for_leader(Duration::from_secs(10))
        .await
        .expect("leader");
    nodes
        .iter()
        .find(|node| node.id() == leader)
        .expect("leader node")
        .clone()
}

#[tokio::test]
async fn single_node_proposes_and_applies() {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry = ChannelRegistry::default();
    let applier = VecApplier::default();
    let node = Node::open(
        1,
        dir.path(),
        test_config(),
        applier.clone(),
        ChannelFactory::new(registry.clone()),
    )
    .await
    .expect("open");
    registry.register(1, node.handle());
    node.initialize(voters(&[1])).await.expect("initialize");
    assert_eq!(
        node.wait_for_leader(Duration::from_secs(10))
            .await
            .expect("leader"),
        1
    );

    let response = node.propose(b"hello".to_vec()).await.expect("propose");
    assert_eq!(response, b"hello");
    assert_eq!(applier.applied(), vec![b"hello".to_vec()]);
}

#[tokio::test]
async fn three_node_channel_cluster_replicates() {
    let dirs = [
        tempfile::tempdir().expect("tempdir"),
        tempfile::tempdir().expect("tempdir"),
        tempfile::tempdir().expect("tempdir"),
    ];
    let registry = ChannelRegistry::default();
    let mut nodes = Vec::new();
    let mut appliers = Vec::new();
    for (idx, dir) in dirs.iter().enumerate() {
        let id = idx as u64 + 1;
        let applier = VecApplier::default();
        let node = Node::open(
            id,
            dir.path(),
            test_config(),
            applier.clone(),
            ChannelFactory::new(registry.clone()),
        )
        .await
        .expect("open");
        registry.register(id, node.handle());
        nodes.push(node);
        appliers.push(applier);
    }
    nodes[0]
        .initialize(voters(&[1, 2, 3]))
        .await
        .expect("initialize");

    let leader = leader_of(&nodes).await;
    for value in [b"a".to_vec(), b"b".to_vec(), b"c".to_vec()] {
        leader.propose(value).await.expect("propose");
    }

    // Followers apply once later heartbeats carry the commit index.
    let want = vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    for applier in &appliers {
        loop {
            if applier.applied() == want {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "follower never applied all entries"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

#[tokio::test]
async fn follower_propose_reports_leader() {
    let dirs = [
        tempfile::tempdir().expect("tempdir"),
        tempfile::tempdir().expect("tempdir"),
        tempfile::tempdir().expect("tempdir"),
    ];
    let registry = ChannelRegistry::default();
    let mut nodes = Vec::new();
    for (idx, dir) in dirs.iter().enumerate() {
        let id = idx as u64 + 1;
        let node = Node::open(
            id,
            dir.path(),
            test_config(),
            VecApplier::default(),
            ChannelFactory::new(registry.clone()),
        )
        .await
        .expect("open");
        registry.register(id, node.handle());
        nodes.push(node);
    }
    nodes[0]
        .initialize(voters(&[1, 2, 3]))
        .await
        .expect("initialize");
    let leader = leader_of(&nodes).await;

    let follower = nodes
        .iter()
        .find(|node| node.id() != leader.id())
        .expect("follower");
    assert!(!follower.is_leader().await);
    match follower.propose(b"x".to_vec()).await {
        Err(NodeError::NotLeader { leader: Some(id) }) => assert_eq!(id, leader.id()),
        other => panic!("want NotLeader, got {other:?}"),
    }
}

#[tokio::test]
async fn restart_reapplies_committed_log() {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry = ChannelRegistry::default();
    {
        let node = Node::open(
            1,
            dir.path(),
            test_config(),
            VecApplier::default(),
            ChannelFactory::new(registry.clone()),
        )
        .await
        .expect("open");
        registry.register(1, node.handle());
        node.initialize(voters(&[1])).await.expect("initialize");
        node.wait_for_leader(Duration::from_secs(10))
            .await
            .expect("leader");
        node.propose(b"one".to_vec()).await.expect("propose");
        node.propose(b"two".to_vec()).await.expect("propose");
        node.shutdown().await;
        registry.unregister(1);
    }

    // Fresh applier, same directory: committed entries re-apply on startup.
    let applier = VecApplier::default();
    let node = Node::open(
        1,
        dir.path(),
        test_config(),
        applier.clone(),
        ChannelFactory::new(registry.clone()),
    )
    .await
    .expect("reopen");
    registry.register(1, node.handle());
    node.wait_for_leader(Duration::from_secs(10))
        .await
        .expect("leader");
    assert_eq!(applier.applied(), vec![b"one".to_vec(), b"two".to_vec()]);

    node.propose(b"three".to_vec()).await.expect("propose");
    assert_eq!(applier.applied().len(), 3);
}

#[tokio::test]
async fn snapshot_restores_applier_on_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry = ChannelRegistry::default();
    {
        let node = Node::open(
            1,
            dir.path(),
            test_config(),
            VecApplier::default(),
            ChannelFactory::new(registry.clone()),
        )
        .await
        .expect("open");
        registry.register(1, node.handle());
        node.initialize(voters(&[1])).await.expect("initialize");
        node.wait_for_leader(Duration::from_secs(10))
            .await
            .expect("leader");
        node.propose(b"snap-1".to_vec()).await.expect("propose");
        node.propose(b"snap-2".to_vec()).await.expect("propose");
        // Snapshot builds asynchronously; wait for the files before shutdown.
        node.trigger_snapshot().await.expect("snapshot");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            if dir.path().join("snapshot.bin").exists()
                && dir.path().join("raft-snapshot.json").exists()
            {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "snapshot files never appeared"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        node.shutdown().await;
        registry.unregister(1);
    }
    assert!(dir.path().join("snapshot.bin").exists());
    assert!(dir.path().join("raft-snapshot.json").exists());

    let applier = VecApplier::default();
    let node = Node::open(
        1,
        dir.path(),
        test_config(),
        applier.clone(),
        ChannelFactory::new(registry.clone()),
    )
    .await
    .expect("reopen");
    registry.register(1, node.handle());
    node.wait_for_leader(Duration::from_secs(10))
        .await
        .expect("leader");
    assert_eq!(
        applier.applied(),
        vec![b"snap-1".to_vec(), b"snap-2".to_vec()]
    );
}

#[tokio::test]
async fn membership_change_seats_a_new_voter() {
    let dir1 = tempfile::tempdir().expect("tempdir");
    let dir2 = tempfile::tempdir().expect("tempdir");
    let registry = ChannelRegistry::default();
    let node1 = Node::open(
        1,
        dir1.path(),
        test_config(),
        VecApplier::default(),
        ChannelFactory::new(registry.clone()),
    )
    .await
    .expect("open");
    registry.register(1, node1.handle());
    node1.initialize(voters(&[1])).await.expect("initialize");
    node1
        .wait_for_leader(Duration::from_secs(10))
        .await
        .expect("leader");

    let applier2 = VecApplier::default();
    let node2 = Node::open(
        2,
        dir2.path(),
        test_config(),
        applier2.clone(),
        ChannelFactory::new(registry.clone()),
    )
    .await
    .expect("open");
    registry.register(2, node2.handle());

    node1
        .add_voter(2, "channel:2".to_owned())
        .await
        .expect("add voter");
    node1.propose(b"joint".to_vec()).await.expect("propose");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if applier2.applied() == vec![b"joint".to_vec()] {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "new voter never applied the entry"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
