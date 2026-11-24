//! S3 client roundtrips against an in-process fake (no network).
//!
//! The fake speaks just enough S3 REST (`PUT`/`GET`/`HEAD`/`DELETE`
//! objects, `ListObjectsV2`, copy-source puts) to prove the `SigV4`
//! signer, framing, pagination, streaming uploads, and rename path.

use std::collections::HashMap;
use std::io::Read;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;

/// One recorded request (auth + framing assertions).
#[derive(Debug, Clone)]
struct Seen {
    /// HTTP method.
    method: String,
    /// Request path.
    path: String,
    /// Authorization header, if sent.
    auth: Option<String>,
    /// Payload-hash header, if sent.
    content_sha: Option<String>,
}

/// Open multipart upload (key + staged parts).
#[derive(Debug, Clone, Default)]
struct Multipart {
    /// Destination `bucket/key`.
    dest: String,
    /// Staged parts by number.
    parts: HashMap<u32, Vec<u8>>,
}

/// Fake object store + request journal.
#[derive(Debug, Clone, Default)]
struct FakeS3 {
    /// `bucket/key` bytes.
    objects: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    /// Seen requests.
    seen: Arc<Mutex<Vec<Seen>>>,
    /// Open multipart uploads by id.
    uploads: Arc<Mutex<HashMap<String, Multipart>>>,
    /// Upload id counter.
    next_id: Arc<Mutex<u64>>,
}

impl FakeS3 {
    /// Records one request's security headers.
    fn record(&self, method: &str, path: &str, headers: &HeaderMap) {
        let auth = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let content_sha = headers
            .get("x-amz-content-sha256")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        self.seen.lock().expect("seen lock").push(Seen {
            method: method.to_owned(),
            path: path.to_owned(),
            auth,
            content_sha,
        });
    }
}

/// Objects failing closed on missing keys (S3 `NoSuchKey` shape).
fn no_such_key() -> axum::response::Response {
    (
        StatusCode::NOT_FOUND,
        r#"<?xml version="1.0"?><Error><Code>NoSuchKey</Code></Error>"#,
    )
        .into_response()
}

