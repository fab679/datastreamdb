# datastreamdb-backup

Online backup orchestration and restore.

- Go source: `dgraph-main/backup` (reference clone at repo root; read-only).
- Status: M6.5b done (manifest contract, binary writer, single-group restore, RDF/JSON/schema export; 18 tests). See [TODO.md](TODO.md) and [docs/PROJECT_MAP.md](../../docs/PROJECT_MAP.md).
- Embedding: this crate must stay usable from `datastreamdb-core` inside one process with many isolated instances.

## Layout

- `src/` - implementation (see [src/README.md](src/README.md)).
