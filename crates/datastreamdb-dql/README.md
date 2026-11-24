# datastreamdb-dql

DQL (GraphQL+-) lexer, parser, and AST.

- Go source: `dgraph-main/dql` (reference clone at repo root; read-only).
- Status: M3 ported (Milestone 3). See [TODO.md](TODO.md) and [docs/PROJECT_MAP.md](../../docs/PROJECT_MAP.md).
- Embedding: this crate must stay usable from `datastreamdb-core` inside one process with many isolated instances.

## Layout

- `src/` - implementation (see [src/README.md](src/README.md)).
