# datastreamdb-acl

Access control: JWT auth, HMAC secrets, rules.

- Go source: `dgraph-main/acl` (reference clone at repo root; read-only).
- Status: M6.1 done (entities, keys/secrets, JWT mint+validate, NQuad builders; 24 tests). See [TODO.md](TODO.md) and [docs/PROJECT_MAP.md](../../docs/PROJECT_MAP.md).
- Embedding: this crate must stay usable from `datastreamdb-core` inside one process with many isolated instances.

## Layout

- `src/` - implementation (see [src/README.md](src/README.md)).
