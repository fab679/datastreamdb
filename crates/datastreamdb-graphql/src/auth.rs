//! Authorization metadata parsing and JWT verification. Port of
//! `dgraph-main/graphql/authorization/auth.go` (`Parse`, `ParseAuthMeta`,
//! claims extraction, audience checks).
//!
//! Deltas from Go: JWK URL fetching is deferred (embedded mode has no HTTP
//! client at this layer; use `VerificationKey`/`Algo`); the
//! unsupported-algorithm message lists algorithms sorted (Go iterates a map,
//! so its order is random); JWT library error text differs.

#![warn(missing_docs)]

use std::collections::HashMap;

use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::Deserialize;

use crate::errors::{GraphQlError, Result};

/// Schema comment marker carrying authorization metadata.
pub const AUTH_META_HEADER: &str = "DataStreamDB.Authorization";

/// Supported JWT algorithms. Port of Go `supportedAlgorithms`.
pub const SUPPORTED_ALGORITHMS: &[&str] = &["RS256", "RS384", "RS512", "HS256", "HS384", "HS512"];

/// Whether the algorithm is an RSA one (PEM public key expected).
#[must_use]
pub fn is_rsa_algorithm(algo: &str) -> bool {
    algo.starts_with("RS")
}

/// Authorization metadata parsed from a `# DataStreamDB.Authorization` comment.
///
/// Field matching is case-insensitive like Go's `encoding/json`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AuthMeta {
    /// HMAC secret or RSA public key (PEM).
    #[serde(
        default,
        alias = "VerificationKey",
        alias = "verificationkey",
        alias = "verificationKey"
    )]
    pub verification_key: String,
    /// Single JWK URL (folded into [`AuthMeta::jwk_urls`]).
    #[serde(default, alias = "JWKUrl", alias = "jwkurl", alias = "jwkUrl")]
    pub jwk_url: String,
    /// JWK URLs.
    #[serde(default, alias = "JWKUrls", alias = "jwkurls", alias = "jwkUrls")]
    pub jwk_urls: Vec<String>,
    /// HTTP header carrying the JWT.
    #[serde(default, alias = "Header", alias = "header")]
    pub header: String,
    /// Claim namespace holding auth variables.
    #[serde(default, alias = "Namespace", alias = "namespace")]
    pub namespace: String,
    /// JWT algorithm (`HS256`, `RS256`, ...).
    #[serde(default, alias = "Algo", alias = "algo")]
    pub algo: String,
    /// Expected audience values.
    #[serde(default, alias = "Audience", alias = "audience")]
    pub audience: Vec<String>,
    /// Whether requests without a JWT are rejected.
    #[serde(
        default,
        alias = "ClosedByDefault",
        alias = "closedbydefault",
        alias = "closedByDefault"
    )]
    pub closed_by_default: bool,
    /// Parsed RSA public key (PEM bytes) when `algo` is RSA-based.
    #[serde(skip)]
    pub rsa_public_key_pem: Option<Vec<u8>>,
}

impl AuthMeta {
    /// HTTP header carrying the JWT (empty when unset).
    #[must_use]
    pub fn get_header(&self) -> &str {
        &self.header
    }

    /// Validates required fields. Port of Go `(*AuthMeta).validate`.
    fn validate(&self) -> Result<()> {
        let mut fields = String::new();
        if !self.jwk_urls.is_empty() || !self.jwk_url.is_empty() {
            if !self.jwk_urls.is_empty() && !self.jwk_url.is_empty() {
                return Err(GraphQlError::new(
                    "expecting either JWKUrl or JWKUrls, both were given",
                ));
            }
            if !self.verification_key.is_empty() || !self.algo.is_empty() {
                return Err(GraphQlError::new(
                    "expecting either JWKUrl/JWKUrls or (VerificationKey, Algo), both were given",
                ));
            }
            if self.audience.is_empty() {
                fields.push_str(" `Audience` ");
            }
        } else {
            if self.verification_key.is_empty() {
                fields.push_str(" `Verification key`/`JWKUrl`/`JWKUrls`");
            }
            if self.algo.is_empty() {
                fields.push_str(" `Algo`");
            }
        }
        if self.header.is_empty() {
            fields.push_str(" `Header`");
        }
        if self.namespace.is_empty() {
            fields.push_str(" `Namespace`");
        }
        if fields.is_empty() {
            Ok(())
        } else {
            Err(GraphQlError::new(format!(
                "required field missing in DataStreamDB.Authorization:{fields}"
            )))
        }
    }