async fn put_object(
    State(fake): State<FakeS3>,
    Path((bucket, key)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let path = format!("/{bucket}/{key}");
    fake.record("PUT", &path, &headers);
    if let Some(source) = headers
        .get("x-amz-copy-source")
        .and_then(|value| value.to_str().ok())
    {
        let source = source.trim_start_matches('/').to_owned();
        let objects = fake.objects.lock().expect("store lock");
        let Some(bytes) = objects.get(&source).cloned() else {
            return no_such_key();
        };
        drop(objects);
        fake.objects
            .lock()
            .expect("store lock")
            .insert(format!("{bucket}/{key}"), bytes);
        return StatusCode::OK.into_response();
    }
    if let (Some(part), Some(upload_id)) = (params.get("partNumber"), params.get("uploadId")) {
        let num: u32 = part.parse().unwrap_or(0);
        let mut uploads = fake.uploads.lock().expect("uploads lock");
        let Some(upload) = uploads.get_mut(upload_id) else {
            return no_such_key();
        };
        upload.parts.insert(num, body.to_vec());
        return ([("ETag", format!("\"p{num}\""))], StatusCode::OK).into_response();
    }
    fake.objects
        .lock()
        .expect("store lock")
        .insert(format!("{bucket}/{key}"), body.to_vec());
    StatusCode::OK.into_response()
}

async fn post_object(
    State(fake): State<FakeS3>,
    Path((bucket, key)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let path = format!("/{bucket}/{key}");
    fake.record("POST", &path, &headers);
    if params.contains_key("uploads") {
        let mut next_id = fake.next_id.lock().expect("id lock");
        *next_id += 1;
        let id = format!("upload-{next_id}");
        fake.uploads.lock().expect("uploads lock").insert(
            id.clone(),
            Multipart {
                dest: format!("{bucket}/{key}"),
                parts: HashMap::new(),
            },
        );
        return format!(
            r#"<?xml version="1.0"?><InitiateMultipartUploadResult><UploadId>{id}</UploadId></InitiateMultipartUploadResult>"#
        )
        .into_response();
    }
    let Some(upload_id) = params.get("uploadId") else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    // Complete: concatenate parts in listed order (fake trusts order).
    let mut uploads = fake.uploads.lock().expect("uploads lock");
    let Some(upload) = uploads.remove(upload_id) else {
        return no_such_key();
    };
    let text = String::from_utf8_lossy(&body).into_owned();
    let mut numbers: Vec<u32> = Vec::new();
    for part in text.split("<Part>") {
        if let Some(rest) = part.split_once("<PartNumber>") {
            if let Some((num, _)) = rest.1.split_once("</PartNumber>") {
                if let Ok(num) = num.parse::<u32>() {
                    numbers.push(num);
                }
            }
        }
    }
    let mut assembled = Vec::new();
    for num in numbers {
        let Some(part) = upload.parts.get(&num) else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        assembled.extend_from_slice(part);
    }
    fake.objects
        .lock()
        .expect("store lock")
        .insert(upload.dest, assembled);
    StatusCode::OK.into_response()
}

async fn delete_object(
    State(fake): State<FakeS3>,
    Path((bucket, key)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> axum::response::Response {
    let path = format!("/{bucket}/{key}");
    fake.record("DELETE", &path, &headers);
    if let Some(upload_id) = params.get("uploadId") {
        fake.uploads.lock().expect("uploads lock").remove(upload_id);
        return StatusCode::NO_CONTENT.into_response();
    }
    fake.objects
        .lock()
        .expect("store lock")
        .remove(&format!("{bucket}/{key}"));
    StatusCode::NO_CONTENT.into_response()
}

async fn get_object(
    State(fake): State<FakeS3>,
    Path((bucket, key)): Path<(String, String)>,
    headers: HeaderMap,
) -> axum::response::Response {
    let path = format!("/{bucket}/{key}");
    fake.record("GET", &path, &headers);
    fake.objects
        .lock()
        .expect("store lock")
        .get(&format!("{bucket}/{key}"))
        .cloned()
        .map_or_else(no_such_key, IntoResponse::into_response)
}

async fn head_object(
    State(fake): State<FakeS3>,
    Path((bucket, key)): Path<(String, String)>,
    headers: HeaderMap,
) -> axum::response::Response {
    let path = format!("/{bucket}/{key}");
    fake.record("HEAD", &path, &headers);
    if fake
        .objects
        .lock()
        .expect("store lock")
        .contains_key(&format!("{bucket}/{key}"))
    {
        StatusCode::OK.into_response()
    } else {
        no_such_key()
    }
}

async fn list_objects(
    State(fake): State<FakeS3>,
    Path(bucket): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> axum::response::Response {
    fake.record("LIST", &format!("/{bucket}"), &headers);
    if params.get("list-type").map(String::as_str) != Some("2") {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let prefix = params.get("prefix").cloned().unwrap_or_default();
    let token = params
        .get("continuation-token")
        .cloned()
        .unwrap_or_default();
    let objects = fake.objects.lock().expect("store lock");
    let mut keys: Vec<String> = objects
        .keys()
        .filter_map(|full| {
            full.strip_prefix(&format!("{bucket}/"))
                .filter(|key| key.starts_with(&prefix))
                .map(str::to_owned)
        })
        .collect();
    keys.sort();
    // Two keys per page so continuation is always exercised.
    if !token.is_empty() {
        keys.retain(|key| key.as_str() > token.as_str());
    }
    let truncated = keys.len() > 2;
    let page: Vec<String> = keys.into_iter().take(2).collect();
    let next = if truncated {
        format!(
            "<NextContinuationToken>{}</NextContinuationToken>",
            page.last().map(String::as_str).unwrap_or_default()
        )
    } else {
        String::new()
    };
    let mut entries = String::new();
    for key in &page {
        entries.push_str("<Key>");
        entries.push_str(key);
        entries.push_str("</Key>");
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><ListBucketResult><Name>{bucket}</Name>{entries}<IsTruncated>{truncated}</IsTruncated>{next}</ListBucketResult>"#
    )
    .into_response()
}

/// Boots the fake on an ephemeral port; returns its endpoint + journal.
fn boot_fake() -> (String, FakeS3) {
    let fake = FakeS3::default();
    let app = axum::Router::new()
        .route(
            "/{bucket}/{*key}",
            get(get_object)
                .put(put_object)
                .post(post_object)
                .head(head_object)
                .delete(delete_object),
        )
        .route("/{bucket}", get(list_objects))
        .route("/{bucket}/", get(list_objects))
        .layer(axum::extract::DefaultBodyLimit::disable())
        .with_state(fake.clone());
    let (tx, rx) = std::sync::mpsc::channel();
    let serve_fake = fake.clone();
    let server = std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind");
            let addr = listener.local_addr().expect("addr");
            tx.send(addr.to_string()).expect("addr send");
            axum::serve(listener, app).await.expect("serve");
        });
    });
    // The server thread runs for the whole test binary (no shutdown).
    std::mem::forget(server);
    let endpoint = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("endpoint");
    (endpoint, serve_fake)
}

/// Client over the fake (unsigned-payload streaming, signed rest).
fn fake_client(endpoint: &str) -> datastreamdb_filestore::S3Client {
    let creds = datastreamdb_filestore::S3Credentials {
        access_key: "TESTKEY".to_owned(),
        secret_key: "TESTSECRET".to_owned(),
        ..datastreamdb_filestore::S3Credentials::default()
    };
    let config = datastreamdb_filestore::S3Config::parse(
        &format!("minio://{endpoint}/bucket/pre?secure=false"),
        creds,
    )
    .expect("config");
    // Loopback stays direct even where a proxy is exported.
    datastreamdb_filestore::S3Client::with_http(
        config,
        reqwest::blocking::Client::builder().no_proxy(),
    )
    .expect("client")
}

#[test]
fn put_get_head_delete_roundtrip() {
    let (endpoint, fake) = boot_fake();
    let client = fake_client(&endpoint);
    assert!(!client.exists("obj"));
    client.put_bytes("obj", b"hello").expect("put");
    assert!(client.exists("obj"));
    assert_eq!(client.get_bytes("obj").expect("get"), b"hello");
    let mut streamed = client.get_stream("obj").expect("stream");
    let mut text = Vec::new();
    streamed.read_to_end(&mut text).expect("read");
    assert_eq!(text, b"hello");

    // Every call carried a well-formed SigV4 header.
    for seen in fake.seen.lock().expect("seen lock").iter() {
        let auth = seen.auth.as_ref().expect("signed");
        assert!(auth.starts_with("AWS4-HMAC-SHA256 "), "{auth}");
        assert!(auth.contains("Credential=TESTKEY/"), "{auth}");
        assert!(auth.contains("SignedHeaders="), "{auth}");
        assert!(auth.contains("Signature="), "{auth}");
        // Payload hash header always present (streaming uses unsigned).
        assert!(seen.content_sha.is_some());
    }

    client.rename("obj", "moved").expect("rename");
    assert!(!client.exists("obj"));
    assert_eq!(client.get_bytes("moved").expect("get"), b"hello");
}

#[test]
fn streaming_upload_roundtrips_large_body() {
    use std::io::Write as _;
    let (endpoint, fake) = boot_fake();
    let client = fake_client(&endpoint);
    // 20 MiB forces two full parts plus a remainder (8 MiB parts).
    let chunk = vec![7u8; 1 << 20];
    let mut writer = client.put_stream("big");
    for _ in 0..20 {
        writer.write_all(&chunk).expect("write");
    }
    writer.close().expect("close");
    assert_eq!(client.get_bytes("big").expect("get").len(), 20 << 20);
    // Multipart handshake happened (create + parts + complete).
    let seen = fake.seen.lock().expect("seen lock");
    let puts = seen.iter().filter(|seen| seen.method == "PUT").count();
    let posts = seen.iter().filter(|seen| seen.method == "POST").count();
    assert!(puts >= 3, "expected part PUTs, saw {puts}");
    assert!(posts >= 2, "expected create + complete, saw {posts}");
    assert!(
        seen.iter().all(|seen| seen.path.starts_with("/bucket/")),
        "all calls stay in-bucket"
    );
}

#[test]
fn list_paginates_and_filters_prefix() {
    let (endpoint, _) = boot_fake();
    let client = fake_client(&endpoint);
    for name in ["pre/a/1", "pre/a/2", "pre/a/3", "pre/b/1"] {
        client.put_bytes(name, b"x").expect("put");
    }
    // Three `a/` keys force a second page (two per page).
    let keys = client.list("pre/a/").expect("list");
    assert_eq!(keys, vec!["pre/a/1", "pre/a/2", "pre/a/3"]);
    let keys = client.list("pre/b/").expect("list");
    assert_eq!(keys, vec!["pre/b/1"]);
    let keys = client.list("pre/nope/").expect("list");
    assert!(keys.is_empty());
}

#[test]
fn missing_objects_fail_like_go() {
    let (endpoint, _) = boot_fake();
    let client = fake_client(&endpoint);
    assert!(!client.exists("missing"));
    assert!(client.get_bytes("missing").is_err());
    assert!(client.get_stream("missing").is_err());
}
