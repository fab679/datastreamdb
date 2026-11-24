//! ACL interop: login, tokens, and predicate enforcement over the wire.
//!
//! Boots a zero plus an ACL-enabled alpha and drives the full story:
//! unauthenticated requests fail closed, groot logs in, refresh rotates
//! tokens, and a read-only user sees exactly their allowed predicates.

#[path = "common/mod.rs"]
mod common;

use std::time::Duration;

use common::{boot_alpha_acl, boot_zero, channel};
use datastreamdb_proto::api::{
    Jwt, LoginRequest, Mutation, NQuad, Operation, Request, Value,
    data_stream_db_client::DataStreamDbClient, value::Val,
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

type AuthedClient = DataStreamDbClient<
    tonic::service::interceptor::InterceptedService<tonic::transport::Channel, Token>,
>;
type AnonClient = DataStreamDbClient<tonic::transport::Channel>;

/// Test HMAC secret (64 bytes; HS256 needs 32+).
fn secret() -> Vec<u8> {
    vec![7u8; 64]
}

/// Client presenting `token` as `accessjwt` metadata.
async fn authed(addr: &str, token: String) -> AuthedClient {
    let token: tonic::metadata::MetadataValue<_> = token.parse().expect("token metadata");
    DataStreamDbClient::with_interceptor(channel(addr).await, Token(token))
}

/// Decodes the login `Jwt` from a response body.
fn decode_jwt(body: &[u8]) -> Jwt {
    Jwt::decode(body).expect("jwt")
}

/// String-valued `NQuad` helper.
fn str_quad(subject: &str, predicate: &str, value: &str) -> NQuad {
    NQuad {
        subject: subject.to_owned(),
        predicate: predicate.to_owned(),
        object_value: Some(Value {
            val: Some(Val::StrVal(value.to_owned())),
        }),
        ..NQuad::default()
    }
}

/// Unauthenticated requests fail closed.
async fn check_anon_closed(client: &mut AnonClient) {
    let err = client
        .query(Request {
            query: "{ q(func: has(name)) { uid } }".to_owned(),
            ..Request::default()
        })
        .await
        .expect_err("anon query");
    assert_eq!(err.code(), tonic::Code::Unauthenticated);
    let err = client
        .alter(Operation {
            schema: "name: string .".to_owned(),
            ..Operation::default()
        })
        .await
        .expect_err("anon alter");
    assert_eq!(err.code(), tonic::Code::Unauthenticated);
}

/// Bad credentials fail; empty fields are invalid.
async fn check_bad_credentials(client: &mut AnonClient) {
    let err = client
        .login(LoginRequest {
            userid: "groot".to_owned(),
            password: "wrong".to_owned(),
            ..LoginRequest::default()
        })
        .await
        .expect_err("bad password");
    assert_eq!(err.code(), tonic::Code::Unauthenticated);
    let err = client
        .login(LoginRequest {
            password: "password".to_owned(),
            ..LoginRequest::default()
        })
        .await
        .expect_err("empty userid");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}

/// Logs in as groot (bootstrap created guardians + groot).
async fn login_groot(client: &mut AnonClient) -> Jwt {
    let jwt = decode_jwt(
        &client
            .login(LoginRequest {
                userid: "groot".to_owned(),
                password: "password".to_owned(),
                ..LoginRequest::default()
            })
            .await
            .expect("groot login")
            .into_inner()
            .json,
    );
    assert!(!jwt.access_jwt.is_empty());
    assert!(!jwt.refresh_jwt.is_empty());
    jwt
}

/// Refresh rotates the pair; bogus refresh fails.
async fn check_refresh(client: &mut AnonClient, jwt: &Jwt) {
    let rotated = decode_jwt(
        &client
            .login(LoginRequest {
                refresh_token: jwt.refresh_jwt.clone(),
                ..LoginRequest::default()
            })
            .await
            .expect("refresh")
            .into_inner()
            .json,
    );
    assert!(!rotated.access_jwt.is_empty());
    let err = client
        .login(LoginRequest {
            refresh_token: "bogus".to_owned(),
            ..LoginRequest::default()
        })
        .await
        .expect_err("bogus refresh");
    assert_eq!(err.code(), tonic::Code::Unauthenticated);
}

/// Guardians alter anything and mutate anything.
async fn guardian_setup(client: &mut AuthedClient) {
    for schema in ["name: string @index(exact) .", "friend: [uid] ."] {
        client
            .alter(Operation {
                schema: schema.to_owned(),
                ..Operation::default()
            })
            .await
            .expect("guardian alter");
    }
    let response = client
        .query(Request {
            mutations: vec![Mutation {
                set_nquads: b"_:a <name> \"Ada\" .\n_:a <friend> _:b .\n_:b <name> \"Bob\" .\n"
                    .to_vec(),
                commit_now: true,
                ..Mutation::default()
            }],
            commit_now: true,
            ..Request::default()
        })
        .await
        .expect("guardian mutate")
        .into_inner();
    assert!(!response.txn.expect("txn").aborted);
}

/// Creates alice + dev + name-read rule, then links them.
async fn create_alice(client: &mut AuthedClient) {
    let response = client
        .query(Request {
            mutations: vec![
                Mutation {
                    set: datastreamdb_acl::nquads::create_user_nquads("alice", "alice123"),
                    ..Mutation::default()
                },
                Mutation {
                    set: datastreamdb_acl::nquads::create_group_nquads("dev"),
                    ..Mutation::default()
                },
                Mutation {
                    set: vec![
                        str_quad("_:rule", "datastreamdb.rule.predicate", "name"),
                        NQuad {
                            subject: "_:rule".to_owned(),
                            predicate: "datastreamdb.rule.permission".to_owned(),
                            object_value: Some(Value {
                                val: Some(Val::IntVal(4)),
                            }),
                            ..NQuad::default()
                        },
                    ],
                    ..Mutation::default()
                },
            ],
            commit_now: true,
            ..Request::default()
        })
        .await
        .expect("create fixtures")
        .into_inner();
    assert!(!response.txn.expect("txn").aborted);
    let uids = &response.uids;
    let link = |subject: &str, predicate: &str, object: &str| Mutation {
        set_nquads: format!("<{subject}> <{predicate}> <{object}> .").into_bytes(),
        ..Mutation::default()
    };
    let response = client
        .query(Request {
            mutations: vec![
                link(
                    &uids["_:newuser"],
                    "datastreamdb.user.group",
                    &uids["_:newgroup"],
                ),
                link(
                    &uids["_:newgroup"],
                    "datastreamdb.acl.rule",
                    &uids["_:rule"],
                ),
            ],
            commit_now: true,
            ..Request::default()
        })
        .await
        .expect("link fixtures")
        .into_inner();
    assert!(!response.txn.expect("txn").aborted);
}

/// Logs alice in once the rule refresh lands (async finalize hook).
async fn wait_alice(client: &mut AnonClient, addr: &str) -> Jwt {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let jwt = decode_jwt(
                &client
                    .login(LoginRequest {
                        userid: "alice".to_owned(),
                        password: "alice123".to_owned(),
                        ..LoginRequest::default()
                    })
                    .await
                    .expect("alice login")
                    .into_inner()
                    .json,
            );
            let mut alice = authed(addr, jwt.access_jwt.clone()).await;
            let body = alice
                .query(Request {
                    query: "{ q(func: has(name)) { uid name } }".to_owned(),
                    ..Request::default()
                })
                .await
                .expect("alice query")
                .into_inner();
            if String::from_utf8_lossy(&body.json).contains("Ada") {
                return jwt;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("alice sees name")
}

/// Read-only alice: name reads, friend degrades, writes denied.
async fn check_alice(addr: &str, jwt: &Jwt) {
    let mut alice = authed(addr, jwt.access_jwt.clone()).await;
    let body = alice
        .query(Request {
            query: "{ q(func: has(name)) { uid name friend { name } } }".to_owned(),
            ..Request::default()
        })
        .await
        .expect("alice degrade")
        .into_inner();
    let text = String::from_utf8_lossy(&body.json).into_owned();
    assert!(text.contains("Ada"), "name readable: {text}");
    assert!(
        !text.contains("\"friend\""),
        "friend subgraph pruned: {text}"
    );
    let err = alice
        .query(Request {
            mutations: vec![Mutation {
                set_nquads: b"_:x <name> \"Mallory\" .".to_vec(),
                commit_now: true,
                ..Mutation::default()
            }],
            commit_now: true,
            ..Request::default()
        })
        .await
        .expect_err("alice mutate");
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
    let err = alice
        .alter(Operation {
            schema: "age: int .".to_owned(),
            ..Operation::default()
        })
        .await
        .expect_err("alice alter");
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
}

/// HTTP login + authed query; anonymous HTTP fails closed.
async fn check_http(ahttp: &str) {
    let http = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("client");
    let tokens: serde_json::Value = http
        .post(format!("http://{ahttp}/login"))
        .body(r#"{"userid":"groot","password":"password"}"#)
        .send()
        .await
        .expect("http login")
        .error_for_status()
        .expect("login status")
        .json()
        .await
        .expect("login json");
    let access = tokens["data"]["accessJWT"].as_str().expect("access");
    assert!(!access.is_empty());
    let body: serde_json::Value = http
        .post(format!("http://{ahttp}/query"))
        .header("X-DataStreamDB-AccessJWT", access)
        .body("{ q(func: has(name)) { uid name } }")
        .send()
        .await
        .expect("http query")
        .json()
        .await
        .expect("query json");
    assert!(body.to_string().contains("Ada"));
    let denied = http
        .post(format!("http://{ahttp}/query"))
        .body("{ q(func: has(name)) { uid name } }")
        .send()
        .await
        .expect("anon http");
    assert_eq!(denied.status(), reqwest::StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acl_login_and_enforcement() {
    let (_zdir, zgrpc, _) = boot_zero(1).await;
    let (_adir, agrpc, ahttp) = boot_alpha_acl(&zgrpc, 1, true, secret()).await;
    let mut anon = DataStreamDbClient::new(channel(&agrpc).await);

    check_anon_closed(&mut anon).await;
    check_bad_credentials(&mut anon).await;
    let jwt = login_groot(&mut anon).await;
    check_refresh(&mut anon, &jwt).await;

    let mut groot = authed(&agrpc, jwt.access_jwt.clone()).await;
    guardian_setup(&mut groot).await;
    create_alice(&mut groot).await;

    let alice_jwt = wait_alice(&mut anon, &agrpc).await;
    check_alice(&agrpc, &alice_jwt).await;
    check_http(&ahttp).await;
}
