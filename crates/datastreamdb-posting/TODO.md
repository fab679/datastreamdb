# TODO - datastreamdb-posting

Port checklist for `posting`. Check items only with tests proving Go parity.

- [x] M0: crate scaffolded, compiles, docs render.
- [x] M3: list model (`list.go`, `lists.go`) onto `datastreamdb-store` traits.
- [x] M3: MVCC layer (`mvcc.go`, `oracle.go`) incl. local conflict detection.
- [x] M3: index mutation path (`index.go` write half) with `datastreamdb-tok` tokenizers.
- [x] M3: index rebuild + drop flows (`rebuild.rs`, single-phase).
- [x] M3: size accounting (`size.go`) and stats (`stats.go`).
- [x] M3: embedded smoke suite (commit/read/rollup/rebuild/drop/conflicts).
- [ ] M4: split-list rollup round-trip + facet tests.
- [ ] M5: port `list_test.go` / `mvcc_test.go` / `index_test.go` unit-runnable cases.
- [ ] M-vector: HNSW/vector branches (`viTxn`, `RunWithoutTemp`, dead nodes).
- [x] M-doc: rustdoc + examples; update `docs/PROJECT_MAP.md` status.
- [x] M-done: `cargo test -p datastreamdb-posting`, clippy `-D warnings`, fmt clean.

## Session notes (M5 distribution support)
- [x] M5: explicit-timestamp transactions (`begin_at`/`commit_at`), oracle
  conflict export/import + delta watermarks (`fetch_max` for concurrent
  raft/direct writers), leased UID ranges, type deletion; predefined-name
  checks never panic.
