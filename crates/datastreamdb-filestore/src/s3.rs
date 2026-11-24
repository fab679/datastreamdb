//! `S3`/`MinIO` object access (Go `s3Handler` + `x/minioclient.go`).
//!
//! [`S3Client`] speaks the S3 REST API with `SigV4` request signing over
//! a blocking HTTP client: `HEAD`/`GET`/`PUT`/`DELETE`, `ListObjectsV2`,
//! multipart uploads, and copy-then-delete rename. Streaming uploads go
//! as 8 MiB multipart parts (like Go's minio client), sidestepping
//! chunked-encoding proxies. Path-style URLs keep dotted buckets
//! working over TLS.
//!
//! Credential chain mirrors Go: explicit request credentials, then
//! environment (`AWS_*` for `s3`, `MINIO_*` for `minio`), otherwise
//! anonymous access when requested. HTTPS endpoints need a TLS-enabled
//! build; this offline build speaks plain HTTP and reports HTTPS
//! destinations as unsupported until the TLS feature lands.

use std::io::{Read, Write};

use crate::s3_auth::{S3Config, S3Error, encode_query, encode_segment, sha256_hex, sign_v4};

/// Blocking S3 client over [`S3Config`].
#[derive(Debug, Clone)]
pub struct S3Client {
    /// Parsed destination.
    config: S3Config,
    /// HTTP client (plain HTTP only in this build).
    http: reqwest::blocking::Client,
}

impl S3Client {
    /// Connects (lazily; no request is sent).
    ///
    /// # Errors
    ///
    /// Returns [`S3Error::Tls`] for `https` endpoints (needs a TLS
    /// build) or [`S3Error::NoCredentials`] when signing is required
    /// but no keys resolved.
    pub fn new(config: S3Config) -> Result<Self, S3Error> {
        if config.secure {
            return Err(S3Error::Tls(config.endpoint.clone()));
        }
        if !config.creds.anonymous && config.creds.access_key.is_empty() {
            return Err(S3Error::NoCredentials);
        }
        Self::with_http(config, reqwest::blocking::Client::builder())
    }

    /// Connects with a caller-built HTTP client (tests disable the
    /// proxy for loopback; production honors proxy environment).
    ///
    /// # Errors
    ///
    /// Returns [`S3Error::Tls`], [`S3Error::NoCredentials`], or
    /// transport failures like [`S3Client::new`].
    pub fn with_http(
        config: S3Config,
        builder: reqwest::blocking::ClientBuilder,
    ) -> Result<Self, S3Error> {
        if config.secure {
            return Err(S3Error::Tls(config.endpoint.clone()));
        }
        if !config.creds.anonymous && config.creds.access_key.is_empty() {
            return Err(S3Error::NoCredentials);
        }
        let http = builder
            .build()
            .map_err(|err| S3Error::Transport(err.to_string()))?;
        Ok(Self { config, http })
    }

    /// Destination config (tests + diagnostics).
    #[must_use]
    pub fn config(&self) -> &S3Config {
        &self.config
    }

    /// Sends one signed request; returns the response on success.
    fn request(
        &self,
        method: &reqwest::Method,
        key: &str,
        query: &[(String, String)],
        extra: &[(String, String)],
        body: Option<reqwest::blocking::Body>,
        payload_hash: &str,
    ) -> Result<reqwest::blocking::Response, S3Error> {
        let path = format!("/{}/{}", self.config.bucket, key);
        let mut url = format!("{}{}", self.config.base_url(), path);
        if !query.is_empty() {
            let encoded = query
                .iter()
                .map(|(name, value)| format!("{}={}", encode_query(name), encode_query(value)))
                .collect::<Vec<_>>()
                .join("&");
            url.push('?');
            url.push_str(&encoded);
        }
        let now = chrono::Utc::now();
        let mut builder = self.http.request(method.to_owned(), &url);
        builder = builder.header("x-amz-content-sha256", payload_hash);
        builder = builder.header("x-amz-date", now.format("%Y%m%dT%H%M%SZ").to_string());
        if !self.config.creds.session_token.is_empty() {
            builder = builder.header("x-amz-security-token", &self.config.creds.session_token);
        }
        for (name, value) in extra {
            builder = builder.header(name.as_str(), value.as_str());
        }
        if !self.config.creds.anonymous {
            let mut headers = vec![("host".to_owned(), self.config.endpoint.clone())];
            headers.extend(extra.iter().cloned());
            let auth = sign_v4(&crate::s3_auth::SignInput {
                method: method.as_str(),
                uri_path: &path,
                query,
                headers: &headers,
                payload_hash,
                creds: &self.config.creds,
                region: &self.config.region,
                now,
            });
            builder = builder.header("authorization", auth);
        }
        if let Some(body) = body {
            builder = builder.body(body);
        }
        builder
            .send()
            .map_err(|err| S3Error::Transport(err.to_string()))
    }

