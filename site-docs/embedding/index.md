# Embedding overview

Every embedding goes through an opaque instance handle. Handles are
`Send + Sync`; calls may run on any thread. Dropping the last handle
closes that instance only: backend flush, partition close, file-lock
release. Flush explicitly first when durability matters.

| Language | Handle | Install |
|---|---|---|
| Rust | `Arc<datastreamdb_core::Instance>` | `datastreamdb-core = "0.2"` |
| TypeScript | `Instance` class | `npm install @datastreamdb/core` |
| Python | `datastreamdb.Instance` | `pip install datastreamdb` |
| C | `datastreamdb_handle_t*` | release asset: `libdatastreamdb_ffi.a` + `datastreamdb.h` |

## What 0.2.0 supports

| Capability | Embedded 0.2.0 | Standalone servers |
|---|---|---|
| Open multiple isolated instances in one process | Yes | N/A |
| Raw key/value `put`, `get`, `delete` | Yes | Yes |
| Prefix `scan` | Yes | Yes |
| Atomic multi-key `batch` | Yes | Yes |
| Explicit `flush` / durability fence | Yes | Yes |
| DQL relationship/pattern queries | Yes | Yes |
| DQL set/delete mutations | Yes | Yes |
| Additive schema alters | Yes | Yes |
| GraphQL queries and mutations | Yes | Yes |
| GraphQL subscriptions | Poll-based | Poll/WebSocket |
| Multi-node Raft, ACL, namespaces | No | Yes |

!!! note "Graph mode is single-node"
    Embedded DQL/GraphQL runs locally with the same executor as the standalone
    server. Use the standalone servers for clustering, ACL, namespaces, and
    WebSocket subscriptions. See [DQL & GraphQL](queries.md) for relationship,
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

## Data model and supported types

The embedded engine is a partition-scoped ordered key/value store. Graph
documents, indexes, and DQL are handled by the standalone server layers.

| Item | Supported form | Notes |
|---|---|---|
| Key | UTF-8 string | Typed APIs take `&str`; keys sort lexicographically. |
| Value — Rust | Arbitrary bytes (`&[u8]`) | Binary-safe. `get` returns `Vec<u8>`. |
| Value — TypeScript / Python / C | UTF-8 string | The JSON envelope only accepts strings. |
| JSON document | Serialize to a string/bytes | `JSON.stringify`, `json.dumps`, or `serde_json`. |
| Binary data | Rust bytes, or base64 string | Node/Python/C should base64-encode binary values. |
| Numbers, booleans, dates | Encode inside a JSON string | No native scalar value types in the KV envelope. |
| Scalar DQL predicates | Auto-derived on first mutation | `{ set { _:a <name> "Ada" . } }` works without schema. |
| UID relationships, reverse edges, indexes, types | Declared schema | Use `dql_alter`; see [DQL & GraphQL](queries.md). |

### Keys

Keys are UTF-8 strings. Use prefixes to model collections:

```text
user:0001
user:0002
post:0001
session:2026-11-25:abc
```

`scan` returns keys in lexicographic order. Zero-pad numeric ids when order
matters (`user:0009` sorts before `user:0010`).

### Values

Rust accepts arbitrary bytes:

```rust
db.put("blob:1", &[0x00, 0xff, 0x10])?;
```

The TypeScript, Python, and C envelopes carry JSON strings. Store structured
data as JSON text:

```json
{ "name": "Ada", "age": 36, "active": true }
```

Serialize before `put`, parse after `get`/`scan`. Do not store raw JSON objects
as envelope values; the value field must be a string.

## Querying data

### Get one key

```json
{ "op": "get", "key": "user:1" }
```

```json
{ "ok": true, "value": "{\"name\":\"Ada\"}" }
```

A missing key returns `"value": null` (Python `None`, Rust `None`).

### Prefix scan

```json
{ "op": "scan", "prefix": "user:", "limit": 100 }
```

```json
{
  "ok": true,
  "pairs": [
    { "key": "user:1", "value": "{\"name\":\"Ada\"}" },
    { "key": "user:2", "value": "{\"name\":\"Bob\"}" }
  ]
}
```

- `prefix` defaults to `""` (scan from the beginning).
- `limit` defaults to **1000** and is clamped to **100,000**.
- Results are in key order.
- There is **no cursor** in 0.2.0. To page, use a narrower prefix or raise
  `limit`; the engine does not return a continuation token.

### Query limits

Scans are bounded. For large collections:

1. Split keys into narrower prefixes (`user:2026:11:`, `user:2026:12:`).
2. Keep the working set below the scan limit.
3. Store an index key if you need a different access path, e.g.
   `index:email:ada@example.com -> user:1`.

## Mutations

### Put and delete

```json
{ "op": "put", "key": "user:1", "value": "{\"name\":\"Ada\"}" }
{ "op": "delete", "key": "user:1" }
```

`put` is an upsert. `delete` is idempotent; deleting a missing key succeeds.

### Atomic batch

```json
{
  "op": "batch",
  "puts": [
    { "key": "user:1", "value": "{\"name\":\"Ada\"}" },
    { "key": "user:2", "value": "{\"name\":\"Bob\"}" }
  ],
  "deletes": ["user:3"]
}
```

The batch is applied atomically. The response reports the number of staged
operations:

```json
{ "ok": true, "applied": 3 }
```

### Flush

```json
{ "op": "flush" }
```

`flush` makes prior writes durable. Dropping the last handle also flushes, but
`Drop` can only log failures; call `flush` explicitly when the result matters.

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

- [Rust](rust.md) — typed byte-oriented API, scans, batches, ranges.
- [TypeScript](typescript.md) — `Instance`, JSON documents, envelope operations.
- [Python](python.md) — `Instance`, JSON documents, envelope operations.
- [C](c.md) — stable ABI, status codes, response buffer ownership.
