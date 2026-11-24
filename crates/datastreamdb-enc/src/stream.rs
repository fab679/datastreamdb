//! AES-CTR stream encryption (Go `GetWriter`/`GetReader`).
//!
//! Keyed streams prefix a random 16-byte IV, then CTR-encrypt inline;
//! unkeyed streams pass bytes through untouched. Decryption reads the
//! IV back first, failing with Go's message on short reads.

use std::io::{Read, Write};

use aes::{Aes128, Aes192, Aes256};
use ctr::cipher::{Array, KeyIvInit, StreamCipher};

use crate::keys::DataKey;

/// AES block/IV length in bytes.
pub const IV_LEN: usize = 16;

/// Stream failures (key material never appears here).
#[derive(Debug, thiserror::Error)]
pub enum StreamError {
    /// Underlying I/O failure.
    #[error("stream I/O failed: {0}")]
    Io(String),
    /// Short IV prefix on an encrypted stream.
    #[error("unable to get IV from encrypted backup. Read {0} bytes, err {1} ")]
    ShortIv(usize, String),
}

/// Encrypting writer: random IV prefix, then CTR stream (Go `GetWriter`).
///
/// Pass `None` for a passthrough writer.
///
/// # Errors
///
/// Returns [`StreamError::Io`] when randomness or the IV write fails.
pub fn encrypting_writer<W: Write>(
    key: Option<&DataKey>,
    mut inner: W,
) -> Result<StreamWriter<W>, StreamError> {
    let Some(key) = key else {
        return Ok(StreamWriter {
            cipher: None,
            inner,
        });
    };
    let mut iv = [0u8; IV_LEN];
    getrandom::fill(&mut iv).map_err(|err| StreamError::Io(err.to_string()))?;
    inner
        .write_all(&iv)
        .map_err(|err| StreamError::Io(err.to_string()))?;
    Ok(StreamWriter {
        cipher: Some(CtrCipher::new(key, &iv)),
        inner,
    })
}

/// Writer returned by [`encrypting_writer`].
pub struct StreamWriter<W: Write> {
    /// Active cipher (`None` passes through).
    cipher: Option<CtrCipher>,
    /// Downstream sink.
    inner: W,
}

impl<W: Write> StreamWriter<W> {
    /// Unwraps the downstream sink (flush it or finish it).
    #[must_use]
    pub fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: Write> Write for StreamWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Some(cipher) = &mut self.cipher {
            let mut owned = buf.to_vec();
            cipher.apply(&mut owned);
            self.inner.write(&owned)
        } else {
            self.inner.write(buf)
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Decrypting reader: consumes the IV prefix, then CTR-decodes (Go
/// `GetReader`). Pass `None` for a passthrough reader.
///
/// # Errors
///
/// Returns [`StreamError::ShortIv`] when fewer than 16 IV bytes are
/// available.
pub fn decrypting_reader<R: Read>(
    key: Option<&DataKey>,
    mut inner: R,
) -> Result<StreamReader<R>, StreamError> {
    let Some(key) = key else {
        return Ok(StreamReader {
            cipher: None,
            inner,
        });
    };
    let mut iv = [0u8; IV_LEN];
    let mut read = 0;
    let mut failure: Option<std::io::Error> = None;
    while read < IV_LEN {
        match inner.read(&mut iv[read..]) {
            Ok(0) => break,
            Ok(count) => read += count,
            Err(err) => {
                failure = Some(err);
                break;
            }
        }
    }
    if read != IV_LEN {
        let err = failure.map_or_else(|| "unexpected EOF".to_owned(), |e| e.to_string());
        return Err(StreamError::ShortIv(read, err));
    }
    Ok(StreamReader {
        cipher: Some(CtrCipher::new(key, &iv)),
        inner,
    })
}

/// Reader returned by [`decrypting_reader`].
pub struct StreamReader<R: Read> {
    /// Active cipher (`None` passes through).
    cipher: Option<CtrCipher>,
    /// Upstream source.
    inner: R,
}

impl<R: Read> Read for StreamReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let count = self.inner.read(buf)?;
        if let Some(cipher) = &mut self.cipher {
            cipher.apply(&mut buf[..count]);
        }
        Ok(count)
    }
}

/// CTR cipher over 128/192/256-bit keys.
enum CtrCipher {
    /// AES-128-CTR.
    Aes128(ctr::Ctr128BE<Aes128>),
    /// AES-192-CTR.
    Aes192(ctr::Ctr128BE<Aes192>),
    /// AES-256-CTR.
    Aes256(ctr::Ctr128BE<Aes256>),
}

impl CtrCipher {
    /// Builds the cipher matching `key`'s length (`DataKey` guarantees
    /// 16/24/32 bytes, so the conversions below cannot fail).
    fn new(key: &DataKey, iv: &[u8; IV_LEN]) -> Self {
        let iv = Array::try_from(iv.as_slice()).expect("16-byte IV");
        match key.bytes() {
            bytes if bytes.len() == 16 => Self::Aes128(ctr::Ctr128BE::new(
                &Array::try_from(bytes).expect("key"),
                &iv,
            )),
            bytes if bytes.len() == 24 => Self::Aes192(ctr::Ctr128BE::new(
                &Array::try_from(bytes).expect("key"),
                &iv,
            )),
            bytes => Self::Aes256(ctr::Ctr128BE::new(
                &Array::try_from(bytes).expect("key"),
                &iv,
            )),
        }
    }

