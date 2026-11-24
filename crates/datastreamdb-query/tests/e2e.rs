//! End-to-end: mutate RDF, run DQL, check JSON.

use std::sync::Arc;

use datastreamdb_posting::PostingEngine;
use datastreamdb_proto::{Directive, SchemaUpdate, ValType};
use datastreamdb_query::QueryEngine;
use datastreamdb_schema::SchemaState;
use datastreamdb_store::MemoryBackend;

fn engine() -> Arc<QueryEngine> {
    let schema = Arc::new(SchemaState::new());
    schema.set(
        "0-name",
        SchemaUpdate {
            predicate: "0-name".to_owned(),
            value_type: ValType::String as i32,
            directive: Directive::Index as i32,
            tokenizer: vec!["exact".to_owned(), "term".to_owned()],
            ..SchemaUpdate::default()
        },
    );
    schema.set(
        "0-age",
        SchemaUpdate {
            predicate: "0-age".to_owned(),
            value_type: ValType::Int as i32,
            directive: Directive::Index as i32,
            tokenizer: vec!["int".to_owned()],
            ..SchemaUpdate::default()
        },
    );
    schema.set(
        "0-friend",
        SchemaUpdate {
            predicate: "0-friend".to_owned(),
            value_type: ValType::Uid as i32,
            directive: Directive::Reverse as i32,
            count: true,
            list: true,
            ..SchemaUpdate::default()
        },
    );
    schema.set(
        "0-datastreamdb.type",
        SchemaUpdate {
            predicate: "0-datastreamdb.type".to_owned(),
            value_type: ValType::String as i32,
            ..SchemaUpdate::default()
        },
    );
    let posting = PostingEngine::open(Arc::new(MemoryBackend::new()), schema, 64);
    QueryEngine::open(posting).expect("engine")
}

fn seed(engine: &Arc<QueryEngine>) {
    engine
        .mutate(
            0,
            r#"{
                set {
                    _:a <name> "Alice" .
                    _:a <age> "30"^^<xs:int> .
                    _:a <datastreamdb.type> "Person" .
                    _:b <name> "Bob" .
                    _:b <age> "25"^^<xs:int> .
                    _:b <datastreamdb.type> "Person" .
                    _:a <friend> _:b .
                }
            }"#,
        )
        .expect("mutate");
}

fn read_ts(engine: &Arc<QueryEngine>) -> u64 {
    engine.posting().read_ts()
}

#[test]
fn query_has_and_traverse() {
    let engine = engine();
    seed(&engine);
    let out = engine
        .query_json(
            0,
            r"{
                people(func: has(name)) {
                    name
                    age
                    friend { name }
                }
            }",
            read_ts(&engine),
        )
        .expect("query");
    let text = String::from_utf8(out).expect("utf8");
    assert!(text.contains("Alice"), "{text}");
    assert!(text.contains("Bob"), "{text}");
    assert!(text.contains("people"), "{text}");
}

#[test]
fn query_eq_root_and_filter() {
    let engine = engine();
    seed(&engine);
    let out = engine
        .query_json(
            0,
            r#"{
                q(func: eq(name, "Alice")) @filter(gt(age, 20)) {
                    name
                    age
                }
            }"#,
            read_ts(&engine),
        )
        .expect("query");
    let text = String::from_utf8(out).expect("utf8");
    assert!(text.contains("Alice"), "{text}");
    assert!(!text.contains("Bob"), "{text}");
}

#[test]
fn query_uid_and_count() {
    let engine = engine();
    seed(&engine);
    let assigned = engine
        .mutate(0, r#"{ set { _:c <name> "Carol" . } }"#)
        .expect("mutate");
    let carol = assigned["_:c"];
    let out = engine
        .query_json(
            0,
            &format!("{{ q(func: uid({carol:#x})) {{ name friend {{ count(uid) }} }} }}"),
            read_ts(&engine),
        )
        .expect("query");
    let text = String::from_utf8(out).expect("utf8");
    assert!(text.contains("Carol"), "{text}");
}

#[test]
fn query_aggregation() {
    let engine = engine();
    seed(&engine);
    let out = engine
        .query_json(
            0,
            r"{
                var(func: has(age)) {
                    a as age
                }
                q() {
                    avg(val(a))
                    sum(val(a))
                    max(val(a))
                }
            }",
            read_ts(&engine),
        )
        .expect("query");
    let text = String::from_utf8(out).expect("utf8");
    assert!(text.contains("avg"), "{text}");
    assert!(text.contains("27.5"), "{text}");
    assert!(text.contains("55"), "{text}");
}

