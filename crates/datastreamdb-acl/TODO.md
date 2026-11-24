# TODO - datastreamdb-acl

Port checklist for `acl`. Check items only with tests proving Go parity.

- [x] M0: crate scaffolded, compiles, docs render.
- [x] M6.1: ACL core — entities/unmarshal/group IDs, user/group NQuad
  builders (`acl/utils.go`).
- [x] M6.1: secret/key handling — alg allowlist, HMAC length rules,
  PEM private→public derivation, redacted `Secret` (`x/acl_enc_keys.go`).
- [x] M6.1: JWT flows with `jsonwebtoken` — access/refresh mint + strict
  validate for HS/ES/RS/PS/EdDSA (ES512 unsupported by backend,
  documented); canonical goldens (24 tests).
- [x] M6.1: replaced non-parity Argon2 scaffold with Go's bcrypt model
  (hashing lives in `datastreamdb-types::password`, M2; verification via
  `checkpwd` at login).
- [x] M6.2: frontend login RPC + authorize enforcement + guardian/groot
  bootstrap (`checkpwd` was already implemented; cluster test green).
- [x] M6.2c: `datastreamdb acl` CLI client flows (add/del/mod/info/chmod)
  in `datastreamdb-server` (`acl_admin.rs` + `datastreamdb acl` subcommand; live
  cluster test green).
- [ ] M6: vault-backed keys; multi-tenant key scoping doc for embedded
  instances.
- [x] M-doc: rustdoc + examples; update `docs/PROJECT_MAP.md` status.
- [x] M-done: `cargo test -p datastreamdb-acl`, clippy `-D warnings`, fmt clean.
