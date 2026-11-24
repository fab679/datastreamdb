# TODO - datastreamdb-lex

Port checklist for `lex`. Check items only with tests proving Go parity.

- [x] M0: crate scaffolded, compiles, docs render.
- [x] M2: `lexer.go` ported (`Lexer`, `Item`, `ItemIterator`, `StateFn`,
  quoted strings, accept/ignore helpers) with unit tests for widths, line/col,
  save/restore/peek, validation, and error paths.
- [x] M2: `iri.go` ported (`iri_ref`, `has_uchars`, `has_xchars`) with all
  `iri_test.go` vectors.
- [x] M2: `logos` token helpers dropped for this crate: the Go original is a
  hand-rolled state-function lexer, so an exact port needs no `logos`
  dependency (`datastreamdb-dql` keeps the workspace pin).
- [ ] M3: Fuzz the IRI lexer (cargo-fuzz) against the Go behavior for parity.
- [x] M-doc: rustdoc + examples; update `docs/PROJECT_MAP.md` status.
- [x] M-done: `cargo test -p datastreamdb-lex` (20 passed), clippy `-D warnings`,
  fmt clean.
