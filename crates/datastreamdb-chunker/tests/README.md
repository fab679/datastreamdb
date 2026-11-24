# datastreamdb-chunker tests

Ported Go test tables (integration tests).

- `rdf_tests.rs` - full `TestLex` table + `parse_rdfs` tests.
- `json_tests.rs` - pure-parsing halves of `json_parser_test.go` (cluster
  `verify()` halves need a live cluster; see crate `TODO.md`).
- `chunk_tests.rs` - `chunk_test.go` ports (`TestFileReader` waits for M5).
