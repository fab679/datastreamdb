# TODO - datastreamdb-tok

Port checklist for `tok`. Check items only with tests proving Go parity.

- [x] M0: crate scaffolded, compiles, docs render.
- [ ] M1: Port tokenizer registry (`tok/tok.go`, `tokens.go`) and `index_factory.go`.
- [ ] M2: Port language base + stemmers + stopwords (`langbase.go`, `stemmers.go`, `stopwords.go`).
- [ ] M3: Decide tantivy integration surface for full-text (replaces bleve in `tok/bleve.go`).
- [ ] M4: Port HNSW vector index hooks (`tok/hnsw`) behind a feature flag.
- [ ] M5: Port `tok/index` exact/trigram/bool/datetime tokenizers with test vectors.
- [ ] M-doc: rustdoc + examples; update `docs/PROJECT_MAP.md` status.
- [ ] M-done: `cargo test -p datastreamdb-tok`, clippy `-D warnings`, fmt clean.

## Session notes (M2 chunker/schema foundations)
- [x] M2: facets store Go-exact binary values (LE ints/floats, 1-byte bools,
  12-byte datetimes); tokens for string facets only; `unquote` is public,
  octal-aware, and rejects `\'` like Go.
- [x] M2: HNSW sidecar suffix constants (`VEC_ENTRY`/`VEC_KEYWORD`/`VEC_DEAD`).
- [x] M6: `copy_facets` alias propagation (wired to `pb.FacetParams`).
