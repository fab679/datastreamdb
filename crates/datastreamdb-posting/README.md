# datastreamdb-posting

Posting lists: MVCC lists over the storage backend.

- Go source: `dgraph-main/posting` (reference clone at repo root; read-only).
- Status: M3 ported (Milestone 3). See [TODO.md](TODO.md) and [docs/PROJECT_MAP.md](../../docs/PROJECT_MAP.md).
- Embedding: every engine instance owns a `PostingEngine`; nothing here is
  global, so many isolated instances share one process.

## Layout

- `src/` - implementation (see [src/README.md](src/README.md)).
- `tests/` - embedded smoke suite (see [tests/README.md](tests/README.md)).

## Deltas from Go

- Version chains stored in each value replace Badger key versions.
- Commits serialize on a mutex and run inline (no Raft apply loop).
- Rollup is synchronous (`rollup_key` / `rollup_pending`); no background loop.
- The oracle assigns timestamps locally with local SSI conflict checks.
- Fingerprints use FNV-1a (Go: FarmHash64); the format is new either way.
- The list cache is a bounded LRU (Go: ristretto with size costs).
