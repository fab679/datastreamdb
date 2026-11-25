//! Multi-zero tests: join, replication, failover.

use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;

use datastreamdb_proto::pb::{Num, num::LeaseType, zero_client::ZeroClient};

/// Monotonic test-port allocator; avoids immediate reuse of OS ephemeral ports.
static NEXT_PORT: AtomicU16 = AtomicU16::new(20_000);

/// Boots a zero; `join` peers or bootstraps when empty.
async fn boot_zero(
    dir: &tempfile::TempDir,
    sub: &str,
    bootstrap: bool,
    join: Vec<String>,
) -> (datastreamdb_server::ZeroServer, String, String) {
    let grpc = free_addr().await;
    let http = free_addr().await;
    let server = datastreamdb_server::ZeroServer::boot(datastreamdb_server::ZeroConfig {
        http_addr: http.parse().expect("addr"),
        grpc_addr: grpc.parse().expect("addr"),
        my_addr: grpc.clone(),
        wal_dir: dir.path().join(sub),
        bootstrap,
        join,
        num_replicas: 1,
        heartbeat_ms: 50,
        election_ms: (150, 300),
    })
    .await
    .expect("zero boot");
    (server, grpc, http)
}

/// Returns a loopback address that is currently bindable and not recently
/// handed out by this test binary.
async fn free_addr() -> String {
    loop {
        let port = NEXT_PORT.fetch_add(1, Ordering::Relaxed);
        if port == 0 {
            continue;
        }
        let addr = format!("127.0.0.1:{port}");
        if tokio::net::TcpListener::bind(&addr).await.is_ok() {
            return addr;
        }
    }
}

/// Zero members visible in `server`'s state.
fn zero_members(server: &datastreamdb_server::ZeroServer) -> Vec<u64> {
    let mut ids: Vec<u64> = server
        .state
        .snapshot_state()
        .zeros
        .keys()
        .copied()
        .collect();
    ids.sort_unstable();
    ids
}

/// Leases one UID through the first zero that serves (followers
/// reject with `NotLeader`; callers retry across members).
async fn lease_uid(addrs: &[String]) -> u64 {
    let mut last_err = String::new();
    for addr in addrs {
        let endpoint =
            tonic::transport::Endpoint::from_shared(format!("http://{addr}")).expect("endpoint");
        let Ok(channel) = endpoint.connect().await else {
            last_err = format!("{addr} unreachable");
            continue;
        };
        let mut client = ZeroClient::new(channel);
        match client
            .assign_ids(Num {
                val: 1,
                r#type: LeaseType::Uid as i32,
                ..Num::default()
            })
            .await
        {
            Ok(response) => return response.into_inner().start_id,
            Err(err) => last_err = err.to_string(),
        }
    }
    panic!("no zero served a lease: {last_err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn zero_join_replicates_and_survives_one_loss() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (zero1, grpc1, _http1) = boot_zero(&dir, "zw1", true, Vec::new()).await;
    let (zero2, grpc2, _http2) = boot_zero(&dir, "zw2", false, vec![grpc1.clone()]).await;
    let (zero3, grpc3, _http3) = boot_zero(&dir, "zw3", false, vec![grpc1.clone()]).await;
    let addrs = vec![grpc1.clone(), grpc2.clone(), grpc3.clone()];

    // All zeros converge on three members.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        if zero_members(&zero1).len() == 3
            && zero_members(&zero2).len() == 3
            && zero_members(&zero3).len() == 3
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "members never converged: {:?} vs {:?} vs {:?}",
            zero_members(&zero1),
            zero_members(&zero2),
            zero_members(&zero3)
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Leases advance monotonically (replicated oracle).
    let first = lease_uid(&addrs).await;
    let second = lease_uid(&addrs).await;
    assert!(second >= first, "{second} vs {first}");

    // Losing one zero leaves a live two-voter cluster.
    zero1.shutdown().await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let survivors = vec![grpc2, grpc3];
    let third = lease_uid(&survivors).await;
    assert!(third >= second, "{third} vs {second}");

    zero2.shutdown().await;
    zero3.shutdown().await;
}

#[test]
fn zero_ids_are_stable_and_nonzero() {
    let first = datastreamdb_server::zero_server::zero_id_for("127.0.0.1:5080");
    let again = datastreamdb_server::zero_server::zero_id_for("127.0.0.1:5080");
    let other = datastreamdb_server::zero_server::zero_id_for("127.0.0.1:5081");
    assert_eq!(first, again);
    assert_ne!(first, 0);
    assert_ne!(first, other);
}
