//! Task processor smoke: has/eq/uid/filter tasks over committed data.

use std::sync::Arc;

use datastreamdb_posting::{LocalCache, PostingEngine};
use datastreamdb_proto::{
    DirectedEdge, EdgeOp, List, SchemaUpdate, SrcFunction, TaskQuery, ValType,
};
use datastreamdb_schema::SchemaState;
use datastreamdb_store::MemoryBackend;
use datastreamdb_task::process_task;

fn test_schema() -> Arc<SchemaState> {
    let schema = Arc::new(SchemaState::new());
    schema.set(
        "0-name",
        SchemaUpdate {
            predicate: "0-name".to_owned(),
            value_type: ValType::String as i32,
            directive: datastreamdb_proto::Directive::Index as i32,
            tokenizer: vec!["exact".to_owned(), "term".to_owned()],
            ..SchemaUpdate::default()
        },
    );
    schema.set(
        "0-age",
        SchemaUpdate {
            predicate: "0-age".to_owned(),
            value_type: ValType::Int as i32,
            directive: datastreamdb_proto::Directive::Index as i32,
            tokenizer: vec!["int".to_owned()],
            ..SchemaUpdate::default()
        },
    );
    schema.set(
        "0-friend",
        SchemaUpdate {
            predicate: "0-friend".to_owned(),
            value_type: ValType::Uid as i32,
            directive: datastreamdb_proto::Directive::Reverse as i32,
            count: true,
            list: true,
            ..SchemaUpdate::default()
        },
    );
    schema
}

fn set_int_edge(entity: u64, attr: &str, text: &str) -> DirectedEdge {
    use datastreamdb_types::{TypeId, Val, Value, convert, marshal};
    let src = Val {
        tid: TypeId::String,
        value: Value::Binary(text.as_bytes().to_vec()),
    };
    let binary = marshal(
        &convert(&src, TypeId::Int).expect("convert"),
        TypeId::Binary,
    )
    .expect("marshal");
    let Value::Binary(bytes) = binary.value else {
        panic!("binary");
    };
    DirectedEdge {
        entity,
        attr: attr.to_owned(),
        value: bytes,
        value_type: ValType::Int as i32,
        op: EdgeOp::Set as i32,
        ..DirectedEdge::default()
    }
}

fn seed(engine: &Arc<PostingEngine>) -> u64 {
    let txn = engine.begin();
    for (uid, name, age) in [(2u64, "Alice", "30"), (3, "Bob", "25")] {
        let key = datastreamdb_x::data_key("0-name", uid);
        let mut edge = DirectedEdge {
            entity: uid,
            attr: "0-name".to_owned(),
            value: name.as_bytes().to_vec(),
            value_type: ValType::String as i32,
            op: EdgeOp::Set as i32,
            ..DirectedEdge::default()
        };
        txn.get(&key)
            .expect("list")
            .add_mutation_with_index(&mut edge, &txn)
            .expect("mutate");
        let key = datastreamdb_x::data_key("0-age", uid);
        let mut edge = set_int_edge(uid, "0-age", age);
        txn.get(&key)
            .expect("list")
            .add_mutation_with_index(&mut edge, &txn)
            .expect("mutate");
    }
    let key = datastreamdb_x::data_key("0-friend", 2);
    let mut edge = DirectedEdge {
        entity: 2,
        attr: "0-friend".to_owned(),
        value_id: 3,
        value_type: ValType::Uid as i32,
        op: EdgeOp::Set as i32,
        ..DirectedEdge::default()
    };
    txn.get(&key)
        .expect("list")
        .add_mutation_with_index(&mut edge, &txn)
        .expect("mutate");
    engine.commit(&txn).expect("commit")
}

fn run(engine: &Arc<PostingEngine>, query: TaskQuery) -> datastreamdb_proto::TaskResult {
    let cache = Arc::new(LocalCache::no_cache(engine.clone(), query.read_ts));
    process_task(engine, &cache, query).expect("task")
}

#[test]
fn has_root_scans() {
    let engine = PostingEngine::open(Arc::new(MemoryBackend::new()), test_schema(), 64);
    let ts = seed(&engine);
    let out = run(
        &engine,
        TaskQuery {
            attr: "0-name".to_owned(),
            src_func: Some(SrcFunction {
                name: "has".to_owned(),
                args: Vec::new(),
                is_count: false,
            }),
            read_ts: ts,
            first: i32::MAX,
            ..TaskQuery::default()
        },
    );
    assert_eq!(out.uid_matrix.len(), 1);
    assert_eq!(out.uid_matrix[0].uids, [2, 3]);
}

