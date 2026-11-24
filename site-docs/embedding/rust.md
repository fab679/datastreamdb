# Rust embedding

```toml
[dependencies]
datastreamdb-core = "0.1"
```

```rust
use datastreamdb_core::{Instance, InstanceConfig};

let a = Instance::open(InstanceConfig::memory())?;
let b = Instance::open(InstanceConfig::from_json(r#"{"tenant": "b"}"#)?)?;
assert_ne!(a.id(), b.id());

a.put("name", b"Ada")?;
assert_eq!(a.get("name")?.as_deref(), Some(b"Ada".as_slice()));
assert_eq!(b.get("name")?, None); // isolated

let ping: serde_json::Value =
    serde_json::from_str(&a.execute(r#"{"op":"ping"}"#)?)?;
assert_eq!(ping["ok"], true);
```

## Persistent instances

```rust
let cfg = InstanceConfig::from_json(r#"{"data_dir": "/var/lib/app/graph-a"}"#)?;
let db = Instance::open(cfg)?; // fjall backend, exclusive LOCK
db.put("k", b"v")?;
db.flush()?; // durability fence before drop
```

`Instance` is `Arc`-friendly (`open` returns `Arc<Self>`) and safe
to share across threads. See `datastreamdb-core` rustdocs for the full API.
