//! Backup destinations: local directories and S3/MinIO (Go `UriHandler`).

use std::path::PathBuf;

use datastreamdb_backup::manifest::{MasterManifest, MasterManifestSummary};

/// Backup manifest file (Go `backupManifest`).
pub(crate) const MANIFEST_FILE: &str = "manifest.json";
/// Temporary manifest swapped into place (Go `tmpManifest`).
pub(crate) const TMP_MANIFEST_FILE: &str = "manifest_tmp.json";
/// Listing manifest (Go `backupManifestSummary`).
pub(crate) const SUMMARY_FILE: &str = "manifest_summary.json";

/// Backup/export destination: a local directory or an S3/MinIO
/// bucket+prefix (Go `UriHandler`). All paths are relative to the
/// destination root with `/` separators.
pub(crate) enum DestStore {
    /// Local directory root.
    Local(PathBuf),
    /// Object store client (bucket+prefix baked in).
    S3(Box<S3ClientState>),
}

/// S3 client plus its destination-relative key mapping.
pub(crate) struct S3ClientState {
    /// Signed client.
    pub(crate) client: datastreamdb_filestore::S3Client,
    /// Destination config (prefix joins keys).
    pub(crate) config: datastreamdb_filestore::S3Config,
}

impl DestStore {
    /// Resolves a destination URL (Go `NewUriHandler`): plain and
    /// `file://` paths stay local; `s3://`/`minio://` dial object
    /// storage with the request credentials.
    pub(crate) fn resolve(
        destination: &str,
        access_key: &str,
        secret_key: &str,
        session_token: &str,
        anonymous: bool,
    ) -> Result<Self, tonic::Status> {
        let scheme = destination.split_once("://").map(|(scheme, _)| scheme);
        match scheme {
            Some("s3" | "minio") => {
                let creds = datastreamdb_filestore::S3Credentials {
                    access_key: access_key.to_owned(),
                    secret_key: secret_key.to_owned(),
                    session_token: session_token.to_owned(),
                    anonymous,
                };
                let config = datastreamdb_filestore::S3Config::parse(destination, creds)
                    .map_err(|err| tonic::Status::invalid_argument(err.to_string()))?;
                let client = datastreamdb_filestore::S3Client::new(config.clone())
                    .map_err(|err| tonic::Status::failed_precondition(err.to_string()))?;
                Ok(Self::S3(Box::new(S3ClientState { client, config })))
            }
            Some("file") => {
                let path = destination
                    .split_once("://")
                    .map(|(_, rest)| rest)
                    .unwrap_or_default();
                Ok(Self::Local(PathBuf::from(path)))
            }
            Some(scheme) => Err(tonic::Status::invalid_argument(format!(
                "unknown destination scheme {scheme:?}"
            ))),
            None => Ok(Self::Local(PathBuf::from(destination))),
        }
    }

    /// Creates a directory (S3 namespaces need nothing, like Go).
    pub(crate) fn create_dir(&self, rel: &str) -> Result<(), tonic::Status> {
        match self {
            Self::Local(root) => std::fs::create_dir_all(root.join(rel))
                .map_err(|err| tonic::Status::internal(format!("cannot create dir: {err}"))),
            Self::S3(_) => Ok(()),
        }
    }

    /// Opens a destination file for writing.
    pub(crate) fn create_file(&self, rel: &str) -> Result<DestFile, tonic::Status> {
        match self {
            Self::Local(root) => {
                let file = std::fs::File::create(root.join(rel)).map_err(|err| {
                    tonic::Status::internal(format!("cannot create {rel}: {err}"))
                })?;
                Ok(DestFile::Local(std::io::BufWriter::new(file)))
            }
            Self::S3(state) => {
                let key = state.config.key_for(rel);
                Ok(DestFile::S3(Box::new(state.client.put_stream(&key))))
            }
        }
    }

    /// Reads a small destination file fully (manifests).
    pub(crate) fn read(&self, rel: &str) -> Result<Vec<u8>, tonic::Status> {
        match self {
            Self::Local(root) => std::fs::read(root.join(rel))
                .map_err(|err| tonic::Status::internal(format!("cannot read {rel}: {err}"))),
            Self::S3(state) => {
                let key = state.config.key_for(rel);
                state
                    .client
                    .get_bytes(&key)
                    .map_err(|err| tonic::Status::internal(err.to_string()))
            }
        }
    }

