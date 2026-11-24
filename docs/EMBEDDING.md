# EMBEDDING - multi-instance hosting model

## Handles

Every embedding goes through an opaque instance handle:

- Rust: `Arc<datastreamdb_core::Instance>`
- C ABI: `datastreamdb_instance_t*` (`datastreamdb_open` / `datastreamdb_close`)
- TypeScript: `Instance` class (napi external wrapping the same Arc)
- Python: `datastreamdb.Instance` (pyo3 wrapper holding the Arc)

Handles are `Send + Sync`; queries may run on any thread. The last handle drop closes
that instance only: backend flush, partition close, file-lock release. Use an
explicit `flush` beforehand when the result matters (`Drop` can only log).

## Configuration

`InstanceConfig` (JSON at the FFI, typed builders in each binding):

```json
{ "data_dir": "/var/lib/app/graph-a", "tenant": "default" }
```

- `data_dir: null` selects the pure in-memory backend (tests, caches, ephemeral).
- A directory selects the fjall backend; one directory per live instance (enforced by an exclusive `LOCK` file; a second open fails until the first drops).
- Unknown fields are rejected so hosts fail fast on version skew.

## Requests

M1 envelope is JSON over the handle (`execute`): `ping`, `put`, `get`,
`delete`, `scan` (prefix + limit, default 1000, clamped), `batch` (atomic
puts + deletes), `flush`. DQL/GraphQL engines exist as crates (`datastreamdb-query`, `datastreamdb-graphql`); envelope ops for them are future work:

```json
{ "op": "batch", "puts": [{ "key": "n", "value": "Ada" }], "deletes": [] }
```

Errors are typed (`code`, `message`) and never panic across the boundary: panics are
caught at the FFI, recorded via `datastreamdb_last_error`, and returned as codes.

## Threading

Core traits are sync; async adapters live at the edge:

- napi: `tokio_rt` executes blocking core calls off the JS thread.
- pyo3: the GIL is released around every engine call.
- axum/tonic servers use `spawn_blocking` for engine work.

## ABI stability

`datastreamdb-ffi` follows semver on symbols: additive only within major versions. CI checks the
exported symbol list (`tools/xtask spec`) against the checked-in allowlist.
