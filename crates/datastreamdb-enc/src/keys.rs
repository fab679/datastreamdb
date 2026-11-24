//! Data keys: loading, validation, and safe handling.
//!
//! AES accepts 16/24/32-byte keys; anything else is rejected like Go's
//! `aes.NewCipher`. Keys wipe on drop and never render in logs.

use zeroize::{Zeroize, ZeroizeOnDrop};

/// A validated AES key (16, 24, or 32 bytes), wiped on drop.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct DataKey(Vec<u8>);

impl std::fmt::Debug for DataKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DataKey([redacted])")
    }
}

/// Key failures (messages only; key material never appears here).
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum KeyError {
    /// Wrong key length.
    #[error("invalid key size {0} (want 16, 24, or 32 bytes)")]
    BadLength(usize),
    /// Key file unreadable.
    #[error("cannot read key file: {0}")]
    Io(String),
}

impl DataKey {
    /// Validates raw key bytes (16/24/32 accepted).
    ///
    /// # Errors
    ///
    /// Returns [`KeyError::BadLength`] for other sizes.
    pub fn new(bytes: Vec<u8>) -> Result<Self, KeyError> {
        match bytes.len() {
            16 | 24 | 32 => Ok(Self(bytes)),
            len => Err(KeyError::BadLength(len)),
        }
    }

    /// Raw key bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
}

/// Loads and validates a key file's raw bytes.
///
/// # Errors
///
/// Returns [`KeyError`] when the file is unreadable or mis-sized.
pub fn load_key_file(path: &std::path::Path) -> Result<DataKey, KeyError> {
    let bytes = std::fs::read(path).map_err(|err| KeyError::Io(err.to_string()))?;
    DataKey::new(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("testdata");
        path.push(name);
        path
    }

    #[test]
    fn accepts_aes_lengths() {
        for len in [16, 24, 32] {
            assert!(DataKey::new(vec![3u8; len]).is_ok());
        }
        assert!(matches!(
            DataKey::new(vec![3u8; 3]),
            Err(KeyError::BadLength(3))
        ));
    }

    #[test]
    fn loads_go_fixtures() {
        let key = load_key_file(&fixture("enc-key")).expect("key");
        assert_eq!(key.bytes(), b"1234567890123456");
        assert!(matches!(
            load_key_file(&fixture("bad-length-enc-key")),
            Err(KeyError::BadLength(3))
        ));
        assert!(matches!(
            load_key_file(&fixture("missing")),
            Err(KeyError::Io(_))
        ));
    }

    #[test]
    fn redacts_and_wipes() {
        let key = DataKey::new(vec![9u8; 16]).expect("key");
        assert_eq!(format!("{key:?}"), "DataKey([redacted])");
        let mut key = key;
        key.0.zeroize();
        assert!(key.bytes().iter().all(|b| *b == 0));
    }
}
