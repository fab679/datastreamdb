//! Embedded graph-mode tests: DQL relationships, patterns, alters, persistence.

#![cfg(feature = "query")]

#[cfg(feature = "graphql")]
use std::time::Duration;

#[cfg(feature = "graphql")]
use datastreamdb_core::SubscriptionEvent;
use datastreamdb_core::{Instance, InstanceConfig};

const SCHEMA: &str = r"
name: string @index(exact) .
friend: [uid] @reverse .
type Person {
    name
    friend
}
";

fn memory() -> std::sync::Arc<Instance> {
    Instance::open(InstanceConfig::memory()).expect("open")
}

fn seed(db: &Instance) {
    db.dql_mutate(
        r#"{
            set {
                _:a <name> "Ada" .
                _:b <name> "Bob" .
                _:a <friend> _:b .
                _:a <datastreamdb.type> "Person" .
                _:b <datastreamdb.type> "Person" .
            }
        }"#,
    )
    .expect("mutate");
}

#[test]
fn dql_relationship_traversal() {
    let db = memory();
    db.dql_alter(SCHEMA).expect("alter");
    seed(&db);

    let result = db
        .dql_query(r#"{ people(func: eq(name, "Ada")) { name friend { name } } }"#)
        .expect("query");
    let text = result.to_string();
    assert!(text.contains("Ada"), "{text}");
    assert!(text.contains("Bob"), "{text}");
}

#[test]
fn dql_nested_pattern_filter() {
    let db = memory();
    db.dql_alter(SCHEMA).expect("alter");
    seed(&db);

    let result = db
        .dql_query(
            r#"{
                q(func: has(name)) @filter(eq(name, "Ada")) {
                    name
                    friend @filter(eq(name, "Bob")) { name }
                }
            }"#,
        )
        .expect("query");
    let text = result.to_string();
    assert!(text.contains("Ada"), "{text}");
    assert!(text.contains("Bob"), "{text}");
}

