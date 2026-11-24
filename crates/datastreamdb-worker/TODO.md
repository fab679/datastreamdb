# TODO - datastreamdb-worker

Port checklist for `worker`. Check items only with tests proving Go parity.

- [x] M0: crate scaffolded, compiles, docs render.
- [x] M5: tablet/predicate ownership model + config (`groups.rs`, `config.rs`).
- [x] M5: mutation serving path — stage/finalize applier, Worker service with leader forwarding, zero conn, group join (`applier.rs`, `service.rs`, `zero.rs`, `raft_service.rs`; 20 tests).
- [x] M6.2: ACL rule cache (`acl_cache.rs`) + finalize hook notifying
  rule-data commits for refresh.
- [x] M6.5b: backup/restore/export RPC drivers (`backup.rs`):
  local-destination backup with manifest chain, gzip export trio,
  manifest-chain restore with drop replay (4 unit + 1 RPC tests).
- [x] M6-enc: encryption-key plumbing (`Options.encryption_key`,
  `--encryption-key-file`): encrypted backup chains with Go chain
  checks, encrypted export files, keyed restore (2 tests).
- [x] M6-moves: predicate moves (`predicate_move.rs` + applier
  `kv`/`clean_predicate` arms): schema-first rolled-up stream,
  raft-proposed apply, tablet cleanup (5 unit + 2 RPC tests;
  zero orchestration deferred).
- [x] M6-cdc: change-data-capture emitter (`cdc.rs` + applier hooks):
  Go-JSON events, pending/commit/abort, file + channel sinks,
  `--cdc` flag, flush-mark replication (7 tests; Kafka deferred).
- [x] M6-backup-fanout: coordinator (`process_backup_request`) +
  data-only group RPC (Go `ProcessBackupRequest`/`BackupGroup`
  shape): per-group legs, unified manifest, remote dial (1 test).
- [x] M6-admin-ops: cluster wrappers (`backup/export/restore_cluster`,
  `delete_namespace_cluster` fan-out with merged responses).
- [x] M6-s3: S3 destinations (`DestStore`): backup/export/restore
  over SigV4 object storage with request credentials (2 fake-S3
  roundtrip tests; HTTPS needs a TLS build). Backup drivers split
  into `backup/{mod,driver,export,restore,dest}.rs` (file-size rule).
- [x] M-doc: rustdoc + examples; update `docs/PROJECT_MAP.md` status.
- [x] M-done: `cargo test -p datastreamdb-worker`, clippy `-D warnings`, fmt clean.
