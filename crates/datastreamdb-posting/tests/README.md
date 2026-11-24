# datastreamdb-posting tests

Embedded engine tests (in-memory backend, no cluster).

- `smoke.rs` - write/commit/read-back, MVCC time travel, abort,
  star-delete with index cleanup, write-write conflicts, index rebuild,
  predicate drop, rollup.
