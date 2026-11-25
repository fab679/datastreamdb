# Rust embedding

```toml
[dependencies]
datastreamdb-core = "0.2"
serde_json = "1"
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

## DQL

```rust
let db = Instance::open(InstanceConfig::memory())?;

db.dql_alter(r#"
name: string @index(exact) .
friend: [uid] @reverse .
type Person {
    name
    friend
}
"#)?;

db.dql_mutate(r#"{
    set {
        _:ada <name> "Ada" .
        _:bob <name> "Bob" .
        _:ada <friend> _:bob .
    }
}"#)?;

let people = db.dql_query(r#"
    { people(func: eq(name, "Ada")) { name friend { name } } }
"#)?;
println!("{people}");
```

`dql_mutate` returns a `HashMap<String, u64>` of blank-node assignments.
`dql_query` returns `serde_json::Value`; `dql_query_json` returns the JSON text.

## GraphQL

```rust
use std::collections::HashMap;

db.set_graphql_schema(r#"
type Post {
    id: ID!
    title: String @search(by: [term])
    author: Author @hasInverse(field: posts)
}
type Author {
    id: ID!
    name: String @id @search(by: [hash])
    posts: [Post]
}
"#)?;

let headers = HashMap::new();
let response = db.graphql(
    r#"mutation {
        addPost(input: [{title: "Hello", author: {name: "Ann"}}]) {
            post { id title author { name } }
            numUids
        }
    }"#,
    None,
    serde_json::json!({}),
    &headers,
)?;
println!("{response}");
```

GraphQL execution errors arrive in `response["errors"]`; schema or transport
failures return `CoreError`.

## Subscriptions

```rust
use std::time::Duration;

let sub = db.graphql_subscribe(
    r#"subscription { queryPost { title } }"#,
    serde_json::json!({}),
    &headers,
    Duration::from_millis(100),
)?;

let event = sub.next()?;
sub.close();
```

`next_timeout` waits with thread sleeps; `next_async` and `stream` are available
for async callers. Close subscriptions before dropping the instance.

## Errors

`Instance` methods return `Result<_, datastreamdb_core::CoreError>`:

```rust
match db.dql_query("{ q(func: has(name)) { name } }") {
    Ok(result) => println!("{result}"),
    Err(err) => eprintln!("query failed: {err}"),
}
```

Malformed configs return `CoreError::BadRequest`; backend failures return
`CoreError::Store`; query and schema failures return `CoreError::Query` and
`CoreError::Schema`.

## Share across threads

```rust
use std::thread;

let db = Instance::open(InstanceConfig::memory())?;
let mut workers = Vec::new();
for _ in 0..4 {
    let db = db.clone();
    workers.push(thread::spawn(move || {
        db.dql_query("{ q(func: has(name)) { name } }").unwrap();
    }));
}
for worker in workers {
    worker.join().unwrap();
}
```

`Instance` is `Send + Sync`. Clone the `Arc` rather than opening the same
persistent directory twice; the second open fails on the file lock.

## Limitations in 0.2.0

- Embedded graph mode is single-node; clustering, ACL, and namespaces require
  the standalone servers.
- DQL alters are additive/updating; drop operations and upserts are not exposed
  in the embedded API.
- GraphQL subscriptions are poll-based; WebSocket push is not exposed.

See [DQL and GraphQL](queries.md) for pattern filters, facets, and subscription
shapes. The separate [low-level key/value API](key-value.md) is available for
hosts that need raw storage.
