# TODO - datastreamdb-enc

Port checklist for `enc`. Check items only with tests proving Go parity.

- [x] M0: crate scaffolded, compiles, docs render.
- [x] M6.4: AES-CTR stream encryption with IV prefix (`enc/util.go`
  parity, including the short-IV error) + key loading/validation with
  Go fixtures (9 tests). Note: Go uses AES-CTR here, not GCM — the
  AES-GCM scaffold was replaced for wire compat.
- [x] M6.4: file re-encryption for key rotation (`rotate_file`,
  atomic temp+rename; encrypt/decrypt/re-key).
- [ ] M6: wire encrypted backup/restore + audit streams (needs the
  backup slice); WAL-at-rest stays deferred (Go delegates to Badger's
  key registry, out of scope for the portable format).
- [x] M-doc: rustdoc + examples; update `docs/PROJECT_MAP.md` status.
- [x] M-done: `cargo test -p datastreamdb-enc`, clippy `-D warnings`, fmt clean.
