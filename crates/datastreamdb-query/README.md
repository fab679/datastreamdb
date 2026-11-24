# datastreamdb-query

Embedded DQL query executor and mutation applier (port of the upstream Go `query` package, executor half).

- Go source: `dgraph-main/query` (reference clone at repo root; read-only).
- Status: M3 ported (Milestone 3). See [TODO.md](TODO.md) and [docs/PROJECT_MAP.md](../../docs/PROJECT_MAP.md).
- Embedding: `QueryEngine::open(store)` builds one isolated instance over any
  `datastreamdb-store` backend; many instances coexist per process with no global state.

## Layout

- `src/` - implementation (see [src/README.md](src/README.md)).
- `tests/` - end-to-end + math suites (see [tests/README.md](tests/README.md)).

## Deltas from Go

- Sequential execution (Go fans subgraph work over goroutines; M4 may parallelize).
- Regex/match fall back to predicate scans via `datastreamdb-task` (trigram extraction deferred).
- JSON output uses a safe `Vec` tree matching Go's byte output (unsafe arena not ported).
- UID allocation from a store-backed `query-meta/max-uid` counter (no Zero leases).
- Math error strings match Go exactly, including the `"fun ceil"` typo.
- Deferred with `Unsupported`/TODO markers: `similar_to`/vector, GraphQL output
  (`outputnode_graphql.go`), upsert execution, xid leasing.
