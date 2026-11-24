# datastreamdb-loader

Bulk and live data loaders: RDF/JSON ingest into engines.

- Go source: `dgraph-main/dgraph/cmd/bulk`, `dgraph-main/dgraph/cmd/live` (reference clone at repo root; read-only).
- Status: M6-loaders done (live, bulk, UID assignment; 10 tests; remote gRPC + sharded map/reduce deferred). See [TODO.md](TODO.md) and [docs/PROJECT_MAP.md](../../docs/PROJECT_MAP.md).
- Embedding: the crate is a library first (`live::load_live`, `bulk::load_bulk` over an engine); the `bulk`/`live` binaries are thin CLI wrappers.

## Layout

- `src/` - implementation (see [src/README.md](src/README.md)).