    /// Checks one response, mapping HTTP errors (Go surfaces the
    /// `Code` element; the body prefix carries it here).
    fn check(
        response: reqwest::blocking::Response,
        op: &str,
    ) -> Result<reqwest::blocking::Response, S3Error> {
        if response.status().is_success() {
            return Ok(response);
        }
        let status = response.status().to_string();
        let body = response.text().unwrap_or_default();
        let prefix: String = body.chars().take(200).collect();
        Err(S3Error::Http(op.to_owned(), format!("{status} {prefix}")))
    }

    /// Whether `key` exists (`HEAD`; missing keys report false like
    /// Go's `FileExists`, other failures log and report false).
    #[must_use]
    pub fn exists(&self, key: &str) -> bool {
        match self.request(
            &reqwest::Method::HEAD,
            key,
            &[],
            &[],
            None,
            &sha256_hex(&[]),
        ) {
            Ok(response) => response.status().is_success(),
            Err(err) => {
                tracing::error!("failed to verify object existence: {err}");
                false
            }
        }
    }

    /// Reads one object fully (Go `Read`).
    ///
    /// # Errors
    ///
    /// Returns [`S3Error`] on HTTP or transport failure.
    pub fn get_bytes(&self, key: &str) -> Result<Vec<u8>, S3Error> {
        let response =
            self.request(&reqwest::Method::GET, key, &[], &[], None, &sha256_hex(&[]))?;
        let response = Self::check(response, "get")?;
        response
            .bytes()
            .map(|bytes| bytes.to_vec())
            .map_err(|err| S3Error::Transport(err.to_string()))
    }

    /// Streams one object (Go `Stream`; the response is the reader).
    ///
    /// # Errors
    ///
    /// Returns [`S3Error`] when the object cannot be opened.
    pub fn get_stream(&self, key: &str) -> Result<Box<dyn Read + Send>, S3Error> {
        let response =
            self.request(&reqwest::Method::GET, key, &[], &[], None, &sha256_hex(&[]))?;
        let response = Self::check(response, "get")?;
        Ok(Box::new(response))
    }

    /// Lists keys under `prefix` (Go `ListPaths`, paginated).
    ///
    /// # Errors
    ///
    /// Returns [`S3Error`] on HTTP or transport failure.
    pub fn list(&self, prefix: &str) -> Result<Vec<String>, S3Error> {
        let mut keys = Vec::new();
        let mut token = String::new();
        loop {
            let mut query = vec![
                ("list-type".to_owned(), "2".to_owned()),
                ("prefix".to_owned(), prefix.to_owned()),
            ];
            if !token.is_empty() {
                query.push(("continuation-token".to_owned(), token.clone()));
            }
            let response = self.request(
                &reqwest::Method::GET,
                "",
                &query,
                &[],
                None,
                &sha256_hex(&[]),
            )?;
            let response = Self::check(response, "list")?;
            let body = response
                .text()
                .map_err(|err| S3Error::Transport(err.to_string()))?;
            let (page, truncated, next) = parse_list_xml(&body);
            keys.extend(page);
            if !truncated {
                break;
            }
            token = next;
        }
        Ok(keys)
    }

