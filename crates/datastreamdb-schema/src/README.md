# datastreamdb-schema/src

Implementation of `datastreamdb-schema`. Public API lives in `lib.rs`.

- `lib.rs` - re-exports, `SchemaError`.
- `lexer.rs` - schema text lexer (port of Go's `schema/state.go`).
- `parse.rs` - schema parser (port of `parse.go`).
- `state.rs` - `SchemaState` + initial data (in-memory port of `schema.go`).
