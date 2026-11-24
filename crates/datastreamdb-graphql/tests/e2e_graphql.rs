//! End-to-end GraphQL: schema, add, query, update, delete over the
//! embedded engine.

use std::collections::HashMap;
use std::sync::Arc;

use datastreamdb_graphql::op::BoundSchema;
use datastreamdb_graphql::resolve::GraphQlEngine;
use datastreamdb_graphql::schemagen::new_handler;
use datastreamdb_posting::PostingEngine;
use datastreamdb_query::QueryEngine;
use datastreamdb_schema::SchemaState;
use datastreamdb_store::MemoryBackend;

const SCHEMA: &str = r"
type Post {
    id: ID!
    title: String @search(by: [term])
    numLikes: Int @search
    author: Author @hasInverse(field: posts)
}
type Author {
    id: ID!
    name: String @id @search(by: [hash])
    posts: [Post]
}
";

fn engine(dg_schema: &str) -> Arc<QueryEngine> {
    let schema = Arc::new(SchemaState::new());
    // `datastreamdb.type` is implicit in DataStreamDB; the embedded schema needs it
    // declared for `type()` roots and filters.
    let dg_schema = format!("{dg_schema}\ndatastreamdb.type: string @index(exact) .\n");
    let parsed = datastreamdb_schema::parse(&dg_schema).expect("parse dg schema");
    for pred in &parsed.preds {
        schema.set(&pred.predicate.clone(), pred.clone());
    }
    for typ in &parsed.types {
        schema.set_type(&typ.type_name.clone(), typ.clone());
    }
    let posting = PostingEngine::open(Arc::new(MemoryBackend::new()), schema, 64);
    QueryEngine::open(posting).expect("engine")
}

fn graphql() -> GraphQlEngine {
    let handler = new_handler(SCHEMA, false).expect("valid schema");
    let engine = engine(handler.dg_schema());
    let bound = BoundSchema::bind(&handler).expect("bound");
    GraphQlEngine::new(bound, engine, 0).expect("graphql engine")
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
async fn add_and_query_posts() {
    let engine = graphql();
    let headers = HashMap::new();
    let add = engine
        .execute(
            r#"mutation {
                addPost(input: [{title: "Hello", numLikes: 42, author: {name: "Ann"}}]) {
                    post { id title numLikes author { name } }
                    numUids
                }
            }"#,
            serde_json::json!({}),
            &headers,
        )
        .await;
    let data = response_data(&add);
    assert_eq!(data["addPost"]["numUids"], serde_json::json!(1), "{data}");
    assert_eq!(
        data["addPost"]["post"][0]["title"],
        serde_json::json!("Hello"),
        "{data}"
    );
    assert_eq!(
        data["addPost"]["post"][0]["author"]["name"],
        serde_json::json!("Ann"),
        "{data}"
    );

    let query = engine
        .execute(
            r#"{ queryPost(filter: {title: {anyofterms: "Hello"}}) { title numLikes author { name } } }"#,
            serde_json::json!({}),
            &headers,
        )
        .await;
    let data = response_data(&query);
    assert_eq!(
        data["queryPost"][0]["title"],
        serde_json::json!("Hello"),
        "{data}"
    );
    assert_eq!(
        data["queryPost"][0]["numLikes"],
        serde_json::json!(42),
        "{data}"
    );
}

#[tokio::test]
async fn update_and_delete_posts() {
    let engine = graphql();
    let headers = HashMap::new();
    engine
        .execute(
            r#"mutation { addPost(input: [{title: "Old"}]) { numUids } }"#,
            serde_json::json!({}),
            &headers,
        )
        .await;

    let update = engine
        .execute(
            r#"mutation {
                updatePost(input: {filter: {title: {anyofterms: "Old"}}, set: {title: "New"}}) {
                    post { title }
                    numUids
                }
            }"#,
            serde_json::json!({}),
            &headers,
        )
        .await;
    let data = response_data(&update);
    assert_eq!(
        data["updatePost"]["post"][0]["title"],
        serde_json::json!("New"),
        "{data}"
    );

    let delete = engine
        .execute(
            r#"mutation { deletePost(filter: {title: {anyofterms: "New"}}) { msg numUids } }"#,
            serde_json::json!({}),
            &headers,
        )
        .await;
    let data = response_data(&delete);
    assert_eq!(
        data["deletePost"]["numUids"],
        serde_json::json!(1),
        "{data}"
    );

    let query = engine
        .execute(r"{ queryPost { title } }", serde_json::json!({}), &headers)
        .await;
    let data = response_data(&query);
    assert_eq!(data["queryPost"], serde_json::json!([]), "{data}");
}

#[tokio::test]
async fn serves_introspection() {
    let engine = graphql();
    let headers = HashMap::new();
    let response = engine
        .execute(
            r"{ __schema { queryType { name } } }",
            serde_json::json!({}),
            &headers,
        )
        .await;
    let data = response_data(&response);
    assert_eq!(
        data["__schema"]["queryType"]["name"],
        serde_json::json!("Query"),
        "{data}"
    );
}

#[tokio::test]
async fn rejects_invalid_queries() {
    let engine = graphql();
    let headers = HashMap::new();
    let response = engine
        .execute(r"{ noSuchField }", serde_json::json!({}), &headers)
        .await;
    assert!(!response.errors.is_empty());
}
