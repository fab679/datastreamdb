# TODO - datastreamdb-task

Port checklist for `task`. Check items only with tests proving Go parity.

- [x] M0: crate scaffolded, compiles, docs render.
- [x] M3: `task/conversion.go` value constructors (`context.rs`).
- [x] M3: task/query prost messages (in `datastreamdb-proto`).
- [x] M3: local task processor (`worker/task.go` local half):
  `parseSrcFn`, value/uid handlers, scalar/regex/match/compare/geo/string/has
  filters, facet trees.
- [x] M3: task smoke suite (has/eq/uid/filter/count/regexp).
- [ ] M4: trigram candidate extraction (`cindex.RegexQuery` port).
- [ ] M4: parallel row fan-out (Go uses goroutines; we run sequentially).
- [ ] M-vector: `similar_to` search.
- [x] M-doc: rustdoc + examples; update `docs/PROJECT_MAP.md` status.
- [x] M-done: `cargo test -p datastreamdb-task`, clippy `-D warnings`, fmt clean.
