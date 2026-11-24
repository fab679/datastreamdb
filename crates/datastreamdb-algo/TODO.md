# TODO - datastreamdb-algo

Port checklist for `algo`. Check items only with tests proving Go parity.

- [x] M0: crate scaffolded, compiles, docs render.
- [ ] M1: Port `algo/uidlist.go` (intersect/merge/difference) with `uidlist_test.go` vectors.
- [ ] M2: Port `algo/packed.go` bit-packed lists and `algo/heap.go` merge heap.
- [ ] M3: Port `algo/cm-sketch.go` count-min sketch for cache admission.
- [ ] M4: Criterion benches matching `algo/benchmarks`; track parity in CI.
- [ ] M-doc: rustdoc + examples; update `docs/PROJECT_MAP.md` status.
- [ ] M-done: `cargo test -p datastreamdb-algo`, clippy `-D warnings`, fmt clean.
