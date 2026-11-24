//! Live loader: stream RDF/JSON files into a live engine.
//!
//! Go's `dgraph/cmd/live` tails chunk files into batched `Mutate`
//! RPCs against a running cluster; the embedded form commits batches
//! straight into [`PostingEngine`].
//! Files decode through the filestore (gzip + AES-CTR aware), RDF
//! parses line by line, JSON parses per document, and blank nodes
//! resolve through one run-scoped [`XidMap`](datastreamdb_xidmap::XidMap).

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use datastreamdb_posting::PostingEngine;

use crate::assign::{advance_oracle_to_stored, batch_uid_map, max_quad_uid, seed_above};

/// Default N-Quads per committed batch.
pub const DEFAULT_BATCH_NQUADS: usize = 1000;

/// Live loader failures.
#[derive(Debug, thiserror::Error)]
pub enum LiveError {
    /// File discovery/read failure.
    #[error("live load file failed: {0}")]
    File(String),
    /// Chunk parse failure (`file:line: message`).
    #[error("live load parse failed: {0}")]
    Parse(String),
    /// Edge conversion failure.
    #[error("live load edge failed: {0}")]
    Edge(String),
    /// Commit failure.
    #[error("live load commit failed: {0}")]
    Commit(String),
    /// UID seeding failure.
    #[error("live load setup failed: {0}")]
    Setup(String),
    /// Schema file failure.
    #[error("live load schema failed: {0}")]
    Schema(String),
}

/// Live load configuration.
#[derive(Debug, Clone)]
pub struct LiveConfig {
    /// RDF/JSON files (plain, `.gz`, or encrypted + `.gz`).
    pub files: Vec<PathBuf>,
    /// Optional schema file installed before loading.
    pub schema_file: Option<PathBuf>,
    /// N-Quads per committed batch.
    pub batch_nquads: usize,
    /// Decryption key for encrypted inputs (`None` = plaintext).
    pub encryption_key: Option<datastreamdb_enc::keys::DataKey>,
}

impl Default for LiveConfig {
    fn default() -> Self {
        Self {
            files: Vec::new(),
            schema_file: None,
            batch_nquads: DEFAULT_BATCH_NQUADS,
            encryption_key: None,
        }
    }
}

/// Live load counters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiveStats {
    /// Files consumed.
    pub files: u64,
    /// N-Quads parsed.
    pub nquads: u64,
    /// Batches committed.
    pub batches: u64,
    /// Edges applied.
    pub edges: u64,
}

/// Whether `path` holds JSON (Go sniffs `.json`/`.json.gz`).
#[allow(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "Go matches extensions case-sensitively"
)]
fn is_json(path: &Path) -> bool {
    let name = path.to_string_lossy();
    let stem = name.strip_suffix(".gz").unwrap_or(&name);
    stem.ends_with(".json")
}

/// Parses one RDF stream into quad batches, committing each full batch.
fn load_rdf_stream<R: Read>(
    engine: &Arc<PostingEngine>,
    xids: &datastreamdb_xidmap::XidMap,
    file: &str,
    reader: R,
    batch_nquads: usize,
    stats: &mut LiveStats,
) -> Result<(), LiveError> {
    let mut lexer = datastreamdb_lex::Lexer::default();
    let mut buffered = BufReader::new(reader);
    let mut line = String::new();
    let mut line_no = 0u64;
    let mut batch: Vec<datastreamdb_chunker::NQuad> = Vec::new();
    loop {
        line.clear();
        let read = buffered
            .read_line(&mut line)
            .map_err(|err| LiveError::File(format!("{file}: {err}")))?;
        if read == 0 {
            break;
        }
        line_no += 1;
        match datastreamdb_chunker::parse_rdf(&line, &mut lexer) {
            Ok(quad) => batch.push(quad),
            Err(datastreamdb_chunker::ChunkError::Empty) => {}
            Err(err) => {
                return Err(LiveError::Parse(format!("{file}:{line_no}: {err}")));
            }
        }
        if batch.len() >= batch_nquads {
            commit_batch(engine, xids, &batch, stats)?;
            batch.clear();
        }
    }
    if !batch.is_empty() {
        commit_batch(engine, xids, &batch, stats)?;
    }
    Ok(())
}

