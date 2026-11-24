# bindings

Host-language bindings over the stable layers: Node-API (TypeScript) and pyo3
(Python) wrap `datastreamdb-core` directly; C and all other languages go through the
C ABI in `datastreamdb-ffi` ([`datastreamdb.h`](../crates/datastreamdb-ffi/datastreamdb.h)).

- [`c/`](c/README.md) — reference C consumer + smoke test.
- [`node/`](node/README.md) — TypeScript (napi-rs).
- [`python/`](python/README.md) — Python (pyo3/maturin).
