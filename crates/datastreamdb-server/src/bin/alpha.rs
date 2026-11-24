//! `alpha`: serve graph queries and mutations.

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Parser, ValueEnum};
use datastreamdb_frontend::server::{AlphaConfig, AlphaServer};
use datastreamdb_worker::{MutationsMode, Options as WorkerOptions};

/// Mutation handling mode (mirrors `--mutations`).
#[derive(Debug, Clone, Copy, ValueEnum)]
enum MutationsArg {
    /// Allow all mutations.
    Allow,
    /// Reject all mutations.
    Disallow,
    /// Allow only mutations touching known predicates.
    Strict,
}

impl From<MutationsArg> for MutationsMode {
    fn from(arg: MutationsArg) -> Self {
        match arg {
            MutationsArg::Allow => Self::Allow,
            MutationsArg::Disallow => Self::Disallow,
            MutationsArg::Strict => Self::Strict,
        }
    }
}

#[derive(Debug, Parser)]
#[command(name = "alpha", about = "Serve graph queries and mutations")]
struct Args {
    /// This alpha's advertised address (ip:grpc-port).
    #[arg(long, default_value = "127.0.0.1:7080")]
    my: String,
    /// Zero address(es), comma-separated.
    #[arg(long, default_value = "127.0.0.1:5080", value_delimiter = ',')]
    zero: Vec<String>,
    /// Posting directory (`:memory:` for the memory backend).
    #[arg(short, long, default_value = "p")]
    postings: PathBuf,
    /// WAL directory for the group log.
    #[arg(short, long, default_value = "w")]
    wal: PathBuf,
    /// HTTP address.
    #[arg(long, default_value = "127.0.0.1:8080")]
    http: SocketAddr,
    /// gRPC address for every service.
    #[arg(long, default_value = "127.0.0.1:7080")]
    grpc: SocketAddr,
    /// Raft group to serve (0 asks zero to place us).
    #[arg(long, default_value_t = 0)]
    group: u32,
    /// Bootstrap a fresh group instead of joining one.
    #[arg(long, default_value_t = true)]
    bootstrap: bool,
    /// Mutation handling mode.
    #[arg(long, value_enum, default_value_t = MutationsArg::Allow)]
    mutations: MutationsArg,
    /// Cache size in MiB shared by posting caches.
    #[arg(long, default_value_t = 1024)]
    cache_mb: i64,
    /// Raft heartbeat interval in milliseconds.
    #[arg(long, default_value_t = 100)]
    heartbeat_ms: u64,
    /// Raft election timeout range in milliseconds.
    #[arg(long, default_value = "1000,2000", value_delimiter = ',')]
    election_ms: Vec<u64>,
    /// ACL superflag (`secret-file=...; access-ttl=6h; refresh-ttl=30d;
    /// jwt-alg=HS256`); empty disables ACL.
    #[arg(long, default_value = "")]
    acl: String,
    /// Audit superflag (`output=...; days=10; size=100`); empty output
    /// disables auditing.
    #[arg(long, default_value = "")]
    audit: String,
    /// Encryption key file (16/24/32 raw bytes) for backups/exports;
    /// absent leaves them plaintext (Go `--encryption-key-file`).
    #[arg(long)]
    encryption_key_file: Option<PathBuf>,
    /// CDC superflag (`file=...`; empty disables CDC, like Go's
    /// empty `--cdc`).
    #[arg(long, default_value = "")]
    cdc: String,
}

/// Parses Go `--acl` superflag pairs (`k=v`, `;`-separated).
fn parse_acl_flag(flag: &str) -> anyhow::Result<datastreamdb_frontend::auth::AclSettings> {
    use std::collections::HashMap;
    let mut settings = datastreamdb_frontend::auth::AclSettings::default();
    let pairs: HashMap<String, String> = flag
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .map(|(key, value)| (key.trim().to_owned(), value.trim().to_owned()))
        .collect();
    if let Some(path) = pairs.get("secret-file").filter(|s| !s.is_empty()) {
        settings.secret = std::fs::read(path)
            .map_err(|err| anyhow::anyhow!("reading ACL secret file {path}: {err}"))?;
    }
    if settings.secret.is_empty() {
        anyhow::bail!("--acl needs secret-file pointing at the HMAC secret or private key");
    }
    if let Some(alg) = pairs.get("jwt-alg") {
        settings.alg.clone_from(alg);
    }
    if let Some(ttl) = pairs.get("access-ttl") {
        settings.access_ttl_secs = parse_duration_secs(ttl)?;
    }
    if let Some(ttl) = pairs.get("refresh-ttl") {
        settings.refresh_ttl_secs = parse_duration_secs(ttl)?;
    }
    Ok(settings)
}

/// Parses Go-style durations (`30s`, `2m`, `6h`, `30d`).
fn parse_duration_secs(text: &str) -> anyhow::Result<u64> {
    let (number, unit) = text
        .find(|c: char| c.is_alphabetic())
        .map(|idx| text.split_at(idx))
        .ok_or_else(|| anyhow::anyhow!("bad duration {text:?}"))?;
    let number: u64 = number
        .parse()
        .map_err(|_| anyhow::anyhow!("bad duration {text:?}"))?;
    let factor = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3_600,
        "d" => 86_400,
        _ => anyhow::bail!("bad duration {text:?}"),
    };
    Ok(number.saturating_mul(factor))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = Args::parse();
    let acl = if args.acl.is_empty() {
        None
    } else {
        Some(parse_acl_flag(&args.acl)?)
    };
    let audit = datastreamdb_audit::parse_audit_flag(&args.audit)
        .map_err(|err| anyhow::anyhow!("bad --audit flag: {err}"))?;
    let encryption_key = args
        .encryption_key_file
        .as_ref()
        .map(|path| {
            datastreamdb_enc::keys::load_key_file(path)
                .map_err(|err| anyhow::anyhow!("bad --encryption-key-file: {err}"))
        })
        .transpose()?;
    let config = AlphaConfig {
        http_addr: args.http,
        grpc_addr: args.grpc,
        my_addr: args.my,
        zero_addrs: args.zero,
        posting_dir: args.postings,
        wal_dir: args.wal,
        group_id: args.group,
        bootstrap: args.bootstrap,
        heartbeat_ms: 100,
        election_ms: (1000, 2000),
        acl,
        audit,
        options: WorkerOptions {
            posting_dir: PathBuf::from("p"),
            wal_dir: PathBuf::from("w"),
            mutations_mode: MutationsMode::from(args.mutations),
            auth_token: String::new(),
            cache_mb: args.cache_mb,
            remove_on_update: false,
            type_filter_uid_limit: 1000,
            encryption_key,
            cdc_conf: args.cdc,
        },
    };
    let election_ms = match args.election_ms.as_slice() {
        [min, max] => (*min, *max),
        _ => anyhow::bail!("--election-ms needs exactly two values (min,max)"),
    };
    // Worker options mirror the top-level dirs (validated together).
    let mut config = config;
    config.heartbeat_ms = args.heartbeat_ms;
    config.election_ms = election_ms;
    config.options.posting_dir = config.posting_dir.clone();
    config.options.wal_dir = config.wal_dir.clone();
    let server = AlphaServer::boot(config)
        .await
        .map_err(|err| anyhow::anyhow!("{err}"))?;
    tracing::info!("alpha serving");
    server.serve().await.map_err(|err| anyhow::anyhow!("{err}"))
}