#[test]
fn query_order_paginate() {
    let engine = engine();
    seed(&engine);
    let out = engine
        .query_json(
            0,
            r"{
                q(func: has(age), orderdesc: age, first: 1) {
                    name
                    age
                }
            }",
            read_ts(&engine),
        )
        .expect("query");
    let text = String::from_utf8(out).expect("utf8");
    assert!(text.contains("Alice"), "{text}");
    assert!(text.contains("30"), "{text}");
    assert!(!text.contains("Bob"), "{text}");
}

#[test]
fn query_recurse() {
    let engine = engine();
    seed(&engine);
    let out = engine
        .query_json(
            0,
            r#"{
                q(func: eq(name, "Alice")) @recurse(depth: 3) {
                    name
                    friend
                }
            }"#,
            read_ts(&engine),
        )
        .expect("query");
    let text = String::from_utf8(out).expect("utf8");
    assert!(text.contains("Alice"), "{text}");
    assert!(text.contains("Bob"), "{text}");
}

#[test]
fn query_groupby() {
    let engine = engine();
    seed(&engine);
    let out = engine
        .query_json(
            0,
            r"{
                q(func: has(age)) @groupby(age) {
                    count(uid)
                }
            }",
            read_ts(&engine),
        )
        .expect("query");
    let text = String::from_utf8(out).expect("utf8");
    assert!(text.contains("@groupby"), "{text}");
}

#[test]
fn query_normalize() {
    let engine = engine();
    seed(&engine);
    let out = engine
        .query_json(
            0,
            r"{
                q(func: has(name)) @normalize {
                    n: name
                    a: age
                }
            }",
            read_ts(&engine),
        )
        .expect("query");
    let text = String::from_utf8(out).expect("utf8");
    assert!(text.contains("\"n\":\"Alice\""), "{text}");
    assert!(text.contains("\"a\":30"), "{text}");
}

#[test]
fn query_delete_mutation() {
    let engine = engine();
    seed(&engine);
    engine
        .mutate(0, r#"{ delete { <0x3> <name> "Bob" . } }"#)
        .expect("delete");
    let out = engine
        .query_json(0, r"{ q(func: has(name)) { name } }", read_ts(&engine))
        .expect("query");
    let text = String::from_utf8(out).expect("utf8");
    assert!(text.contains("Alice"), "{text}");
    assert!(!text.contains("Bob"), "{text}");
}

#[test]
fn query_rdf_output() {
    let engine = engine();
    seed(&engine);
    let (result, _) = engine
        .query(
            0,
            r"{ q(func: has(name)) { name friend { name } } }",
            read_ts(&engine),
        )
        .expect("query");
    let rdf = datastreamdb_query::rdf::to_rdf(&result.subgraphs).expect("rdf");
    let text = String::from_utf8(rdf).expect("utf8");
    assert!(text.contains("<0x2> <name>"), "{text}");
    assert!(text.contains("Alice"), "{text}");
}

#[test]
fn query_shortest_path() {
    let engine = engine();
    seed(&engine);
    let out = engine
        .query_json(
            0,
            r"{
                shortest(from: 0x2, to: 0x3) {
                    friend
                }
            }",
            read_ts(&engine),
        )
        .expect("query");
    let text = String::from_utf8(out).expect("utf8");
    assert!(text.contains("_path_"), "{text}");
    assert!(text.contains("0x3"), "{text}");
}

#[test]
fn query_facets() {
    let engine = engine();
    seed(&engine);
    engine
        .mutate(0, r"{ set { <0x2> <friend> <0x3> (close=true) . } }")
        .expect("mutate");
    let out = engine
        .query_json(
            0,
            r"{ q(func: has(name)) { friend @facets(close) { name } } }",
            read_ts(&engine),
        )
        .expect("query");
    let text = String::from_utf8(out).expect("utf8");
    assert!(text.contains("close"), "{text}");
}

#[test]
fn query_math() {
    let engine = engine();
    seed(&engine);
    let out = engine
        .query_json(
            0,
            r"{
                var(func: has(age)) {
                    a as age
                    b as math(a + 1)
                }
                q() {
                    sum(val(a))
                    sum(val(b))
                }
            }",
            read_ts(&engine),
        )
        .expect("query");
    let text = String::from_utf8(out).expect("utf8");
    assert!(text.contains("55"), "{text}");
    assert!(text.contains("57"), "{text}");
}
