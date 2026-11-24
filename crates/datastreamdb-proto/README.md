# datastreamdb-proto

Protobuf message definitions (prost) shared by all crates.

- Go source: `dgraph-main/protos` (reference clone at repo root; read-only).
- Status: M5 done (api/pb/badger codegen, raftpb mirrors, wire goldens). See [TODO.md](TODO.md) and [docs/PROJECT_MAP.md](../../docs/PROJECT_MAP.md).
- Embedding: this crate must stay usable from `datastreamdb-core` inside one process with many isolated instances.

## Layout

- `src/` - implementation (see [src/README.md](src/README.md)).
