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

## Data model

Embedded graph mode uses the same model as the standalone servers: predicates,
types, UID relationships, reverse edges, and facets. DQL and GraphQL are the
supported interface. Scalar predicates auto-derive on first mutation; UID
relationships, indexes, and types require a schema alter.

```rust
db.dql_alter(
    "name: string @index(exact) . friend: [uid] @reverse . type Person {\n    name\n    friend\n}",
)?;
db.dql_mutate(r#"{ set { _:a <name> "Ada" . _:b <name> "Bob" . _:a <friend> _:b . } }"#)?;
let people = db.dql_query(r#"{ people(func: eq(name, "Ada")) { name friend { name } } }"#)?;
```

GraphQL subscriptions are poll-based (`graphql_subscribe` / `graphql_next` /
`graphql_unsubscribe`); WebSocket push remains server work. See
`site-docs/embedding/queries.md` for relationship, pattern, GraphQL, and
subscription examples.

## Graph requests

Envelope over the handle (`execute`):

| `op` | Request | Success response |
|---|---|---|
| `dql_query` | `query` | `{ok:true, data:<json>}` |
| `dql_mutate` | `query` | `{ok:true, uids:{blank:uid}}` |
| `dql_alter` | `schema` | `{ok:true}` |
| `set_graphql_schema` | `sdl` | `{ok:true}` |
| `graphql_schema` | — | `{ok:true, sdl:string-or-null}` |
| `graphql` | `query`, `operation?`, `variables?`, `headers?` | `{ok:true, response:{data,errors}}` |
| `graphql_subscribe` | `query`, `variables?`, `headers?`, `poll_ms?` | `{ok:true, id, response}` |
| `graphql_next` | `id`, `wait_ms?` | `{ok:true, changed, closed, response?}` |
| `graphql_unsubscribe` | `id` | `{ok:true, removed}` |

## Low-level key/value requests

The raw storage methods remain available for hosts that need them, but they are
not part of the graph model and should not be used for graph records.

| `op` | Request | Success response |
|---|---|---|
| `ping` | `{"op":"ping"}` | `{ok, instance, tenant, backend}` |
| `put` | `key`, `value` | `{ok:true}` |
| `get` | `key` | `{ok:true, value:string-or-null}` |
| `delete` | `key` | `{ok:true}` |
| `scan` | `prefix`, `limit?` | `{ok:true, pairs:[{key,value}]}` |
| `batch` | `puts[]`, `deletes[]` | `{ok:true, applied:n}` |
| `flush` | `{"op":"flush"}` | `{ok:true}` |

`scan` returns keys in order. `limit` defaults to 1000 and clamps to 100,000;
there is no continuation cursor in 0.2.0. `batch` applies all puts/deletes
atomically. `delete` is idempotent. See `site-docs/embedding/key-value.md` for
the value and key rules.

## Errors

Errors are language-native; there is no JSON error envelope in 0.2.0.

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
language-specific examples for Rust, TypeScript, Python, and C. Complete
movies/TODO/orders walkthroughs are in `site-docs/tutorials/`, and the shared
data model is explained in `site-docs/concepts/`.
