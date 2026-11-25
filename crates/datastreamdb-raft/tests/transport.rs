//! Tonic transport test: two nodes replicate over TCP.

#![allow(unknown_lints)]
#![allow(clippy::unused_async_trait_impl)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use datastreamdb_raft::{Applier, Node, TonicFactory, TransportServer};

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

/// Binds an ephemeral loopback port and returns its address.
async fn free_addr() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr").to_string();
    drop(listener);
    addr
}

#[tokio::test]
async fn two_nodes_replicate_over_tonic() {
    let addr1 = free_addr().await;
    let addr2 = free_addr().await;
    let dir1 = tempfile::tempdir().expect("tempdir");
    let dir2 = tempfile::tempdir().expect("tempdir");

    let node1 = Node::open(
        1,
        dir1.path(),
        test_config(),
        VecApplier::default(),
        TonicFactory::new(1),
    )
    .await
    .expect("open");
    let applier2 = VecApplier::default();
    let node2 = Node::open(
        2,
        dir2.path(),
        test_config(),
        applier2.clone(),
        TonicFactory::new(2),
    )
    .await
    .expect("open");

    for (node, addr) in [(&node1, addr1.clone()), (&node2, addr2.clone())] {
        let server = TransportServer::new(node.handle());
        let socket: std::net::SocketAddr = addr.parse().expect("addr");
        tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(
                    datastreamdb_raft::network::transport::raft_transport_server::RaftTransportServer::new(
                        server,
                    ),
                )
                .serve(socket)
                .await
                .expect("serve");
        });
    }
    // Let both listeners come up before the first vote RPCs.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let voters: BTreeMap<u64, String> = [(1, addr1), (2, addr2)].into_iter().collect();
    node1.initialize(voters).await.expect("initialize");
    // Retry like a real client: the first proposes can land before the
    // leader commits its blank entry (not yet writable).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let leader = node1
            .wait_for_leader(Duration::from_secs(5))
            .await
            .expect("leader");
        let writer = if leader == 1 { &node1 } else { &node2 };
        match writer.propose(b"over-tcp".to_vec()).await {
            Ok(_) => break,
            Err(datastreamdb_raft::NodeError::NotLeader { .. }) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "leader never became writable"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(other) => panic!("propose failed: {other:?}"),
        }
    }

    // Node 2 applies once a later heartbeat carries the commit index.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if applier2.applied() == vec![b"over-tcp".to_vec()] {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "node 2 never applied the entry"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
