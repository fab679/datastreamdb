# PROJECT_MAP - Go package to Rust crate

Reference: `dgraph-main/` at repo root (module `github.com/dgraph-io/dgraph/v25`).

| Go package | Rust crate | Status |
| --- | --- | --- |
| `x` | `datastreamdb-x` | **M2 done** (keys/namespaces/predicates; rest planned) |
| `lex` | `datastreamdb-lex` | **M2 done** (lexer engine + IRI, 20 tests) |
| `types` | `datastreamdb-types` | **M2 done** (58 tests; 2 geo divergences characterized) |
| `codec` | `datastreamdb-codec` | **M2 done** (14 tests) |
| `algo` | `datastreamdb-algo` | **M2 done** (23 tests) |
| `tok` | `datastreamdb-tok` | **M2 done** (39 tests; HNSW engine M5) |
| `chunker` | `datastreamdb-chunker` | **M2 done** (41 tests; FileReader/cluster tests M5) |
| `schema` | `datastreamdb-schema` | **M2 done** (22 tests; disk sync M5) |
| `dql` | `datastreamdb-dql` | **M3 done** (AST + lexer + query/mutation/upsert parser, 289 tests) |
| `graphql/...` | `datastreamdb-graphql` | **M4 done** (schema-gen, auth, resolvers, subscriptions; 56 tests; ws deferred) + **M6-slice** admin API (health/backup/export/restore/namespaces; 4 tests; rest of admin/e2e deferred) |
| `posting` | `datastreamdb-posting` | **M3 done** (MVCC lists, oracle, writer, index, rollup; 9 unit + 6 smoke) + M5 explicit-ts txns/delta watermarks |
| `xidmap` | `datastreamdb-xidmap` | scaffold + mem map |
| `task` | `datastreamdb-task` | **M3 done** (local task processor, 4 smoke; trigram/parallel deferred to M5) |
| `query` | `datastreamdb-query` | **M3 done** (executor, vars, groupby, recurse, shortest, JSON/RDF, mutations; 13 e2e + 10 math) |
| `worker` | `datastreamdb-worker` | **M5 done** (groups/applier, Worker service, zero conn, join; 20 tests) + **M6.5b** backup/restore/export RPC drivers, local destinations (5 tests) + **M6-moves** predicate moves via raft (7 tests) + **M6-cdc** emitter, file/channel sinks, `--cdc` (7 tests) + **M6-backup-fanout** coordinator, multi-group legs, unified manifest (1 test; S3/Kafka/zero-orchestration deferred) |
| `frontend` | `datastreamdb-frontend` | **M5 done** (DataStreamDB API, coordinator, HTTP, alpha assembly; 8 tests) + **M6** auth + tenancy (create/drop/list, gRPC RPCs) + admin slice (`POST /admin`: backup/export/restore/health/namespaces; 21 tests; rest of admin deferred) |
| `raftwal`, `conn` | `datastreamdb-raft` | **M5 done** (Go-compat WAL, openraft consensus, channel+tonic transports, snapshots; 19 tests) |
| `protos` | `datastreamdb-proto` | **M5 done** (api/pb/badger codegen, raftpb mirrors, wire goldens; 16 tests) |
| `acl` | `datastreamdb-acl` | **M6.1 done** (entities, keys/secrets, JWT mint+validate all algs but ES512, NQuad builders; 24 tests; login/enforce/CLI next) |
| `audit` | `datastreamdb-audit` | **M6.3 done** (events, masking, sinks, alpha gRPC+HTTP wiring, `--audit` flag; 17 tests; compression/encryption deferred to enc) |
| `enc` | `datastreamdb-enc` | **M6.4 done** (AES-CTR streams, keys, rotation; 9 tests; Go fixtures; backup/audit wiring next) |
| `backup` | `datastreamdb-backup` | **M6.5b done** (manifest contract, binary Snappy writer, single-group restore + drops, RDF/JSON/schema export with Go goldens; 18 tests; S3/multi-group/map-reduce deferred) |
| `filestore` | `datastreamdb-filestore` | **M6-s3 done** (trait + local + gzip/enc chunk readers + SigV4 S3/MinIO client with multipart uploads; 12 tests; TLS + live-MinIO deferred) |
| `dgraph/cmd` | `datastreamdb-server` | **M5 done** (alpha/zero/datastreamdb binaries, zero services, 7 zero + 4 cluster interop tests) + **M6-loaders** `bulk`/`live` CLIs (3 roundtrip tests; remote/sharded deferred) + **M6-multizero** join/replication/failover (2 tests) |
| `dgraph/cmd/bulk`, `dgraph/cmd/live` | `datastreamdb-loader` | **M6-loaders done** (live streaming + batch commits, bulk + Go-parity schema inference, UID seeding/`BumpTo`/oracle advance, encrypted inputs; 11 tests; remote gRPC + sharded map/reduce deferred) |
| `worker` enc plumbing | `datastreamdb-worker::backup` + `alpha` | **M6-enc done** (`--encryption-key-file` → encrypted backup chains with Go checks, encrypted exports, keyed restore; 2 tests; encrypted WAL deferred) |
| `badger` (dep) | `datastreamdb-store` | **M1 done** (batches, range delete, locking, close) |
| (new) | `datastreamdb-core` | **0.2.0** (typed KV + DQL relationships/patterns + GraphQL queries/mutations/poll subscriptions + schema persistence) |
| (new) | `datastreamdb-ffi` | **M1 done** (`datastreamdb.h`, isolation tests, C smoke) |
| (new) | `bindings/node`, `bindings/python` | **M1 done** (built + smoke-tested, publish deferred) |
| `datastreamdbapi` | surfaced via `datastreamdb-core` API | planned |
| `datastreamdbtest`, `testutil` | `tests/`, `tools/xtask` | planned |
| `systest` | `tests/` conformance + interop | planned |
| `upgrade` | `tools/xtask` + `datastreamdb-proto` versioning | planned |
| `buildvars` | `tools/xtask codegen` | planned |

Status legend: `scaffold` (compiles, port pending) -> `porting` -> `parity` (Go vectors green) -> `done`.
Update this table on every port PR.