    /// Checks the algorithm against the supported set.
    fn init_signing_method(&self) -> Result<()> {
        if !self.jwk_urls.is_empty() || !self.jwk_url.is_empty() {
            return Ok(());
        }
        if SUPPORTED_ALGORITHMS.contains(&self.algo.as_str()) {
            return Ok(());
        }
        let mut options = SUPPORTED_ALGORITHMS.to_vec();
        options.sort_unstable();
        Err(GraphQlError::new(format!(
            "invalid jwt algorithm: found {}, but supported options are: {}",
            self.algo,
            options.join(",")
        )))
    }
}

/// Parses authorization metadata from a schema string.
///
/// Accepts the JSON form (`# DataStreamDB.Authorization {...}`) and the legacy
/// space-separated form. Port of Go `Parse`.
///
/// # Errors
///
/// Returns [`GraphQlError`] on malformed metadata.
pub fn parse(schema: &str) -> Result<Option<AuthMeta>> {
    let Some(idx) = schema.rfind(AUTH_META_HEADER) else {
        return Ok(None);
    };
    let auth_info = &schema[idx..];
    let payload = &auth_info[AUTH_META_HEADER.len()..];
    if let Ok(mut meta) = serde_json::from_str::<AuthMeta>(payload) {
        meta.validate()?;
        meta.init_signing_method()?;
        if !meta.jwk_url.is_empty() {
            meta.jwk_urls.push(std::mem::take(&mut meta.jwk_url));
        }
        return Ok(Some(meta));
    }

    tracing::debug!(
        "falling back to parsing `DataStreamDB.Authorization` in old format; \
         see https://graphql.dgraph.io/authorization/"
    );
    parse_legacy(auth_info)
}

/// Parses the legacy space-separated authorization form.
///
/// Format: `# DataStreamDB.Authorization <header> <namespace> <algo> "<key>"`.
fn parse_legacy(auth_info: &str) -> Result<Option<AuthMeta>> {
    let text = format!("# {auth_info}");
    let mut parts = text.splitn(6, char::is_whitespace);
    let hash = parts.next().unwrap_or_default();
    let marker = parts.next().unwrap_or_default();
    let header = parts.next().unwrap_or_default();
    let namespace = parts.next().unwrap_or_default();
    let algo = parts.next().unwrap_or_default();
    let key = parts.next().unwrap_or_default();
    let valid = hash == "#"
        && marker == AUTH_META_HEADER
        && !header.is_empty()
        && !namespace.is_empty()
        && !algo.is_empty()
        && key.len() >= 2
        && key.starts_with('"')
        && key.ends_with('"')
        && !key[1..key.len() - 1].contains('"');
    if !valid {
        return Err(GraphQlError::new(format!(
            "Invalid `DataStreamDB.Authorization` format: {text}"
        )));
    }
    let meta = AuthMeta {
        header: header.to_owned(),
        namespace: namespace.to_owned(),
        algo: algo.to_owned(),
        verification_key: key[1..key.len() - 1].to_owned(),
        ..AuthMeta::default()
    };
    meta.init_signing_method()?;
    Ok(Some(meta))
}

