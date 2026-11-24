# TODO - datastreamdb-query/src

- [x] One module per ported Go file, each with unit tests + doc examples.
- [x] No `unwrap`/`expect` on user-controlled paths; errors via `thiserror`.
      (`expect` remains only on internal invariants, each with a `# Panics` doc.)
- [x] No global mutable state (multi-instance embedding requirement).
- [x] No file over ~1000 lines (`output_render.rs`, `var_levels.rs` splits).
- [ ] M4: upsert execution module (`query/upsert.go`).
- [ ] M-graphql: GraphQL response module (`query/outputnode_graphql.go`).
