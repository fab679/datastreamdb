# Rust embedding

```toml
[dependencies]
datastreamdb-core = "0.1"
serde_json = "1" # only if you store JSON documents
```

`datastreamdb-core` returns `Arc<Instance>`. Clone the `Arc` to share one
instance across threads.

## Open an instance

```rust
use datastreamdb_core::{Instance, InstanceConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // In-memory, isolated instance.
    let memory = Instance::open(InstanceConfig::memory())?;

    // Persistent instance with a tenant label.
    let cfg = InstanceConfig::from_json(
        r#"{"data_dir": "/var/lib/app/graph-a", "tenant": "app"}"#,
    )?;
    let persistent = Instance::open(cfg)?;

    println!("{} {}", memory.id(), memory.backend_name());
    println!("{} {}", persistent.id(), persistent.backend_name());
    Ok(())
}
```

- `data_dir: null` or omitted selects memory.
- A directory selects fjall and takes an exclusive `LOCK`.
- `tenant` is a label, not an isolation boundary. Use separate instances for
  separate data.

## Supported types

| Rust API | Type | Notes |
|---|---|---|
| `put` key | `&str` | UTF-8; keys sort lexicographically. |
| `put` value | `&[u8]` | Arbitrary bytes, including binary. |
| `get` | `Result<Option<Vec<u8>>, _>` | `None` when missing. |
| `scan` | `Vec<(String, Vec<u8>)>` | Keys are lossy UTF-8; values stay raw bytes. |
| `execute` value | JSON string | The envelope is string-only even in Rust. |

## Store and read a JSON document

```rust
use datastreamdb_core::{Instance, InstanceConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = Instance::open(InstanceConfig::memory())?;

    let user = serde_json::json!({ "name": "Ada", "age": 36, "active": true });
    db.put("user:1", serde_json::to_vec(&user)?.as_slice())?;

    let raw = db.get("user:1")?.expect("user:1 exists");
    let decoded: serde_json::Value = serde_json::from_slice(&raw)?;
    println!("{} is {}", decoded["name"], decoded["age"]);
    Ok(())
}
```

## Query with prefix scans

```rust
let pairs = db.scan("user:", 100)?; // max 100 pairs
for (key, value) in pairs {
    let doc: serde_json::Value = serde_json::from_slice(&value)?;
    println!("{key}: {}", doc["name"]);
}
```

- `scan(prefix, limit)` returns keys in order.
- `limit` is clamped to `MAX_SCAN_LIMIT` (100,000).
- There is no cursor; narrow the prefix or raise the limit to page.
- Use zero-padded ids for stable ordering (`user:0001`).

## Atomic writes

```rust
use datastreamdb_core::{DEFAULT_PARTITION, WriteBatch};

let mut batch = WriteBatch::new();
batch
    .put(DEFAULT_PARTITION, b"user:1", br#"{"name":"Ada"}"#)
    .put(DEFAULT_PARTITION, b"user:2", br#"{"name":"Bob"}"#)
    .delete(DEFAULT_PARTITION, b"user:3");
db.write_batch(&batch)?;

println!("staged {} operations", batch.len());
```

`write_batch` applies all operations atomically. `delete` is idempotent.

## Delete and range delete

```rust
db.delete("user:1")?; // missing key is a no-op

// Half-open range [start, end): ';' is the byte after ':'.
let removed = db.delete_range("session:", "session;")?;
println!("removed {removed} session keys");
```

`delete_range` is a typed Rust helper; it is not exposed in the JSON envelope.

## Execute the JSON envelope

Use `execute` when you want the same request format as the other bindings:

```rust
let response = db.execute(r#"{"op":"scan","prefix":"user:","limit":100}"#)?;
let json: serde_json::Value = serde_json::from_str(&response)?;
for pair in json["pairs"].as_array().expect("pairs") {
    println!("{} = {}", pair["key"], pair["value"]);
}
```

Envelope values are strings. For binary data, use the typed `put`/`get` API.

## Durability

```rust
db.put("checkpoint", b"ok")?;
db.flush()?; // returns storage errors
```

Dropping the last `Arc` also flushes, but `Drop` can only log failures. Call
`flush` explicitly when the write must be durable before the process exits.

## Errors

`Instance` methods return `Result<_, datastreamdb_core::CoreError>`:

```rust
match db.get("missing") {
    Ok(Some(value)) => println!("{} bytes", value.len()),
    Ok(None) => println!("not found"),
    Err(err) => eprintln!("storage error: {err}"),
}
```

Malformed configs and envelopes return `CoreError::BadRequest`; backend failures
return `CoreError::Store`.

## Share across threads

```rust
use std::thread;

let db = Instance::open(InstanceConfig::memory())?;
let mut workers = Vec::new();
for worker in 0..4 {
    let db = db.clone();
    workers.push(thread::spawn(move || {
        db.put(&format!("worker:{worker}"), b"ready").unwrap();
    }));
}
for worker in workers {
    worker.join().unwrap();
}
```

`Instance` is `Send + Sync`. Use `Arc` clones rather than opening the same
persistent directory twice; the second open fails on the file lock.

## Limitations in 0.1.0

- Embedded mode is key/value only. DQL and GraphQL run against the standalone
  servers.
- Prefix scans are bounded and have no continuation cursor.
- The JSON envelope is string-only; use the typed `put`/`get` API for binary
  values.
