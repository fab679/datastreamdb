# TODO - bindings/node

- [x] M1: `open/query/mutate` via `Instance` + `execute`; typed JS errors; smoke tests green (no publish).
- [ ] M1-followup: move blocking engine calls off the JS thread (`AsyncTask`).
- [ ] napi prebuilds for linux-x64, macos-arm64, windows-x64 (publish gate, deferred).
- [ ] Extend `test/` to full conformance (scan/batch/delete/flush, error codes).
- [ ] TypeDoc API reference + runnable examples.
