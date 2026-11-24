//! `S3` credentials, destination parsing, and `SigV4` signing.

/// Default S3 endpoint when the URI carries no dotted host (Go
/// `defaultEndpointS3`).
const DEFAULT_S3_ENDPOINT: &str = "s3.amazonaws.com";
/// Fallback region (`MinIO` default; Go relies on bucket location).
const DEFAULT_REGION: &str = "us-east-1";
/// `SigV4` service name.
const SERVICE: &str = "s3";

/// S3 failures.
#[derive(Debug, thiserror::Error)]
pub enum S3Error {
    /// Malformed destination URL.
    #[error("bad s3 url: {0}")]
    Url(String),
    /// No usable credentials and access is not anonymous.
    #[error("no s3 credentials (set request keys or environment)")]
    NoCredentials,
    /// HTTPS needs a TLS-enabled build.
    #[error("https s3 endpoints need TLS support: {0}")]
    Tls(String),
    /// HTTP failure (status + body prefix).
    #[error("s3 {0} failed: {1}")]
    Http(String, String),
    /// Transport failure.
    #[error("s3 transport failed: {0}")]
    Transport(String),
    /// Upload-stream failure.
    #[error("s3 upload failed: {0}")]
    Upload(String),
}

/// S3 credentials (Go `MinioCredentials`).
#[derive(Debug, Clone, Default)]
pub struct S3Credentials {
    /// Access key id (empty falls back to the environment).
    pub access_key: String,
    /// Secret access key.
    pub secret_key: String,
    /// Session token (STS).
    pub session_token: String,
    /// Skip signing entirely (public buckets).
    pub anonymous: bool,
}

impl S3Credentials {
    /// Fills empty fields from the environment (Go credential chain).
    #[must_use]
    pub fn with_env(mut self, scheme: &str) -> Self {
        if self.anonymous || !self.access_key.is_empty() {
            return self;
        }
        let (user_var, pass_var, token_var) = if scheme == "s3" {
            (
                "AWS_ACCESS_KEY_ID",
                "AWS_SECRET_ACCESS_KEY",
                "AWS_SESSION_TOKEN",
            )
        } else {
            (
                "MINIO_ACCESS_KEY",
                "MINIO_SECRET_KEY",
                "MINIO_SESSION_TOKEN",
            )
        };
        if let Ok(value) = std::env::var(user_var) {
            self.access_key = value;
        }
        if let Ok(value) = std::env::var(pass_var) {
            self.secret_key = value;
        }
        if let Ok(value) = std::env::var(token_var) {
            self.session_token = value;
        }
        self
    }
}

/// Parsed S3 destination (Go `s3Handler` fields).
#[derive(Debug, Clone)]
pub struct S3Config {
    /// `host:port` endpoint.
    pub endpoint: String,
    /// Use `https` (fails without a TLS build).
    pub secure: bool,
    /// Bucket name.
    pub bucket: String,
    /// Key prefix inside the bucket (may be empty).
    pub prefix: String,
    /// Signing region.
    pub region: String,
    /// Credentials.
    pub creds: S3Credentials,
}

