# TODO - datastreamdb master plan

Milestones for the full port. Crate-level checklists live in each `crates/*/TODO.md`.

- [x] **M0 - Scaffold**: workspace, 28 crates, node+python bindings, C ABI skeleton, CI, docs. All green.
- [x] **M1 - Embedded core**: hardened `datastreamdb-store` (atomic batches, range deletes, dir locking, close/drop semantics); `datastreamdb-core` typed KV + full envelope (ping/put/get/delete/scan/batch/flush) + graceful close; expanded conformance (isolation, churn, soak, config matrix); checked-in `datastreamdb.h` with C smoke passing; TS + Python bindings built and smoke-tested locally. Publishing deferred by decision (no registry releases yet).
- [x] **M2 - Types & codecs**: `datastreamdb-types` (58 tests), `datastreamdb-codec` (14), `datastreamdb-algo` (23), `datastreamdb-tok` (39), `datastreamdb-chunker` (41: full RDF table + JSON parsing halves + chunker), `datastreamdb-schema` (22: full parse suite) at Go parity with ported test vectors; plus `datastreamdb-lex` (20), `datastreamdb-x` keys, and schema/metadata prost types. Gates green: fmt, clippy `-D warnings`, workspace tests, docs, node/python/C smokes on fresh builds.
- [x] **M3 - Query**: `datastreamdb-dql` parser (289 tests), `datastreamdb-posting` MVCC lists (9 unit + 6 smoke), `datastreamdb-task` local processor (4 smoke), `datastreamdb-query` executor (13 e2e + 10 math groups); DQL read-your-writes in embedded mode. Gates green: fmt, clippy `-D warnings`, workspace tests, docs.
- [x] **M4 - GraphQL**: `datastreamdb-graphql` schema-gen (directive tables, validation, interface expansion, byte-exact `genDgSchema`, completion, SDL), auth (JWT HS/RS, `@auth` RBAC + static eval), resolvers (query/mutation rewriters over `datastreamdb-query`, async-graphql dynamic execution), poll-based subscriptions (56 tests). Deferred: JWK fetch, custom HTTP/lambda execution, remote schemas, admin API, e2e cluster suite, ws transport, trigram/parallel fan-out. Gates green: fmt, clippy `-D warnings`, workspace tests, docs.
- [x] **M5 - Distributed**: `datastreamdb-proto` api/pb/badger codegen + raftpb mirrors + wire goldens (16 tests); `datastreamdb-raft` Go-compat WAL + openraft consensus + channel/tonic transports + snapshots (19 tests); `datastreamdb-worker` groups/applier/service/zero-conn/join (20 tests); `datastreamdb-frontend` DataStreamDB API + coordinator + HTTP + alpha assembly (8 tests); `datastreamdb-server` alpha/zero/datastreamdb binaries + zero services; Go interop suite (7 zero + 4 cluster tests incl. two-alpha replication both directions and restart recovery). Deferred: multi-zero join, predicate moves, backups/restores/exports, CDC, ACL, namespaces, GraphQL admin, bulk/live loaders, Jepsen scenarios. Gates green: fmt, clippy `-D warnings`, workspace tests, docs.
- [ ] **M6 - Ops parity**: ACL, audit, encryption, backup, bulk/live loaders, upgrade tooling, Jepsen scenarios.
  - [x] M6.1 ACL crate (24 tests); M6.2 auth enforcement + login + `datastreamdb acl` CLI; M6.3 audit pipeline (17 tests); M6.4 enc streams/keys/rotation (9 tests); M6.5 manifest + filestore.
  - [x] M6.5b export engine (Go-verbatim RDF/JSON/schema goldens) + binary backup writer + single-group restore + worker RPC drivers (18 new tests).
  - [x] Packaging: LICENSE, docs.rs links, `cargo package --workspace` verifies all 30 members, [publish runbook](docs/PUBLISH.md), node/python/C binding tests green, `cargo xtask check-versions` implemented.
  - [x] M6-loaders: `datastreamdb-loader` (live streaming, bulk + inference, UID assignment; 11 tests) + `bulk`/`live` CLIs (3 roundtrip tests).
  - [x] M6-enc: encryption-key plumbing for backups/exports/loaders (`--encryption-key-file`, Go chain checks; 3 tests).
  - [x] M6-moves: predicate moves (schema-first stream, raft apply, cleanup; 7 tests; zero orchestration deferred).
  - [x] M6-cdc: CDC emitter (Go-JSON events, pending/commit/abort, file + channel sinks, `--cdc`, flush-mark replication; 7 tests; Kafka deferred).
  - [x] M6-backup-fanout: backup coordinator (Go `ProcessBackupRequest` shape; per-group legs, unified manifest; 1 test).
  - [x] M6-s3: S3/MinIO destinations (SigV4 client with openssl-oracle vector, multipart uploads, backup/export/restore + loader inputs; 15 tests; HTTPS/live-MinIO deferred).
  - [x] M6-multizero: zero join (TCP raft transport, stable IDs, leader seating, lease resync; 3-zero failover test; 2 tests).
  - [ ] Remaining: tenancy/admin, Jepsen.

## Standing rules

- [ ] Keep `docs/PROJECT_MAP.md` status column current on every port PR.
- [ ] Keep workspace dependency pins on latest releases (`cargo xtask check-versions` in CI).
- [ ] Every new `unsafe` block needs a safety comment + Miri test.
- [ ] Keep every source file under 1000 lines; split growing modules early.
- [ ] Bench regressions over 10% fail CI once M2 benches exist.
