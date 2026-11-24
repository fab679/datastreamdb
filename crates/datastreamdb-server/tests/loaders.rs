//! `bulk`/`live` CLI roundtrips over real database directories.

use std::process::Command;
use std::sync::Arc;

const RDF: &str =
    "<0x2> <name> \"Alice\" .\n<0x2> <age> \"30\"^^<xs:int> .\n_:bob <name> \"Bob\" .\n";

fn write_rdf(dir: &tempfile::TempDir) -> std::path::PathBuf {
    let path = dir.path().join("data.rdf");
    std::fs::write(&path, RDF).expect("write rdf");
    path
}

/// Opens `dir` and returns the sorted names stored per `0-name` list.
fn stored_names(data_dir: &std::path::Path) -> Vec<String> {
    let backend = Arc::new(datastreamdb_store::FjallBackend::open(data_dir).expect("open backend"));
    let schema = Arc::new(datastreamdb_schema::SchemaState::new());
    let engine = datastreamdb_posting::PostingEngine::open(backend, schema, 64);
    let pairs = engine.store().scan_prefix("posting", b"").expect("scan");
    let mut names = Vec::new();
    for (key_bytes, _) in &pairs {
        let Ok(key) = datastreamdb_x::keycodec::parse(key_bytes) else {
            continue;
        };
        if !key.is_data() || key.attr != "0-name" {
            continue;
        }
        let list = engine.get_list(key_bytes, u64::MAX).expect("list");
        for value in list.all_values(u64::MAX).expect("values") {
            let text = match &value.value {
                datastreamdb_types::Value::Binary(bytes) => {
                    String::from_utf8_lossy(bytes).into_owned()
                }
                datastreamdb_types::Value::String(text) => text.clone(),
                other => format!("{other:?}"),
            };
            names.push(text);
        }
    }
    names.sort();
    names
}

#[test]
fn bulk_cli_loads_fresh_dir() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rdf = write_rdf(&dir);
    let data_dir = dir.path().join("p");
    let output = Command::new(env!("CARGO_BIN_EXE_bulk"))
        .arg(&rdf)
        .arg("--data-dir")
        .arg(&data_dir)
        .output()
        .expect("run bulk");
    assert!(
        output.status.success(),
        "bulk failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("bulk load complete"), "stdout: {stdout}");
    assert!(stdout.contains("3 nquads"), "stdout: {stdout}");
    let names = stored_names(&data_dir);
    assert!(names.iter().any(|name| name.contains("Alice")), "{names:?}");
    assert!(names.iter().any(|name| name.contains("Bob")), "{names:?}");
}

#[test]
fn live_cli_appends_to_dir() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rdf = write_rdf(&dir);
    let data_dir = dir.path().join("p");
    for _ in 0..2 {
        let output = Command::new(env!("CARGO_BIN_EXE_live"))
            .arg(&rdf)
            .arg("--data-dir")
            .arg(&data_dir)
            .output()
            .expect("run live");
        assert!(
            output.status.success(),
            "live failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    // Alice reloaded onto uid 2; each run minted Bob a fresh blank UID.
    let names = stored_names(&data_dir);
    assert_eq!(names.iter().filter(|name| name.contains("Bob")).count(), 2);
    assert!(names.iter().any(|name| name.contains("Alice")), "{names:?}");
}

#[test]
fn loader_clis_reject_bad_input() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bad = dir.path().join("bad.rdf");
    std::fs::write(&bad, "<0x2> <oops .\n").expect("write");
    let output = Command::new(env!("CARGO_BIN_EXE_live"))
        .arg(&bad)
        .arg("--data-dir")
        .arg(":memory:")
        .output()
        .expect("run live");
    assert!(!output.status.success());
    let output = Command::new(env!("CARGO_BIN_EXE_live"))
        .arg(&bad)
        .arg("--data-dir")
        .arg(":memory:")
        .arg("--alpha")
        .arg("127.0.0.1:9080")
        .output()
        .expect("run live remote");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("gRPC client epic"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