/// Parses one JSON document and commits it in batches.
fn load_json_bytes(
    engine: &Arc<PostingEngine>,
    xids: &datastreamdb_xidmap::XidMap,
    file: &str,
    bytes: &[u8],
    batch_nquads: usize,
    stats: &mut LiveStats,
) -> Result<(), LiveError> {
    let (quads, _) = datastreamdb_chunker::parse_json(bytes, datastreamdb_chunker::Op::Set)
        .map_err(|err| LiveError::Parse(format!("{file}: {err}")))?;
    for batch in quads.chunks(batch_nquads.max(1)) {
        commit_batch(engine, xids, batch, stats)?;
    }
    Ok(())
}

/// Converts one batch to edges and commits it.
fn commit_batch(
    engine: &Arc<PostingEngine>,
    xids: &datastreamdb_xidmap::XidMap,
    batch: &[datastreamdb_chunker::NQuad],
    stats: &mut LiveStats,
) -> Result<(), LiveError> {
    xids.bump_to(max_quad_uid(batch));
    let uid_map = batch_uid_map(xids, batch);
    let txn = engine.begin();
    for quad in batch {
        let wrapped = datastreamdb_dql::NQuad(quad.clone());
        let mut edge = wrapped
            .to_edge_using(&uid_map)
            .map_err(|err| LiveError::Edge(err.to_string()))?;
        qualify_attr(&mut edge);
        let key = datastreamdb_x::keycodec::data_key(&edge.attr, edge.entity);
        txn.get(&key)
            .map_err(|err| LiveError::Commit(err.to_string()))?
            .add_mutation_with_index(&mut edge, &txn)
            .map_err(|err| LiveError::Commit(err.to_string()))?;
        stats.edges += 1;
    }
    engine
        .commit(&txn)
        .map_err(|err| LiveError::Commit(err.to_string()))?;
    stats.batches += 1;
    stats.nquads += batch.len() as u64;
    Ok(())
}

/// Prefixes the bare predicate with its namespace (`ns-pred`).
fn qualify_attr(edge: &mut datastreamdb_proto::DirectedEdge) {
    if edge.attr.contains('-') {
        return;
    }
    edge.attr = format!("{:x}-{}", edge.namespace, edge.attr);
}

/// Pre-scans every file for its highest explicit UID (streaming;
/// feeds the blank-node floor so blanks never take an explicit UID).
fn max_explicit_uid(
    files: &[PathBuf],
    key: Option<&datastreamdb_enc::keys::DataKey>,
) -> Result<u64, LiveError> {
    let mut max = 0;
    for file in files {
        let name = file.to_string_lossy().into_owned();
        let reader = datastreamdb_filestore::open_input(&name, key)
            .map_err(|err| LiveError::File(format!("{name}: {err}")))?;
        if is_json(file) {
            let mut bytes = Vec::new();
            let mut reader = reader;
            reader
                .read_to_end(&mut bytes)
                .map_err(|err| LiveError::File(format!("{name}: {err}")))?;
            let (quads, _) =
                datastreamdb_chunker::parse_json(bytes.as_slice(), datastreamdb_chunker::Op::Set)
                    .map_err(|err| LiveError::Parse(format!("{name}: {err}")))?;
            max = max.max(max_quad_uid(&quads));
        } else {
            let mut lexer = datastreamdb_lex::Lexer::default();
            let mut buffered = BufReader::new(reader);
            let mut line = String::new();
            let mut line_no = 0u64;
            loop {
                line.clear();
                let read = buffered
                    .read_line(&mut line)
                    .map_err(|err| LiveError::File(format!("{name}: {err}")))?;
                if read == 0 {
                    break;
                }
                line_no += 1;
                match datastreamdb_chunker::parse_rdf(&line, &mut lexer) {
                    Ok(quad) => max = max.max(max_quad_uid(std::slice::from_ref(&quad))),
                    Err(datastreamdb_chunker::ChunkError::Empty) => {}
                    Err(err) => {
                        return Err(LiveError::Parse(format!("{name}:{line_no}: {err}")));
                    }
                }
            }
        }
    }
    Ok(max)
}

/// Installs a schema file into the engine's schema state.
fn install_schema(engine: &PostingEngine, path: &Path) -> Result<(), LiveError> {
    let text = std::fs::read_to_string(path)
        .map_err(|err| LiveError::Schema(format!("{}: {err}", path.display())))?;
    let parsed = datastreamdb_schema::parse_with_namespace(&text, 0)
        .map_err(|err| LiveError::Schema(err.to_string()))?;
    for pred in parsed.preds {
        engine.schema().set(&pred.predicate.clone(), pred);
    }
    for typ in parsed.types {
        engine.schema().set_type(&typ.type_name.clone(), typ);
    }
    Ok(())
}

