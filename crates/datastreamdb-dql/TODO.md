# TODO - datastreamdb-dql

Port checklist for `dql`. Check items only with tests proving Go parity.

- [x] M0: crate scaffolded, compiles, docs render.
- [x] M3: `dql/parser.go` + `parser_mutation.go`: full query/mutation grammar
  with `parser_test.go` vectors (289 tests across 8 area files + `common/`).
- [x] M3: `dql/state.go` lexer states and error positions.
- [x] M3: math expression support (`dql/math.go`).
- [ ] M4: AST -> `datastreamdb-task` lowering; keep AST serializable for bindings.
- [ ] M5: Re-enable fuzz target (`parser_fuzz_test.go` equivalent) under cargo-fuzz.
- [x] M-doc: rustdoc + examples; update `docs/PROJECT_MAP.md` status.
- [x] M-done: `cargo test -p datastreamdb-dql`, clippy `-D warnings`, fmt clean.