    /// Uploads bytes in one `PUT`.
    ///
    /// # Errors
    ///
    /// Returns [`S3Error`] on HTTP or transport failure.
    pub fn put_bytes(&self, key: &str, bytes: &[u8]) -> Result<(), S3Error> {
        let hash = sha256_hex(bytes);
        let body = reqwest::blocking::Body::from(bytes.to_vec());
        let response = self.request(&reqwest::Method::PUT, key, &[], &[], Some(body), &hash)?;
        Self::check(response, "put")?;
        Ok(())
    }

    /// Streams an upload as 8 MiB multipart parts (Go `CreateFile`
    /// mechanics): bytes buffer locally and ship per part;
    /// [`S3Writer::close`] finishes with a single `PUT` for small
    /// bodies or completes the multipart upload.
    #[must_use]
    pub fn put_stream(&self, key: &str) -> S3Writer {
        S3Writer::begin(self, key)
    }

    /// Copies then deletes (Go `Rename`, with Go's 100×1s copy retry).
    ///
    /// # Errors
    ///
    /// Returns [`S3Error`] when the copy never lands or the delete
    /// fails.
    pub fn rename(&self, src: &str, dst: &str) -> Result<(), S3Error> {
        let source = format!(
            "/{}/{}",
            self.config.bucket,
            src.split('/')
                .map(encode_segment)
                .collect::<Vec<_>>()
                .join("/")
        );
        let mut last_err = String::new();
        for _ in 0..100 {
            let response = self.request(
                &reqwest::Method::PUT,
                dst,
                &[],
                &[("x-amz-copy-source".to_owned(), source.clone())],
                None,
                &sha256_hex(&[]),
            );
            match response.and_then(|response| Self::check(response, "copy")) {
                Ok(_) => {
                    last_err.clear();
                    break;
                }
                Err(err) => {
                    last_err = err.to_string();
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
            }
        }
        if !last_err.is_empty() {
            return Err(S3Error::Http("rename".to_owned(), last_err));
        }
        let response = self.request(
            &reqwest::Method::DELETE,
            src,
            &[],
            &[],
            None,
            &sha256_hex(&[]),
        )?;
        Self::check(response, "delete")?;
        Ok(())
    }
}

/// Extracts `<Key>` entries plus truncation state from a
/// `ListObjectsV2` document (hand-rolled: the shape is tiny and no XML
/// crate is vendored).
fn parse_list_xml(body: &str) -> (Vec<String>, bool, String) {
    let keys = xml_tag_contents(body, "Key");
    let truncated = xml_tag_contents(body, "IsTruncated")
        .first()
        .is_some_and(|flag| flag == "true");
    let next = xml_tag_contents(body, "NextContinuationToken")
        .first()
        .cloned()
        .unwrap_or_default();
    (keys, truncated, next)
}

/// Extracts every `<tag>…</tag>` body (tiny hand-rolled XML scan).
fn xml_tag_contents(body: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find(open.as_str()) {
        rest = &rest[start + open.len()..];
        let Some(end) = rest.find(close.as_str()) else {
            break;
        };
        out.push(rest[..end].to_owned());
        rest = &rest[end + close.len()..];
    }
    out
}

impl S3Client {
    /// Starts a multipart upload (Go minio streams unknown-length
    /// `PUT`s as multipart under the hood).
    ///
    /// # Errors
    ///
    /// Returns [`S3Error`] on HTTP or transport failure.
    pub fn create_multipart(&self, key: &str) -> Result<String, S3Error> {
        let query = vec![("uploads".to_owned(), String::new())];
        let response = self.request(
            &reqwest::Method::POST,
            key,
            &query,
            &[],
            None,
            &sha256_hex(&[]),
        )?;
        let response = Self::check(response, "create-multipart")?;
        let body = response
            .text()
            .map_err(|err| S3Error::Transport(err.to_string()))?;
        xml_tag_contents(&body, "UploadId")
            .into_iter()
            .next()
            .ok_or_else(|| {
                S3Error::Http("create-multipart".to_owned(), "missing UploadId".to_owned())
            })
    }