    /// Encrypts/decrypts `buf` in place (CTR is symmetric).
    fn apply(&mut self, buf: &mut [u8]) {
        match self {
            Self::Aes128(cipher) => cipher.apply_keystream(buf),
            Self::Aes192(cipher) => cipher.apply_keystream(buf),
            Self::Aes256(cipher) => cipher.apply_keystream(buf),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(len: usize) -> DataKey {
        DataKey::new(vec![0x5au8; len]).expect("key")
    }

    #[test]
    fn round_trips_all_key_lengths() {
        for len in [16, 24, 32] {
            let key = key(len);
            let mut encrypted = Vec::new();
            {
                let mut writer = encrypting_writer(Some(&key), &mut encrypted).expect("writer");
                writer.write_all(b"backup payload bytes").expect("write");
            }
            // IV prefix plus ciphertext.
            assert_eq!(encrypted.len(), IV_LEN + 20);
            let mut decrypted = Vec::new();
            decrypting_reader(Some(&key), encrypted.as_slice())
                .expect("reader")
                .read_to_end(&mut decrypted)
                .expect("read");
            assert_eq!(decrypted, b"backup payload bytes");
        }
    }

    #[test]
    fn unkeyed_streams_pass_through() {
        let mut out = Vec::new();
        encrypting_writer::<_>(None, &mut out)
            .expect("writer")
            .write_all(b"plain")
            .expect("write");
        assert_eq!(out, b"plain");
        let mut back = Vec::new();
        decrypting_reader::<_>(None, out.as_slice())
            .expect("reader")
            .read_to_end(&mut back)
            .expect("read");
        assert_eq!(back, b"plain");
    }

    #[test]
    fn short_iv_fails_like_go() {
        let key = key(16);
        let Err(err) = decrypting_reader(Some(&key), [1u8; 7].as_slice()) else {
            panic!("short IV read must fail")
        };
        assert_eq!(
            err.to_string(),
            "unable to get IV from encrypted backup. Read 7 bytes, err unexpected EOF "
        );
    }

    #[test]
    fn wrong_key_garbles() {
        let key = key(16);
        let mut encrypted = Vec::new();
        encrypting_writer(Some(&key), &mut encrypted)
            .expect("writer")
            .write_all(b"0123456789abcdef")
            .expect("write");
        let wrong = DataKey::new(vec![0x3cu8; 16]).expect("key");
        let mut decrypted = Vec::new();
        decrypting_reader(Some(&wrong), encrypted.as_slice())
            .expect("reader")
            .read_to_end(&mut decrypted)
            .expect("read");
        assert_ne!(decrypted, b"0123456789abcdef");
    }
}
