# TODO - datastreamdb-types

Port checklist for `types`. Check items only with tests proving Go parity.

- [x] M0: crate scaffolded, compiles, docs render.
- [x] M2: scalar table with frozen discriminants, names, `ParseTime` vectors.
- [x] M2: conversion matrix (`Convert`, `Marshal`, `ObjectValue` mirror) with ported vectors.
- [x] M2: compare/sort/select (`Less`/`Equal`/`Compare*`, `Sort*`, quickselect, ICU collation).
- [x] M2: password (bcrypt cost 10, Go hash vectors verify).
- [x] M2: facets value model (see `datastreamdb-tok` for the tokenizer glue).
- [x] M2: geo (GeoJSON/WKB, earth, S2 loops, index tokens, query filters) with ported vectors.
- [ ] M3: reconcile two characterized divergences with a Go toolchain: MA-box `within` verdict, near-cover token count (see `geofilter.rs`).
- [ ] M3: M-dimension WKB support (XY/XYZ done); collation-key API for exact-lang indexes.
- [ ] M-doc: rustdoc + examples; update `docs/PROJECT_MAP.md` status.
- [ ] M-done: `cargo test -p datastreamdb-types`, clippy `-D warnings`, fmt clean.
- [x] M2: `strconv` base-0 int/uint parsing (`underscoreOK` port) and Go
  `Quote`, plus `TryFrom<i32>` for `TypeId`.
- [x] M2: `parse_v_float` matches Go exactly (TrimRight `]`, 32-bit range,
  processed-string errors).
