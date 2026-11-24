# CONTRIBUTING

## Workflow

1. Pick a crate and read its `README.md` + `TODO.md` plus the matching Go package in `dgraph-main/` (read-only reference).
2. Port file-by-file: one Rust module per Go file, same item order where practical.
3. Bring the Go tests over as test vectors first (they must fail), then implement.
4. Update the crate `TODO.md`, `docs/PROJECT_MAP.md` status, and add a CHANGELOG-style note in the PR.

## Standards

- `cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings` must pass.
- Public items need rustdoc; `#![warn(missing_docs)]` is on for every crate.
- Keep every source file under 1000 lines; split growing modules early (one module per ported Go file already pushes this way).
- No `unwrap`/`expect` on host- or user-controlled paths; return `thiserror` errors.
- No global mutable state below `datastreamdb-core` (multi-instance requirement); if you think you need one, open a design discussion first.
- `unsafe` only in `datastreamdb-ffi`; every block needs a `// SAFETY:` comment and a Miri test.
- Property/golden tests preferred for codecs, parsers, and type conversions.

## Reviews

Port PRs should link the exact Go source commit + file, show test-vector parity evidence, and call out intentional behavior deltas in the crate README.
