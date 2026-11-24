//! Subscription tests: initial snapshot, polled updates, epoch end.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use datastreamdb_graphql::op::BoundSchema;
use datastreamdb_graphql::resolve::GraphQlEngine;
use datastreamdb_graphql::schemagen::new_handler;
use datastreamdb_graphql::subscription::SubscriptionHub;
use datastreamdb_posting::PostingEngine;
use datastreamdb_query::QueryEngine;
use datastreamdb_schema::SchemaState;
use datastreamdb_store::MemoryBackend;
use futures::StreamExt;

const SCHEMA: &str = r"
type Post @withSubscription {
    id: ID!
    title: String @search(by: [term])
}
";

fn hub() -> (SubscriptionHub, Arc<GraphQlEngine>) {
    let handler = new_handler(SCHEMA, false).expect("valid schema");
    let dg = format!(
        "{}\ndatastreamdb.type: string @index(exact) .\n",
        handler.dg_schema()
    );
    let schema = Arc::new(SchemaState::new());
    let parsed = datastreamdb_schema::parse(&dg).expect("parse");
    for pred in &parsed.preds {
        schema.set(&pred.predicate.clone(), pred.clone());
    }
    let posting = PostingEngine::open(Arc::new(MemoryBackend::new()), schema, 64);
    let engine = QueryEngine::open(posting).expect("engine");
    let bound = BoundSchema::bind(&handler).expect("bound");
    let engine = Arc::new(GraphQlEngine::new(bound, engine, 0).expect("gql"));
    let hub_engine = GraphQlEngine::new(
        BoundSchema::bind(&new_handler(SCHEMA, false).expect("valid")).expect("bound"),
        engine.shared().engine.clone(),
        0,
    )
    .expect("gql");
    (
        SubscriptionHub::new(hub_engine, Duration::from_millis(20)),
        engine,
    )
}

fn response_data(response: &async_graphql::Response) -> serde_json::Value {
    assert!(
        response.errors.is_empty(),
        "errors: {:?}",
        response
            .errors
            .iter()
            .map(|err| &err.message)
            .collect::<Vec<_>>()
    );
    let json = serde_json::to_value(&response.data).expect("json");
    json.get("data").cloned().unwrap_or(json)
}

#[tokio::test]
async fn streams_initial_and_updates() {
    let (hub, engine) = hub();
    let headers = HashMap::new();
    engine
        .execute(
            r#"mutation { addPost(input: [{title: "One"}]) { numUids } }"#,
            serde_json::json!({}),
            &headers,
        )
        .await;
    let mut stream = hub
        .subscribe(
            "subscription { queryPost { title } }",
            serde_json::json!({}),
            &headers,
        )
        .await
        .expect("subscribe");
    let first = stream.next().await.expect("initial");
    let data = response_data(&first);
    assert_eq!(
        data["queryPost"][0]["title"],
        serde_json::json!("One"),
        "{data}"
    );

    engine
        .execute(
            r#"mutation { addPost(input: [{title: "Two"}]) { numUids } }"#,
            serde_json::json!({}),
            &headers,
        )
        .await;
    let second = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("update in time")
        .expect("update");
    let data = response_data(&second);
    assert_eq!(
        data["queryPost"].as_array().map(Vec::len),
        Some(2),
        "{data}"
    );
}

#[tokio::test]
async fn ends_on_epoch_bump() {
    let (hub, _engine) = hub();
    let headers = HashMap::new();
    let mut stream = hub
        .subscribe(
            "subscription { queryPost { title } }",
            serde_json::json!({}),
            &headers,
        )
        .await
        .expect("subscribe");
    stream.next().await.expect("initial");
    hub.bump_epoch();
    let end = tokio::time::timeout(Duration::from_secs(5), stream.next()).await;
    assert!(end.expect("stream ends").is_none());
}

#[tokio::test]
async fn rejects_non_subscriptions() {
    let (hub, _engine) = hub();
    let headers = HashMap::new();
    let result = hub
        .subscribe("{ queryPost { title } }", serde_json::json!({}), &headers)
        .await;
    match result {
        Ok(_) => panic!("must fail"),
        Err(err) => assert!(err.message.contains("subscription"), "{err}"),
    }
}
