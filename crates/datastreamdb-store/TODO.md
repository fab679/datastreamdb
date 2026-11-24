# TODO - datastreamdb-store

Port checklist for `(new) Badger replacement`. Check items only with tests proving the contract.

- [x] M0: crate scaffolded, compiles, docs render.
- [x] M1: `Backend` trait finalized (get/put/delete/scan/batch/range-delete/flush/close).
- [x] M1: fjall backend hardened (LOCK file, atomic cross-keyspace batches, close/drop persist, reopen tests).
- [x] M1: backends feature-gated (`memory` always, `fjall` default).
- [ ] M2: benchmarks vs Badger-equivalent workloads (write batch, point read, prefix scan).
- [ ] M2: real crash test (kill -9 mid-write, assert prefix-consistency) in soak CI.
- [ ] M3: WAL-friendly primitives driven by posting-list access patterns (append/truncate cursors).
- [ ] M-doc: rustdoc + examples; update `docs/PROJECT_MAP.md` status.
- [ ] M-done: `cargo test -p datastreamdb-store`, clippy `-D warnings`, fmt clean.