    /// Whether a destination file exists.
    pub(crate) fn exists(&self, rel: &str) -> bool {
        match self {
            Self::Local(root) => root.join(rel).exists(),
            Self::S3(state) => state.client.exists(&state.config.key_for(rel)),
        }
    }

    /// Opens a destination file for streaming reads.
    pub(crate) fn open_stream(
        &self,
        rel: &str,
    ) -> Result<Box<dyn std::io::Read + Send>, tonic::Status> {
        match self {
            Self::Local(root) => {
                let file = std::fs::File::open(root.join(rel))
                    .map_err(|err| tonic::Status::internal(format!("cannot open {rel}: {err}")))?;
                Ok(Box::new(std::io::BufReader::new(file)))
            }
            Self::S3(state) => {
                let key = state.config.key_for(rel);
                state
                    .client
                    .get_stream(&key)
                    .map_err(|err| tonic::Status::internal(err.to_string()))
            }
        }
    }

    /// Renames within the destination (S3 copies then deletes).
    pub(crate) fn rename(&self, src: &str, dst: &str) -> Result<(), tonic::Status> {
        match self {
            Self::Local(root) => std::fs::rename(root.join(src), root.join(dst))
                .map_err(|err| tonic::Status::internal(format!("cannot install {dst}: {err}"))),
            Self::S3(state) => {
                let src_key = state.config.key_for(src);
                let dst_key = state.config.key_for(dst);
                state
                    .client
                    .rename(&src_key, &dst_key)
                    .map_err(|err| tonic::Status::internal(err.to_string()))
            }
        }
    }
}

/// Writable destination file (local buffered file or S3 multipart).
pub(crate) enum DestFile {
    /// Local file.
    Local(std::io::BufWriter<std::fs::File>),
    /// S3 multipart stream.
    S3(Box<datastreamdb_filestore::S3Writer>),
}

impl std::io::Write for DestFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Local(file) => file.write(buf),
            Self::S3(writer) => writer.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Local(file) => file.flush(),
            Self::S3(writer) => writer.flush(),
        }
    }
}

impl DestFile {
    /// Finishes the file (S3 completes/uploads; local flushes).
    pub(crate) fn finish(self) -> Result<(), tonic::Status> {
        use std::io::Write as _;
        match self {
            Self::Local(mut file) => file
                .flush()
                .map_err(|err| tonic::Status::internal(format!("flush failed: {err}"))),
            Self::S3(writer) => writer
                .close()
                .map_err(|err| tonic::Status::internal(err.to_string())),
        }
    }
}

/// Reads the destination's master manifest (missing file = empty chain).
pub(crate) fn read_master(dest: &DestStore) -> Result<MasterManifest, tonic::Status> {
    if !dest.exists(MANIFEST_FILE) {
        return Ok(MasterManifest {
            manifests: Vec::new(),
        });
    }
    let bytes = dest.read(MANIFEST_FILE)?;
    let text = String::from_utf8_lossy(&bytes);
    MasterManifest::decode(&text)
        .map_err(|err| tonic::Status::internal(format!("malformed manifest: {err}")))
}

/// Writes the manifest via temp-file + rename (Go `CreateManifest`).
pub(crate) fn write_master(
    dest: &DestStore,
    manifest: &MasterManifest,
) -> Result<(), tonic::Status> {
    use std::io::Write as _;
    let mut tmp = dest.create_file(TMP_MANIFEST_FILE)?;
    tmp.write_all(manifest.encode().as_bytes())
        .map_err(|err| tonic::Status::internal(format!("cannot write manifest: {err}")))?;
    tmp.finish()?;
    dest.rename(TMP_MANIFEST_FILE, MANIFEST_FILE)?;
    // Best-effort summary (Go `CreateManifestSummary` never aborts).
    let summary = MasterManifestSummary::summarize(manifest);
    match dest.create_file(SUMMARY_FILE) {
        Ok(mut file) => {
            let outcome = file
                .write_all(summary.encode().as_bytes())
                .map_err(|err| err.to_string())
                .and_then(|()| file.finish().map_err(|err| err.to_string()));
            if let Err(err) = outcome {
                tracing::warn!("failed to write backup summary manifest (non-fatal): {err}");
            }
        }
        Err(err) => tracing::warn!("failed to write backup summary manifest (non-fatal): {err}"),
    }
    Ok(())
}
