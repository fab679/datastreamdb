# TODO - datastreamdb-proto

Port checklist for `protos`. Check items only with tests proving Go parity.

- [x] M0: crate scaffolded, compiles, docs render.
- [x] M5: `api.proto` + `pb.proto` + `badger.proto` codegen via protoc + tonic-prost-build (clients/servers + compat re-exports).
- [x] M5: hand-authored etcd `raftpb` mirrors (prost cannot compile proto2) with wire-golden tests.
- [x] M5: wire compatibility tests against Go vectors (16 tests).
- [ ] M6: version the schema; document upgrade rules (see `upgrade/` package).
- [x] M-doc: rustdoc + examples; update `docs/PROJECT_MAP.md` status.
- [x] M-done: `cargo test -p datastreamdb-proto`, clippy `-D warnings`, fmt clean.

## Session notes (M3 task/query messages)
- [x] M3: `PostingList`, `BackupPostingList`, `Facets(List)`, `TxnStatus`,
  `OracleDelta`, `TaskQuery/TaskResult/TaskValue/SrcFunction/FilterTree/
  FilterFunction/ValueList/LangList/SortMessage/SortResult` prost types.

## Session notes (M2 schema foundations)
- [x] M2: hand-written prost `SchemaUpdate`, `Directive`, `TypeUpdate`,
  `VectorIndexSpec`, `OptionPair`, `Metadata`, `HintType` (wire-compatible
  field numbers, roundtrip tests).
- [ ] M4: split `api` vs `pb` modules when `api` prost types land.
