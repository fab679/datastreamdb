# Embedding overview

Every embedding goes through an opaque instance handle. Handles are
`Send + Sync`; calls may run on any thread. Dropping the last handle
closes that instance only: backend flush, partition close, file-lock
release. Flush explicitly first when durability matters.

| Language | Handle |
|---|---|
| Rust | `Arc<datastreamdb_core::Instance>` |
| TypeScript | `Instance` class |
| Python | `datastreamdb.Instance` |
| C | `datastreamdb_handle_t*` |

## Configuration

Instances open from JSON (typed builders exist in each binding):

```json
{ "data_dir": "/var/lib/app/graph-a", "tenant": "default" }
```

- `data_dir: null` (or `{}`) selects the pure in-memory backend.
- A directory selects the persistent fjall backend — one directory
  per live instance, enforced by an exclusive `LOCK` file.
- Unknown fields are rejected so hosts fail fast on version skew.

## The `execute` envelope

All four bindings expose `execute(request_json) -> response_json`:

| `op` | Request fields | Effect |
|---|---|---|
| `ping` | — | Liveness + identity (`instance`, `tenant`, `backend`) |
| `put` | `key`, `value` | Raw key write |
| `get` | `key` | Raw key read (`value` or `null`) |
| `delete` | `key` | Raw key delete |
| `scan` | `prefix`, `limit?` | Prefix scan (default limit 1000, clamped) |
| `batch` | `puts[]`, `deletes[]` | Atomic multi-write |
| `flush` | — | Durability fence |

```json
{ "op": "batch", "puts": [{ "key": "n", "value": "Ada" }], "deletes": [] }
```

Errors are typed JSON (`code`, `message`) and never panic across
the boundary: the C ABI catches panics and reports them through
`datastreamdb_last_error`.

!!! note
    `query`/`mutate`/`alter` envelope ops are planned but not in
    0.1.0 — graph operations currently run against the standalone
    servers ([Quickstart](../quickstart.md)).

## Language guides

- [Rust](rust.md) · [TypeScript](typescript.md) ·
  [Python](python.md) · [C](c.md)
