//! Embedded-mode conformance: isolation, concurrency, envelopes, lifecycle.
//! Every binding (TS, Python, C) must satisfy these behaviors.

use std::sync::Arc;

use datastreamdb_core::{Instance, InstanceConfig};

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("datastreamdb-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn two_memory_instances_are_isolated() {
    let a = Instance::open(InstanceConfig::memory()).expect("open a");
    let b = Instance::open(InstanceConfig::memory()).expect("open b");
    assert_ne!(a.id(), b.id());

    a.put("name", b"Ada").expect("put");
    assert_eq!(
        a.get("name").expect("get").as_deref(),
        Some(b"Ada".as_slice())
    );
    assert_eq!(b.get("name").expect("get"), None);

    b.put("name", b"Bob").expect("put");
    assert_eq!(
        a.get("name").expect("get").as_deref(),
        Some(b"Ada".as_slice())
    );
}

#[cfg(feature = "fjall")]
#[test]
fn config_matrix() {
    // Bare config: memory backend, default tenant.
    let bare = Instance::open(InstanceConfig::from_json("{}").expect("parse")).expect("open");
    assert_eq!(bare.backend_name(), "memory");
    assert_eq!(bare.tenant(), "default");

    // Tenant label flows through to ping responses.
    let tenant =
        Instance::open(InstanceConfig::from_json("{\"tenant\": \"acme\"}").expect("parse"))
            .expect("open");
    let ping: serde_json::Value =
        serde_json::from_str(&tenant.execute("{\"op\":\"ping\"}").expect("exec")).expect("json");
    assert_eq!(ping["tenant"], "acme");

    // Directory config selects fjall.
    let dir = temp_dir("config-matrix");
    let durable = Instance::open(InstanceConfig {
        data_dir: Some(dir.clone()),
        tenant: "t".to_owned(),
    })
    .expect("open");
    assert_eq!(durable.backend_name(), "fjall");
    drop(durable);
    std::fs::remove_dir_all(&dir).expect("cleanup");

    // Unknown fields and malformed JSON are rejected, never ignored.
    assert!(InstanceConfig::from_json("{\"data_dir\": null, \"wat\": 1}").is_err());
    assert!(InstanceConfig::from_json("{\"tenant\": 42}").is_err());
    assert!(InstanceConfig::from_json("{broken").is_err());
}

#[test]
fn errors_are_isolated_per_instance() {
    let a = Instance::open(InstanceConfig::memory()).expect("open a");
    let b = Instance::open(InstanceConfig::memory()).expect("open b");

    assert!(a.execute("not json").is_err());
    assert!(a.execute("{\"op\":\"drop_everything\"}").is_err());
    assert!(a.execute("{\"op\":\"put\",\"key\":\"k\"}").is_err()); // missing value

    // Failures on A neither touch B's data nor its counters.
    assert_eq!(b.executed(), 0);
    b.put("k", b"v").expect("put");
    assert_eq!(a.get("k").expect("get"), None);
    assert_eq!(a.executed(), 3);
    assert_eq!(b.executed(), 0);
}

#[cfg(feature = "fjall")]
#[test]
fn dir_backed_instance_persists_across_reopen() {
    let dir = temp_dir("conformance");
    let config = InstanceConfig {
        data_dir: Some(dir.clone()),
        tenant: "t".to_owned(),
    };
    let db = Instance::open(config.clone()).expect("open");
    assert_eq!(db.backend_name(), "fjall");
    db.put("k", b"v").expect("put");
    db.flush().expect("flush");
    drop(db);

    let reopened = Instance::open(config).expect("reopen");
    assert_eq!(
        reopened.get("k").expect("get").as_deref(),
        Some(b"v".as_slice())
    );
    drop(reopened);
    std::fs::remove_dir_all(&dir).expect("cleanup");
}

#[cfg(feature = "fjall")]
#[test]
fn drop_closes_and_flushes_without_explicit_flush() {
    let dir = temp_dir("drop-close");
    {
        let db = Instance::open(InstanceConfig {
            data_dir: Some(dir.clone()),
            tenant: "t".to_owned(),
        })
        .expect("open");
        db.put("k", b"v").expect("put");
        // No flush: Drop must close durably.
    }
    let reopened = Instance::open(InstanceConfig {
        data_dir: Some(dir.clone()),
        tenant: "t".to_owned(),
    })
    .expect("reopen");
    assert_eq!(
        reopened.get("k").expect("get").as_deref(),
        Some(b"v".as_slice())
    );
    drop(reopened);
    std::fs::remove_dir_all(&dir).expect("cleanup");
}

