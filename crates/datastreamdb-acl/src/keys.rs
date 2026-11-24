//! Signing algorithms, secrets, and key loading.
//!
//! Port of `x/acl_enc_keys.go`: algorithm selection (`--acl jwt-alg`,
//! default HS256), HMAC secret-length rules, and private-key parsing with
//! public-key derivation. TTL defaults (6h access, 30d refresh) mirror
//! Go's `AclDefaults`. Vault-backed keys stay deferred (enterprise).

use std::fmt;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{DecodingKey, EncodingKey};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Default JWT algorithm (`--acl jwt-alg`).
pub const DEFAULT_ALG: &str = "HS256";
/// Default access-token TTL, seconds (6h).
pub const DEFAULT_ACCESS_TTL_SECS: u64 = 6 * 3_600;
/// Default refresh-token TTL, seconds (30d).
pub const DEFAULT_REFRESH_TTL_SECS: u64 = 30 * 86_400;

/// JWT signing algorithms `DataStreamDB` accepts for ACL tokens.
///
/// Deliberate divergence: Go also accepts ES512, but `jsonwebtoken` has no
/// P-521 backend, so it parses as unsupported here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Alg {
    /// HMAC-SHA256.
    Hs256,
    /// HMAC-SHA384.
    Hs384,
    /// HMAC-SHA512.
    Hs512,
    /// ECDSA P-256.
    Es256,
    /// ECDSA P-384.
    Es384,
    /// RSASSA-PKCS1-v1_5 with SHA-256.
    Rs256,
    /// RSASSA-PKCS1-v1_5 with SHA-384.
    Rs384,
    /// RSASSA-PKCS1-v1_5 with SHA-512.
    Rs512,
    /// RSASSA-PSS with SHA-256.
    Ps256,
    /// RSASSA-PSS with SHA-384.
    Ps384,
    /// RSASSA-PSS with SHA-512.
    Ps512,
    /// Ed25519.
    Eddsa,
}

impl Alg {
    /// Parses a Go `jwt-alg` flag value (Go `jwt.GetSigningMethod`).
    ///
    /// # Errors
    ///
    /// Returns [`KeyError::UnsupportedAlg`] for unknown names.
    pub fn parse(name: &str) -> Result<Self, KeyError> {
        match name {
            "HS256" => Ok(Self::Hs256),
            "HS384" => Ok(Self::Hs384),
            "HS512" => Ok(Self::Hs512),
            "ES256" => Ok(Self::Es256),
            "ES384" => Ok(Self::Es384),
            "RS256" => Ok(Self::Rs256),
            "RS384" => Ok(Self::Rs384),
            "RS512" => Ok(Self::Rs512),
            "PS256" => Ok(Self::Ps256),
            "PS384" => Ok(Self::Ps384),
            "PS512" => Ok(Self::Ps512),
            "EdDSA" => Ok(Self::Eddsa),
            _ => Err(KeyError::UnsupportedAlg(name.to_owned())),
        }
    }

    /// Canonical flag spelling.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Hs256 => "HS256",
            Self::Hs384 => "HS384",
            Self::Hs512 => "HS512",
            Self::Es256 => "ES256",
            Self::Es384 => "ES384",
            Self::Rs256 => "RS256",
            Self::Rs384 => "RS384",
            Self::Rs512 => "RS512",
            Self::Ps256 => "PS256",
            Self::Ps384 => "PS384",
            Self::Ps512 => "PS512",
            Self::Eddsa => "EdDSA",
        }
    }

    /// Whether this family signs with a shared secret.
    #[must_use]
    pub fn is_hmac(self) -> bool {
        matches!(self, Self::Hs256 | Self::Hs384 | Self::Hs512)
    }

    /// Minimum HMAC secret length in bits (Go `checkAclKeyLength`).
    fn hmac_bits(self) -> Option<usize> {
        match self {
            Self::Hs256 => Some(256),
            Self::Hs384 => Some(384),
            Self::Hs512 => Some(512),
            _ => None,
        }
    }

    /// Maps onto `jsonwebtoken` algorithms.
    #[must_use]
    pub fn jwt_alg(self) -> jsonwebtoken::Algorithm {
        use jsonwebtoken::Algorithm as J;
        match self {
            Self::Hs256 => J::HS256,
            Self::Hs384 => J::HS384,
            Self::Hs512 => J::HS512,
            Self::Es256 => J::ES256,
            Self::Es384 => J::ES384,
            Self::Rs256 => J::RS256,
            Self::Rs384 => J::RS384,
            Self::Rs512 => J::RS512,
            Self::Ps256 => J::PS256,
            Self::Ps384 => J::PS384,
            Self::Ps512 => J::PS512,
            Self::Eddsa => J::EdDSA,
        }
    }
}

