# TODO - datastreamdb-core

Port checklist for `(new) request facade over all crates`. Check items only with tests proving the contract.

- [x] M0: crate scaffolded, compiles, docs render.
- [x] M1: `Engine` handle registry (`Arc<Instance>`, isolated, `Send + Sync`, panic-free FFI boundary).
- [x] M1: `InstanceConfig` (data dir with lock, in-memory, tenant id).
- [x] M1: open path over `datastreamdb-store`; typed KV + full envelope (ping/put/get/delete/scan/batch/flush).
- [x] M1: graceful close (explicit `flush`/`close`, `Drop` flushes, lock released on drop).
- [x] M1: conformance suite (isolation, config matrix, churn, ignored soak).
- [ ] M3: request pipeline (parse `datastreamdb-dql`/`datastreamdb-graphql` -> plan -> execute -> encode JSON).
- [ ] M5: lifecycle semantics from `dgraph/cmd/alpha/run.go` (init sequence, clog, crash recovery).
- [ ] M-doc: rustdoc + examples; update `docs/PROJECT_MAP.md` status.
- [ ] M-done: `cargo test -p datastreamdb-core`, clippy `-D warnings`, fmt clean.