impl S3Config {
    /// Parses `s3://[host/]bucket/prefix` or
    /// `minio://host:port/bucket/prefix` (Go `NewS3Handler`).
    /// `?secure=false` disables TLS; `?region=` overrides it.
    ///
    /// # Errors
    ///
    /// Returns [`S3Error::Url`] for unknown schemes, missing hosts
    /// (minio), or missing buckets.
    pub fn parse(destination: &str, creds: S3Credentials) -> Result<Self, S3Error> {
        let (scheme, rest) = destination
            .split_once("://")
            .ok_or_else(|| S3Error::Url(destination.to_owned()))?;
        if scheme != "s3" && scheme != "minio" {
            return Err(S3Error::Url(format!("unknown scheme {scheme}")));
        }
        let (authority_path, query) = match rest.split_once('?') {
            Some((head, tail)) => (head, tail),
            None => (rest, ""),
        };
        let (host, path) = match authority_path.split_once('/') {
            Some((host, path)) => (host, path),
            None => (authority_path, ""),
        };
        let mut host = host.to_owned();
        if scheme == "s3" && !host.contains('.') {
            if host.is_empty() {
                // `s3:///bucket/...`: empty host means default endpoint.
            } else {
                // `s3://bucket/...`: single label is the bucket.
                host.clear();
            }
        }
        if scheme == "minio" && host.is_empty() {
            return Err(S3Error::Url("minio handler requires a host".to_owned()));
        }
        if scheme == "s3" && host.is_empty() {
            DEFAULT_S3_ENDPOINT.clone_into(&mut host);
        }
        // When the bucket rode in the host slot, prepend it to the path.
        let full_path = if scheme == "s3"
            && authority_path
                .split_once('/')
                .is_none_or(|(h, _)| !h.contains('.'))
            && !authority_path.is_empty()
            && !authority_path.starts_with('/')
        {
            authority_path.to_owned()
        } else {
            path.to_owned()
        };
        let mut segments = full_path.split('/').filter(|part| !part.is_empty());
        let Some(bucket) = segments.next() else {
            return Err(S3Error::Url(format!("invalid bucket: {destination:?}")));
        };
        let prefix = segments.collect::<Vec<_>>().join("/");
        let mut secure = true;
        let mut region = DEFAULT_REGION.to_owned();
        for pair in query.split('&') {
            if pair.is_empty() {
                continue;
            }
            match pair.split_once('=') {
                Some(("secure", "false")) => secure = false,
                Some(("region", value)) if !value.is_empty() => value.clone_into(&mut region),
                _ => {}
            }
        }
        Ok(Self {
            endpoint: host,
            secure,
            bucket: bucket.to_owned(),
            prefix,
            region,
            creds: creds.with_env(scheme),
        })
    }

    /// Base URL (`http(s)://endpoint`).
    #[must_use]
    pub fn base_url(&self) -> String {
        format!(
            "http{}://{}",
            if self.secure { "s" } else { "" },
            self.endpoint
        )
    }

    /// Object key for a destination-relative path.
    #[must_use]
    pub fn key_for(&self, path: &str) -> String {
        let clean = path.trim_start_matches('/');
        if self.prefix.is_empty() {
            clean.to_owned()
        } else if clean.is_empty() {
            self.prefix.clone()
        } else {
            format!("{}/{}", self.prefix, clean)
        }
    }
}

/// Percent-encodes one URI segment (`SigV4` canonical form).
pub(crate) fn encode_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            use std::fmt::Write as _;
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// Percent-encodes a query name/value (`SigV4` also escapes `/` here).
pub(crate) fn encode_query(text: &str) -> String {
    text.bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

/// Lowercase hex encoding.
pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Hex SHA-256.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    hex_encode(&sha2::Sha256::digest(bytes))
}

/// HMAC-SHA256.
pub(crate) fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    use hmac::{KeyInit as _, Mac as _};
    let mut mac =
        hmac::Hmac::<sha2::Sha256>::new_from_slice(key).expect("hmac accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// One request's signing inputs.
#[derive(Debug)]
pub struct SignInput<'a> {
    /// HTTP method (`GET`, `PUT`, …).
    pub method: &'a str,
    /// Raw path (`/bucket/key`).
    pub uri_path: &'a str,
    /// Raw (unencoded) query pairs.
    pub query: &'a [(String, String)],
    /// Signed headers beyond the standard set (e.g. `x-amz-copy-source`).
    pub headers: &'a [(String, String)],
    /// Hex payload hash (or `UNSIGNED-PAYLOAD`).
    pub payload_hash: &'a str,
    /// Credentials.
    pub creds: &'a S3Credentials,
    /// Signing region.
    pub region: &'a str,
    /// Request timestamp.
    pub now: chrono::DateTime<chrono::Utc>,
}