    /// Uploads one part (`num` from 1); returns its entity tag.
    ///
    /// # Errors
    ///
    /// Returns [`S3Error`] on HTTP or transport failure.
    pub fn upload_part(
        &self,
        key: &str,
        upload_id: &str,
        num: u32,
        bytes: &[u8],
    ) -> Result<String, S3Error> {
        let query = vec![
            ("partNumber".to_owned(), num.to_string()),
            ("uploadId".to_owned(), upload_id.to_owned()),
        ];
        let hash = sha256_hex(bytes);
        let body = reqwest::blocking::Body::from(bytes.to_vec());
        let response = self.request(&reqwest::Method::PUT, key, &query, &[], Some(body), &hash)?;
        let response = Self::check(response, "upload-part")?;
        response
            .headers()
            .get("etag")
            .and_then(|value| value.to_str().ok())
            .map(|etag| etag.trim_matches('"').to_owned())
            .ok_or_else(|| S3Error::Http("upload-part".to_owned(), "missing ETag".to_owned()))
    }

    /// Completes a multipart upload from `(number, etag)` pairs.
    ///
    /// # Errors
    ///
    /// Returns [`S3Error`] on HTTP or transport failure.
    pub fn complete_multipart(
        &self,
        key: &str,
        upload_id: &str,
        parts: &[(u32, String)],
    ) -> Result<(), S3Error> {
        let query = vec![("uploadId".to_owned(), upload_id.to_owned())];
        let mut xml = String::from("<CompleteMultipartUpload>");
        for (num, etag) in parts {
            use std::fmt::Write as _;
            let _ = write!(
                xml,
                "<Part><PartNumber>{num}</PartNumber><ETag>\"{etag}\"</ETag></Part>"
            );
        }
        xml.push_str("</CompleteMultipartUpload>");
        let hash = sha256_hex(xml.as_bytes());
        let body = reqwest::blocking::Body::from(xml.into_bytes());
        let response = self.request(&reqwest::Method::POST, key, &query, &[], Some(body), &hash)?;
        Self::check(response, "complete-multipart")?;
        Ok(())
    }

    /// Aborts a multipart upload (best effort on drop paths).
    ///
    /// # Errors
    ///
    /// Returns [`S3Error`] on HTTP or transport failure.
    pub fn abort_multipart(&self, key: &str, upload_id: &str) -> Result<(), S3Error> {
        let query = vec![("uploadId".to_owned(), upload_id.to_owned())];
        let response = self.request(
            &reqwest::Method::DELETE,
            key,
            &query,
            &[],
            None,
            &sha256_hex(&[]),
        )?;
        Self::check(response, "abort-multipart")?;
        Ok(())
    }
}

/// Streaming S3 upload (Go `s3Writer` shape, multipart mechanics):
/// `Write` buffers 8 MiB parts and uploads each synchronously;
/// [`S3Writer::close`] finishes with a single `PUT` when nothing was
/// parted, else completes the multipart upload.
pub struct S3Writer {
    /// Owned client (cheap clone).
    client: S3Client,
    /// Destination key.
    key: String,
    /// Active multipart id (`None` until the first part ships).
    upload_id: Option<String>,
    /// Buffered bytes below one part.
    buf: Vec<u8>,
    /// Finished `(number, etag)` pairs.
    parts: Vec<(u32, String)>,
    /// Next part number.
    next_part: u32,
    /// Sticky failure (poisons later writes).
    failed: Option<String>,
}

impl S3Writer {
    /// Part size (S3 needs ≥5 MiB except the last part).
    const PART_SIZE: usize = 8 << 20;

    /// Starts buffering for `key` (no request is sent yet).
    fn begin(client: &S3Client, key: &str) -> Self {
        Self {
            client: client.clone(),
            key: key.to_owned(),
            upload_id: None,
            buf: Vec::new(),
            parts: Vec::new(),
            next_part: 1,
            failed: None,
        }
    }

