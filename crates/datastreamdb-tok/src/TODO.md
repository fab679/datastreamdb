# TODO - datastreamdb-tok/src

- [ ] One module per ported Go file, each with unit tests + doc examples.
- [ ] No `unwrap`/`expect` on user-controlled paths; errors via `thiserror`.
- [ ] No global mutable state (multi-instance embedding requirement).
