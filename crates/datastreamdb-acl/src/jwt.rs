//! Access/refresh token minting and validation.
//!
//! Port of the JWT core (`x/jwt_helper.go`, `x/authn.go`, and the
//! `edgraph/access.go` mint/validate paths). Access tokens carry
//! `userid`/`groups`/`namespace`/`exp`; refresh tokens carry everything
//! but `groups`. Expiry is mandatory, the algorithm must match exactly,
//! and claim-type errors mirror Go's messages.

use std::collections::BTreeMap;

use jsonwebtoken::{Header, Validation, decode, encode};
use serde_json::{Map, Value};

use crate::keys::{DEFAULT_ACCESS_TTL_SECS, DEFAULT_REFRESH_TTL_SECS, Secrets};

/// Validated token identity (Go `userData` + principal fields).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// `userid` claim.
    pub user_id: String,
    /// `groups` claim (empty when absent).
    pub groups: Vec<String>,
    /// `namespace` claim.
    pub namespace: u64,
}

impl Identity {
    /// Synthetic guardian identity for trusted internal operations
    /// (login lookups, bootstrap), which bypass authorization exactly
    /// like a guardian token would.
    #[must_use]
    pub fn guardian() -> Self {
        Self {
            user_id: "groot".to_owned(),
            groups: vec!["guardians".to_owned()],
            namespace: 0,
        }
    }
}

/// Token failures (messages mirror Go's).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JwtError {
    /// Signature, format, or algorithm failure.
    #[error("unable to parse jwt token: {0}")]
    Parse(String),
    /// Missing or past `exp` (Go treats both as expired).
    #[error("Token is expired")]
    Expired,
    /// `userid` claim is not a string.
    #[error("userid in claims is not a string:{0}")]
    BadUserId(String),
    /// `namespace` claim is not a number.
    #[error("namespace in claims is not valid:{0}")]
    BadNamespace(String),
    /// A `groups` member is not a string.
    #[error("unable to convert group to string:{0}")]
    BadGroup(String),
    /// Minting failed (backend error).
    #[error("unable to encode jwt to string: {0}")]
    Encode(String),
}

/// Current Unix time in seconds.
fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// Mints an access token (Go `getAccessJwt`).
///
/// `groups` serializes as `null` when empty, matching Go's nil slice.
///
/// # Errors
///
/// Returns [`JwtError::Encode`] when the backend cannot sign.
pub fn mint_access(
    secrets: &Secrets,
    user_id: &str,
    groups: &[String],
    namespace: u64,
    ttl_secs: u64,
) -> Result<String, JwtError> {
    mint_access_at(secrets, user_id, groups, namespace, now_unix(), ttl_secs)
}

/// Mints an access token with an explicit issue time (deterministic goldens).
///
/// # Errors
///
/// Returns [`JwtError::Encode`] when the backend cannot sign.
pub fn mint_access_at(
    secrets: &Secrets,
    user_id: &str,
    groups: &[String],
    namespace: u64,
    now_unix: i64,
    ttl_secs: u64,
) -> Result<String, JwtError> {
    let mut claims = Map::new();
    claims.insert("userid".to_owned(), Value::String(user_id.to_owned()));
    // Go marshals a nil slice as null; only non-empty lists encode arrays.
    if groups.is_empty() {
        claims.insert("groups".to_owned(), Value::Null);
    } else {
        claims.insert(
            "groups".to_owned(),
            Value::Array(groups.iter().cloned().map(Value::String).collect()),
        );
    }
    claims.insert("namespace".to_owned(), Value::Number(namespace.into()));
    let exp = now_unix.saturating_add_unsigned(ttl_secs);
    claims.insert("exp".to_owned(), Value::Number(exp.into()));
    encode_claims(secrets, &claims)
}

/// Mints an access token with Go's default TTL (6h).
///
/// # Errors
///
/// Returns [`JwtError::Encode`] when the backend cannot sign.
pub fn mint_access_default(
    secrets: &Secrets,
    user_id: &str,
    groups: &[String],
    namespace: u64,
) -> Result<String, JwtError> {
    mint_access(secrets, user_id, groups, namespace, DEFAULT_ACCESS_TTL_SECS)
}

/// Mints a refresh token (Go `getRefreshJwt`; no `groups` claim).
///
/// # Errors
///
/// Returns [`JwtError::Encode`] when the backend cannot sign.
pub fn mint_refresh(
    secrets: &Secrets,
    user_id: &str,
    namespace: u64,
    ttl_secs: u64,
) -> Result<String, JwtError> {
    mint_refresh_at(secrets, user_id, namespace, now_unix(), ttl_secs)
}

