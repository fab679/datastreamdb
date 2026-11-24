//! `live`: stream RDF/JSON files into an embedded engine.
//!
//! Go's live loader tails chunk files into batched `Mutate` RPCs
//! against a running cluster; this binary commits batches straight
//! into an embedded engine over `--data-dir` (`:memory:` for
//! ephemeral loads). Remote `--alpha` mode arrives with the gRPC
//! client epic.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "live", about = "Stream RDF/JSON files into an engine")]
struct Args {
    /// RDF or JSON source files (plain, `.gz`, or encrypted + `.gz`).
    #[arg(required = true)]
    files: Vec<PathBuf>,
    /// Schema file installed before loading.
    #[arg(short, long)]
    schema: Option<PathBuf>,
    /// Target database directory (`:memory:` for ephemeral).
    #[arg(long, default_value = "p")]
    data_dir: PathBuf,
    /// N-Quads per committed batch.
    #[arg(short, long, default_value_t = datastreamdb_loader::live::DEFAULT_BATCH_NQUADS)]
    batch: usize,
    /// Remote alpha address (reserved; remote mode is not implemented).
    #[arg(short, long, default_value = "")]
    alpha: String,
    /// Encryption key file for encrypted inputs (Go parity).
    #[arg(long)]
    encryption_key_file: Option<PathBuf>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    if !args.alpha.is_empty() {
        anyhow::bail!("remote --alpha live-load arrives with the gRPC client epic");
    }
    let backend: Arc<dyn datastreamdb_store::Backend> = if args.data_dir.as_os_str() == ":memory:" {
        Arc::new(datastreamdb_store::MemoryBackend::new())
    } else {
        Arc::new(
            datastreamdb_store::FjallBackend::open(&args.data_dir)
                .with_context(|| format!("cannot open {}", args.data_dir.display()))?,
        )
    };
    let schema = Arc::new(datastreamdb_schema::SchemaState::new());
    let engine = datastreamdb_posting::PostingEngine::open(backend, schema, 64);
    let encryption_key = args
        .encryption_key_file
        .as_deref()
        .map(datastreamdb_enc::keys::load_key_file)
        .transpose()
        .map_err(|err| anyhow::anyhow!("bad --encryption-key-file: {err}"))?;
    let config = datastreamdb_loader::live::LiveConfig {
        files: args.files,
        schema_file: args.schema,
        batch_nquads: args.batch.max(1),
        encryption_key,
    };
    let stats = datastreamdb_loader::live::load_live(&engine, &config)
        .map_err(|err| anyhow::anyhow!("live load failed: {err}"))?;
    println!(
        "live load complete: {} files, {} nquads, {} batches, {} edges",
        stats.files, stats.nquads, stats.batches, stats.edges
    );
    Ok(())
}
