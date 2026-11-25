# Low-level key/value API

Graph applications should use DQL and GraphQL. This page documents the raw
storage methods for hosts that need a plain ordered key/value store.

The key/value surface uses the `default` partition; graph data uses the posting
partition. They are independent: a `put` does not create a predicate, and a DQL
mutation is not visible through `get`.

## Values and keys

| Item | Supported form | Notes |
|---|---|---|
| Key | UTF-8 string | Typed APIs take `&str`; keys sort lexicographically. |
| Value — Rust | Arbitrary bytes (`&[u8]`) | Binary-safe. `get` returns `Vec<u8>`. |
| Value — TypeScript / Python / C | UTF-8 string | The JSON envelope only accepts strings. |
| JSON document | Serialize to a string/bytes | `JSON.stringify`, `json.dumps`, or `serde_json`. |
| Binary data | Rust bytes, or base64 string | Node/Python/C should base64-encode binary values. |
| Numbers, booleans, dates | Encode inside a JSON string | No native scalar value types in the KV envelope. |

Keys are UTF-8 strings. Use prefixes to model collections:

```text
user:0001
user:0002
post:0001
session:2026-11-25:abc
```

`scan` returns keys in lexicographic order. Zero-pad numeric ids when order
matters (`user:0009` sorts before `user:0010`).

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

## Read one key

```json
{ "op": "get", "key": "user:1" }
```

```json
{ "ok": true, "value": "{\"name\":\"Ada\"}" }
```

A missing key returns `"value": null` (Python `None`, Rust `None`).

## Prefix scan

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
- There is **no cursor** in 0.2.1. To page, use a narrower prefix or raise
  `limit`; the engine does not return a continuation token.

For large collections, split keys into narrower prefixes
(`user:2026:11:`, `user:2026:12:`) or store an index key if you need a different
access path, e.g. `index:email:ada@example.com -> user:1`.

## Write and delete

```json
{ "op": "put", "key": "user:1", "value": "{\"name\":\"Ada\"}" }
{ "op": "delete", "key": "user:1" }
```

`put` is an upsert. `delete` is idempotent; deleting a missing key succeeds.

## Atomic batch

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

## Flush

```json
{ "op": "flush" }
```

`flush` makes prior writes durable. Dropping the last handle also flushes, but
`Drop` can only log failures; call `flush` explicitly when the result matters.

## Typed Rust helpers

The Rust API exposes typed helpers beyond the envelope: `put`, `get`, `scan`,
`delete`, `delete_range`, and `write_batch`. The other bindings use `execute`
with the envelopes above. See the Rust rustdoc for signatures.
