# TODO - datastreamdb-xidmap

Port checklist for `xidmap`. Check items only with tests proving Go parity.

- [x] M0: crate scaffolded, compiles, docs render.
- [ ] M1: Port trie (`xidmap/trie.go`) with `trie_test.go` vectors.
- [ ] M2: Port sharded concurrent map (`xidmap/xidmap.go`) with `xidmap_test.go` suite.
- [ ] M3: Back persistence with `datastreamdb-store`; crash-recovery test.
- [ ] M-doc: rustdoc + examples; update `docs/PROJECT_MAP.md` status.
- [ ] M-done: `cargo test -p datastreamdb-xidmap`, clippy `-D warnings`, fmt clean.
