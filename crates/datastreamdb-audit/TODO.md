# TODO - datastreamdb-audit

Port checklist for `audit`. Check items only with tests proving Go parity.

- [x] M0: crate scaffolded, compiles, docs render.
- [x] M6.3: audit events + skip tables + truncation (`audit/audit.go`
  constants; per-instance `Auditor`, no globals).
- [x] M6.3: password masking for login/gRPC + HTTP bodies (GQL variable
  indirection deferred with `/admin`).
- [x] M6.3: sinks for embedded mode — callback, channel, stdout, file
  with size/age rotation (17 tests).
- [x] M6.3: `--audit` superflag parse; alpha wiring (DataStreamDB gRPC
  handlers + HTTP middleware + `--audit` flag); file-sink interop test.
- [ ] M6: audit compression/encryption of rotated files (enc slice);
  zero wiring (its RPCs are near-all skipped).
- [x] M-doc: rustdoc + examples; update `docs/PROJECT_MAP.md` status.
- [x] M-done: `cargo test -p datastreamdb-audit`, clippy `-D warnings`, fmt clean.
