# TODO - datastreamdb-audit/src

- [x] One module per ported Go file, each with unit tests + doc examples.
  (`event.rs` ← constants, `mask.rs` ← `checkRequestBody`,
  `sink.rs` ← logger outputs, `config.rs` ← superflag parse,
  `auditor.rs` ← logger + interceptors.)
- [x] No `unwrap`/`expect` on user-controlled paths; errors via `thiserror`.
- [x] No global mutable state (multi-instance embedding requirement).
- [ ] Rotated-file compression/encryption hooks (enc slice).
