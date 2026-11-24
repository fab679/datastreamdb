# datastreamdb-audit

Audit logging pipeline and filters.

- Go source: `dgraph-main/audit` (reference clone at repo root; read-only).
- Status: M6.3 done (events, masking, sinks, alpha wiring; 17 tests + file-sink interop). See [TODO.md](TODO.md) and [docs/PROJECT_MAP.md](../../docs/PROJECT_MAP.md).
- Embedding: this crate must stay usable from `datastreamdb-core` inside one process with many isolated instances.

## Layout

- `src/` - implementation (see [src/README.md](src/README.md)).
