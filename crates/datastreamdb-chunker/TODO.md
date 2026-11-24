# TODO - datastreamdb-chunker

Port checklist for `chunker`. Check items only with tests proving Go parity.

- [x] M0: crate scaffolded, compiles, docs render.
- [x] M2: `chunk.go` ported (`Chunker`, RDF/JSON chunking, `DataFormat`,
  `IsJSONData`); `FileReader`/`StreamReader` deferred to M5 (need gzip+enc).
- [x] M2: `rdf_state.go` + `rdf_parser.go` ported with the full
  `rdf_parser_test.go` table (~100 cases) + `data.rdf` fixture.
- [x] M2: `json_parser.go` ported with the pure-parsing halves of
  `json_parser_test.go` (25 tests; cluster `verify()` halves need M5+).
- [ ] M4: wire `api` prost types (`NQuad`/`Value`/`Facet`) + `From` conversions.
- [ ] M4: zero-copy path: mmap input and stream chunks to bulk loader.
- [ ] M5: `FileReader`/`StreamReader` (gzip detection + `enc` decryption).
- [ ] M5: cluster-backed JSON tests (Go `TestMain` spins alpha+zero).
- [x] M-doc: rustdoc + examples; update `docs/PROJECT_MAP.md` status.
- [x] M-done: `cargo test -p datastreamdb-chunker` (41 passed), clippy `-D warnings`,
  fmt clean.
