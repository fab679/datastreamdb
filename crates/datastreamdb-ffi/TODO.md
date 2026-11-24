# TODO - datastreamdb-ffi

Port checklist for `(new) embedding ABI`. Check items only with tests proving the contract.

- [x] M0: crate scaffolded, compiles, docs render.
- [x] M1: `datastreamdb.h` checked in (open/close/execute/free/last-error/version + status codes).
- [x] M1: thread-safety tests (multi-handle isolation, thread-local errors) + panic guards.
- [x] M1: reference C consumer (`bindings/c/smoke.c`) passing against the built library.
- [ ] M2: `xtask codegen` generates `datastreamdb.h`; CI diffs it + checks the symbol allowlist.
- [ ] M2: callback (logging) support for hosts.
- [ ] M3: reference Go and Rust `unsafe` client examples.
- [ ] M-doc: rustdoc + examples; update `docs/PROJECT_MAP.md` status.
- [ ] M-done: `cargo test -p datastreamdb-ffi`, clippy `-D warnings`, fmt clean.
