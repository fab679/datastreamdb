# TESTING

## Suites

| Command | What it runs |
| --- | --- |
| `cargo test --workspace` | Unit + integration tests for all crates |
| `cargo test -p datastreamdb-core -p datastreamdb-ffi -p datastreamdb-store` | Embedded conformance core (`make conformance`) |
| `cargo test --workspace -- --include-ignored` | Includes soak/fuzz-smoke tests (slow) |
| `node --test bindings/node/test/` | TypeScript binding smoke (needs `cargo build -p datastreamdb-node`) |
| `pytest bindings/python/tests` | Python binding smoke (needs maturin develop) |
| `cargo fuzz run <target>` | Structure-aware fuzzers (parsers, codecs) once added |

## Environment

- Fake-S3 suites (`datastreamdb-worker/tests/s3_fake.rs`) bind loopback
  servers. Where an HTTP proxy is exported, keep loopback direct:
  `NO_PROXY=127.0.0.1,localhost cargo test --workspace`.
  (The fake-server suites in `datastreamdb-filestore` bypass proxies
  internally and need no setup.)

## Conventions

- Ported Go vectors keep their Go test names as comments so parity is auditable.
- Snapshot goldens via `insta` for query output; byte-exact fixtures for codecs.
- Multi-instance tests must assert isolation (data, schema, errors) and run under `--test-threads` stress in CI.
- Benchmarks live next to the ported code (`benches/`) and mirror Go `Benchmark*` cases.

## Interop (from M5)

Mixed Go/Rust cluster tests compose a Go zero with a Rust alpha (and reverse) using `compose/` fixtures; see `tests/TODO.md`.
