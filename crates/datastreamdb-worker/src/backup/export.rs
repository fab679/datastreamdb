//! Export file writer (Go `exportInternal`, local form).

use std::sync::Arc;

use datastreamdb_posting::PostingEngine;
use datastreamdb_proto::pb::{ExportRequest, ExportResponse};

use super::dest::DestStore;
use crate::groups::GroupState;

/// Gzip export stack over a local file or an S3 multipart stream.
enum GzipStack {
    /// Local buffered file.
    Local(
        Box<
            flate2::write::GzEncoder<
                datastreamdb_enc::stream::StreamWriter<std::io::BufWriter<std::fs::File>>,
            >,
        >,
    ),
    /// S3 multipart stream.
    S3(
        Box<
            flate2::write::GzEncoder<
                datastreamdb_enc::stream::StreamWriter<datastreamdb_filestore::S3Writer>,
            >,
        >,
    ),
}

impl std::io::Write for GzipStack {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Local(encoder) => encoder.write(buf),
            Self::S3(encoder) => encoder.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Local(encoder) => encoder.flush(),
            Self::S3(encoder) => encoder.flush(),
        }
    }
}

impl GzipStack {
    /// Finishes the stack: gzip, then encryption/buffer flush (Go
    /// `ExportWriter.Close`), then S3 completion for remote files.
    fn finish(self, what: &str) -> Result<(), tonic::Status> {
        use std::io::Write as _;
        match self {
            Self::Local(encoder) => {
                let mut inner = encoder.finish().map_err(|err| {
                    tonic::Status::internal(format!("{what} finish failed: {err}"))
                })?;
                inner
                    .flush()
                    .map_err(|err| tonic::Status::internal(format!("{what} flush failed: {err}")))
            }
            Self::S3(encoder) => {
                let inner = encoder.finish().map_err(|err| {
                    tonic::Status::internal(format!("{what} finish failed: {err}"))
                })?;
                let writer = inner.into_inner();
                writer
                    .close()
                    .map_err(|err| tonic::Status::internal(err.to_string()))
            }
        }
    }
}

/// Opens a `BestSpeed` gzip file writer (Go `ExportWriter.open`):
/// file bytes are gzip-compressed, then AES-CTR encrypted when `key`
/// is set.
fn gzip_writer(
    dest: &DestStore,
    rel: &str,
    key: Option<&datastreamdb_enc::keys::DataKey>,
) -> Result<GzipStack, tonic::Status> {
    match dest {
        DestStore::Local(root) => {
            let file = std::fs::File::create(root.join(rel))
                .map_err(|err| tonic::Status::internal(format!("cannot create {rel}: {err}")))?;
            let buffered = std::io::BufWriter::new(file);
            let encrypted = datastreamdb_enc::stream::encrypting_writer(key, buffered)
                .map_err(|err| tonic::Status::internal(format!("cannot encrypt {rel}: {err}")))?;
            Ok(GzipStack::Local(Box::new(flate2::write::GzEncoder::new(
                encrypted,
                flate2::Compression::fast(),
            ))))
        }
        DestStore::S3(state) => {
            let writer = state.client.put_stream(&state.config.key_for(rel));
            let encrypted = datastreamdb_enc::stream::encrypting_writer(key, writer)
                .map_err(|err| tonic::Status::internal(format!("cannot encrypt {rel}: {err}")))?;
            Ok(GzipStack::S3(Box::new(flate2::write::GzEncoder::new(
                encrypted,
                flate2::Compression::fast(),
            ))))
        }
    }
}

/// Gzip data-file extension per format (Go `exportFormats`).
fn export_ext(format: &str) -> Result<&'static str, tonic::Status> {
    match format {
        "json" => Ok(".json.gz"),
        "rdf" => Ok(".rdf.gz"),
        other => Err(tonic::Status::invalid_argument(format!(
            "invalid export format: {other}"
        ))),
    }
}

