//! `bulk`: load RDF/JSON files into a fresh database.
//!
//! Go's bulk loader maps RDF over sharded stores and reduces per
//! group; the embedded single-group form infers the schema in one
//! streaming pass and commits batches in a second pass (see
//! `datastreamdb-loader`). Sharded map/shuffle/reduce arrives with the
//! multi-group scale epic.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "bulk", about = "Load RDF/JSON files into a fresh database")]
struct Args {
    /// RDF or JSON source files (plain, `.gz`, or encrypted + `.gz`).
    #[arg(required = true)]
    files: Vec<PathBuf>,
    /// Explicit schema file (skips inference).
    #[arg(short, long)]
    schema: Option<PathBuf>,
    /// Target database directory (`:memory:` for ephemeral).
    #[arg(long, default_value = "p")]
    data_dir: PathBuf,
    /// N-Quads per committed batch.
    #[arg(short, long, default_value_t = datastreamdb_loader::live::DEFAULT_BATCH_NQUADS)]
    batch: usize,
    /// Encryption key file for encrypted inputs (Go parity).
    #[arg(long)]
    encryption_key_file: Option<PathBuf>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
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
    let config = datastreamdb_loader::bulk::BulkConfig {
        files: args.files,
        schema_file: args.schema,
        batch_nquads: args.batch.max(1),
        encryption_key,
    };
    let stats = datastreamdb_loader::bulk::load_bulk(&engine, &config)
        .map_err(|err| anyhow::anyhow!("bulk load failed: {err}"))?;
    println!(
        "bulk load complete: {} files, {} nquads, {} batches, {} edges ({} inferred, {} conflicts)",
        stats.live.files,
        stats.live.nquads,
        stats.live.batches,
        stats.live.edges,
        stats.inferred,
        stats.conflicts
    );
    Ok(())
}
