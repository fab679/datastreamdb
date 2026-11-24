# TODO - datastreamdb-loader

Port checklist for `bulk`/`live`. Check items only with tests proving Go parity.

- [x] M0: crate scaffolded, compiles, docs render.
- [x] M6-loaders: live file streaming + batch commits into a live
  engine (`live.rs`; RDF line batches, JSON docs, schema install,
  file:line errors; 5 tests).
- [x] M6-loaders: bulk load into a fresh engine with schema inference
  (`bulk.rs`; Go `validateType` parity incl. uid-lists; 2 tests).
- [x] M6-loaders: UID assignment (`assign.rs`; seed above stored +
  file explicits, per-batch `BumpTo`, oracle advance on reopen;
  3 tests).
- [x] M6-enc: `--encryption-key-file` on both CLIs; encrypted
  input files stream through the filestore decryptor (1 test).
- [x] M6-s3: `s3://`/`minio://` inputs via `open_input` (env
  credentials like Go; dispatch test).
- [ ] M6-loaders: remote live-load over gRPC against a running alpha.
- [ ] M6-loaders: sharded bulk map/shuffle/reduce (multi-group scale path).
- [ ] M-doc: rustdoc + examples; update `docs/PROJECT_MAP.md` status.
- [x] M-done: `cargo test -p datastreamdb-loader`, clippy `-D warnings`, fmt clean.
