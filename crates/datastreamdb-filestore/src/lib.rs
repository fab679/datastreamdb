//! Local/remote file access for bulk load and backup.
//!
//! Rust port of the upstream Go `filestore` package: the [`FileStore`] trait,
//! [`LocalFiles`] for disk paths, and URL dispatch ([`store_for`]).
//! Remote (`s3`/`minio`) backends need an object-store client plus a
//! live minio for tests, so those URLs parse but report
//! [`StoreError::RemoteUnsupported`] until that slice lands.

#![warn(missing_docs)]

pub mod local;
pub mod s3;
pub mod s3_auth;

pub use local::LocalFiles;
pub use s3::{S3Client, S3Writer};
pub use s3_auth::{S3Config, S3Credentials, S3Error, sign_v4};

use std::io::Read;

/// File access failures.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// Underlying I/O failure.
    #[error("file store I/O failed: {0}")]
    Io(String),
    /// Remote backends are not wired yet.
    #[error("remote file stores (s3/minio) are not supported yet: {0}")]
    RemoteUnsupported(String),
    /// S3/MinIO request failure.
    #[error("s3 request failed: {0}")]
    Remote(String),
    /// Stream failure (decrypt/decompress).
    #[error("{0}")]
    Stream(String),
}

/// File or directory access, local or remote (Go `FileStore`).
pub trait FileStore: Send + Sync {
    /// Opens one file for reading (like `os.Open`).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Io`] when the file cannot be opened.
    fn open(&self, path: &str) -> Result<Box<dyn Read + Send>, StoreError>;

    /// Whether `path` exists (remote always reports true, like Go).
    fn exists(&self, path: &str) -> bool;

    /// Lists data files under comma-separated `dirs` matching `ext`.
    fn find_data_files(&self, dirs: &str, ext: &[&str]) -> Vec<String>;

    /// Buffered reader over a (possibly gzipped+encrypted) chunk file.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the file cannot be opened or the
    /// stream fails to decode.
    fn chunk_reader(
        &self,
        file: &str,
        key: Option<&datastreamdb_enc::keys::DataKey>,
    ) -> Result<Box<dyn Read + Send>, StoreError>;
}

/// Builds the store for a path or URL (Go `NewFileStore`).
///
/// # Errors
///
/// Returns [`StoreError::RemoteUnsupported`] for `s3`/`minio` URLs.
pub fn store_for(path: &str) -> Result<LocalFiles, StoreError> {
    let scheme = path.split_once("://").map(|(scheme, _)| scheme);
    match scheme {
        Some("s3" | "minio") => Err(StoreError::RemoteUnsupported(path.to_owned())),
        _ => Ok(LocalFiles::new()),
    }
}

/// Opens one file for reading (Go `Open`).
///
/// # Errors
///
/// Returns [`StoreError`] when the path is remote or unreadable.
pub fn open(path: &str) -> Result<Box<dyn Read + Send>, StoreError> {
    store_for(path)?.open(path)
}

/// Whether a file exists (Go `Exists`).
#[must_use]
pub fn exists(path: &str) -> bool {
    store_for(path).is_ok_and(|store| store.exists(path))
}

/// Opens a chunk input: local paths via [`LocalFiles`], `s3://` /
/// `minio://` URLs via [`S3Client`] with environment credentials (Go
/// loaders pass nil creds, resolving from env/IAM). Decrypts and
/// gunzips like [`FileStore::chunk_reader`].
///
/// # Errors
///
/// Returns [`StoreError`] when the file cannot be opened, signed, or
/// decoded.
pub fn open_input(
    path: &str,
    enc_key: Option<&datastreamdb_enc::keys::DataKey>,
) -> Result<Box<dyn Read + Send>, StoreError> {
    let is_remote = path
        .split_once("://")
        .is_some_and(|(scheme, _)| scheme == "s3" || scheme == "minio");
    if !is_remote {
        return LocalFiles::new().chunk_reader(path, enc_key);
    }
    let config = s3_auth::S3Config::parse(path, s3_auth::S3Credentials::default())
        .map_err(|err| StoreError::Remote(err.to_string()))?;
    let object = config.key_for("");
    let client = s3::S3Client::new(config).map_err(|err| StoreError::Remote(err.to_string()))?;
    let stream = client
        .get_stream(&object)
        .map_err(|err| StoreError::Remote(err.to_string()))?;
    local::decode_stream(stream, path, enc_key)
}
