# datastreamdb-graphql

GraphQL layer: schema-gen, auth, resolve, subscribe.

- Go source: `dgraph-main/graphql` (reference clone at repo root; read-only).
- Status: **M4 done** — schema-gen, auth, resolvers, poll-based subscriptions; 56 tests green. See [TODO.md](TODO.md) and [docs/PROJECT_MAP.md](../../docs/PROJECT_MAP.md).
- Embedding: this crate must stay usable from `datastreamdb-core` inside one process with many isolated instances.

## Layout

- `src/` - implementation (see [src/README.md](src/README.md)).