/// `SigV4` authorization header value for one request (pure in `now`).
#[must_use]
pub fn sign_v4(input: &SignInput<'_>) -> String {
    let amz_date = input.now.format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = input.now.format("%Y%m%d").to_string();
    let mut all_headers: Vec<(String, String)> = input
        .headers
        .iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    all_headers.push((
        "x-amz-content-sha256".to_owned(),
        input.payload_hash.to_owned(),
    ));
    all_headers.push(("x-amz-date".to_owned(), amz_date.clone()));
    if !input.creds.session_token.is_empty() {
        all_headers.push((
            "x-amz-security-token".to_owned(),
            input.creds.session_token.clone(),
        ));
    }
    all_headers.sort_by(|left, right| left.0.cmp(&right.0));
    let canonical_uri: String = input
        .uri_path
        .split('/')
        .map(encode_segment)
        .collect::<Vec<_>>()
        .join("/");
    let canonical_uri = if canonical_uri.starts_with('/') {
        canonical_uri
    } else {
        format!("/{canonical_uri}")
    };
    let mut pairs: Vec<(String, String)> = input
        .query
        .iter()
        .map(|(name, value)| (encode_query(name), encode_query(value)))
        .collect();
    pairs.sort();
    let mut canonical_query = String::new();
    for (index, (name, value)) in pairs.iter().enumerate() {
        if index > 0 {
            canonical_query.push('&');
        }
        canonical_query.push_str(name);
        canonical_query.push('=');
        canonical_query.push_str(value);
    }
    let mut canonical_headers = String::new();
    for (name, value) in &all_headers {
        canonical_headers.push_str(name);
        canonical_headers.push(':');
        canonical_headers.push_str(value);
        canonical_headers.push('\n');
    }
    let signed_headers = all_headers
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let canonical_request = format!(
        "{}\n{canonical_uri}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{}",
        input.method, input.payload_hash
    );
    let scope = format!("{date_stamp}/{}/{SERVICE}/aws4_request", input.region);
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );
    let signing_key = hmac_sha256(
        &hmac_sha256(
            &hmac_sha256(
                &hmac_sha256(
                    format!("AWS4{}", input.creds.secret_key).as_bytes(),
                    date_stamp.as_bytes(),
                ),
                input.region.as_bytes(),
            ),
            SERVICE.as_bytes(),
        ),
        b"aws4_request",
    );
    let signature = hex_encode(&hmac_sha256(&signing_key, string_to_sign.as_bytes()));
    format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        input.creds.access_key
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone as _;

    #[test]
    fn signer_matches_openssl_oracle() {
        // Independent vector: canonical request + HMAC chain computed
        // with python-hashlib/openssl (see task notes), AKID/SECRET,
        // GET /bucket/key, us-east-1, 20240115T093045Z, empty body.
        let creds = S3Credentials {
            access_key: "AKID".to_owned(),
            secret_key: "SECRET".to_owned(),
            ..S3Credentials::default()
        };
        let now = chrono::Utc
            .with_ymd_and_hms(2024, 1, 15, 9, 30, 45)
            .single()
            .expect("time");
        let auth = sign_v4(&SignInput {
            method: "GET",
            uri_path: "/bucket/key",
            query: &[],
            headers: &[("host".to_owned(), "127.0.0.1:9000".to_owned())],
            payload_hash: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            creds: &creds,
            region: "us-east-1",
            now,
        });
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 Credential=AKID/20240115/us-east-1/s3/aws4_request, \
             SignedHeaders=host;x-amz-content-sha256;x-amz-date, \
             Signature=f82cf4d0a7680b7da975b69b8bf834713fce39c2b0e58a97b1b5eb2c75afe907"
        );
    }

    #[test]
    fn urls_parse_like_go() {
        let creds = S3Credentials::default();
        let config = S3Config::parse("s3:///bucket/folder", creds.clone()).expect("s3");
        assert_eq!(config.endpoint, DEFAULT_S3_ENDPOINT);
        assert_eq!(config.bucket, "bucket");
        assert_eq!(config.prefix, "folder");
        assert!(config.secure);

        let config = S3Config::parse("minio://127.0.0.1:9000/bkt/pre?secure=false", creds.clone())
            .expect("minio");
        assert_eq!(config.endpoint, "127.0.0.1:9000");
        assert_eq!(config.bucket, "bkt");
        assert_eq!(config.prefix, "pre");
        assert!(!config.secure);
        assert_eq!(config.base_url(), "http://127.0.0.1:9000");
        assert_eq!(config.key_for("a/b"), "pre/a/b");

        assert!(S3Config::parse("minio:///bkt", creds.clone()).is_err());
        assert!(S3Config::parse("s3://", creds.clone()).is_err());
        assert!(S3Config::parse("ftp://x/y", creds).is_err());
    }
}