/// Mints a refresh token with an explicit issue time.
///
/// # Errors
///
/// Returns [`JwtError::Encode`] when the backend cannot sign.
pub fn mint_refresh_at(
    secrets: &Secrets,
    user_id: &str,
    namespace: u64,
    now_unix: i64,
    ttl_secs: u64,
) -> Result<String, JwtError> {
    let mut claims = Map::new();
    claims.insert("userid".to_owned(), Value::String(user_id.to_owned()));
    claims.insert("namespace".to_owned(), Value::Number(namespace.into()));
    let exp = now_unix.saturating_add_unsigned(ttl_secs);
    claims.insert("exp".to_owned(), Value::Number(exp.into()));
    encode_claims(secrets, &claims)
}

/// Mints a refresh token with Go's default TTL (30d).
///
/// # Errors
///
/// Returns [`JwtError::Encode`] when the backend cannot sign.
pub fn mint_refresh_default(
    secrets: &Secrets,
    user_id: &str,
    namespace: u64,
) -> Result<String, JwtError> {
    mint_refresh(secrets, user_id, namespace, DEFAULT_REFRESH_TTL_SECS)
}

/// Encodes `claims` under `secrets` (sorted keys; deterministic bytes).
fn encode_claims(secrets: &Secrets, claims: &Map<String, Value>) -> Result<String, JwtError> {
    // BTreeMap ordering keeps goldens stable; Go's map order is random,
    // so byte equality with Go output is not asserted anywhere.
    let ordered: BTreeMap<&String, &Value> = claims.iter().collect();
    let header = Header::new(secrets.alg().jwt_alg());
    encode(&header, &ordered, secrets.encoding_key())
        .map_err(|err| JwtError::Encode(err.to_string()))
}

/// Validates a token and extracts its identity (Go `validateToken`).
///
/// The algorithm must match `secrets` exactly and `exp` must be present
/// and unexpired (zero leeway, like Go).
///
/// # Errors
///
/// Returns [`JwtError::Expired`] for missing/past expiry,
/// [`JwtError::Parse`] for signature/format/algorithm failures, and the
/// claim errors for mistyped `userid`/`namespace`/`groups`.
pub fn validate(secrets: &Secrets, token: &str) -> Result<Identity, JwtError> {
    let mut validation = Validation::new(secrets.alg().jwt_alg());
    validation.leeway = 0;
    validation.validate_aud = false;
    let data = decode::<Map<String, Value>>(token, secrets.decoding_key(), &validation).map_err(
        |err| match err.kind() {
            jsonwebtoken::errors::ErrorKind::ExpiredSignature => JwtError::Expired,
            // Missing `exp` surfaces here; Go reports it as expired too.
            jsonwebtoken::errors::ErrorKind::MissingRequiredClaim(name) if name == "exp" => {
                JwtError::Expired
            }
            _ => JwtError::Parse(err.to_string()),
        },
    )?;
    identity_from_claims(&data.claims)
}

/// Extracts [`Identity`] from decoded claims (shared validation tail).
fn identity_from_claims(claims: &Map<String, Value>) -> Result<Identity, JwtError> {
    let user_id = match claims.get("userid") {
        Some(Value::String(id)) => id.clone(),
        other => {
            return Err(JwtError::BadUserId(
                other.map_or_else(|| "null".to_owned(), ToString::to_string),
            ));
        }
    };
    // Go reads the JSON number as float64 (2^52 ceiling documented there).
    let namespace = match claims.get("namespace").and_then(Value::as_f64) {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Some(number) => number as u64,
        None => {
            return Err(JwtError::BadNamespace(
                claims
                    .get("namespace")
                    .map_or_else(|| "null".to_owned(), ToString::to_string),
            ));
        }
    };
    // Absent/non-array groups mean "no groups" (Go returns nil,nil);
    // only non-string members are an error.
    let groups = match claims.get("groups") {
        Some(Value::Array(members)) => {
            let mut out = Vec::with_capacity(members.len());
            for member in members {
                match member {
                    Value::String(group) => out.push(group.clone()),
                    other => return Err(JwtError::BadGroup(other.to_string())),
                }
            }
            out
        }
        _ => Vec::new(),
    };
    Ok(Identity {
        user_id,
        groups,
        namespace,
    })
}

/// Extracts the user id without full validation context (Go `ExtractUserName`).
///
/// # Errors
///
/// Returns [`JwtError`] when the token is invalid or `userid` mistyped.
pub fn extract_user_id(secrets: &Secrets, token: &str) -> Result<String, JwtError> {
    validate(secrets, token).map(|identity| identity.user_id)
}

