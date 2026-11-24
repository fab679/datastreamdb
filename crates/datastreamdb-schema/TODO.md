# TODO - datastreamdb-schema

Port checklist for `schema`. Check items only with tests proving Go parity.

- [x] M0: crate scaffolded, compiles, docs render.
- [x] M2: schema model as prost types in `datastreamdb-proto` (`SchemaUpdate`,
  `TypeUpdate`, `VectorIndexSpec`, `OptionPair`, `Directive`), re-exported.
- [x] M2: `parse.go` ported with all `parse_test.go` vectors (22 tests).
- [x] M2: `state.go` lexer ported (`lexer.rs`).
- [x] M2: in-memory `schema.go` state ported as instance-based `SchemaState`
  (no globals, for embedding).
- [ ] M4: alter-path compatibility rules (`CheckAndModify` callers).
- [ ] M5: disk sync (`Init`, `Load`, timestamped `Delete`) on the store layer.
- [x] M-doc: rustdoc + examples; update `docs/PROJECT_MAP.md` status.
- [x] M-done: `cargo test -p datastreamdb-schema` (22 passed), clippy `-D warnings`,
  fmt clean.