/// Key loading failures (messages mirror Go's).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    /// Unknown `--acl jwt-alg` value.
    #[error("Unsupported JWT signing algorithm for ACL: {0}")]
    UnsupportedAlg(String),
    /// HMAC secret shorter than the hash (bits).
    #[error("ACL key length [{bits} <= {need}] bits for JWT algorithm [{alg}]")]
    ShortHmac {
        /// Secret length in bits.
        bits: usize,
        /// Required bits.
        need: usize,
        /// Algorithm name.
        alg: &'static str,
    },
    /// Private-key bytes do not parse.
    #[error("error parsing ACL key as {0} private key: {1}")]
    Parse(&'static str, String),
    /// Key rejected by the JWT backend.
    #[error("unusable ACL key: {0}")]
    Backend(String),
}

/// A loaded secret that never prints and zeroes on drop (Go `x.Sensitive`).
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct Secret(Vec<u8>);

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([redacted])")
    }
}

impl Secret {
    /// Wraps secret bytes.
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// Raw secret bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
}

/// Reads a secret-file's raw bytes (Vault-backed keys stay deferred).
///
/// # Errors
///
/// Returns the I/O error when the file cannot be read.
pub fn read_secret_file(path: &std::path::Path) -> std::io::Result<Secret> {
    std::fs::read(path).map(Secret::new)
}

/// Signing + verification keys for one algorithm.
#[derive(Clone)]
pub struct Secrets {
    /// Selected algorithm.
    alg: Alg,
    /// Private signing key.
    encoding: EncodingKey,
    /// Public verification key.
    decoding: DecodingKey,
}

impl fmt::Debug for Secrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Secrets")
            .field("alg", &self.alg)
            .finish_non_exhaustive()
    }
}

impl Secrets {
    /// Builds HMAC keys from a shared secret (Go `parseJWTKey`, HS branch).
    ///
    /// # Errors
    ///
    /// Returns [`KeyError`] when `alg` is not HMAC, the secret is short,
    /// or the backend rejects it.
    pub fn from_hmac(alg: Alg, secret: &[u8]) -> Result<Self, KeyError> {
        let Some(need) = alg.hmac_bits() else {
            return Err(KeyError::UnsupportedAlg(alg.name().to_owned()));
        };
        let bits = secret.len() * 8;
        if bits < need {
            return Err(KeyError::ShortHmac {
                bits,
                need,
                alg: alg.name(),
            });
        }
        Ok(Self {
            alg,
            encoding: EncodingKey::from_secret(secret)
                .map_err(|err| KeyError::Backend(err.to_string()))?,
            decoding: DecodingKey::from_secret(secret)
                .map_err(|err| KeyError::Backend(err.to_string()))?,
        })
    }

    /// Parses a private key and derives the public half (Go `parseJWTKey`).
    ///
    /// HMAC algorithms treat `key` as raw secret bytes; asymmetric ones
    /// accept PEM private keys (PKCS#8, plus PKCS#1 for RSA and SEC1 for
    /// ECDSA, like Go's `jwt.Parse*PrivateKeyFromPEM`).
    ///
    /// # Errors
    ///
    /// Returns [`KeyError`] when the bytes do not parse or the backend
    /// rejects the derived keys.
    pub fn from_private_key(alg: Alg, key: &[u8]) -> Result<Self, KeyError> {
        if alg.is_hmac() {
            return Self::from_hmac(alg, key);
        }
        let (encoding, decoding) = match alg {
            Alg::Es256 => Self::ec_keys::<p256::SecretKey>(key)?,
            Alg::Es384 => Self::ec_keys::<p384::SecretKey>(key)?,
            Alg::Rs256 | Alg::Rs384 | Alg::Rs512 | Alg::Ps256 | Alg::Ps384 | Alg::Ps512 => {
                Self::rsa_keys(key)?
            }
            Alg::Eddsa => Self::ed_keys(key)?,
            _ => unreachable!("HMAC handled above"),
        };
        Ok(Self {
            alg,
            encoding,
            decoding,
        })
    }

    /// Selected algorithm.
    #[must_use]
    pub fn alg(&self) -> Alg {
        self.alg
    }

    /// Private signing key.
    #[must_use]
    pub fn encoding_key(&self) -> &EncodingKey {
        &self.encoding
    }

    /// Public verification key.
    #[must_use]
    pub fn decoding_key(&self) -> &DecodingKey {
        &self.decoding
    }

