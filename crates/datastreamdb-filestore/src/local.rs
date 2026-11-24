//! Disk-backed file store (Go `localFiles` + `x.FindDataFiles`).
//!
//! [`LocalFiles`] serves plain paths; chunk files transparently decode
//! gzip (`.gz` suffix) and AES-CTR encryption (when a key is given),
//! matching what Go's `chunker.FileReader` accepts.

use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use crate::{FileStore, StoreError};

/// Disk-backed store.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalFiles;

impl LocalFiles {
    /// Creates the disk store.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl FileStore for LocalFiles {
    fn open(&self, path: &str) -> Result<Box<dyn Read + Send>, StoreError> {
        let file = std::fs::File::open(path).map_err(|err| StoreError::Io(err.to_string()))?;
        Ok(Box::new(file))
    }

    fn exists(&self, path: &str) -> bool {
        std::fs::metadata(path).is_ok()
    }

    fn find_data_files(&self, dirs: &str, ext: &[&str]) -> Vec<String> {
        find_data_files(dirs, ext)
    }

    fn chunk_reader(
        &self,
        file: &str,
        key: Option<&datastreamdb_enc::keys::DataKey>,
    ) -> Result<Box<dyn Read + Send>, StoreError> {
        let opened: Box<dyn Read + Send> = self.open(file)?;
        decode_stream(opened, file, key)
    }
}

/// Decrypts then gunzips a chunk stream (Go chunk-reader order).
/// Non-gz names still sniff magic (backups store
/// encrypted-compressed bytes either way).
///
/// # Errors
///
/// Returns [`StoreError`] when decryption or sniffing fails.
pub(crate) fn decode_stream(
    opened: Box<dyn Read + Send>,
    file: &str,
    key: Option<&datastreamdb_enc::keys::DataKey>,
) -> Result<Box<dyn Read + Send>, StoreError> {
    let decrypted = datastreamdb_enc::stream::decrypting_reader(key, opened)
        .map_err(|err| StoreError::Stream(err.to_string()))?;
    let mut decrypted: Box<dyn Read + Send> = Box::new(decrypted);
    // Case-sensitive like Go's `filepath.Ext` check.
    #[allow(clippy::case_sensitive_file_extension_comparisons)]
    if file.ends_with(".gz") {
        let decoder = flate2::read::GzDecoder::new(decrypted);
        return Ok(Box::new(BufReader::new(decoder)));
    }
    let mut magic = Vec::new();
    std::io::Read::by_ref(&mut decrypted)
        .take(2)
        .read_to_end(&mut magic)
        .map_err(|err| StoreError::Io(err.to_string()))?;
    let rest: Box<dyn Read + Send> = Box::new(std::io::Cursor::new(magic.clone()).chain(decrypted));
    if magic == [0x1f, 0x8b] {
        let decoder = flate2::read::GzDecoder::new(rest);
        return Ok(Box::new(BufReader::new(decoder)));
    }
    Ok(Box::new(BufReader::new(rest)))
}

/// Lists data files (Go `x.FindDataFiles`).
///
/// Empty input yields nothing; a comma-separated list passes through;
/// a single file passes through; a single directory walks recursively
/// (lexical order, like Go) keeping suffix matches. Missing paths log
/// nothing and yield nothing (Go only logs).
#[must_use]
pub fn find_data_files(dirs: &str, ext: &[&str]) -> Vec<String> {
    if dirs.is_empty() {
        return Vec::new();
    }
    if dirs == "-" || dirs.contains(',') {
        return dirs.split(',').map(str::to_owned).collect();
    }
    let path = Path::new(dirs);
    let Ok(meta) = std::fs::metadata(path) else {
        return Vec::new();
    };
    if !meta.is_dir() {
        return vec![dirs.to_owned()];
    }
    let mut out = Vec::new();
    walk_sorted(path, &mut out);
    out.into_iter()
        .filter(|file| ext.iter().any(|suffix| file.ends_with(suffix)))
        .collect()
}

