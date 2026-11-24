# bindings/python

Python bindings via pyo3 (maturin mixed layout): `datastreamdb.Instance` with
sync APIs today, asyncio helpers in M2. The GIL is released around every
engine call.

## Build and test (local, no publish)

```sh
cd bindings/python
uv venv .venv
uv pip install --python .venv/bin/python maturin pytest
.venv/bin/maturin develop
.venv/bin/pytest -q
```

## Offline fallback (no maturin/pytest on PATH)

```sh
# from the workspace root
cargo build -p datastreamdb-py --offline
so=bindings/python/python/datastreamdb/_native$(python3 -c "import sysconfig; print(sysconfig.get_config_var('EXT_SUFFIX'))")
cp target/debug/libdatastreamdb_py.so "$so"
cd bindings/python && PYTHONPATH=python:tests python3 - <<'EOF'
import test_smoke as t
for name in sorted(n for n in dir(t) if n.startswith("test_")):
    getattr(t, name)()
    print("PASS", name)
EOF
```

The fallback needs a `pytest` stub for `test_smoke.py`'s
`pytest.importorskip("datastreamdb")` (a two-line module on
`PYTHONPATH` returning `importlib.import_module(name)`).
