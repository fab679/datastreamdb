# TODO - datastreamdb-codec

Port checklist for `codec`. Check items only with tests proving Go parity.

- [x] M0: crate scaffolded, compiles, docs render.
- [ ] M1: Port `codec/codec.go` encode/decode with byte-exact vectors from `codec_test.go`.
- [ ] M2: Port group-varint posting codec; benchmark vs Go (`codec/benchmark`).
- [ ] M3: Property tests: roundtrip arbitrary uid lists through every codec.
- [ ] M-doc: rustdoc + examples; update `docs/PROJECT_MAP.md` status.
- [ ] M-done: `cargo test -p datastreamdb-codec`, clippy `-D warnings`, fmt clean.
