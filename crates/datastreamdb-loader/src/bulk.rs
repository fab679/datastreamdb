//! Bulk loader: files into a fresh engine with schema inference.
//!
//! Go's `dgraph/cmd/bulk` maps RDF over sharded Badger stores and
//! reduces per group; the embedded single-group form streams each
//! file twice — once to infer the schema (Go `bulk/schema.go`), once
//! to commit batches through the [`crate::live`] loader — which keeps
//! memory flat while matching Go's inferred types.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use datastreamdb_posting::PostingEngine;

use crate::live::{LiveConfig, LiveError, LiveStats, load_live};

/// Bulk load configuration.
#[derive(Debug, Clone)]
pub struct BulkConfig {
    /// RDF/JSON files (plain, `.gz`, or encrypted + `.gz`).
    pub files: Vec<PathBuf>,
    /// Explicit schema file (skips inference when set).
    pub schema_file: Option<PathBuf>,
    /// N-Quads per committed batch.
    pub batch_nquads: usize,
    /// Decryption key for encrypted inputs (`None` = plaintext).
    pub encryption_key: Option<datastreamdb_enc::keys::DataKey>,
}

impl Default for BulkConfig {
    fn default() -> Self {
        Self {
            files: Vec::new(),
            schema_file: None,
            batch_nquads: crate::live::DEFAULT_BATCH_NQUADS,
            encryption_key: None,
        }
    }
}

/// Bulk load counters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BulkStats {
    /// Underlying live-load counters.
    pub live: LiveStats,
    /// Predicates installed by inference (0 with an explicit schema).
    pub inferred: u64,
    /// Predicates whose values disagreed on a type (first wins).
    pub conflicts: u64,
}

/// Maps a quad object to its schema value type (Go `inferType`).
fn inferred_type(quad: &datastreamdb_chunker::NQuad) -> datastreamdb_proto::ValType {
    use datastreamdb_proto::ValType;
    if !quad.object_id.is_empty() {
        return ValType::Uid;
    }
    match &quad.object_value {
        Some(datastreamdb_types::ApiValue::Str(_)) => ValType::String,
        Some(datastreamdb_types::ApiValue::Default(_)) | None => ValType::Default,
        Some(datastreamdb_types::ApiValue::Int(_)) => ValType::Int,
        Some(datastreamdb_types::ApiValue::Double(_)) => ValType::Float,
        Some(datastreamdb_types::ApiValue::Bool(_)) => ValType::Bool,
        Some(datastreamdb_types::ApiValue::Bytes(_)) => ValType::Binary,
        Some(datastreamdb_types::ApiValue::Bigfloat(_)) => ValType::Bigfloat,
        Some(datastreamdb_types::ApiValue::Geo(_)) => ValType::Geo,
        Some(datastreamdb_types::ApiValue::Datetime(_)) => ValType::Datetime,
        Some(datastreamdb_types::ApiValue::Password(_)) => ValType::Password,
        Some(datastreamdb_types::ApiValue::Vfloat(_)) => ValType::Vfloat,
    }
}

/// Inferred predicate record.
#[derive(Debug, Clone, Default)]
struct Inferred {
    /// First-seen value type.
    value_type: i32,
    /// Whether any subject carries several values.
    list: bool,
    /// Whether any value carries a language tag.
    lang: bool,
    /// Subjects already seen (list detection).
    seen_subjects: HashSet<String>,
}

/// Folds one quad into the inference map (Go `validateType`: first
/// type wins, UID predicates are always lists).
fn observe(inferred: &mut HashMap<String, Inferred>, quad: &datastreamdb_chunker::NQuad) -> bool {
    let mut conflict = false;
    let value_type = inferred_type(quad);
    let entry = inferred
        .entry(quad.predicate.clone())
        .or_insert_with(|| Inferred {
            value_type: value_type as i32,
            list: value_type == datastreamdb_proto::ValType::Uid,
            ..Inferred::default()
        });
    if entry.value_type != value_type as i32 {
        conflict = true;
    }
    if !quad.lang.is_empty() {
        entry.lang = true;
    }
    if !entry.seen_subjects.insert(quad.subject.clone()) {
        entry.list = true;
    }
    conflict
}

/// Streams RDF quads through `observe` (JSON parses per document).
fn infer_from_files(
    files: &[PathBuf],
    key: Option<&datastreamdb_enc::keys::DataKey>,
) -> Result<(HashMap<String, Inferred>, u64), LiveError> {
    let mut inferred = HashMap::new();
    let mut conflicts = 0;
    for file in files {
        let name = file.to_string_lossy().into_owned();
        let reader = datastreamdb_filestore::open_input(&name, key)
            .map_err(|err| LiveError::File(format!("{name}: {err}")))?;
        if is_json_path(file) {
            let mut bytes = Vec::new();
            let mut reader = reader;
            reader
                .read_to_end(&mut bytes)
                .map_err(|err| LiveError::File(format!("{name}: {err}")))?;
            let (quads, _) =
                datastreamdb_chunker::parse_json(bytes.as_slice(), datastreamdb_chunker::Op::Set)
                    .map_err(|err| LiveError::Parse(format!("{name}: {err}")))?;
            for quad in &quads {
                conflicts += u64::from(observe(&mut inferred, quad));
            }
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
                    Ok(quad) => conflicts += u64::from(observe(&mut inferred, &quad)),
                    Err(datastreamdb_chunker::ChunkError::Empty) => {}
                    Err(err) => {
                        return Err(LiveError::Parse(format!("{name}:{line_no}: {err}")));
                    }
                }
            }
        }
    }
    Ok((inferred, conflicts))
}

