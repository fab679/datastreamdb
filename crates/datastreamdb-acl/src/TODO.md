# TODO - datastreamdb-acl/src

- [x] One module per ported Go file, each with unit tests + doc examples.
  (`entities.rs` ← `acl/utils.go` model, `nquads.rs` ← builders,
  `keys.rs` ← `x/acl_enc_keys.go`, `jwt.rs` ← `x/jwt_helper.go` +
  `x/authn.go` + `edgraph/access.go` mint/validate.)
- [x] No `unwrap`/`expect` on user-controlled paths; errors via `thiserror`.
- [x] No global mutable state (multi-instance embedding requirement).
- [ ] `datastreamdb acl` CLI flows (`acl.go` add/del/mod/info) when the server
  binary slice lands (needs a live client).
