# TODO - datastreamdb-backup

Port checklist for `backup`. Check items only with tests proving Go parity.

- [x] M0: crate scaffolded, compiles, docs render.
- [x] M6.5: manifest JSON contract (`Manifest`/`MasterManifest`,
  chain verify + restore selection + file naming, Go byte-layout;
  5 tests).
- [x] M6.5b: export engine (RDF/JSON/schema, Go-verbatim goldens),
  binary backup writer (Snappy framing, AES-CTR, drop-op records),
  single-group restore + drop replay, manifest file contract fixes
  (`Manifests` key, `omitempty` drops, summary view, version 2105).
- [ ] M6: multi-group backup fan-out, S3 destinations, bulk-loader
  map/reduce restore, server encryption-key plumbing for backups.
- [x] M-doc: rustdoc + examples; update `docs/PROJECT_MAP.md` status.
- [x] M-done: `cargo test -p datastreamdb-backup`, clippy `-D warnings`, fmt clean.
