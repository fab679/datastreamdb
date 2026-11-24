//! Audit interop: events land in the file sink with masked secrets.

#[path = "common/mod.rs"]
mod common;

use common::{boot_alpha_acl_audit, boot_zero, channel};
use datastreamdb_proto::api::{
    Check, Jwt, LoginRequest, Request, data_stream_db_client::DataStreamDbClient,
};
use prost::Message as _;

/// Interceptor presenting a fixed token as `accessjwt` metadata.
#[derive(Clone)]
struct Token(tonic::metadata::MetadataValue<tonic::metadata::Ascii>);

impl tonic::service::Interceptor for Token {
    fn call(
        &mut self,
        mut request: tonic::Request<()>,
    ) -> Result<tonic::Request<()>, tonic::Status> {
        request.metadata_mut().insert("accessjwt", self.0.clone());
        Ok(request)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn audit_records_masked_events() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("audit.log");
    let (_zdir, zgrpc, _) = boot_zero(1).await;
    let (_adir, agrpc, ahttp) = boot_alpha_acl_audit(
        &zgrpc,
        1,
        true,
        vec![7u8; 64],
        Some(log.to_string_lossy().into_owned()),
    )
    .await;
    let mut anon = DataStreamDbClient::new(channel(&agrpc).await);

    // Login (masked), version check, and one failed call.
    let jwt = Jwt::decode(
        anon.login(LoginRequest {
            userid: "groot".to_owned(),
            password: "password".to_owned(),
            ..LoginRequest::default()
        })
        .await
        .expect("login")
        .into_inner()
        .json
        .as_slice(),
    )
    .expect("jwt");
    anon.check_version(Check::default()).await.expect("version");
    anon.query(Request {
        query: "{ q(func: has(name)) { uid } }".to_owned(),
        ..Request::default()
    })
    .await
    .expect_err("anon denied");
    let token: tonic::metadata::MetadataValue<_> = jwt.access_jwt.parse().expect("token");
    let mut groot = DataStreamDbClient::with_interceptor(channel(&agrpc).await, Token(token));
    groot
        .query(Request {
            query: "{ q(func: has(name)) { uid } }".to_owned(),
            ..Request::default()
        })
        .await
        .expect("authed query");

    // HTTP query lands; /health is skipped.
    let http = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("client");
    http.post(format!("http://{ahttp}/query"))
        .header("X-DataStreamDB-AccessJWT", &jwt.access_jwt)
        .body("{ q(func: has(name)) { uid } }")
        .send()
        .await
        .expect("http query");
    http.get(format!("http://{ahttp}/health"))
        .send()
        .await
        .expect("health");

    let text = std::fs::read_to_string(&log).expect("audit log");
    let lines: Vec<&str> = text.lines().collect();
    assert!(!lines.is_empty());
    let mut endpoints: Vec<String> = Vec::new();
    for line in &lines {
        let event: serde_json::Value = serde_json::from_str(line).expect("jsonl");
        let endpoint = event["endpoint"].as_str().expect("endpoint").to_owned();
        // Leak-check: login bodies mask the password value.
        if endpoint == "/api.DataStreamDB/Login" {
            let body = event["req_body"].as_str().unwrap_or_default();
            assert!(body.contains("*******"), "{body}");
            assert!(!body.contains("password: \"password\""), "{body}");
        }
        endpoints.push(endpoint);
    }
    for want in [
        "/api.DataStreamDB/Login",
        "/api.DataStreamDB/CheckVersion",
        "/api.DataStreamDB/Query",
        "/query",
    ] {
        assert!(
            endpoints.contains(&want.to_owned()),
            "{want} in {endpoints:?}"
        );
    }
    assert!(!endpoints.iter().any(|e| e == "/health"));
    assert!(text.contains("groot"));
    assert!(text.contains("UnauthorisedUser"));
    assert!(text.contains("\"status\":\"OK\""));
    assert!(text.contains("Unauthenticated"));
}