#[cfg(feature = "fjall")]
#[test]
fn locked_directory_rejects_second_instance() {
    let dir = temp_dir("locked-dir");
    let config = InstanceConfig {
        data_dir: Some(dir.clone()),
        tenant: "t".to_owned(),
    };
    let first = Instance::open(config.clone()).expect("first open");
    assert!(Instance::open(config.clone()).is_err());
    drop(first);
    Instance::open(config).expect("open after close");
    std::fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn execute_envelope_roundtrips() {
    let db = Instance::open(InstanceConfig::memory()).expect("open");
    let put: serde_json::Value = serde_json::from_str(
        &db.execute(r#"{"op":"put","key":"n","value":"Ada"}"#)
            .expect("put"),
    )
    .expect("json");
    assert_eq!(put["ok"], true);
    let get: serde_json::Value =
        serde_json::from_str(&db.execute(r#"{"op":"get","key":"n"}"#).expect("get")).expect("json");
    assert_eq!(get["value"], "Ada");

    let batch: serde_json::Value = serde_json::from_str(
        &db.execute(r#"{"op":"batch","puts":[{"key":"a","value":"1"}],"deletes":["n"]}"#)
            .expect("batch"),
    )
    .expect("json");
    assert_eq!(batch["applied"], 2);

    let scan: serde_json::Value = serde_json::from_str(
        &db.execute(r#"{"op":"scan","prefix":"","limit":0}"#)
            .expect("scan"),
    )
    .expect("json");
    assert_eq!(scan["pairs"].as_array().expect("array").len(), 0);
    let scan: serde_json::Value =
        serde_json::from_str(&db.execute(r#"{"op":"scan","prefix":""}"#).expect("scan"))
            .expect("json");
    let pairs = scan["pairs"].as_array().expect("array");
    assert_eq!(pairs.len(), 1);
    assert_eq!(pairs[0]["key"], "a");

    let del: serde_json::Value =
        serde_json::from_str(&db.execute(r#"{"op":"delete","key":"a"}"#).expect("del"))
            .expect("json");
    assert_eq!(del["ok"], true);
    let missing: serde_json::Value =
        serde_json::from_str(&db.execute(r#"{"op":"get","key":"a"}"#).expect("get")).expect("json");
    assert!(missing["value"].is_null());

    let flush: serde_json::Value =
        serde_json::from_str(&db.execute(r#"{"op":"flush"}"#).expect("flush")).expect("json");
    assert_eq!(flush["ok"], true);

    assert!(db.execute("not json").is_err());
    assert!(db.execute(r#"{"op":"drop_everything"}"#).is_err());
    assert!(
        db.execute(r#"{"op":"get","key":"a","extra":true}"#)
            .is_err()
    );
    assert_eq!(db.executed(), 11);
}

#[test]
fn concurrent_instances_share_nothing() {
    let mut handles = Vec::new();
    for i in 0..8u32 {
        handles.push(std::thread::spawn(move || {
            let db = Instance::open(InstanceConfig::memory()).expect("open");
            for n in 0..100 {
                let key = format!("k{n}");
                db.put(&key, &i.to_be_bytes()).expect("put");
                assert_eq!(
                    db.get(&key).expect("get").as_deref(),
                    Some(i.to_be_bytes().as_slice())
                );
            }
            db.id()
        }));
    }
    let ids: Vec<_> = handles
        .into_iter()
        .map(|h| h.join().expect("join"))
        .collect();
    let mut sorted = ids.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), ids.len(), "instance ids must be unique");
}

#[test]
fn open_close_churn_is_clean() {
    let mut handles = Vec::new();
    for worker in 0..4u32 {
        handles.push(std::thread::spawn(move || {
            for round in 0..25 {
                let db = Instance::open(InstanceConfig::memory()).expect("open");
                let key = format!("w{worker}-{round}");
                db.put(&key, &worker.to_be_bytes()).expect("put");
                assert_eq!(
                    db.get(&key).expect("get").as_deref(),
                    Some(worker.to_be_bytes().as_slice())
                );
                let ping: serde_json::Value =
                    serde_json::from_str(&db.execute("{\"op\":\"ping\"}").expect("exec"))
                        .expect("json");
                assert_eq!(ping["instance"], db.id().to_string());
                drop(db);
            }
        }));
    }
    for handle in handles {
        handle.join().expect("join");
    }
}

/// Slow mixed-operation soak. Run explicitly:
/// `cargo test -p datastreamdb-core -- --include-ignored`.
#[test]
#[ignore = "slow soak; run explicitly with --include-ignored"]
fn soak_mixed_operations() {
    let db: Arc<Instance> = Instance::open(InstanceConfig::memory()).expect("open");
    let mut handles = Vec::new();
    for worker in 0..4u32 {
        let db = db.clone();
        handles.push(std::thread::spawn(move || {
            for n in 0..1000 {
                let key = format!("w{worker}-{n:04}");
                db.put(&key, b"x").expect("put");
                if n % 10 == 0 {
                    let mut batch = datastreamdb_core::WriteBatch::new();
                    batch
                        .put(
                            datastreamdb_core::DEFAULT_PARTITION,
                            format!("b{worker}-{n}").as_bytes(),
                            b"y",
                        )
                        .delete(
                            datastreamdb_core::DEFAULT_PARTITION,
                            format!("w{worker}-{n:04}").as_bytes(),
                        );
                    db.write_batch(&batch).expect("batch");
                }
                if n % 50 == 0 {
                    let _ = db.scan(&format!("w{worker}-"), 100).expect("scan");
                    db.flush().expect("flush");
                }
            }
        }));
    }
    for handle in handles {
        handle.join().expect("join");
    }
    assert_eq!(db.scan("w", usize::MAX).expect("scan").len(), 4 * 900);
    assert_eq!(db.scan("b", usize::MAX).expect("scan").len(), 4 * 100);
}