#[test]
fn eq_root_uses_index() {
    let engine = PostingEngine::open(Arc::new(MemoryBackend::new()), test_schema(), 64);
    let ts = seed(&engine);
    let out = run(
        &engine,
        TaskQuery {
            attr: "0-name".to_owned(),
            src_func: Some(SrcFunction {
                name: "eq".to_owned(),
                args: vec!["Alice".to_owned()],
                is_count: false,
            }),
            read_ts: ts,
            ..TaskQuery::default()
        },
    );
    assert_eq!(out.uid_matrix.len(), 1);
    assert_eq!(out.uid_matrix[0].uids, [2]);

    // Integer comparison at root.
    let out = run(
        &engine,
        TaskQuery {
            attr: "0-age".to_owned(),
            src_func: Some(SrcFunction {
                name: "gt".to_owned(),
                args: vec!["26".to_owned()],
                is_count: false,
            }),
            read_ts: ts,
            ..TaskQuery::default()
        },
    );
    let mut got: Vec<u64> = out
        .uid_matrix
        .iter()
        .flat_map(|row| row.uids.clone())
        .collect();
    got.sort_unstable();
    assert_eq!(got, [2]);
}

#[test]
fn uid_fetch_and_filter() {
    let engine = PostingEngine::open(Arc::new(MemoryBackend::new()), test_schema(), 64);
    let ts = seed(&engine);

    // Plain UID fetch of a scalar: value matrix holds the names.
    let out = run(
        &engine,
        TaskQuery {
            attr: "0-name".to_owned(),
            uid_list: Some(List { uids: vec![2, 3] }),
            read_ts: ts,
            ..TaskQuery::default()
        },
    );
    assert_eq!(out.value_matrix.len(), 2);
    assert_eq!(out.value_matrix[0].values[0].val, b"Alice");
    assert_eq!(out.value_matrix[1].values[0].val, b"Bob");

    // UID fetch of a UID predicate: uid rows.
    let out = run(
        &engine,
        TaskQuery {
            attr: "0-friend".to_owned(),
            uid_list: Some(List { uids: vec![2, 3] }),
            read_ts: ts,
            ..TaskQuery::default()
        },
    );
    assert_eq!(out.uid_matrix[0].uids, [3]);
    assert!(out.uid_matrix[1].uids.is_empty());

    // eq as a filter over candidates.
    let out = run(
        &engine,
        TaskQuery {
            attr: "0-name".to_owned(),
            uid_list: Some(List { uids: vec![2, 3] }),
            src_func: Some(SrcFunction {
                name: "eq".to_owned(),
                args: vec!["Bob".to_owned()],
                is_count: false,
            }),
            read_ts: ts,
            ..TaskQuery::default()
        },
    );
    let mut got: Vec<u64> = out
        .uid_matrix
        .iter()
        .flat_map(|row| row.uids.clone())
        .collect();
    got.sort_unstable();
    assert_eq!(got, [3]);
}

#[test]
fn count_and_regex() {
    let engine = PostingEngine::open(Arc::new(MemoryBackend::new()), test_schema(), 64);
    let ts = seed(&engine);

    // do_count over candidates.
    let out = run(
        &engine,
        TaskQuery {
            attr: "0-friend".to_owned(),
            uid_list: Some(List { uids: vec![2, 3] }),
            read_ts: ts,
            do_count: true,
            ..TaskQuery::default()
        },
    );
    assert_eq!(out.counts, [1, 0]);

    // Regex filter over candidates (no trigram index needed embedded).
    let out = run(
        &engine,
        TaskQuery {
            attr: "0-name".to_owned(),
            uid_list: Some(List { uids: vec![2, 3] }),
            src_func: Some(SrcFunction {
                name: "regexp".to_owned(),
                args: vec!["^A".to_owned(), String::new()],
                is_count: false,
            }),
            read_ts: ts,
            ..TaskQuery::default()
        },
    );
    let mut got: Vec<u64> = out
        .uid_matrix
        .iter()
        .flat_map(|row| row.uids.clone())
        .collect();
    got.sort_unstable();
    assert_eq!(got, [2]);
}