/// Extracts the namespace without full validation context.
///
/// # Errors
///
/// Returns [`JwtError`] when the token is invalid or `namespace` mistyped.
pub fn extract_namespace(secrets: &Secrets, token: &str) -> Result<u64, JwtError> {
    validate(secrets, token).map(|identity| identity.namespace)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{Alg, Secrets};
    use jsonwebtoken::Algorithm;
    use std::path::PathBuf;

    /// Fixed issue time for deterministic goldens (2026-01-01T00:00:00Z).
    const GOLDEN_NOW: i64 = 1_767_225_600;

    fn hmac_secrets() -> Secrets {
        Secrets::from_hmac(Alg::Hs256, &[7u8; 32]).expect("hmac")
    }

    fn fixture(name: &str) -> Vec<u8> {
        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("testdata");
        path.push(name);
        std::fs::read(&path).unwrap_or_else(|_| panic!("fixture {name}"))
    }

    #[test]
    fn golden_access_token_is_stable() {
        let mint = || {
            mint_access_at(
                &hmac_secrets(),
                "alice",
                &["dev".to_owned(), "ops".to_owned()],
                2,
                GOLDEN_NOW,
                3_600,
            )
            .expect("mint")
        };
        // Same inputs byte-identical outputs (sorted-claim canonical form).
        let (first, second) = (mint(), mint());
        assert_eq!(first, second);
        // Header pins alg + typ for any spec-compliant reader.
        let header_b64 = first.split('.').next().expect("header");
        let header: serde_json::Value =
            serde_json::from_slice(&base64_decode(header_b64)).expect("json");
        assert_eq!(header["alg"], "HS256");
        assert_eq!(header["typ"], "JWT");
        // Payload pins the exact Go claim shapes (Go's own map order is
        // random, so only shapes — not Go bytes — are asserted).
        let payload_b64 = first.split('.').nth(1).expect("payload");
        let payload: serde_json::Value =
            serde_json::from_slice(&base64_decode(payload_b64)).expect("json");
        assert_eq!(payload["userid"], "alice");
        assert_eq!(payload["groups"], serde_json::json!(["dev", "ops"]));
        assert_eq!(payload["namespace"], 2);
        assert_eq!(payload["exp"], GOLDEN_NOW + 3_600);
    }

    #[test]
    fn empty_groups_encode_as_null() {
        let token = mint_access_at(&hmac_secrets(), "solo", &[], 1, now_unix(), 600).expect("mint");
        let payload_b64 = token.split('.').nth(1).expect("payload");
        let payload: serde_json::Value =
            serde_json::from_slice(&base64_decode(payload_b64)).expect("json");
        assert_eq!(payload["groups"], serde_json::Value::Null);
        let identity = validate(&hmac_secrets(), &token).expect("validate");
        assert!(identity.groups.is_empty());
    }

    fn base64_decode(input: &str) -> Vec<u8> {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        URL_SAFE_NO_PAD.decode(input).expect("b64")
    }

    #[test]
    fn refresh_has_no_groups_claim() {
        let token = mint_refresh_at(&hmac_secrets(), "alice", 2, now_unix(), 600).expect("mint");
        let payload_b64 = token.split('.').nth(1).expect("payload");
        let payload: serde_json::Value =
            serde_json::from_slice(&base64_decode(payload_b64)).expect("json");
        assert!(payload.get("groups").is_none());
        let identity = validate(&hmac_secrets(), &token).expect("validate");
        assert_eq!(identity.user_id, "alice");
        assert_eq!(identity.namespace, 2);
    }

    #[test]
    fn rejects_tampered_wrong_key_and_wrong_alg() {
        let token = mint_access_default(&hmac_secrets(), "alice", &[], 1).expect("mint");
        // Tampered signature.
        let mut bad = token.clone();
        bad.pop();
        bad.push(if bad.ends_with('A') { 'B' } else { 'A' });
        assert!(matches!(
            validate(&hmac_secrets(), &bad),
            Err(JwtError::Parse(_))
        ));
        // Right shape, wrong key.
        let other = Secrets::from_hmac(Alg::Hs256, &[9u8; 32]).expect("hmac");
        assert!(matches!(validate(&other, &token), Err(JwtError::Parse(_))));
        // Right key, wrong algorithm enforced.
        let hs384 = Secrets::from_hmac(Alg::Hs384, &[7u8; 48]).expect("hmac");
        assert!(matches!(validate(&hs384, &token), Err(JwtError::Parse(_))));
    }

    #[test]
    fn rejects_missing_and_past_expiry() {
        // Past expiry.
        let token =
            mint_access_at(&hmac_secrets(), "alice", &[], 1, GOLDEN_NOW - 7_200, 60).expect("mint");
        assert_eq!(validate(&hmac_secrets(), &token), Err(JwtError::Expired));
        // Missing expiry: craft claims without exp via a raw token.
        let header = Header::new(Algorithm::HS256);
        let mut claims = Map::new();
        claims.insert("userid".to_owned(), Value::String("alice".to_owned()));
        let raw = encode(&header, &claims, hmac_secrets().encoding_key()).expect("encode");
        assert_eq!(validate(&hmac_secrets(), &raw), Err(JwtError::Expired));
    }

    #[test]
    fn rejects_mistyped_claims() {
        let mint = |claims: &Map<String, Value>| {
            let header = Header::new(Algorithm::HS256);
            encode(&header, claims, hmac_secrets().encoding_key()).expect("encode")
        };
        let mut base = Map::new();
        base.insert("userid".to_owned(), Value::String("alice".to_owned()));
        base.insert("namespace".to_owned(), Value::Number(1.into()));
        base.insert("exp".to_owned(), Value::Number((now_unix() + 600).into()));

        let mut claims = base.clone();
        claims.insert("userid".to_owned(), Value::Number(7.into()));
        assert!(matches!(
            validate(&hmac_secrets(), &mint(&claims)),
            Err(JwtError::BadUserId(_))
        ));

        let mut claims = base.clone();
        claims.insert("namespace".to_owned(), Value::String("x".to_owned()));
        assert!(matches!(
            validate(&hmac_secrets(), &mint(&claims)),
            Err(JwtError::BadNamespace(_))
        ));

        let mut claims = base.clone();
        claims.insert(
            "groups".to_owned(),
            Value::Array(vec![Value::Number(7.into())]),
        );
        assert!(matches!(
            validate(&hmac_secrets(), &mint(&claims)),
            Err(JwtError::BadGroup(_))
        ));

        // Non-array groups degrade to empty (Go returns nil,nil).
        let mut claims = base.clone();
        claims.insert("groups".to_owned(), Value::String("dev".to_owned()));
        let identity = validate(&hmac_secrets(), &mint(&claims)).expect("validate");
        assert!(identity.groups.is_empty());
    }

    #[test]
    fn asymmetric_families_round_trip() {
        let cases = [
            (Alg::Es256, "es256.pem"),
            (Alg::Es384, "es384.pem"),
            (Alg::Rs256, "rsa.pem"),
            (Alg::Rs384, "rsa.pem"),
            (Alg::Rs512, "rsa.pem"),
            (Alg::Ps256, "rsa.pem"),
            (Alg::Ps384, "rsa.pem"),
            (Alg::Ps512, "rsa.pem"),
            (Alg::Eddsa, "eddsa.pem"),
        ];
        for (alg, name) in cases {
            let secrets = Secrets::from_private_key(alg, &fixture(name)).expect("keys");
            let token =
                mint_access_default(&secrets, "alice", &["dev".to_owned()], 3).expect("mint");
            let identity = validate(&secrets, &token).expect("validate");
            assert_eq!(identity.user_id, "alice");
            assert_eq!(identity.groups, vec!["dev".to_owned()]);
            assert_eq!(identity.namespace, 3);
        }
    }

    #[test]
    fn extractors_match_validate() {
        let token = mint_refresh_default(&hmac_secrets(), "groot", 0).expect("mint");
        assert_eq!(
            extract_user_id(&hmac_secrets(), &token).expect("user"),
            "groot"
        );
        assert_eq!(extract_namespace(&hmac_secrets(), &token).expect("ns"), 0);
    }

    #[test]
    fn alg_mapping_covers_all() {
        for (alg, jwt) in [
            (Alg::Hs256, Algorithm::HS256),
            (Alg::Hs384, Algorithm::HS384),
            (Alg::Hs512, Algorithm::HS512),
            (Alg::Es256, Algorithm::ES256),
            (Alg::Es384, Algorithm::ES384),
            (Alg::Rs256, Algorithm::RS256),
            (Alg::Rs384, Algorithm::RS384),
            (Alg::Rs512, Algorithm::RS512),
            (Alg::Ps256, Algorithm::PS256),
            (Alg::Ps384, Algorithm::PS384),
            (Alg::Ps512, Algorithm::PS512),
            (Alg::Eddsa, Algorithm::EdDSA),
        ] {
            assert_eq!(alg.jwt_alg(), jwt);
        }
    }
}