/// Streams `config.files` into `engine` (Go `live` loader, embedded).
///
/// # Errors
///
/// Returns [`LiveError`] on file, parse, edge, commit, or schema
/// failure. Successfully committed batches stay committed.
pub fn load_live(engine: &Arc<PostingEngine>, config: &LiveConfig) -> Result<LiveStats, LiveError> {
    let mut stats = LiveStats::default();
    if let Some(schema) = &config.schema_file {
        install_schema(engine, schema)?;
    }
    advance_oracle_to_stored(engine).map_err(|err| LiveError::Setup(err.to_string()))?;
    let floor = max_explicit_uid(&config.files, config.encryption_key.as_ref())?;
    let xids = seed_above(engine, floor).map_err(|err| LiveError::Setup(err.to_string()))?;
    for file in &config.files {
        let name = file.to_string_lossy().into_owned();
        let reader = datastreamdb_filestore::open_input(&name, config.encryption_key.as_ref())
            .map_err(|err| LiveError::File(format!("{name}: {err}")))?;
        if is_json(file) {
            let mut bytes = Vec::new();
            let mut reader = reader;
            reader
                .read_to_end(&mut bytes)
                .map_err(|err| LiveError::File(format!("{name}: {err}")))?;
            load_json_bytes(
                engine,
                &xids,
                &name,
                &bytes,
                config.batch_nquads,
                &mut stats,
            )?;
        } else {
            load_rdf_stream(
                engine,
                &xids,
                &name,
                reader,
                config.batch_nquads,
                &mut stats,
            )?;
        }
        stats.files += 1;
        tracing::info!("live-loaded {name} ({} nquads)", stats.nquads);
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_engine() -> Arc<PostingEngine> {
        let schema = Arc::new(datastreamdb_schema::SchemaState::new());
        schema
            .parse_bytes(b"name: string .\nage: int .")
            .expect("schema");
        PostingEngine::open(
            Arc::new(datastreamdb_store::MemoryBackend::new()),
            schema,
            64,
        )
    }

    fn write_temp(dir: &tempfile::TempDir, name: &str, contents: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, contents).expect("write");
        path
    }

    #[test]
    fn live_loads_rdf_batches() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rdf = write_temp(
            &dir,
            "data.rdf",
            "<0x2> <name> \"Alice\" .\n<0x3> <name> \"Bob\" .\n<0x2> <age> \"30\"^^<xs:int> .\n",
        );
        let engine = test_engine();
        let stats = load_live(
            &engine,
            &LiveConfig {
                files: vec![rdf],
                batch_nquads: 2,
                ..LiveConfig::default()
            },
        )
        .expect("load");
        assert_eq!(
            stats,
            LiveStats {
                files: 1,
                nquads: 3,
                batches: 2,
                edges: 3,
            }
        );
        let read_ts = engine.oracle().max_assigned();
        let key = datastreamdb_x::data_key("0-name", 2);
        let list = engine.get_list(&key, read_ts).expect("list");
        let values = list.all_values(read_ts).expect("values");
        assert_eq!(values.len(), 1);
    }

    #[test]
    fn live_resolves_blank_nodes_stably() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rdf = write_temp(
            &dir,
            "data.rdf",
            "_:alice <name> \"Alice\" .\n_:alice <age> \"30\"^^<xs:int> .\n",
        );
        let engine = test_engine();
        let stats = load_live(
            &engine,
            &LiveConfig {
                files: vec![rdf],
                ..LiveConfig::default()
            },
        )
        .expect("load");
        assert_eq!(stats.edges, 2);
        // Both quads landed on one node: exactly one name list exists.
        let pairs = engine
            .store()
            .scan_prefix(datastreamdb_posting::store::PARTITION, b"")
            .expect("scan");
        let name_lists = pairs
            .iter()
            .filter(|(key, _)| {
                datastreamdb_x::keycodec::parse(key)
                    .is_ok_and(|parsed| parsed.is_data() && parsed.attr == "0-name")
            })
            .count();
        assert_eq!(name_lists, 1);
    }

    #[test]
    fn live_keeps_blanks_above_explicit_uids() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Blank first, explicit 0x2 later, one quad per batch: the
        // blank must not take uid 2.
        let rdf = write_temp(
            &dir,
            "data.rdf",
            "_:early <name> \"Early\" .\n<0x2> <name> \"Alice\" .\n",
        );
        let engine = test_engine();
        load_live(
            &engine,
            &LiveConfig {
                files: vec![rdf],
                batch_nquads: 1,
                ..LiveConfig::default()
            },
        )
        .expect("load");
        let read_ts = engine.oracle().max_assigned();
        let key = datastreamdb_x::data_key("0-name", 2);
        let list = engine.get_list(&key, read_ts).expect("list");
        let values = list.all_values(read_ts).expect("values");
        assert_eq!(values.len(), 1);
        let pairs = engine
            .store()
            .scan_prefix(datastreamdb_posting::store::PARTITION, b"")
            .expect("scan");
        let name_uids: Vec<u64> = pairs
            .iter()
            .filter_map(|(key, _)| datastreamdb_x::keycodec::parse(key).ok())
            .filter(|parsed| parsed.is_data() && parsed.attr == "0-name")
            .map(|parsed| parsed.uid)
            .collect();
        assert_eq!(name_uids.len(), 2);
        assert!(name_uids.contains(&2));
        assert!(name_uids.iter().any(|uid| *uid > 2));
    }

    #[test]
    fn live_installs_schema_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let schema = write_temp(&dir, "schema.txt", "nick: string .\n");
        let rdf = write_temp(&dir, "data.rdf", "<0x2> <nick> \"Al\" .\n");
        let schema_state = Arc::new(datastreamdb_schema::SchemaState::new());
        let engine = PostingEngine::open(
            Arc::new(datastreamdb_store::MemoryBackend::new()),
            schema_state.clone(),
            64,
        );
        load_live(
            &engine,
            &LiveConfig {
                files: vec![rdf],
                schema_file: Some(schema),
                ..LiveConfig::default()
            },
        )
        .expect("load");
        assert!(schema_state.get("0-nick", false).is_some());
    }

    #[test]
    fn live_loads_encrypted_inputs() {
        use std::io::Write as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let key = datastreamdb_enc::keys::DataKey::new(vec![3u8; 32]).expect("key");
        let rdf = dir.path().join("data.rdf");
        let file = std::fs::File::create(&rdf).expect("create");
        let mut encrypted =
            datastreamdb_enc::stream::encrypting_writer(Some(&key), file).expect("writer");
        encrypted
            .write_all(b"<0x2> <name> \"Alice\" .\n")
            .expect("write");
        encrypted.flush().expect("flush");
        drop(encrypted);

        let engine = test_engine();
        let stats = load_live(
            &engine,
            &LiveConfig {
                files: vec![rdf],
                encryption_key: Some(key),
                ..LiveConfig::default()
            },
        )
        .expect("load");
        assert_eq!(stats.edges, 1);
        let read_ts = engine.oracle().max_assigned();
        let list_key = datastreamdb_x::data_key("0-name", 2);
        let list = engine.get_list(&list_key, read_ts).expect("list");
        assert_eq!(list.all_values(read_ts).expect("values").len(), 1);
    }

    #[test]
    fn live_dispatches_remote_inputs_to_s3() {
        // Closed port: the failure must come from the S3 stack (sign +
        // transport), proving remote dispatch instead of local open.
        let engine = test_engine();
        let err = load_live(
            &engine,
            &LiveConfig {
                files: vec![PathBuf::from(
                    "minio://127.0.0.1:1/bucket/data.rdf?secure=false",
                )],
                ..LiveConfig::default()
            },
        )
        .expect_err("unreachable s3");
        let text = err.to_string();
        assert!(
            text.contains("s3") || text.contains("transport"),
            "wrong stack: {text}"
        );
    }

    #[test]
    fn live_reports_file_and_line() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rdf = write_temp(
            &dir,
            "data.rdf",
            "<0x2> <name> \"Alice\" .\n<0x3> <oops .\n",
        );
        let engine = test_engine();
        let err = load_live(
            &engine,
            &LiveConfig {
                files: vec![rdf],
                ..LiveConfig::default()
            },
        )
        .expect_err("bad line");
        let text = err.to_string();
        assert!(text.contains("data.rdf:2"), "missing context: {text}");
    }
}
