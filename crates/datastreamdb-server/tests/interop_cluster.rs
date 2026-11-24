//! Cluster interop: real zero+alpha bring-up driven over the wire.
//!
//! These tests boot actual servers on ephemeral ports and speak to them
//! with generated tonic clients (dgo-compatible request shapes) and HTTP,
//! covering alter/mutate/query/commit, multi-alpha replication, and
//! restart recovery.

#[path = "common/mod.rs"]
mod common;

use std::time::Duration;

use common::{boot_alpha, boot_zero, channel};
use datastreamdb_proto::api::{
    Mutation, Operation, Request, data_stream_db_client::DataStreamDbClient,
};

/// Polls a DQL query until `needle` appears (delta streams are async).
async fn wait_visible(client: &mut DataStreamDbClient<tonic::transport::Channel>, needle: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let response = client
            .query(Request {
                query: "{ q(func: has(name)) { uid name } }".to_owned(),
                ..Request::default()
            })
            .await
            .expect("query")
            .into_inner();
        let body = String::from_utf8_lossy(&response.json).into_owned();
        if body.contains(needle) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "never became visible: {needle} (last: {body})"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cluster_grpc_roundtrip() {
    let (_zdir, zgrpc, _) = boot_zero(1).await;
    let (_adir, agrpc, _) = boot_alpha(&zgrpc, 1, true).await;
    let mut client = DataStreamDbClient::new(channel(&agrpc).await);

    client
        .alter(Operation {
            schema: "name: string @index(exact) .".to_owned(),
            ..Operation::default()
        })
        .await
        .expect("alter");

    let response = client
        .query(Request {
            mutations: vec![Mutation {
                set_nquads: b"_:a <name> \"Ada\" .".to_vec(),
                commit_now: true,
                ..Mutation::default()
            }],
            commit_now: true,
            ..Request::default()
        })
        .await
        .expect("mutate")
        .into_inner();
    let txn = response.txn.expect("txn");
    assert!(!txn.aborted);
    assert!(txn.commit_ts > 0);
    assert_eq!(response.uids.len(), 1);

    // Deltas stream asynchronously; the read below waits for them.
    wait_visible(&mut client, "Ada").await;

    // Explicit commit flow with conflict-free data.
    let response = client
        .query(Request {
            mutations: vec![Mutation {
                set_nquads: b"_:b <name> \"Grace\" .".to_vec(),
                ..Mutation::default()
            }],
            ..Request::default()
        })
        .await
        .expect("mutate")
        .into_inner();
    let txn = response.txn.expect("txn");
    let committed = client
        .commit_or_abort(txn)
        .await
        .expect("commit")
        .into_inner();
    assert!(!committed.aborted);
    wait_visible(&mut client, "Grace").await;

    // Indexed lookup over the wire.
    let response = client
        .query(Request {
            query: "{ q(func: eq(name, \"Ada\")) { uid name } }".to_owned(),
            ..Request::default()
        })
        .await
        .expect("query")
        .into_inner();
    assert!(String::from_utf8_lossy(&response.json).contains("Ada"));

    // Abort flow: staged data never lands.
    let response = client
        .query(Request {
            mutations: vec![Mutation {
                set_nquads: b"_:c <name> \"Dropped\" .".to_vec(),
                ..Mutation::default()
            }],
            ..Request::default()
        })
        .await
        .expect("mutate")
        .into_inner();
    let mut txn = response.txn.expect("txn");
    txn.aborted = true;
    let aborted = client
        .commit_or_abort(txn)
        .await
        .expect("abort")
        .into_inner();
    assert!(aborted.aborted);

    // Version handshake.
    let version = client
        .check_version(datastreamdb_proto::api::Check {})
        .await
        .expect("version")
        .into_inner();
    assert!(!version.tag.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cluster_http_roundtrip() {
    let (_zdir, zgrpc, zhttp) = boot_zero(1).await;
    let (_adir, _agrpc, ahttp) = boot_alpha(&zgrpc, 1, true).await;
    // Sandboxes proxy localhost; bypass it for direct testing.
    let http = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("client");

    let health: serde_json::Value = http
        .get(format!("http://{ahttp}/health"))
        .send()
        .await
        .expect("health")
        .json()
        .await
        .expect("health json");
    assert_eq!(health["status"], "ok");

    let state: serde_json::Value = http
        .get(format!("http://{zhttp}/state"))
        .send()
        .await
        .expect("state")
        .json()
        .await
        .expect("state json");
    assert!(state.get("groups").is_some());

    http.post(format!("http://{ahttp}/alter"))
        .body("name: string @index(exact) .")
        .send()
        .await
        .expect("alter")
        .error_for_status()
        .expect("alter status");

    let receipt: serde_json::Value = http
        .post(format!("http://{ahttp}/mutate?commitNow=true"))
        .header("content-type", "application/rdf")
        .body("_:a <name> \"Http\" .")
        .send()
        .await
        .expect("mutate")
        .json()
        .await
        .expect("mutate json");
    assert_eq!(receipt["extensions"]["txn"]["aborted"], false);

    // Poll: the commit delta streams asynchronously.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let body: serde_json::Value = http
            .post(format!("http://{ahttp}/query"))
            .body("{ q(func: has(name)) { uid name } }")
            .send()
            .await
            .expect("query")
            .json()
            .await
            .expect("query json");
        if body.to_string().contains("Http") {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "never became visible: {body}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_alpha_group_replicates() {
    let (_zdir, zgrpc, _) = boot_zero(3).await;
    let (_dir1, grpc1, _) = boot_alpha(&zgrpc, 1, true).await;
    let (_dir2, grpc2, _) = boot_alpha(&zgrpc, 1, false).await;
    let mut one = DataStreamDbClient::new(channel(&grpc1).await);
    let mut two = DataStreamDbClient::new(channel(&grpc2).await);

    // The joiner needs a moment to seat (JoinCluster + membership).
    tokio::time::sleep(Duration::from_secs(2)).await;

    one.alter(Operation {
        schema: "name: string .".to_owned(),
        ..Operation::default()
    })
    .await
    .expect("alter");

    // Mutate via alpha 1 (leader or forwarder), read via alpha 2.
    let response = one
        .query(Request {
            mutations: vec![Mutation {
                set_nquads: b"_:a <name> \"One\" .".to_vec(),
                commit_now: true,
                ..Mutation::default()
            }],
            commit_now: true,
            ..Request::default()
        })
        .await
        .expect("mutate")
        .into_inner();
    assert!(!response.txn.expect("txn").aborted);
    wait_visible(&mut two, "One").await;

    // Mutate via alpha 2 (forwards when follower), read via alpha 1.
    let response = two
        .query(Request {
            mutations: vec![Mutation {
                set_nquads: b"_:b <name> \"Two\" .".to_vec(),
                commit_now: true,
                ..Mutation::default()
            }],
            commit_now: true,
            ..Request::default()
        })
        .await
        .expect("mutate")
        .into_inner();
    assert!(!response.txn.expect("txn").aborted);
    wait_visible(&mut one, "Two").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_recovers_data_and_leases() {
    use datastreamdb_proto::pb::{Num, num::LeaseType};

    let zdir = tempfile::tempdir().expect("tempdir");
    let zgrpc = common::free_addr().await;
    let zhttp = common::free_addr().await;
    let server = datastreamdb_server::ZeroServer::boot(datastreamdb_server::ZeroConfig {
        http_addr: zhttp.parse().expect("addr"),
        grpc_addr: zgrpc.parse().expect("addr"),
        my_addr: zgrpc.clone(),
        wal_dir: zdir.path().join("zw"),
        bootstrap: true,
        join: Vec::new(),
        num_replicas: 1,
        heartbeat_ms: 50,
        election_ms: (150, 300),
    })
    .await
    .expect("zero boot");
    let zero_task = tokio::spawn(async move {
        server.serve().await.expect("zero serve");
    });
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Lease once, restart, lease again: ranges must not overlap.
    let mut zero = datastreamdb_proto::pb::zero_client::ZeroClient::new(channel(&zgrpc).await);
    let first = zero
        .assign_ids(Num {
            val: 10,
            r#type: LeaseType::Uid as i32,
            ..Num::default()
        })
        .await
        .expect("lease")
        .into_inner();
    zero_task.abort();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let server = datastreamdb_server::ZeroServer::boot(datastreamdb_server::ZeroConfig {
        http_addr: zhttp.parse().expect("addr"),
        grpc_addr: zgrpc.parse().expect("addr"),
        my_addr: zgrpc.clone(),
        wal_dir: zdir.path().join("zw"),
        bootstrap: true,
        join: Vec::new(),
        num_replicas: 1,
        heartbeat_ms: 50,
        election_ms: (150, 300),
    })
    .await
    .expect("zero reboot");
    tokio::spawn(async move {
        server.serve().await.expect("zero serve");
    });
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut zero = datastreamdb_proto::pb::zero_client::ZeroClient::new(channel(&zgrpc).await);
    let second = zero
        .assign_ids(Num {
            val: 10,
            r#type: LeaseType::Uid as i32,
            ..Num::default()
        })
        .await
        .expect("lease")
        .into_inner();
    assert!(
        second.start_id > first.end_id,
        "lease reused after restart: {first:?} then {second:?}"
    );
}
