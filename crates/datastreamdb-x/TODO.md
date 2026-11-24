# TODO - datastreamdb-x

Port checklist for `x`. Check items only with tests proving Go parity.

- [x] M0: crate scaffolded, compiles, docs render.
- [ ] M1: Port error taxonomy used across packages into `Error`/`Result` (`x` package equivalents).
- [ ] M2: Port config helpers: env parsing, size parsing (e.g. `--cache` strings), superflag parsing.
- [ ] M3: Expose a process-wide Prometheus registry with per-instance label support.
- [ ] M4: tracing setup helpers (fmt/json layers) shared by server binaries and embedding hosts.
- [ ] M5: Port rate-limit / throttle primitives used by bulk loader and live loader.
- [ ] M6: Audit every helper for `Send + Sync` so all crates stay embed-safe.
- [ ] M-doc: rustdoc + examples; update `docs/PROJECT_MAP.md` status.
- [ ] M-done: `cargo test -p datastreamdb-x`, clippy `-D warnings`, fmt clean.

## Session notes (M2 chunker/schema foundations)
- [x] M2: key helpers ported (`keys.rs`): `STAR`, `FACET_DELIMITER`,
  `ROOT_NAMESPACE`, `PredicateLang`, namespace attr/parse fns, pre-defined
  predicate/type checks, with unit tests.
