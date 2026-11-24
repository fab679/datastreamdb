//! Backup/restore/export roundtrips against a fake S3 (no network).
//!
//! A minimal in-process object store (plain `PUT`/`GET`/`HEAD`/
//! `DELETE`, copy-source puts, multipart assembly) proves the driver
//! destination dispatch end to end. Protocol depth (signing,
//! pagination, streaming) lives in `datastreamdb-filestore`'s own suite.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;

/// Staged multipart parts by upload id, then part number.
type Uploads = Arc<Mutex<HashMap<String, HashMap<u32, Vec<u8>>>>>;

/// Fake objects + open multipart uploads.
#[derive(Debug, Clone, Default)]
struct FakeS3 {
    /// `bucket/key` bytes.
    objects: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    /// Staged multipart parts by upload id.
    uploads: Uploads,
}

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
        upload.insert(num, body.to_vec());
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
    body: Bytes,
) -> axum::response::Response {
    if params.contains_key("uploads") {
        let id = format!("upload-{}", fake.uploads.lock().expect("id lock").len());
        fake.uploads
            .lock()
            .expect("uploads lock")
            .insert(id.clone(), HashMap::new());
        // Remember the destination alongside part 0's slot trick: the
        // fake keys uploads by id and recovers dest from completion.
        fake.objects.lock().expect("store lock").insert(
            format!("__upload__/{id}"),
            format!("{bucket}/{key}").into_bytes(),
        );
        return format!(
            r#"<?xml version="1.0"?><InitiateMultipartUploadResult><UploadId>{id}</UploadId></InitiateMultipartUploadResult>"#
        )
        .into_response();
    }
    let Some(upload_id) = params.get("uploadId") else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let mut uploads = fake.uploads.lock().expect("uploads lock");
    let Some(parts) = uploads.remove(upload_id) else {
        return no_such_key();
    };
    let dest = fake
        .objects
        .lock()
        .expect("store lock")
        .remove(&format!("__upload__/{upload_id}"))
        .and_then(|raw| String::from_utf8(raw).ok())
        .unwrap_or_default();
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
        let Some(part) = parts.get(&num) else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        assembled.extend_from_slice(part);
    }
    fake.objects
        .lock()
        .expect("store lock")
        .insert(dest, assembled);
    StatusCode::OK.into_response()
}

async fn get_object(
    State(fake): State<FakeS3>,
    Path((bucket, key)): Path<(String, String)>,
) -> axum::response::Response {
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
) -> axum::response::Response {
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

async fn delete_object(
    State(fake): State<FakeS3>,
    Path((bucket, key)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
) -> axum::response::Response {
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

/// Boots the fake on an ephemeral port.
fn boot_fake() -> String {
    use axum::routing::get;
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
        .layer(axum::extract::DefaultBodyLimit::disable())
        .with_state(fake);
    let (tx, rx) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind");
            tx.send(listener.local_addr().expect("addr").to_string())
                .expect("addr send");
            axum::serve(listener, app).await.expect("serve");
        });
    });
    std::mem::forget(server);
    rx.recv_timeout(std::time::Duration::from_secs(10))
        .expect("endpoint")
}

fn test_setup() -> (
    std::sync::Arc<datastreamdb_posting::PostingEngine>,
    std::sync::Arc<datastreamdb_worker::GroupState>,
) {
    let schema = std::sync::Arc::new(datastreamdb_schema::SchemaState::new());
    schema.set(
        "0-name",
        datastreamdb_proto::SchemaUpdate {
            predicate: "0-name".to_owned(),
            value_type: datastreamdb_proto::ValType::String as i32,
            ..datastreamdb_proto::SchemaUpdate::default()
        },
    );
    let engine = datastreamdb_posting::PostingEngine::open(
        std::sync::Arc::new(datastreamdb_store::MemoryBackend::new()),
        schema,
        64,
    );
    let txn = engine.begin();
    let mut edge = datastreamdb_proto::DirectedEdge {
        entity: 2,
        attr: "0-name".to_owned(),
        value: b"Alice".to_vec(),
        value_type: datastreamdb_proto::ValType::String as i32,
        op: datastreamdb_proto::EdgeOp::Set as i32,
        ..datastreamdb_proto::DirectedEdge::default()
    };
    let key = datastreamdb_x::data_key("0-name", 2);
    txn.get(&key)
        .expect("list")
        .add_mutation_with_index(&mut edge, &txn)
        .expect("mutate");
    engine.commit(&txn).expect("commit");
    let groups = std::sync::Arc::new(datastreamdb_worker::GroupState::new(
        1,
        1,
        "test".to_owned(),
    ));
    (engine, groups)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backup_restore_roundtrip_over_s3() {
    let (engine, groups) = test_setup();
    let endpoint = boot_fake();
    let destination = format!("minio://{endpoint}/bucket/backups?secure=false");
    let peers = std::sync::Mutex::new(std::collections::HashMap::new());
    let req = datastreamdb_proto::pb::BackupRequest {
        destination: destination.clone(),
        access_key: "TESTKEY".to_owned(),
        secret_key: "TESTSECRET".to_owned(),
        ..datastreamdb_proto::pb::BackupRequest::default()
    };
    datastreamdb_worker::backup::process_backup_request(&engine, &groups, &peers, &req, None)
        .await
        .expect("backup");

    let fresh_schema = std::sync::Arc::new(datastreamdb_schema::SchemaState::new());
    let fresh = datastreamdb_posting::PostingEngine::open(
        std::sync::Arc::new(datastreamdb_store::MemoryBackend::new()),
        fresh_schema.clone(),
        64,
    );
    let restore = datastreamdb_proto::pb::RestoreRequest {
        group_id: 1,
        location: destination,
        access_key: "TESTKEY".to_owned(),
        secret_key: "TESTSECRET".to_owned(),
        ..datastreamdb_proto::pb::RestoreRequest::default()
    };
    // Blocking driver IO runs off the runtime (like the RPC handlers).
    let moved = fresh.clone();
    let status = tokio::task::spawn_blocking(move || {
        datastreamdb_worker::backup::run_restore(&moved, &groups, &restore, None)
    })
    .await
    .expect("join")
    .expect("restore");
    assert_eq!(status.code, 0);
    assert_eq!(fresh_schema.predicates(), vec!["0-name".to_owned()]);
    let read_ts = fresh.oracle().max_assigned();
    let key = datastreamdb_x::data_key("0-name", 2);
    let list = fresh.get_list(&key, read_ts).expect("list");
    assert_eq!(list.all_values(read_ts).expect("values").len(), 1);
}

#[test]
fn export_writes_three_files_over_s3() {
    let (engine, groups) = test_setup();
    let endpoint = boot_fake();
    let req = datastreamdb_proto::pb::ExportRequest {
        group_id: 1,
        format: "rdf".to_owned(),
        destination: format!("minio://{endpoint}/bucket/exports?secure=false"),
        access_key: "TESTKEY".to_owned(),
        secret_key: "TESTSECRET".to_owned(),
        ..datastreamdb_proto::pb::ExportRequest::default()
    };
    let response =
        datastreamdb_worker::backup::run_export(&engine, &groups, &req, None).expect("export");
    assert_eq!(response.code, 0);
    assert_eq!(response.files.len(), 3);
    assert!(response.files[0].ends_with(".rdf.gz"));
}
