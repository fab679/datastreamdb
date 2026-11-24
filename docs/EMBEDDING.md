# EMBEDDING - multi-instance hosting model

## Handles

Every embedding goes through an opaque instance handle:

- Rust: `Arc<datastreamdb_core::Instance>`
- C ABI: `datastreamdb_handle_t*` (`datastreamdb_open` / `datastreamdb_close`)
- TypeScript: `Instance` class (napi external wrapping the same Arc)
- Python: `datastreamdb.Instance` (pyo3 wrapper holding the Arc)

Handles are `Send + Sync`; queries may run on any thread. The last handle drop
closes that instance only: backend flush, partition close, file-lock release.
Use an explicit `flush` beforehand when the result matters (`Drop` can only
log).

## Configuration

`InstanceConfig` (JSON at the FFI, typed builders in each binding):

```json
{ "data_dir": "/var/lib/app/graph-a", "tenant": "default" }
```

- `data_dir: null` selects the pure in-memory backend (tests, caches, ephemeral).
- A directory selects the fjall backend; one directory per live instance
  (enforced by an exclusive `LOCK` file; a second open fails until the first
  drops).
- `tenant` is a label used in logs, metrics, and responses. It is not a data
  isolation boundary.
- Unknown fields are rejected so hosts fail fast on version skew.

## Data model and supported types

The embedded engine is an ordered, partition-scoped key/value store.

| Item | Supported form | Notes |
|---|---|---|
| Key | UTF-8 string | Typed APIs take `&str`; keys sort lexicographically. |
| Value (Rust) | arbitrary bytes (`&[u8]`) | Binary-safe; `get` returns `Vec<u8>`. |
| Value (TS/Python/C envelope) | UTF-8 string | JSON values must be strings. |
| JSON document | serialize to string/bytes | `serde_json`, `JSON.stringify`, `json.dumps`. |
| Binary (TS/Python/C) | base64 string | The envelope has no byte-array value. |
| Numbers/bools/dates | inside a JSON string | No native scalar value types in 0.1.0. |

Use key prefixes to model collections (`user:0001`, `post:0001`). Zero-pad
numeric ids when lexicographic order matters.

## Requests

M1 envelope is JSON over the handle (`execute`):

| `op` | Request | Success response |
|---|---|---|
| `ping` | `{"op":"ping"}` | `{ok, instance, tenant, backend}` |
| `put` | `key`, `value` | `{ok:true}` |
| `get` | `key` | `{ok:true, value:string-or-null}` |
| `delete` | `key` | `{ok:true}` |
| `scan` | `prefix`, `limit?` | `{ok:true, pairs:[{key,value}]}` |
| `batch` | `puts[]`, `deletes[]` | `{ok:true, applied:n}` |
| `flush` | `{"op":"flush"}` | `{ok:true}` |

```json
{ "op": "batch", "puts": [{ "key": "n", "value": "Ada" }], "deletes": [] }
```

`scan` returns keys in order. `limit` defaults to 1000 and clamps to 100,000;
there is no continuation cursor in 0.1.0. `batch` applies all puts/deletes
atomically. `delete` is idempotent.

DQL/GraphQL engines exist as crates (`datastreamdb-query`,
`datastreamdb-graphql`); embedded envelope ops for them are future work. Use
the standalone servers for DQL and GraphQL today.

## Errors

Errors are language-native; there is no JSON error envelope in 0.1.0.

- Rust: `Result<_, datastreamdb_core::CoreError>`.
- TypeScript: thrown `Error`.
- Python: `RuntimeError`.
- C: non-zero status code plus `datastreamdb_last_error()`.

C codes: `0` OK, `1` invalid argument/envelope, `2` config rejected, `3` engine
failure, `4` caught panic. Panics never cross the FFI boundary.

## Threading

Core traits are sync; async adapters live at the edge:

- napi: `tokio_rt` executes blocking core calls off the JS thread.
- pyo3: the GIL is released around every engine call.
- axum/tonic servers use `spawn_blocking` for engine work.

## ABI stability

`datastreamdb-ffi` follows semver on symbols: additive only within major
versions. CI checks the exported symbol list (`tools/xtask spec`) against the
checked-in allowlist.

The published developer guide lives at `site-docs/embedding/index.md` and has
language-specific examples for Rust, TypeScript, Python, and C.