/// GraphQL schema entries for the `.gql_schema.gz` sidecar (Go
/// `ToExportKvList`, schema arm), rendered JSON sorted by namespace.
fn gql_schema_entries(engine: &PostingEngine, read_ts: u64) -> Vec<String> {
    #[derive(serde::Serialize)]
    struct Exported {
        #[serde(rename = "Namespace")]
        namespace: u64,
        #[serde(rename = "Schema")]
        schema: String,
    }
    let mut rendered: Vec<(u64, String)> = Vec::new();
    let Ok(pairs) = engine
        .store()
        .scan_prefix(datastreamdb_posting::store::PARTITION, b"")
    else {
        return Vec::new();
    };
    for (key_bytes, _) in &pairs {
        let Ok(key) = datastreamdb_x::keycodec::parse(key_bytes) else {
            continue;
        };
        let Some((ns, bare)) = key
            .attr
            .split_once('-')
            .and_then(|(ns, rest)| u64::from_str_radix(ns, 16).ok().map(|ns| (ns, rest)))
        else {
            continue;
        };
        if bare != "datastreamdb.graphql.schema" {
            continue;
        }
        let Ok(list) = engine.get_list(key_bytes, read_ts) else {
            continue;
        };
        let Ok(values) = list.all_values(read_ts) else {
            continue;
        };
        if values.len() != 1 {
            continue;
        }
        let text = match &values[0].value {
            datastreamdb_types::Value::Binary(bytes) => String::from_utf8_lossy(bytes).into_owned(),
            datastreamdb_types::Value::String(text) => text.clone(),
            _ => continue,
        };
        let exported = Exported {
            namespace: ns,
            schema: text,
        };
        rendered.push((ns, serde_json::to_string(&exported).unwrap_or_default()));
    }
    rendered.sort_by_key(|(ns, _)| *ns);
    rendered.into_iter().map(|(_, json)| json).collect()
}

/// Writes one group's export files (Go `exportInternal`, local form).
///
/// `key` encrypts the gzip files (Go `ExportWriter` encryption).
///
/// # Errors
///
/// Returns [`tonic::Status`] when the group is not served, the format
/// is unknown, the destination is remote, or any write fails.
pub fn run_export(
    engine: &Arc<PostingEngine>,
    groups: &GroupState,
    req: &ExportRequest,
    key: Option<&datastreamdb_enc::keys::DataKey>,
) -> Result<ExportResponse, tonic::Status> {
    if req.group_id != groups.group_id() {
        return Err(tonic::Status::failed_precondition(format!(
            "export request group mismatch. Mine: {}. Requested: {}",
            groups.group_id(),
            req.group_id
        )));
    }
    let ext = export_ext(&req.format)?;
    let dest = DestStore::resolve(
        &req.destination,
        &req.access_key,
        &req.secret_key,
        &req.session_token,
        req.anonymous,
    )?;
    dest.create_dir("")?;

    let read_ts = if req.read_ts == 0 {
        engine.oracle().max_assigned()
    } else {
        req.read_ts
    };
    engine.oracle().wait_for_ts(read_ts);
    let stamp = chrono::DateTime::from_timestamp(req.unix_ts, 0).map_or_else(
        || chrono::Utc::now().format("%m%d.%H%M").to_string(),
        |ts| ts.format("%m%d.%H%M").to_string(),
    );
    let export_dir = format!("datastreamdb.r{read_ts}.u{stamp}");
    dest.create_dir(&export_dir)?;

    let namespace = req.namespace;
    let stem = format!("g{:02}", req.group_id);
    let data_name = format!("{stem}{ext}");
    let schema_name = format!("{stem}.schema.gz");
    let gql_name = format!("{stem}.gql_schema.gz");

    let mut data_out = gzip_writer(&dest, &format!("{export_dir}/{data_name}"), key)?;
    let data_stats = if req.format == "json" {
        datastreamdb_backup::export::export_json(engine, namespace, read_ts, &mut data_out)
    } else {
        datastreamdb_backup::export::export_rdf(engine, namespace, read_ts, &mut data_out)
    }
    .map_err(|err| tonic::Status::internal(format!("export failed: {err}")))?;
    data_out.finish("export")?;

    let mut schema_out = gzip_writer(&dest, &format!("{export_dir}/{schema_name}"), key)?;
    datastreamdb_backup::export::export_schema(engine.schema(), namespace, &mut schema_out)
        .map_err(|err| tonic::Status::internal(format!("schema export failed: {err}")))?;
    schema_out.finish("schema export")?;

    let mut gql_out = gzip_writer(&dest, &format!("{export_dir}/{gql_name}"), key)?;
    {
        use std::io::Write as _;
        gql_out
            .write_all(b"[\n")
            .map_err(|err| tonic::Status::internal(err.to_string()))?;
        for (index, entry) in gql_schema_entries(engine, read_ts).iter().enumerate() {
            if index > 0 {
                gql_out
                    .write_all(b",\n")
                    .map_err(|err| tonic::Status::internal(err.to_string()))?;
            }
            gql_out
                .write_all(entry.as_bytes())
                .map_err(|err| tonic::Status::internal(err.to_string()))?;
        }
        gql_out
            .write_all(b"\n]\n")
            .map_err(|err| tonic::Status::internal(err.to_string()))?;
    }
    gql_out.finish("gql export")?;

    tracing::info!(
        "export complete: group {} at {read_ts} ({} records)",
        req.group_id,
        data_stats.written
    );
    Ok(ExportResponse {
        code: 0,
        msg: String::new(),
        files: vec![
            format!("{export_dir}/{data_name}"),
            format!("{export_dir}/{schema_name}"),
            format!("{export_dir}/{gql_name}"),
        ],
    })
}
