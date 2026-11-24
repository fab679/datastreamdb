# datastreamdb-worker

Tablet serving: predicates, mutations, backup, CDC.

- Go source: `dgraph-main/worker` (reference clone at repo root; read-only).
- Status: M5 done (group applier, Worker/zero/join surface). See [TODO.md](TODO.md) and [docs/PROJECT_MAP.md](../../docs/PROJECT_MAP.md).
- Embedding: this crate must stay usable from `datastreamdb-core` inside one process with many isolated instances.

## Layout

- `src/` - implementation (see [src/README.md](src/README.md)).
