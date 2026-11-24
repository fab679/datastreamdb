# TODO - datastreamdb-enc/src

- [x] One module per ported Go file, each with unit tests + doc examples.
  (`keys.rs` ← key handling, `stream.rs` ← `GetWriter`/`GetReader`,
  `rotate.rs` ← re-encryption.)
- [x] No `unwrap`/`expect` on user-controlled paths; errors via `thiserror`.
- [x] No global mutable state (multi-instance embedding requirement).