/// Parses authorization metadata, decoding RSA public keys.
///
/// Port of Go `ParseAuthMeta`.
///
/// # Errors
///
/// Returns [`GraphQlError`] on malformed metadata or undecodable PEM.
pub fn parse_auth_meta(schema: &str) -> Result<Option<AuthMeta>> {
    let Some(mut meta) = parse(schema)? else {
        return Ok(None);
    };
    if is_rsa_algorithm(&meta.algo) {
        let pem = meta.verification_key.replace("\\n", "\n");
        if !pem.contains("BEGIN PUBLIC KEY") {
            return Err(GraphQlError::new(
                "invalid RSA public key: expected PEM with BEGIN PUBLIC KEY",
            ));
        }
        meta.rsa_public_key_pem = Some(pem.into_bytes());
    }
    Ok(Some(meta))
}

/// Strips an optional `Bearer ` prefix from a header value. Port of the
/// prefix handling in Go `AttachAuthorizationJwt`.
///
/// # Errors
///
/// Returns [`GraphQlError`] on malformed `Bearer` values.
pub fn bearer_token(header_value: &str) -> Result<&str> {
    if header_value.to_lowercase().starts_with("bearer ") {
        let parts: Vec<&str> = header_value.split(' ').collect();
        if parts.len() != 2 {
            return Err(GraphQlError::new(format!(
                "invalid Bearer-formatted header value for JWT ({header_value})"
            )));
        }
        return Ok(parts[1]);
    }
    Ok(header_value)
}

/// Maps a configured algorithm name to a JWT algorithm.
fn jwt_algorithm(algo: &str) -> Result<Algorithm> {
    match algo {
        "HS256" => Ok(Algorithm::HS256),
        "HS384" => Ok(Algorithm::HS384),
        "HS512" => Ok(Algorithm::HS512),
        "RS256" => Ok(Algorithm::RS256),
        "RS384" => Ok(Algorithm::RS384),
        "RS512" => Ok(Algorithm::RS512),
        _ => Err(GraphQlError::new(format!(
            "unsupported jwt algorithm: {algo}"
        ))),
    }
}

/// Constant-time string equality for audience comparison.
fn constant_time_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (left, right) in left.bytes().zip(right.bytes()) {
        diff |= left ^ right;
    }
    diff == 0
}

/// Validates a token's audience against the expected values. Port of Go
/// `validateAudience`.
fn validate_audience(meta: &AuthMeta, claims: &serde_json::Value) -> Result<()> {
    let audiences: Vec<String> = match claims.get("aud") {
        Some(serde_json::Value::String(aud)) => vec![aud.clone()],
        Some(serde_json::Value::Array(auds)) => auds
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(str::to_owned)
            .collect(),
        None | Some(_) => return Ok(()),
    };
    if audiences.is_empty() {
        return Ok(());
    }
    if meta.audience.is_empty() {
        return Err(GraphQlError::new(
            "audience value was expected but not provided",
        ));
    }
    for aud in &audiences {
        for expected in &meta.audience {
            if constant_time_eq(aud, expected) {
                return Ok(());
            }
        }
    }
    Err(GraphQlError::new(
        "JWT `aud` value doesn't match with the audience",
    ))
}

/// Merges namespaced auth variables over the token claims. Port of Go
/// `CustomClaims.UnmarshalJSON`.
fn merge_auth_vars(
    meta: &AuthMeta,
    claims: &serde_json::Value,
) -> Result<HashMap<String, serde_json::Value>> {
    let mut result: HashMap<String, serde_json::Value> = claims
        .as_object()
        .map(|map| {
            map.iter()
                .map(|(key, val)| (key.clone(), val.clone()))
                .collect()
        })
        .unwrap_or_default();
    if let Some(namespaced) = result.get(&meta.namespace).cloned() {
        let vars: HashMap<String, serde_json::Value> = match namespaced {
            serde_json::Value::String(text) => serde_json::from_str(&text)
                .map_err(|err| GraphQlError::new(format!("unable to parse jwt token:{err}")))?,
            serde_json::Value::Object(map) => map.into_iter().collect(),
            _ => HashMap::new(),
        };
        result.remove(&meta.namespace);
        result.extend(vars);
    }
    Ok(result)
}

