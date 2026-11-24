# datastreamdb-frontend

Request orchestration: HTTP/gRPC serving, auth, admin.

- Go source: `dgraph-main/edgraph` (reference clone at repo root; read-only).
- Status: M5 done (DataStreamDB API, coordinator, HTTP, alpha assembly). See [TODO.md](TODO.md) and [docs/PROJECT_MAP.md](../../docs/PROJECT_MAP.md).
- Embedding: this crate must stay usable from `datastreamdb-core` inside one process with many isolated instances.

## Layout

- `src/` - implementation (see [src/README.md](src/README.md)).
