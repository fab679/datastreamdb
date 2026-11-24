# datastreamdb-dql test helpers

Shared helpers for the `parser_*.rs` integration-test targets
(`parse_str`, `parse_err`, `child_attrs`, `math_debug`, `parse_nquads`,
`VarArrayCase`). Each target includes `mod.rs` via `#[path]` and imports
only the helpers it uses; the module carries `#![allow(dead_code)]`
because each target uses a different subset.