/// Whether `path` holds JSON.
#[allow(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "Go matches extensions case-sensitively"
)]
fn is_json_path(path: &Path) -> bool {
    let name = path.to_string_lossy();
    let stem = name.strip_suffix(".gz").unwrap_or(&name);
    stem.ends_with(".json")
}

/// Loads `config.files` into `engine`, inferring the schema first
/// unless `schema_file` is set (Go `bulk` loader, embedded).
///
/// # Errors
///
/// Returns [`LiveError`] on file, parse, edge, commit, or schema
/// failure. Successfully committed batches stay committed.
pub fn load_bulk(engine: &Arc<PostingEngine>, config: &BulkConfig) -> Result<BulkStats, LiveError> {
    let mut stats = BulkStats::default();
    if config.schema_file.is_none() {
        let (inferred, conflicts) =
            infer_from_files(&config.files, config.encryption_key.as_ref())?;
        stats.conflicts = conflicts;
        for (pred, record) in &inferred {
            let namespaced = format!("0-{pred}");
            let update = datastreamdb_proto::SchemaUpdate {
                predicate: namespaced.clone(),
                value_type: record.value_type,
                list: record.list,
                lang: record.lang,
                ..datastreamdb_proto::SchemaUpdate::default()
            };
            engine.schema().set(&namespaced, update);
            stats.inferred += 1;
        }
        tracing::info!(
            "bulk inferred {} predicates ({} conflicts)",
            stats.inferred,
            stats.conflicts
        );
    }
    let live_config = LiveConfig {
        files: config.files.clone(),
        schema_file: config.schema_file.clone(),
        batch_nquads: config.batch_nquads,
        encryption_key: config.encryption_key.clone(),
    };
    stats.live = load_live(engine, &live_config)?;
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_engine() -> Arc<PostingEngine> {
        PostingEngine::open(
            Arc::new(datastreamdb_store::MemoryBackend::new()),
            Arc::new(datastreamdb_schema::SchemaState::new()),
            64,
        )
    }

    #[test]
    fn bulk_infers_schema_and_loads() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rdf = dir.path().join("data.rdf");
        std::fs::write(
            &rdf,
            "<0x2> <name> \"Alice\" .\n\
             <0x2> <age> \"30\"^^<xs:int> .\n\
             <0x2> <friend> <0x3> .\n\
             <0x2> <nick> \"Al\" .\n\
             <0x2> <nick> \"Allie\" .\n",
        )
        .expect("write");
        let engine = test_engine();
        let stats = load_bulk(
            &engine,
            &BulkConfig {
                files: vec![rdf],
                ..BulkConfig::default()
            },
        )
        .expect("load");
        assert_eq!(stats.live.edges, 5);
        assert_eq!(stats.inferred, 4);
        assert_eq!(stats.conflicts, 0);
        let schema = engine.schema();
        let name = schema.get("0-name", false).expect("name schema");
        // Plain literals infer `default`, like Go's `validateType`.
        assert_eq!(name.value_type, datastreamdb_proto::ValType::Default as i32);
        let age = schema.get("0-age", false).expect("age schema");
        assert_eq!(age.value_type, datastreamdb_proto::ValType::Int as i32);
        let friend = schema.get("0-friend", false).expect("friend schema");
        assert_eq!(friend.value_type, datastreamdb_proto::ValType::Uid as i32);
        assert!(friend.list, "uid predicates are always lists");
        let nick = schema.get("0-nick", false).expect("nick schema");
        assert!(nick.list, "repeated nick should infer a list");

        let read_ts = engine.oracle().max_assigned();
        let key = datastreamdb_x::data_key("0-name", 2);
        let list = engine.get_list(&key, read_ts).expect("list");
        assert_eq!(list.all_values(read_ts).expect("values").len(), 1);
    }

    #[test]
    fn bulk_first_type_wins_on_conflict() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rdf = dir.path().join("data.rdf");
        std::fs::write(&rdf, "<0x2> <v> \"30\"^^<xs:int> .\n<0x3> <v> \"oops\" .\n")
            .expect("write");
        let engine = test_engine();
        let stats = load_bulk(
            &engine,
            &BulkConfig {
                files: vec![rdf],
                ..BulkConfig::default()
            },
        )
        .expect("load");
        assert_eq!(stats.conflicts, 1);
        let pred = engine.schema().get("0-v", false).expect("v schema");
        assert_eq!(pred.value_type, datastreamdb_proto::ValType::Int as i32);
    }
}
