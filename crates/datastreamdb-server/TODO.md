# TODO - datastreamdb-server

Port checklist for `dgraph/cmd`. Check items only with tests proving Go parity.

- [x] M0: crate scaffolded, compiles, docs render.
- [x] M5: alpha/zero/datastreamdb binaries with flags + serve paths (`bin/`).
- [x] M5: zero state/oracle/leases/service/server (`zero_*.rs`; single-zero clusters; 7 zero tests + 4 cluster interop tests incl. two-alpha replication and restart recovery).
- [x] M6-loaders: `bulk`/`live` CLIs over `datastreamdb-loader`
  (embedded fjall dirs, `:memory:`, schema/batch flags; 3 CLI
  roundtrip tests; remote `--alpha` deferred).
- [x] M6-multizero: zero join (`zero_raft.rs` + `--join`): TCP
  raft transport, stable voter IDs, leader-seated membership,
  lease resync on leadership (Go `updateLeases` parity), 3-zero
  join/replication/failover test (2 tests).
- [ ] M6: namespaces, GraphQL admin (backups/restores/exports live
  in `datastreamdb-worker::backup`; ACL in `acl_admin.rs`).
- [x] M-doc: rustdoc + examples; update `docs/PROJECT_MAP.md` status.
- [x] M-done: `cargo test -p datastreamdb-server`, clippy `-D warnings`, fmt clean.