    /// ECDSA keys: PKCS#8 (then SEC1) private PEM in, PKCS#8 DER signing
    /// key plus uncompressed-point verification key out.
    fn ec_keys<S>(pem: &[u8]) -> Result<(EncodingKey, DecodingKey), KeyError>
    where
        S: EcSecret,
    {
        let text =
            std::str::from_utf8(pem).map_err(|err| KeyError::Parse("ECDSA", err.to_string()))?;
        let secret = S::parse(text)?;
        let private_der = secret.to_pkcs8_der()?;
        let (x, y) = secret.public_coords()?;
        let encoding = EncodingKey::from_ec_der(private_der.as_bytes());
        let decoding =
            DecodingKey::from_ec_components(&URL_SAFE_NO_PAD.encode(x), &URL_SAFE_NO_PAD.encode(y))
                .map_err(|err| KeyError::Backend(err.to_string()))?;
        Ok((encoding, decoding))
    }

    /// RSA keys: PKCS#1 (then PKCS#8) private PEM in, PKCS#1 DER signing
    /// key plus PKCS#1 DER public key out.
    fn rsa_keys(pem: &[u8]) -> Result<(EncodingKey, DecodingKey), KeyError> {
        use rsa::pkcs1::{DecodeRsaPrivateKey as _, EncodeRsaPrivateKey as _};
        let text =
            std::str::from_utf8(pem).map_err(|err| KeyError::Parse("RSA", err.to_string()))?;
        let private = rsa::PrivateKey::from_pkcs1_pem(text)
            .or_else(|_| {
                use rsa::pkcs8::DecodePrivateKey as _;
                rsa::PrivateKey::from_pkcs8_pem(text)
            })
            .map_err(|err| KeyError::Parse("RSA", err.to_string()))?;
        let private_der = private
            .to_pkcs1_der()
            .map_err(|err| KeyError::Parse("RSA", err.to_string()))?;
        let public_der = {
            use rsa::pkcs1::EncodeRsaPublicKey as _;
            private
                .to_public_key()
                .to_pkcs1_der()
                .map_err(|err| KeyError::Parse("RSA", err.to_string()))?
        };
        Ok((
            EncodingKey::from_rsa_der(private_der.as_bytes()),
            DecodingKey::from_rsa_der(public_der.as_bytes()),
        ))
    }

    /// Ed25519 keys: PKCS#8 private PEM in, PKCS#8 DER signing key plus
    /// raw-point verification key out.
    fn ed_keys(pem: &[u8]) -> Result<(EncodingKey, DecodingKey), KeyError> {
        use pkcs8::{DecodePrivateKey as _, EncodePrivateKey as _};
        let text =
            std::str::from_utf8(pem).map_err(|err| KeyError::Parse("EdDSA", err.to_string()))?;
        let signing = ed25519_dalek::SigningKey::from_pkcs8_pem(text)
            .map_err(|err| KeyError::Parse("EdDSA", err.to_string()))?;
        let private_der = signing
            .to_pkcs8_der()
            .map_err(|err| KeyError::Parse("EdDSA", err.to_string()))?;
        let public_b64 = URL_SAFE_NO_PAD.encode(signing.verifying_key().to_bytes());
        let decoding = DecodingKey::from_ed_components(&public_b64)
            .map_err(|err| KeyError::Backend(err.to_string()))?;
        Ok((EncodingKey::from_ed_der(private_der.as_bytes()), decoding))
    }
}

/// ECDSA secret-key operations over the three NIST curves.
trait EcSecret: Sized {
    /// Parses PKCS#8 (then SEC1) private PEM, like Go.
    fn parse(pem: &str) -> Result<Self, KeyError>;
    /// PKCS#8 DER for the signing backend.
    fn to_pkcs8_der(&self) -> Result<pkcs8::SecretDocument, KeyError>;
    /// Uncompressed-point x/y coordinates for verification.
    fn public_coords(&self) -> Result<(Vec<u8>, Vec<u8>), KeyError>;
}

macro_rules! ec_secret {
    ($curve:ty) => {
        impl EcSecret for $curve {
            fn parse(pem: &str) -> Result<Self, KeyError> {
                // Label dispatch covers PKCS#8 and SEC1, like Go's parser.
                Self::from_pem(pem).map_err(|err| KeyError::Parse("ECDSA", err.to_string()))
            }

            fn to_pkcs8_der(&self) -> Result<pkcs8::SecretDocument, KeyError> {
                <Self as pkcs8::EncodePrivateKey>::to_pkcs8_der(self)
                    .map_err(|err| KeyError::Parse("ECDSA", err.to_string()))
            }

            fn public_coords(&self) -> Result<(Vec<u8>, Vec<u8>), KeyError> {
                use elliptic_curve::sec1::ToSec1Point as _;
                let point = self.public_key().to_sec1_point(false);
                let x = point
                    .x()
                    .ok_or(KeyError::Parse("ECDSA", "missing x coordinate".to_owned()))?;
                let y = point
                    .y()
                    .ok_or(KeyError::Parse("ECDSA", "missing y coordinate".to_owned()))?;
                Ok((x.to_vec(), y.to_vec()))
            }
        }
    };
}

