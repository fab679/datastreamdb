# datastreamdb-server

Standalone binaries: alpha, zero, bulk, live.

- Go source: `dgraph-main/dgraph/cmd` (reference clone at repo root; read-only).
- Status: M5 done (alpha/zero/datastreamdb binaries, zero cluster services, interop suite). See [TODO.md](TODO.md) and [docs/PROJECT_MAP.md](../../docs/PROJECT_MAP.md).
- Embedding: this crate must stay usable from `datastreamdb-core` inside one process with many isolated instances.

## Layout

- `src/` - implementation (see [src/README.md](src/README.md)).
