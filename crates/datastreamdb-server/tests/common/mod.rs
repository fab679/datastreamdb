//! Shared bring-up for interop tests: ephemeral ports, zero, alphas.

// Each test binary uses a different subset of these helpers.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;

/// Monotonic test-port allocator; avoids immediate reuse of OS ephemeral ports.
static NEXT_PORT: AtomicU16 = AtomicU16::new(20_000);

/// Returns a loopback address that is currently bindable and not recently
/// handed out by this test binary.
pub async fn free_addr() -> String {
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

/// Fast raft timing for tests.
pub fn test_timing() -> (u64, (u64, u64)) {
    (50, (150, 300))
}

/// Boots a zero on ephemeral ports (tempdir WAL), serving in background.
pub async fn boot_zero(replicas: u32) -> (tempfile::TempDir, String, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let grpc = free_addr().await;
    let http = free_addr().await;
    let grpc_addr: SocketAddr = grpc.parse().expect("addr");
    let http_addr: SocketAddr = http.parse().expect("addr");
    let (heartbeat_ms, election_ms) = test_timing();
    let server = datastreamdb_server::ZeroServer::boot(datastreamdb_server::ZeroConfig {
        http_addr,
        grpc_addr,
        my_addr: grpc.clone(),
        wal_dir: dir.path().join("zw"),
        bootstrap: true,
        join: Vec::new(),
        num_replicas: replicas,
        heartbeat_ms,
        election_ms,
    })
    .await
    .expect("zero boot");
    tokio::spawn(async move {
        server.serve().await.expect("zero serve");
    });
    // Zero serves before alphas join it.
    tokio::time::sleep(Duration::from_millis(300)).await;
    (dir, grpc, http)
}

/// Boots an alpha against `zero_grpc` (tempdir WAL, memory postings).
#[allow(clippy::too_many_arguments)]
pub async fn boot_alpha(
    zero_grpc: &str,
    group_id: u32,
    bootstrap: bool,
) -> (tempfile::TempDir, String, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let grpc = free_addr().await;
    let http = free_addr().await;
    let grpc_addr: SocketAddr = grpc.parse().expect("addr");
    let http_addr: SocketAddr = http.parse().expect("addr");
    let (heartbeat_ms, election_ms) = test_timing();
    let server = datastreamdb_frontend::server::AlphaServer::boot(
        datastreamdb_frontend::server::AlphaConfig {
            http_addr,
            grpc_addr,
            my_addr: grpc.clone(),
            zero_addrs: vec![zero_grpc.to_owned()],
            posting_dir: PathBuf::from(":memory:"),
            wal_dir: dir.path().join("w"),
            group_id,
            bootstrap,
            heartbeat_ms,
            election_ms,
            acl: None,
            audit: datastreamdb_audit::AuditSettings::default(),
            options: datastreamdb_worker::Options {
                posting_dir: PathBuf::from(":memory:"),
                wal_dir: dir.path().join("w"),
                mutations_mode: datastreamdb_worker::MutationsMode::Allow,
                auth_token: String::new(),
                cache_mb: 64,
                remove_on_update: false,
                type_filter_uid_limit: 1000,
                encryption_key: None,
                cdc_conf: String::new(),
            },
        },
    )
    .await
    .expect("alpha boot");
    tokio::spawn(async move {
        server.serve().await.expect("alpha serve");
    });
    (dir, grpc, http)
}

/// Boots an alpha with ACL enabled (HS256 over `secret`).
pub async fn boot_alpha_acl(
    zero_grpc: &str,
    group_id: u32,
    bootstrap: bool,
    secret: Vec<u8>,
) -> (tempfile::TempDir, String, String) {
    boot_alpha_acl_audit(zero_grpc, group_id, bootstrap, secret, None).await
}

/// Boots an ACL alpha with audit output set (`None` disables auditing).
pub async fn boot_alpha_acl_audit(
    zero_grpc: &str,
    group_id: u32,
    bootstrap: bool,
    secret: Vec<u8>,
    audit_output: Option<String>,
) -> (tempfile::TempDir, String, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let grpc = free_addr().await;
    let http = free_addr().await;
    let grpc_addr: SocketAddr = grpc.parse().expect("addr");
    let http_addr: SocketAddr = http.parse().expect("addr");
    let (heartbeat_ms, election_ms) = test_timing();
    let server = datastreamdb_frontend::server::AlphaServer::boot(
        datastreamdb_frontend::server::AlphaConfig {
            http_addr,
            grpc_addr,
            my_addr: grpc.clone(),
            zero_addrs: vec![zero_grpc.to_owned()],
            posting_dir: PathBuf::from(":memory:"),
            wal_dir: dir.path().join("w"),
            group_id,
            bootstrap,
            heartbeat_ms,
            election_ms,
            acl: Some(datastreamdb_frontend::auth::AclSettings {
                alg: "HS256".to_owned(),
                secret,
                access_ttl_secs: 3_600,
                refresh_ttl_secs: 86_400,
            }),
            audit: datastreamdb_audit::AuditSettings {
                output: audit_output.unwrap_or_default(),
                ..datastreamdb_audit::AuditSettings::default()
            },
            options: datastreamdb_worker::Options {
                posting_dir: PathBuf::from(":memory:"),
                wal_dir: dir.path().join("w"),
                mutations_mode: datastreamdb_worker::MutationsMode::Allow,
                auth_token: String::new(),
                cache_mb: 64,
                remove_on_update: false,
                type_filter_uid_limit: 1000,
                encryption_key: None,
                cdc_conf: String::new(),
            },
        },
    )
    .await
    .expect("alpha boot");
    tokio::spawn(async move {
        server.serve().await.expect("alpha serve");
    });
    (dir, grpc, http)
}

/// Dials a tonic endpoint (test helper).
pub async fn channel(addr: &str) -> tonic::transport::Channel {
    tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
        .expect("endpoint")
        .connect()
        .await
        .expect("connect")
}
