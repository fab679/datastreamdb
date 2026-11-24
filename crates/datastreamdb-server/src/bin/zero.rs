//! `zero`: cluster membership, timestamps, tablets.

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;
use datastreamdb_server::{ZeroConfig, ZeroServer};

#[derive(Debug, Parser)]
#[command(
    name = "zero",
    about = "Cluster coordination: membership, leases, tablets"
)]
struct Args {
    /// This zero's advertised address (ip:grpc-port).
    #[arg(long, default_value = "127.0.0.1:5080")]
    my: String,
    /// WAL directory for the zero group log.
    #[arg(short, long, default_value = "zw")]
    wal: PathBuf,
    /// HTTP address.
    #[arg(long, default_value = "127.0.0.1:6080")]
    http: SocketAddr,
    /// gRPC address.
    #[arg(long, default_value = "127.0.0.1:5080")]
    grpc: SocketAddr,
    /// Bootstrap a fresh zero group instead of joining one.
    #[arg(long, default_value_t = true)]
    bootstrap: bool,
    /// Peer zero gRPC addrs to join through (with `--no-bootstrap`).
    #[arg(long, value_delimiter = ',')]
    join: Vec<String>,
    /// Alpha replicas wanted per group (Connect placement).
    #[arg(long, default_value_t = 1)]
    replicas: u32,
    /// Raft heartbeat interval in milliseconds.
    #[arg(long, default_value_t = 100)]
    heartbeat_ms: u64,
    /// Raft election timeout range in milliseconds.
    #[arg(long, default_value = "1000,2000", value_delimiter = ',')]
    election_ms: Vec<u64>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = Args::parse();
    if args.replicas % 2 == 0 {
        anyhow::bail!(
            "number of replicas must be odd for consensus, found: {}",
            args.replicas
        );
    }
    let election_ms = match args.election_ms.as_slice() {
        [min, max] => (*min, *max),
        _ => anyhow::bail!("--election-ms needs exactly two values (min,max)"),
    };
    let config = ZeroConfig {
        http_addr: args.http,
        grpc_addr: args.grpc,
        my_addr: args.my,
        wal_dir: args.wal,
        bootstrap: args.bootstrap,
        join: args.join,
        num_replicas: args.replicas,
        heartbeat_ms: args.heartbeat_ms,
        election_ms,
    };
    let server = ZeroServer::boot(config)
        .await
        .map_err(|err| anyhow::anyhow!("{err}"))?;
    tracing::info!("zero serving");
    server.serve().await.map_err(|err| anyhow::anyhow!("{err}"))
}