/// Extracts auth variables from a request header value.
///
/// Handles missing tokens (`ClosedByDefault`), `Bearer` prefixes, signature
/// verification, and audience checks. Port of Go `ExtractCustomClaims` plus
/// `validateJWTCustomClaims`.
///
/// # Errors
///
/// Returns [`GraphQlError`] on missing/invalid tokens or verification failure.
pub fn extract_custom_claims(
    meta: &AuthMeta,
    header_value: Option<&str>,
) -> Result<HashMap<String, serde_json::Value>> {
    let Some(header_value) = header_value else {
        if meta.closed_by_default {
            return Err(GraphQlError::new(
                "a valid JWT is required but was not provided",
            ));
        }
        return Ok(HashMap::new());
    };
    if header_value.is_empty() {
        if meta.closed_by_default {
            return Err(GraphQlError::new(
                "a valid JWT is required but was not provided",
            ));
        }
        return Ok(HashMap::new());
    }
    let token = bearer_token(header_value)?;
    if !meta.jwk_urls.is_empty() {
        return Err(GraphQlError::new(
            "JWK URL verification is not supported in embedded mode; use VerificationKey/Algo",
        ));
    }
    if meta.algo.is_empty() {
        return Err(GraphQlError::new(
            "jwt token cannot be validated because verification algorithm is not set",
        ));
    }
    let header = decode_header(token)
        .map_err(|err| GraphQlError::new(format!("unable to parse jwt token:{err}")))?;
    let token_algo = match header.alg {
        Algorithm::HS256 => "HS256",
        Algorithm::HS384 => "HS384",
        Algorithm::HS512 => "HS512",
        Algorithm::RS256 => "RS256",
        Algorithm::RS384 => "RS384",
        Algorithm::RS512 => "RS512",
        _ => "unknown",
    };
    if token_algo != meta.algo {
        return Err(GraphQlError::new(format!(
            "unexpected signing method: Expected {} Found {token_algo}",
            meta.algo
        )));
    }
    let key = if is_rsa_algorithm(&meta.algo) {
        let pem = meta.rsa_public_key_pem.as_deref().unwrap_or_default();
        DecodingKey::from_rsa_pem(pem)
            .map_err(|err| GraphQlError::new(format!("unable to parse jwt token:{err}")))?
    } else {
        DecodingKey::from_secret(meta.verification_key.as_bytes())
            .map_err(|err| GraphQlError::new(format!("unable to parse jwt token:{err}")))?
    };
    let mut validation = Validation::new(jwt_algorithm(&meta.algo)?);
    validation.validate_aud = false;
    validation.required_spec_claims.clear();
    let data = decode::<serde_json::Value>(token, &key, &validation)
        .map_err(|err| GraphQlError::new(format!("unable to parse jwt token:{err}")))?;
    validate_audience(meta, &data.claims)?;
    merge_auth_vars(meta, &data.claims)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_json_form() {
        let schema = r#"# DataStreamDB.Authorization {"VerificationKey":"secret","Header":"X-Auth","Namespace":"https://example.com/claims","Algo":"HS256"}"#;
        let meta = parse(schema).expect("parse").expect("meta");
        assert_eq!(meta.verification_key, "secret");
        assert_eq!(meta.header, "X-Auth");
        assert_eq!(meta.algo, "HS256");
    }

    #[test]
    fn parses_legacy_form() {
        let schema = r#"# DataStreamDB.Authorization X-Test-Auth https://xyz.io/jwt/claims HS256 "secretkey""#;
        let meta = parse(schema).expect("parse").expect("meta");
        assert_eq!(meta.header, "X-Test-Auth");
        assert_eq!(meta.namespace, "https://xyz.io/jwt/claims");
        assert_eq!(meta.algo, "HS256");
        assert_eq!(meta.verification_key, "secretkey");
    }

    #[test]
    fn rejects_missing_fields() {
        let schema = r#"# DataStreamDB.Authorization {"Header":"X-Auth"}"#;
        let err = parse(schema).expect_err("must fail");
        assert!(err.message.contains("required field missing"), "{err}");
    }

    #[test]
    fn rejects_bad_algorithm() {
        let schema = r#"# DataStreamDB.Authorization {"VerificationKey":"s","Header":"H","Namespace":"N","Algo":"none"}"#;
        let err = parse(schema).expect_err("must fail");
        assert!(err.message.contains("invalid jwt algorithm"), "{err}");
    }

    fn hs_meta() -> AuthMeta {
        parse(
            r#"# DataStreamDB.Authorization {"VerificationKey":"secret","Header":"X-Auth","Namespace":"https://example.com/claims","Algo":"HS256"}"#,
        )
        .expect("parse")
        .expect("meta")
    }

    fn hs_token(claims: &serde_json::Value) -> String {
        jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
            claims,
            &jsonwebtoken::EncodingKey::from_secret(b"secret").expect("key"),
        )
        .expect("encode")
    }

    #[test]
    fn verifies_hs256_and_merges_namespace() {
        let meta = hs_meta();
        let token = hs_token(&serde_json::json!({
            "https://example.com/claims": {"USER": "uid", "ROLE": "admin"},
            "iss": "test",
        }));
        let vars = extract_custom_claims(&meta, Some(&token)).expect("claims");
        assert_eq!(vars.get("USER").and_then(|val| val.as_str()), Some("uid"));
        assert_eq!(vars.get("ROLE").and_then(|val| val.as_str()), Some("admin"));
        assert_eq!(vars.get("iss").and_then(|val| val.as_str()), Some("test"));
        assert!(!vars.contains_key("https://example.com/claims"));
    }

    #[test]
    fn accepts_bearer_prefix() {
        let meta = hs_meta();
        let token = hs_token(&serde_json::json!({"USER": "uid"}));
        let vars = extract_custom_claims(&meta, Some(&format!("Bearer {token}"))).expect("claims");
        assert_eq!(vars.get("USER").and_then(|val| val.as_str()), Some("uid"));
    }

    #[test]
    fn rejects_wrong_signature() {
        let meta = hs_meta();
        let token = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
            &serde_json::json!({"USER": "uid"}),
            &jsonwebtoken::EncodingKey::from_secret(b"other").expect("key"),
        )
        .expect("encode");
        let err = extract_custom_claims(&meta, Some(&token)).expect_err("must fail");
        assert!(err.message.contains("unable to parse jwt token"), "{err}");
    }

    #[test]
    fn rejects_algorithm_mismatch() {
        let meta = hs_meta();
        let token = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS384),
            &serde_json::json!({"USER": "uid"}),
            &jsonwebtoken::EncodingKey::from_secret(b"secret").expect("key"),
        )
        .expect("encode");
        let err = extract_custom_claims(&meta, Some(&token)).expect_err("must fail");
        assert!(err.message.contains("unexpected signing method"), "{err}");
    }

    #[test]
    fn enforces_closed_by_default() {
        let mut meta = hs_meta();
        meta.closed_by_default = true;
        let err = extract_custom_claims(&meta, None).expect_err("must fail");
        assert!(err.message.contains("a valid JWT is required"), "{err}");
        let vars = extract_custom_claims(&hs_meta(), None).expect("open by default");
        assert!(vars.is_empty());
    }

    #[test]
    fn checks_audience() {
        let mut meta = hs_meta();
        meta.audience = vec!["api".to_owned()];
        let token = hs_token(&serde_json::json!({"aud": "api"}));
        extract_custom_claims(&meta, Some(&token)).expect("aud match");
        let token = hs_token(&serde_json::json!({"aud": "other"}));
        let err = extract_custom_claims(&meta, Some(&token)).expect_err("must fail");
        assert!(
            err.message.contains("doesn't match with the audience"),
            "{err}"
        );
    }
}
