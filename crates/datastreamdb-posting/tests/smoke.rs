//! End-to-end smoke: schema, mutate, commit, read back, roll up.

use std::sync::Arc;

use datastreamdb_posting::PostingEngine;
use datastreamdb_proto::{DirectedEdge, EdgeOp, SchemaUpdate, ValType};
use datastreamdb_schema::SchemaState;
use datastreamdb_store::MemoryBackend;

fn test_schema() -> Arc<SchemaState> {
    let schema = Arc::new(SchemaState::new());
    schema.set(
        "0-name",
        SchemaUpdate {
            predicate: "0-name".to_owned(),
            value_type: ValType::String as i32,
            directive: datastreamdb_proto::Directive::Index as i32,
            tokenizer: vec!["exact".to_owned()],
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

fn set_edge(entity: u64, attr: &str, value: &[u8], value_type: ValType) -> DirectedEdge {
    DirectedEdge {
        entity,
        attr: attr.to_owned(),
        value: value.to_vec(),
        value_type: value_type as i32,
        op: EdgeOp::Set as i32,
        ..DirectedEdge::default()
    }
}

#[test]
fn write_commit_read_back() {
    let engine = PostingEngine::open(Arc::new(MemoryBackend::new()), test_schema(), 64);

    // Txn 1: two names and a friendship.
    let txn = engine.begin();
    let key = datastreamdb_x::data_key("0-name", 2);
    let list = txn.get(&key).expect("list");
    list.add_mutation_with_index(&mut set_edge(2, "0-name", b"Alice", ValType::String), &txn)
        .expect("mutate");
    let key = datastreamdb_x::data_key("0-name", 3);
    let list = txn.get(&key).expect("list");
    list.add_mutation_with_index(&mut set_edge(3, "0-name", b"Bob", ValType::String), &txn)
        .expect("mutate");
    let key = datastreamdb_x::data_key("0-friend", 2);
    let list = txn.get(&key).expect("list");
    let mut edge = DirectedEdge {
        entity: 2,
        attr: "0-friend".to_owned(),
        value_id: 3,
        value_type: ValType::Uid as i32,
        op: EdgeOp::Set as i32,
        ..DirectedEdge::default()
    };
    list.add_mutation_with_index(&mut edge, &txn)
        .expect("mutate");
    let commit_ts = engine.commit(&txn).expect("commit");
    assert!(commit_ts > txn.start_ts());

    // Read-your-writes at the commit timestamp.
    let key = datastreamdb_x::data_key("0-name", 2);
    let list = engine.get_list(&key, commit_ts).expect("read");
    let val = list.value(commit_ts).expect("value");
    assert_eq!(
        val.value,
        datastreamdb_types::Value::Binary(b"Alice".to_vec())
    );

    // Exact index resolves Alice -> 2.
    let token_key = datastreamdb_x::index_key("0-name", &[0x02, b'A', b'l', b'i', b'c', b'e']);
    let index = engine.get_list(&token_key, commit_ts).expect("index");
    let uids = index
        .uids(&datastreamdb_posting::ListOptions {
            read_ts: commit_ts,
            ..Default::default()
        })
        .expect("uids");
    assert_eq!(uids, [2]);

    // Reverse edge 3 -> 2 exists.
    let rev_key = datastreamdb_x::reverse_key("0-friend", 3);
    let rev = engine.get_list(&rev_key, commit_ts).expect("reverse");
    let ruids = rev
        .uids(&datastreamdb_posting::ListOptions {
            read_ts: commit_ts,
            ..Default::default()
        })
        .expect("uids");
    assert_eq!(ruids, [2]);

    // Count index: bucket 1 holds entity 2.
    let count_key = datastreamdb_x::count_key("0-friend", 1, false);
    let counts = engine.get_list(&count_key, commit_ts).expect("counts");
    let cuids = counts
        .uids(&datastreamdb_posting::ListOptions {
            read_ts: commit_ts,
            ..Default::default()
        })
        .expect("uids");
    assert_eq!(cuids, [2]);
}

#[test]
fn overwrite_delete_and_abort() {
    let engine = PostingEngine::open(Arc::new(MemoryBackend::new()), test_schema(), 64);

    let txn = engine.begin();
    let key = datastreamdb_x::data_key("0-name", 2);
    txn.get(&key)
        .expect("list")
        .add_mutation_with_index(&mut set_edge(2, "0-name", b"Alice", ValType::String), &txn)
        .expect("mutate");
    let ts1 = engine.commit(&txn).expect("commit");

    // Overwrite.
    let txn = engine.begin();
    txn.get(&key)
        .expect("list")
        .add_mutation_with_index(&mut set_edge(2, "0-name", b"Alicia", ValType::String), &txn)
        .expect("mutate");
    let ts2 = engine.commit(&txn).expect("commit");

    // Old timestamp still reads Alice (MVCC).
    let list = engine.get_list(&key, ts1).expect("read");
    assert_eq!(
        list.value(ts1).expect("value").value,
        datastreamdb_types::Value::Binary(b"Alice".to_vec())
    );
    let list = engine.get_list(&key, ts2).expect("read");
    assert_eq!(
        list.value(ts2).expect("value").value,
        datastreamdb_types::Value::Binary(b"Alicia".to_vec())
    );

    // Aborted writes never appear.
    let txn = engine.begin();
    txn.get(&key)
        .expect("list")
        .add_mutation_with_index(
            &mut set_edge(2, "0-name", b"Mallory", ValType::String),
            &txn,
        )
        .expect("mutate");
    engine.abort(&txn);
    let ts3 = engine.read_ts();
    let list = engine.get_list(&key, ts3).expect("read");
    assert_eq!(
        list.value(ts3).expect("value").value,
        datastreamdb_types::Value::Binary(b"Alicia".to_vec())
    );

    // Star delete wipes the value and its index entry.
    let txn = engine.begin();
    let mut edge = set_edge(2, "0-name", b"_STAR_ALL", ValType::String);
    edge.op = EdgeOp::Del as i32;
    txn.get(&key)
        .expect("list")
        .add_mutation_with_index(&mut edge, &txn)
        .expect("delete");
    let ts4 = engine.commit(&txn).expect("commit");
    let list = engine.get_list(&key, ts4).expect("read");
    assert!(list.value(ts4).is_err());
    let token_key =
        datastreamdb_x::index_key("0-name", &[0x02, b'A', b'l', b'i', b'c', b'i', b'a']);
    let index = engine.get_list(&token_key, ts4).expect("index");
    assert!(
        index
            .uids(&datastreamdb_posting::ListOptions {
                read_ts: ts4,
                ..Default::default()
            })
            .expect("uids")
            .is_empty()
    );
}

#[test]
fn conflicts_abort_loser() {
    let engine = PostingEngine::open(Arc::new(MemoryBackend::new()), test_schema(), 64);

    let key = datastreamdb_x::data_key("0-name", 2);
    let a = engine.begin();
    let b = engine.begin();
    a.get(&key)
        .expect("list")
        .add_mutation_with_index(&mut set_edge(2, "0-name", b"A", ValType::String), &a)
        .expect("mutate");
    b.get(&key)
        .expect("list")
        .add_mutation_with_index(&mut set_edge(2, "0-name", b"B", ValType::String), &b)
        .expect("mutate");
    engine.commit(&a).expect("first commits");
    assert!(engine.commit(&b).is_err(), "second must conflict");
}

#[test]
fn rebuild_adds_index() {
    use datastreamdb_posting::IndexRebuild;

    let schema = test_schema();
    let engine = PostingEngine::open(Arc::new(MemoryBackend::new()), schema.clone(), 64);

    // Write names while only `exact` is indexed.
    let txn = engine.begin();
    for (uid, name) in [(2u64, "Alice"), (3, "Bob")] {
        let key = datastreamdb_x::data_key("0-name", uid);
        txn.get(&key)
            .expect("list")
            .add_mutation_with_index(
                &mut set_edge(uid, "0-name", name.as_bytes(), ValType::String),
                &txn,
            )
            .expect("mutate");
    }
    engine.commit(&txn).expect("commit");

    // Add the `term` tokenizer: drop nothing, rebuild term entries.
    let old = schema.get("0-name", false).expect("schema");
    let mut current = old.clone();
    current.tokenizer.push("term".to_owned());
    schema.set("0-name", current.clone());
    let rb = IndexRebuild {
        attr: "0-name".to_owned(),
        start_ts: engine.read_ts(),
        old_schema: Some(old),
        current_schema: current,
    };
    assert!(rb.need_index_rebuild());
    rb.rebuild(&engine).expect("rebuild");

    // Term index resolves the lowercased token.
    let ts = engine.read_ts();
    let token_key = datastreamdb_x::index_key("0-name", &[0x01, b'a', b'l', b'i', b'c', b'e']);
    let index = engine.get_list(&token_key, ts).expect("index");
    let uids = index
        .uids(&datastreamdb_posting::ListOptions {
            read_ts: ts,
            ..Default::default()
        })
        .expect("uids");
    assert_eq!(uids, [2]);

    // Exact entries survived the rebuild.
    let exact_key = datastreamdb_x::index_key("0-name", &[0x02, b'B', b'o', b'b']);
    let index = engine.get_list(&exact_key, ts).expect("index");
    let uids = index
        .uids(&datastreamdb_posting::ListOptions {
            read_ts: ts,
            ..Default::default()
        })
        .expect("uids");
    assert_eq!(uids, [3]);
}

#[test]
fn drop_predicate_clears_everything() {
    let engine = PostingEngine::open(Arc::new(MemoryBackend::new()), test_schema(), 64);
    let txn = engine.begin();
    let key = datastreamdb_x::data_key("0-name", 2);
    txn.get(&key)
        .expect("list")
        .add_mutation_with_index(&mut set_edge(2, "0-name", b"Alice", ValType::String), &txn)
        .expect("mutate");
    let ts = engine.commit(&txn).expect("commit");
    assert!(engine.get_list(&key, ts).is_ok());

    engine.delete_predicate("0-name").expect("drop");
    let ts = engine.read_ts();
    let list = engine.get_list(&key, ts).expect("read");
    assert!(list.is_empty(ts, 0).expect("empty"));
    let token_key = datastreamdb_x::index_key("0-name", &[0x02, b'A', b'l', b'i', b'c', b'e']);
    let index = engine.get_list(&token_key, ts).expect("index");
    assert!(index.is_empty(ts, 0).expect("empty"));
}

#[test]
fn rollup_keeps_reads() {
    let engine = PostingEngine::open(Arc::new(MemoryBackend::new()), test_schema(), 0);
    let key = datastreamdb_x::data_key("0-friend", 2);
    for uid in 3..13u64 {
        let txn = engine.begin();
        let mut edge = DirectedEdge {
            entity: 2,
            attr: "0-friend".to_owned(),
            value_id: uid,
            value_type: ValType::Uid as i32,
            op: EdgeOp::Set as i32,
            ..DirectedEdge::default()
        };
        txn.get(&key)
            .expect("list")
            .add_mutation_with_index(&mut edge, &txn)
            .expect("mutate");
        engine.commit(&txn).expect("commit");
    }
    engine.rollup_key(&key).expect("rollup");
    let ts = engine.read_ts();
    let list = engine.get_list(&key, ts).expect("read");
    let uids = list
        .uids(&datastreamdb_posting::ListOptions {
            read_ts: ts,
            ..Default::default()
        })
        .expect("uids");
    assert_eq!(uids, (3..13u64).collect::<Vec<_>>());
    engine.rollup_pending().expect("pending");
}
