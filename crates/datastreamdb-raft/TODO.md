# TODO - datastreamdb-raft

Port checklist for `raftwal, conn`. Check items only with tests proving Go parity.

- [x] M0: crate scaffolded, compiles, docs render.
- [x] M5: Go-compatible WAL storage layout (`wal.rs`, `storage.rs`, `log.rs`, `meta.rs`, `mmap.rs`; 12 WAL tests).
- [x] M5: openraft integration for leader election + log replication (`node.rs`, `log_store.rs`, `state_machine.rs`; index-base mapping at the WAL boundary).
- [x] M5: channel + tonic transports (`network.rs`, `raft_transport.proto`; multi-node consensus + TCP replication tests).
- [x] M5: snapshot build/install/restore (`state_machine.rs` + applier hooks; policy-driven, no compression yet).
- [ ] M6: Jepsen-style membership tests (see `contrib/jepsen` for scenarios).
- [ ] M6: snapshot compression (`conn/snappy.go` -> `snap`).
- [x] M-doc: rustdoc + examples; update `docs/PROJECT_MAP.md` status.
- [x] M-done: `cargo test -p datastreamdb-raft`, clippy `-D warnings`, fmt clean.
