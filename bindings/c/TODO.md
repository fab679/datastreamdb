# TODO - bindings/c

- [x] M1: `datastreamdb.h` checked in; `smoke.c` proves open/execute/close + isolation.
- [ ] M2: `xtask codegen` generates `datastreamdb.h` from the Rust signatures; CI diffs it.
- [ ] M2: ABI symbol-allowlist check in CI (additive-only within majors).
- [ ] M3: reference Go and Zig consumers against the same header.
