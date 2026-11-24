# TODO - bindings/python

- [x] M1: `Instance` open/execute/put/get, GIL released, maturin develop + pytest green (no publish).
- [ ] M1-followup: context-manager (`with Instance(...)`) + `close()`; map error codes to exception types.
- [ ] Asyncio executor bindings + pure-Python convenience wrappers.
- [ ] PyPI wheels via maturin trusted publishing (deferred).
- [ ] Extend `tests/` to full conformance (scan/batch/delete/flush, error codes).
