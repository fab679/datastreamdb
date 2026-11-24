# DataStreamDB

DataStreamDB is an embeddable graph database in Rust, ported from the
original Go implementation ([dgraph-io/dgraph](https://github.com/dgraph-io/dgraph))
and designed from day one to be **embedded**: any application can spin up
**many isolated graph-database instances in one process**, from Rust directly,
from TypeScript via Node-API, from Python via pyo3, or from any language
through a stable C ABI.

Reference Go clone lives in [`dgraph-main/`](dgraph-main/) (read-only; never edit it).

## Layout

| Path | Contents |
| --- | --- |
| `crates/` | One crate per DataStreamDB subsystem, plus new embedding crates |
| `crates/datastreamdb-core` | Embeddable engine facade: open, query, mutate, close |
| `crates/datastreamdb-store` | Pluggable LSM storage (memory + fjall backends) |
| `crates/datastreamdb-ffi` | Stable C ABI (`datastreamdb.h` contract) |
| `crates/datastreamdb-server` | Standalone `alpha`, `zero`, `bulk`, `live` binaries |
| `bindings/node` | TypeScript bindings (napi-rs) |
| `bindings/python` | Python bindings (pyo3/maturin) |
| `docs/` | [Port map](docs/PROJECT_MAP.md), [embedding model](docs/EMBEDDING.md), [storage](docs/STORAGE.md), [publishing](docs/PUBLISH.md) |
| `tools/xtask` | Codegen and release tasks (`cargo xtask ...`) |
| `tests/` | Workspace-level integration plans |

Every directory carries a `README.md` (what lives here) and a `TODO.md` (what is next).

## Quickstart

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Open two isolated instances in Rust:

```rust
let a = datastreamdb_core::Instance::open("{\"data_dir\": null}".parse()?)?;
let b = datastreamdb_core::Instance::open("{\"data_dir\": \"/tmp/instance-b\"}".parse()?)?;
a.put("name", b"Ada")?;
assert_eq!(a.get("name")?.as_deref(), Some(b"Ada".as_slice()));
assert_eq!(b.get("name")?, None); // isolated
```

TypeScript and Python quickstarts live in [`bindings/node`](bindings/node/README.md) and
[`bindings/python`](bindings/python/README.md).

## Engineering rules

- One crate per Go package (see [`docs/PROJECT_MAP.md`](docs/PROJECT_MAP.md)); port file-by-file with the Go tests as vectors.
- No global mutable state anywhere below `datastreamdb-core`: embedding N instances must be safe.
- `unsafe` only in `datastreamdb-ffi` (FFI boundary) with Miri coverage; everything else is safe Rust.
- Latest stable dependency versions, pinned in the workspace `Cargo.toml`; `cargo xtask check-versions` re-verifies.
- Every PR: `cargo fmt --check`, `clippy -D warnings`, tests, and updated crate `TODO.md` status.

See [`ARCHITECTURE.md`](ARCHITECTURE.md), [`CONTRIBUTING.md`](CONTRIBUTING.md), [`TESTING.md`](TESTING.md).
