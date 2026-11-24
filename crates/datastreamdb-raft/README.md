# datastreamdb-raft

Consensus, WAL, membership, and internode transport.

- Go source: `dgraph-main/raftwal, conn` (reference clone at repo root; read-only).
- Status: M5 done (WAL + openraft consensus + transports + snapshots). See [TODO.md](TODO.md) and [docs/PROJECT_MAP.md](../../docs/PROJECT_MAP.md).
- Embedding: this crate must stay usable from `datastreamdb-core` inside one process with many isolated instances.

## Layout

- `src/` - implementation (see [src/README.md](src/README.md)).