ec_secret!(p256::SecretKey);
ec_secret!(p384::SecretKey);

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> Vec<u8> {
        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("testdata");
        path.push(name);
        std::fs::read(&path).unwrap_or_else(|_| panic!("fixture {name}"))
    }

    #[test]
    fn alg_names_round_trip() {
        for name in [
            "HS256", "HS384", "HS512", "ES256", "ES384", "RS256", "RS384", "RS512", "PS256",
            "PS384", "PS512", "EdDSA",
        ] {
            assert_eq!(Alg::parse(name).expect("alg").name(), name);
        }
        assert!(matches!(
            Alg::parse("none"),
            Err(KeyError::UnsupportedAlg(_))
        ));
        assert!(matches!(Alg::parse(""), Err(KeyError::UnsupportedAlg(_))));
        // ES512 has no jsonwebtoken backend (documented divergence).
        assert!(matches!(
            Alg::parse("ES512"),
            Err(KeyError::UnsupportedAlg(_))
        ));
    }

    #[test]
    fn hmac_length_rules_match_go() {
        // Boundaries: exact fit passes, one byte short fails.
        assert!(Secrets::from_hmac(Alg::Hs256, &[7u8; 32]).is_ok());
        assert!(matches!(
            Secrets::from_hmac(Alg::Hs256, &[7u8; 31]),
            Err(KeyError::ShortHmac {
                bits: 248,
                need: 256,
                ..
            })
        ));
        assert!(Secrets::from_hmac(Alg::Hs384, &[7u8; 48]).is_ok());
        assert!(Secrets::from_hmac(Alg::Hs512, &[7u8; 64]).is_ok());
        assert!(Secrets::from_private_key(Alg::Hs256, &[7u8; 31]).is_err());
        // Non-HMAC algorithms reject raw secrets.
        assert!(Secrets::from_hmac(Alg::Es256, &[7u8; 64]).is_err());
    }

    #[test]
    fn hmac_error_message_matches_go() {
        let err = Secrets::from_hmac(Alg::Hs256, &[7u8; 31]).expect_err("short");
        assert_eq!(
            err.to_string(),
            "ACL key length [248 <= 256] bits for JWT algorithm [HS256]"
        );
    }

    #[test]
    fn parses_all_private_fixtures() {
        assert!(Secrets::from_private_key(Alg::Es256, &fixture("es256.pem")).is_ok());
        assert!(Secrets::from_private_key(Alg::Es256, &fixture("es256-pkcs8.pem")).is_ok());
        assert!(Secrets::from_private_key(Alg::Es384, &fixture("es384.pem")).is_ok());
        assert!(Secrets::from_private_key(Alg::Rs256, &fixture("rsa.pem")).is_ok());
        assert!(Secrets::from_private_key(Alg::Rs256, &fixture("rsa-pkcs1.pem")).is_ok());
        assert!(Secrets::from_private_key(Alg::Ps256, &fixture("rsa.pem")).is_ok());
        assert!(Secrets::from_private_key(Alg::Eddsa, &fixture("eddsa.pem")).is_ok());
    }

    #[test]
    fn rejects_garbage_keys() {
        assert!(matches!(
            Secrets::from_private_key(Alg::Es256, b"not a key"),
            Err(KeyError::Parse("ECDSA", _))
        ));
        assert!(matches!(
            Secrets::from_private_key(Alg::Rs256, b"not a key"),
            Err(KeyError::Parse("RSA", _))
        ));
        assert!(matches!(
            Secrets::from_private_key(Alg::Eddsa, b"not a key"),
            Err(KeyError::Parse("EdDSA", _))
        ));
        // Wrong-curve key for the algorithm.
        assert!(Secrets::from_private_key(Alg::Es384, &fixture("es256.pem")).is_err());
    }

    #[test]
    fn secret_redacts_and_zeroes() {
        let secret = Secret::new(vec![1, 2, 3]);
        assert_eq!(format!("{secret:?}"), "Secret([redacted])");
        assert_eq!(secret.bytes(), &[1, 2, 3]);
        let mut secret = secret;
        secret.0.zeroize();
        assert!(secret.bytes().is_empty());
    }

    #[test]
    fn reads_secret_file() {
        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("testdata");
        path.push("hmac.key");
        let secret = read_secret_file(&path).expect("read");
        assert_eq!(secret.bytes().len(), 64);
    }
}
