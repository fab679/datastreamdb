# datastreamdb-dql tests

Ported Go test tables (integration tests).

- `parser_*.rs` (8 targets) - all of `parser_test.go` (289 tests),
  split by area to honor the 1000-line file rule.
- `common/mod.rs` - shared helpers (`parse_str`, `parse_err`,
  `child_attrs`, `math_debug`, `parse_nquads`, `VarArrayCase`),
  included by each `parser_*.rs` via `#[path]`.
- `upsert_tests.rs` - `upsert_test.go` + `dql_test.go`.
- Lexer tests (`state_test.go`) live in `src/state.rs`, like Go's
  in-package tests.
