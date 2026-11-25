---
title: Embedding overview
---

# Embedding overview

Every embedding goes through an opaque instance handle. Handles are
`Send + Sync`; calls may run on any thread. Dropping the last handle
closes that instance only: backend flush, partition close, file-lock
release. Graph mutations commit before the call returns.

| Language | Handle | Install |
|---|---|---|
| Rust | `Arc<Instance>` | `cargo add datastreamdb-core` |
| TypeScript | `Instance` class | `npm install @datastreamdb/core` |
| Python | `datastreamdb.Instance` | `pip install datastreamdb` |
| C | `datastreamdb_handle_t*` | release asset: `libdatastreamdb_ffi.a` + `datastreamdb.h` |

## What 0.2.0 supports

| Capability | Embedded 0.2.0 | Standalone servers |
|---|---|---|
| Open multiple isolated instances in one process | Yes | N/A |
| DQL relationship/pattern queries | Yes | Yes |
| DQL set/delete mutations | Yes | Yes |
| Additive schema alters | Yes | Yes |
| GraphQL queries and mutations | Yes | Yes |
| GraphQL subscriptions | Poll-based | Poll/WebSocket |
| Multi-node Raft, ACL, namespaces | No | Yes |

!!! note "Graph mode is single-node"
    Embedded DQL/GraphQL runs locally with the same executor as the standalone
    server. Use the standalone servers for clustering, ACL, namespaces, and
    WebSocket subscriptions. See [DQL and GraphQL](queries.md) for relationship,
    pattern, schema, mutation, and subscription examples.

## Configuration

Instances open from a JSON object. Unknown fields are rejected so hosts fail
fast on version skew.

```json
{ "data_dir": "/var/lib/app/graph-a", "tenant": "default" }
```

| Field | JSON type | Default | Meaning |
|---|---|---|---|
| `data_dir` | string path or `null` | `null` | `null`/omitted = pure in-memory backend. A path = persistent fjall backend. |
| `tenant` | string | `"default"` | Label used in logs, metrics, and responses. It is **not** a data-isolation boundary. |

- One directory per live instance. The persistent backend takes an exclusive
  `LOCK`; a second open of the same directory fails until the first closes.
- Instances are isolated by handle. Two `Instance` objects never share keys,
  even when both use the in-memory backend.
- Use separate instances (and separate directories) for separate tenants or
  applications.

## What you get

Embedded 0.2.0 runs the same graph engine as the standalone servers:

| Capability | Embedded API |
|---|---|
| Schema alters, including index rebuilds | `dql_alter` / `dqlAlter` / `dql_alter` |
| DQL set/delete mutations | `dql_mutate` / `dqlMutate` / `dql_mutate` |
| DQL relationship and pattern queries | `dql_query` / `dqlQuery` / `dql_query` |
| GraphQL schema, queries, mutations | `set_graphql_schema`, `graphql` |
| Poll-based GraphQL subscriptions | `graphql_subscribe` / `graphql_next` |

There is no separate server, port, or network hop. The instance owns the graph
engine and the storage.

## Quick start

```rust
use datastreamdb_core::{Instance, InstanceConfig};

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

let result = db.dql_query(r#"
    { people(func: eq(name, "Ada")) { name friend { name } } }
"#)?;
println!("{result}");
```

The same calls in TypeScript, Python, and C are on the language pages below.
For GraphQL, install an SDL with `set_graphql_schema` and send documents to
`graphql`; see [DQL and GraphQL](queries.md).

## Data model

Data is stored as predicates and nodes, exactly as in standalone mode:

- Scalar predicates auto-derive on first mutation.
- UID relationships, reverse edges, indexes, and types need a schema alter.
- `type(Name)` selects nodes by `datastreamdb.type`; GraphQL mutations set it
  automatically.
- Facets describe an edge, not a node.

The shared model is explained in [Concepts](../concepts/index.md). The full
type and tokenizer table is in [DQL and GraphQL](queries.md#supported-types).

## Error handling

Errors are language-native in 0.2.0 — there is no JSON error envelope.

| Binding | Failure surface |
|---|---|
| Rust | `Result<_, datastreamdb_core::CoreError>` |
| TypeScript | thrown `Error` |
| Python | `RuntimeError` |
| C | non-zero status code + `datastreamdb_last_error()` |

Common failures:

- malformed JSON or unknown request fields (`bad request`)
- unknown `op`
- invalid config or unknown config fields
- storage/backend failure
- opening a persistent directory that is already locked

C status codes: `0` OK, `1` invalid argument, `2` config rejected, `3` engine
failure, `4` caught panic.

## Lifecycle and concurrency

- Rust `Instance::open` returns `Arc<Instance>`; clone the `Arc` for shared use.
- TypeScript runs blocking engine calls off the JS thread.
- Python releases the GIL around every engine call.
- C handles are `Send + Sync`; `datastreamdb_last_error` is thread-local.
- The last close/drop releases the instance’s file lock and flushes its backend.

## Language guides

- [Rust](rust.md) — `Instance`, DQL and GraphQL, threading, errors.
- [TypeScript](typescript.md) — `Instance`, DQL and GraphQL, envelope calls.
- [Python](python.md) — `Instance`, DQL and GraphQL, GIL handling.
- [C](c.md) — stable ABI, DQL and GraphQL envelopes, buffer ownership.

The low-level key/value methods (`put`, `get`, `scan`, `batch`) remain available
for hosts that need raw storage, but graph applications should use DQL and
GraphQL. They are documented separately in [Low-level key/value](key-value.md).
