//! File re-encryption for key rotation.
//!
//! [`rotate_file`] streams a file through decrypt+encrypt into a
//! temporary sibling, then atomically renames it over the original.
//! Either side may be unkeyed, covering encrypt-in-place, decrypt, and
//! re-key operations with one mechanism.

use std::fs::File;
use std::io::{Read as _, Write as _};
use std::path::Path;

use crate::keys::DataKey;
use crate::stream::{decrypting_reader, encrypting_writer};

/// Rotation failures.
#[derive(Debug, thiserror::Error)]
pub enum RotateError {
    /// Underlying I/O failure.
    #[error("rotation I/O failed: {0}")]
    Io(String),
    /// Stream failure.
    #[error("{0}")]
    Stream(#[from] crate::stream::StreamError),
}

/// Re-encrypts `path` from `old_key` to `new_key` atomically.
///
/// # Errors
///
/// Returns [`RotateError`] when any read, write, or rename fails. The
/// original is untouched unless the replacement completes.
pub fn rotate_file(
    path: &Path,
    old_key: Option<&DataKey>,
    new_key: Option<&DataKey>,
) -> Result<(), RotateError> {
    let mut staged_path = path.as_os_str().to_owned();
    staged_path.push(".rotate");
    let staged = Path::new(&staged_path);
    let result = (|| -> Result<(), RotateError> {
        let input = File::open(path).map_err(|err| RotateError::Io(err.to_string()))?;
        let mut reader = decrypting_reader(old_key, input)?;
        let output = File::create(staged).map_err(|err| RotateError::Io(err.to_string()))?;
        let mut writer = encrypting_writer(new_key, output)?;
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            let count = reader
                .read(&mut chunk)
                .map_err(|err| RotateError::Io(err.to_string()))?;
            if count == 0 {
                break;
            }
            writer
                .write_all(&chunk[..count])
                .map_err(|err| RotateError::Io(err.to_string()))?;
        }
        writer
            .flush()
            .map_err(|err| RotateError::Io(err.to_string()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(staged);
        return result;
    }
    std::fs::rename(staged, path).map_err(|err| RotateError::Io(err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> DataKey {
        DataKey::new(vec![byte; 32]).expect("key")
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("enc-rotate-test-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir.join("data.bin")
    }

    #[test]
    fn rotate_rekeys_encrypted_file() {
        let path = scratch("rekey");
        std::fs::write(&path, b"secret payload").expect("seed");
        let old = key(0x11);
        let new = key(0x22);
        // Encrypt in place, then re-key, then decrypt back.
        rotate_file(&path, None, Some(&old)).expect("encrypt");
        rotate_file(&path, Some(&old), Some(&new)).expect("rekey");
        rotate_file(&path, Some(&new), None).expect("decrypt");
        assert_eq!(std::fs::read(&path).expect("read"), b"secret payload");
        std::fs::remove_dir_all(path.parent().expect("dir")).expect("cleanup");
    }

    #[test]
    fn rotate_missing_file_errors_cleanly() {
        let path = scratch("missing").join("nope.bin");
        let key = key(0x11);
        assert!(matches!(
            rotate_file(&path, None, Some(&key)),
            Err(RotateError::Io(_))
        ));
        assert!(!path.with_extension("bin.rotate").exists());
    }
}
