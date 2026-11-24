# datastreamdb-task

Local query task processor (single-group worker/task port).

- Go sources: `dgraph-main/worker/task.go`, `dgraph-main/worker/tokens.go`,
  `dgraph-main/worker/match.go`, `dgraph-main/worker/stringfilter.go`,
  `dgraph-main/worker/trigram.go`, `dgraph-main/task` (read-only reference).
- Status: M3 ported (Milestone 3). See [TODO.md](TODO.md) and [docs/PROJECT_MAP.md](../../docs/PROJECT_MAP.md).
- Embedding: tasks run against one `PostingEngine`; no network/tablet code.

## Layout

- `src/` - implementation (see [src/README.md](src/README.md)).
- `tests/` - task smoke suite (see [tests/README.md](tests/README.md)).

## Deltas from Go

- Sequential row processing (Go fans out over goroutines).
- Regex/match fall back to predicate scans (trigram extraction deferred).
- `similar_to` reports unsupported (vector milestone).
- `uid_read_first` and count scans use i64 (Go `int`).
