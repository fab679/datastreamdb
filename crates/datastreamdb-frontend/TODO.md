# TODO - datastreamdb-frontend

Port checklist for `frontend`. Check items only with tests proving Go parity.

- [x] M0: crate scaffolded, compiles, docs render.
- [x] M5: server wiring — DataStreamDB gRPC service, HTTP routes, txn coordinator, alpha assembly (`service.rs`, `http.rs`, `txn.rs`, `convert.rs`, `server.rs`; 8 tests).
- [x] M5: alter/drop paths (single namespace); embedded + standalone alpha from one codebase.
- [x] M6.2: authN/Z pipeline — login RPC, JWT metadata auth, predicate
  authorization (query degrade, mutate/alter deny, guardian bypass),
  rule-cache refresh on finalize, guardian/groot bootstrap, `/login`
  route, initial-schema install at boot.
- [x] M6: namespaces/multi-tenancy (create/drop/list + gRPC RPCs, guardian-gated), admin slice (`tenancy.rs`, `admin.rs`, `POST /admin` with backup/export/restore/health/add/deleteNamespace; 21 tests).
- [x] M6: `/mutate` RDF `{ set/delete }` blocks (Go `ParseDQL` parity; bare bodies stay sets) + `Option<PathBuf>` encryption flag fix (clap rejected the empty default; alpha could not start).
- [ ] M6-full: remaining admin (schema/config/users/groups/drain/shutdown), async backup tasks with task ids, all-namespaces export, JSON `{query,mutations:[{set,delete,cond}]}` mutate envelope.
- [x] M-doc: rustdoc + examples; update `docs/PROJECT_MAP.md` status.
- [x] M-done: `cargo test -p datastreamdb-frontend`, clippy `-D warnings`, fmt clean.