/// Recursively collects files in lexical order (Go `filepath.Walk`).
fn walk_sorted(dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            walk_sorted(&path, out);
        } else if let Some(text) = path.to_str() {
            out.push(text.to_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_tree(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("filestore-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).expect("mkdir");
        std::fs::write(dir.join("a.rdf.gz"), [1u8]).expect("write");
        std::fs::write(dir.join("b.txt"), [2u8]).expect("write");
        std::fs::write(dir.join("sub").join("c.rdf.gz"), [3u8]).expect("write");
        dir
    }

    #[test]
    fn open_exists_round_trip() {
        let dir = fixture_tree("open");
        let store = LocalFiles::new();
        let file = dir.join("a.rdf.gz");
        let name = file.to_string_lossy().into_owned();
        assert!(store.exists(&name));
        assert!(!store.exists(&format!("{name}.missing")));
        let mut buf = Vec::new();
        store
            .open(&name)
            .expect("open")
            .read_to_end(&mut buf)
            .expect("read");
        assert_eq!(buf, [1u8]);
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn find_lists_sorted_matches() {
        let dir = fixture_tree("find");
        let root = dir.to_string_lossy().into_owned();
        let found = find_data_files(&root, &[".rdf.gz"]);
        assert_eq!(found.len(), 2);
        assert!(found[0] < found[1]);
        assert!(find_data_files("", &[".gz"]).is_empty());
        assert_eq!(find_data_files("-", &[".gz"]), vec!["-".to_owned()]);
        assert_eq!(
            find_data_files("a,b", &[".gz"]),
            vec!["a".to_owned(), "b".to_owned()]
        );
        assert_eq!(
            find_data_files(&format!("{root}/a.rdf.gz"), &[".nope"]),
            vec![format!("{root}/a.rdf.gz")]
        );
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn chunk_reader_decodes_gzip_and_encryption() {
        use std::io::{Read as _, Write as _};
        let dir = std::env::temp_dir().join(format!("filestore-chunk-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let store = LocalFiles::new();

        // Plain passthrough.
        let plain = dir.join("plain.rdf");
        std::fs::write(&plain, b"_:a <name> \"Ada\" .\n").expect("write");
        let mut text = String::new();
        store
            .chunk_reader(&plain.to_string_lossy(), None)
            .expect("reader")
            .read_to_string(&mut text)
            .expect("read");
        assert!(text.contains("Ada"));

        // Gzip decodes.
        let gzipped = dir.join("data.rdf.gz");
        {
            let file = std::fs::File::create(&gzipped).expect("create");
            let mut encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
            encoder.write_all(b"_:b <name> \"Bob\" .\n").expect("gzip");
            encoder.finish().expect("finish");
        }
        let mut text = String::new();
        store
            .chunk_reader(&gzipped.to_string_lossy(), None)
            .expect("reader")
            .read_to_string(&mut text)
            .expect("read");
        assert!(text.contains("Bob"));

        // Encrypted globs decrypt with the key.
        let key = datastreamdb_enc::keys::DataKey::new(vec![0x42u8; 32]).expect("key");
        let enc = dir.join("enc.rdf");
        {
            let file = std::fs::File::create(&enc).expect("create");
            let mut writer =
                datastreamdb_enc::stream::encrypting_writer(Some(&key), file).expect("writer");
            writer.write_all(b"_:c <name> \"Cy\" .\n").expect("write");
        }
        let mut text = String::new();
        store
            .chunk_reader(&enc.to_string_lossy(), Some(&key))
            .expect("reader")
            .read_to_string(&mut text)
            .expect("read");
        assert!(text.contains("Cy"));

        // Encrypted gzip: decrypt first, then gunzip (Go order).
        let mut gzipped = Vec::new();
        {
            let mut encoder =
                flate2::write::GzEncoder::new(&mut gzipped, flate2::Compression::fast());
            encoder.write_all(b"_:d <name> \"Dee\" .\n").expect("gzip");
            encoder.finish().expect("finish");
        }
        let both = dir.join("both.rdf.gz");
        {
            let file = std::fs::File::create(&both).expect("create");
            let mut writer =
                datastreamdb_enc::stream::encrypting_writer(Some(&key), file).expect("writer");
            writer.write_all(&gzipped).expect("write");
        }
        let mut text = String::new();
        store
            .chunk_reader(&both.to_string_lossy(), Some(&key))
            .expect("reader")
            .read_to_string(&mut text)
            .expect("read");
        assert!(text.contains("Dee"));
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn remote_urls_report_unsupported() {
        assert!(matches!(
            crate::store_for("s3://bucket/dir"),
            Err(StoreError::RemoteUnsupported(_))
        ));
        assert!(crate::store_for("/tmp/x").is_ok());
    }
}