#[test]
fn dql_alter_rebuilds_existing_data() {
    let db = memory();
    db.dql_alter("name: string .").expect("alter");
    db.dql_mutate(r#"{ set { _:a <name> "Ada" . } }"#)
        .expect("mutate");

    db.dql_alter("name: string @index(exact) .")
        .expect("alter index");

    let result = db
        .dql_query(r#"{ q(func: eq(name, "Ada")) { name } }"#)
        .expect("query");
    assert!(result.to_string().contains("Ada"));
}

#[test]
fn dql_schema_and_data_survive_reopen() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config =
        InstanceConfig::from_json(&format!(r#"{{"data_dir": "{}"}}"#, dir.path().display()))
            .expect("config");

    {
        let db = Instance::open(config.clone()).expect("open");
        db.dql_alter(SCHEMA).expect("alter");
        seed(&db);
        db.flush().expect("flush");
    }

    let db = Instance::open(config).expect("reopen");
    let result = db
        .dql_query(r"{ people(func: has(name)) { name friend { name } } }")
        .expect("query after reopen");
    let text = result.to_string();
    assert!(text.contains("Ada"), "{text}");
    assert!(text.contains("Bob"), "{text}");
}

#[test]
fn dql_envelope_ops() {
    let db = memory();
    let alter = db
        .execute(r#"{"op":"dql_alter","schema":"name: string @index(exact) ."}"#)
        .expect("alter op");
    assert!(alter.contains("\"ok\":true"), "{alter}");

    let mutate = db
        .execute(r#"{"op":"dql_mutate","query":"{ set { _:a <name> \"Ada\" . } }"}"#)
        .expect("mutate op");
    assert!(mutate.contains("_:a"), "{mutate}");

    let query = db
        .execute(r#"{"op":"dql_query","query":"{ q(func: eq(name, \"Ada\")) { name } }"}"#)
        .expect("query op");
    assert!(query.contains("Ada"), "{query}");
}

#[cfg(feature = "graphql")]
const GQL_SCHEMA: &str = r"
type Post @withSubscription {
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

#[cfg(feature = "graphql")]
fn graphql_db() -> std::sync::Arc<Instance> {
    let db = memory();
    db.set_graphql_schema(GQL_SCHEMA).expect("graphql schema");
    db
}

#[cfg(feature = "graphql")]
#[test]
fn graphql_mutation_and_relationship_query() {
    let db = graphql_db();
    let headers = std::collections::HashMap::new();
    let added = db
        .graphql(
            r#"mutation {
                addPost(input: [{title: "Hello", numLikes: 42, author: {name: "Ann"}}]) {
                    post { id title numLikes author { name } }
                    numUids
                }
            }"#,
            None,
            serde_json::json!({}),
            &headers,
        )
        .expect("add");
    assert_eq!(
        added["data"]["addPost"]["numUids"],
        serde_json::json!(1),
        "{added}"
    );

    let queried = db
        .graphql(
            r#"{ queryPost(filter: {title: {anyofterms: "Hello"}}) {
                title numLikes author { name }
            } }"#,
            None,
            serde_json::json!({}),
            &headers,
        )
        .expect("query");
    let text = queried.to_string();
    assert!(text.contains("Hello"), "{text}");
    assert!(text.contains("Ann"), "{text}");
}

#[cfg(feature = "graphql")]
#[test]
fn graphql_subscription_polls_and_closes_on_schema_change() {
    let db = graphql_db();
    let headers = std::collections::HashMap::new();
    db.graphql(
        r#"mutation {
            addPost(input: [{title: "Hello", numLikes: 42, author: {name: "Ann"}}]) {
                numUids
            }
        }"#,
        None,
        serde_json::json!({}),
        &headers,
    )
    .expect("seed post");

    let sub = db
        .graphql_subscribe(
            r#"subscription {
                queryPost(filter: {title: {anyofterms: "Hello"}}) {
                    title numLikes
                }
            }"#,
            serde_json::json!({}),
            &headers,
            Duration::from_millis(10),
        )
        .expect("subscribe");
    let initial = sub.next().expect("initial");
    assert!(
        matches!(initial, SubscriptionEvent::Changed(_)),
        "{initial:?}"
    );
    assert!(matches!(
        sub.next().expect("unchanged"),
        SubscriptionEvent::Unchanged
    ));

    db.graphql(
        r#"mutation {
            updatePost(input: {
                filter: {title: {anyofterms: "Hello"}},
                set: {numLikes: 43}
            }) { numUids }
        }"#,
        None,
        serde_json::json!({}),
        &headers,
    )
    .expect("update post");

    let changed = sub.next_timeout(Duration::from_secs(2)).expect("changed");
    assert!(
        matches!(changed, SubscriptionEvent::Changed(_)),
        "{changed:?}"
    );

    db.set_graphql_schema(GQL_SCHEMA).expect("replace schema");
    assert!(matches!(
        sub.next().expect("closed"),
        SubscriptionEvent::Closed
    ));
}

#[cfg(feature = "graphql")]
#[test]
fn graphql_schema_and_data_survive_reopen() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config =
        InstanceConfig::from_json(&format!(r#"{{"data_dir": "{}"}}"#, dir.path().display()))
            .expect("config");
    let headers = std::collections::HashMap::new();

    {
        let db = Instance::open(config.clone()).expect("open");
        db.set_graphql_schema(GQL_SCHEMA).expect("schema");
        db.graphql(
            r#"mutation {
                addPost(input: [{title: "Hello", author: {name: "Ann"}}]) {
                    numUids
                }
            }"#,
            None,
            serde_json::json!({}),
            &headers,
        )
        .expect("add");
        db.flush().expect("flush");
    }

    let db = Instance::open(config).expect("reopen");
    assert_eq!(db.graphql_schema().as_deref(), Some(GQL_SCHEMA));
    let queried = db
        .graphql(
            r#"{ queryPost(filter: {title: {anyofterms: "Hello"}}) {
                title author { name }
            } }"#,
            None,
            serde_json::json!({}),
            &headers,
        )
        .expect("query after reopen");
    let text = queried.to_string();
    assert!(text.contains("Hello"), "{text}");
    assert!(text.contains("Ann"), "{text}");
}
