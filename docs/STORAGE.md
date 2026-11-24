# STORAGE - backends and compatibility

## Trait

`datastreamdb_store::Backend` is the only storage interface the engine uses: point
get/put/delete, prefix scan, atomic write batches, range deletes, flush, and
close. It is sync, `Send + Sync`, and partition-scoped
(`default`, `wal`, `index`, ...). `close` is the last fallible shutdown step;
locks release on drop, so owners must drop the backend after closing.

## Backends

| Backend | Feature | Use |
| --- | --- | --- |
| `MemoryBackend` | always | tests, ephemeral embeds, WASM-safe core |
| `FjallBackend` | `fjall` (default) | durable single-directory instances |

## fjall mapping

- One `fjall::Database` per instance rooted at `data_dir`, guarded by an exclusive `LOCK` file.
- One keyspace per partition; keys are namespaced per tenant inside shared deployments.
- Atomic batches commit across keyspaces in one `WriteBatch`.
- Durability: `PersistMode::SyncAll` on close/drop and on explicit `flush`; group commit later.

## Badger compatibility

On-disk formats are NOT Badger-compatible (different LSM). Migration path is logical:
export from Go (RDF/JSON) and bulk-load into Rust (`datastreamdb-server bulk`). A Badger SST
reader is explicitly out of scope unless a customer needs zero-downtime file migration.

## Crash model

- Torn-write safety comes from the backend journal; the engine adds checksums on value envelopes.
- Recovery tests kill -9 mid-write and assert prefix-consistency (see `datastreamdb-store` TODO).