    /// Ships one full part from the buffer head.
    fn ship_part(&mut self) -> Result<(), S3Error> {
        if self.upload_id.is_none() {
            let id = self.client.create_multipart(&self.key)?;
            self.upload_id = Some(id);
        }
        let id = self.upload_id.clone().expect("multipart active");
        let chunk: Vec<u8> = self.buf.drain(..Self::PART_SIZE).collect();
        let etag = self
            .client
            .upload_part(&self.key, &id, self.next_part, &chunk)?;
        self.parts.push((self.next_part, etag));
        self.next_part += 1;
        Ok(())
    }

    /// Finishes the upload: single `PUT` for small bodies, multipart
    /// completion otherwise (Go `s3Writer.Close` semantics).
    ///
    /// # Errors
    ///
    /// Returns [`S3Error`] when any part or the completion fails.
    ///
    /// # Panics
    ///
    /// Panics only on internal state corruption (unreachable: the
    /// multipart id is always set before parts ship).
    pub fn close(mut self) -> Result<(), S3Error> {
        if let Some(err) = self.failed.clone() {
            self.abort_best_effort();
            return Err(S3Error::Upload(err));
        }
        if self.upload_id.is_none() {
            return self.client.put_bytes(&self.key, &self.buf);
        }
        if !self.buf.is_empty() {
            let id = self.upload_id.clone().expect("multipart active");
            let chunk = std::mem::take(&mut self.buf);
            match self
                .client
                .upload_part(&self.key, &id, self.next_part, &chunk)
            {
                Ok(etag) => self.parts.push((self.next_part, etag)),
                Err(err) => {
                    self.abort_best_effort();
                    return Err(err);
                }
            }
        }
        let id = self.upload_id.clone().expect("multipart active");
        match self.client.complete_multipart(&self.key, &id, &self.parts) {
            Ok(()) => {
                self.upload_id = None;
                Ok(())
            }
            Err(err) => {
                self.abort_best_effort();
                Err(err)
            }
        }
    }

    /// Aborts the multipart upload, logging failures.
    fn abort_best_effort(&mut self) {
        if let Some(id) = self.upload_id.take() {
            if let Err(err) = self.client.abort_multipart(&self.key, &id) {
                tracing::warn!("s3 abort failed for {}: {err}", self.key);
            }
        }
    }
}

impl Write for S3Writer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Some(err) = &self.failed {
            return Err(std::io::Error::other(err.clone()));
        }
        self.buf.extend_from_slice(buf);
        while self.buf.len() >= Self::PART_SIZE {
            if let Err(err) = self.ship_part() {
                let message = err.to_string();
                self.failed = Some(message.clone());
                return Err(std::io::Error::other(message));
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for S3Writer {
    fn drop(&mut self) {
        // Unclosed writers abort any open upload (their bytes are
        // incomplete by definition).
        self.abort_best_effort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::s3_auth::S3Credentials;

    #[test]
    fn list_xml_parses_pages() {
        let body = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult><Name>b</Name><Prefix>p</Prefix><Key>p/a</Key><Key>p/b</Key>
<IsTruncated>true</IsTruncated><NextContinuationToken>tok</NextContinuationToken></ListBucketResult>"#;
        let (keys, truncated, next) = parse_list_xml(body);
        assert_eq!(keys, vec!["p/a".to_owned(), "p/b".to_owned()]);
        assert!(truncated);
        assert_eq!(next, "tok");
        let (keys, truncated, _) = parse_list_xml("<ListBucketResult></ListBucketResult>");
        assert!(keys.is_empty());
        assert!(!truncated);
    }

    #[test]
    fn https_needs_tls_build() {
        let config =
            S3Config::parse("s3://host.example/bucket", S3Credentials::default()).expect("parse");
        assert!(config.secure);
        assert!(matches!(S3Client::new(config), Err(S3Error::Tls(_))));
    }
}
