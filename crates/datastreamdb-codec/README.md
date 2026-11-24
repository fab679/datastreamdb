# datastreamdb-codec

Posting-list codecs: varint packing, group varint, compression.

- Go source: `dgraph-main/codec` (reference clone at repo root; read-only).
- Status: scaffold (Milestone 0). See [TODO.md](TODO.md) and [docs/PROJECT_MAP.md](../../docs/PROJECT_MAP.md).
- Embedding: this crate must stay usable from `datastreamdb-core` inside one process with many isolated instances.

## Layout

- `src/` - implementation (see [src/README.md](src/README.md)).
