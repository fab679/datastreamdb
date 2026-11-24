# datastreamdb

Python bindings for DataStreamDB — embedded, multi-instance graph database.
This package ships an abi3 wheel for Python 3.10+ on Linux x64 (glibc).

```sh
pip install datastreamdb
```

Full developer guide: <https://fab679.github.io/datastreamdb/embedding/python/>

## Quick start

```python
import json
from datastreamdb import Instance, version

print(version())  # "0.1.0"

db = Instance("{}")  # isolated in-memory instance
db.put("user:1", json.dumps({"name": "Ada", "age": 36}))

raw = db.get("user:1")
print(None if raw is None else json.loads(raw)["name"])  # Ada
```

Persistent instance:

```python
db = Instance(json.dumps({
    "data_dir": "/var/lib/app/graph-a",
    "tenant": "app",
}))
```

`data_dir` takes an exclusive lock; a second open of the same directory fails
until the first closes. `tenant` is a label, not an isolation boundary.

## Query with prefix scans

```python
result = json.loads(db.execute(json.dumps({
    "op": "scan",
    "prefix": "user:",
    "limit": 100,
})))

for pair in result["pairs"]:
    print(pair["key"], json.loads(pair["value"]))
```

Keys are returned in lexicographic order. `limit` defaults to 1000 and is
clamped to 100,000. There is no continuation cursor in 0.1.0.

## Atomic writes

```python
result = json.loads(db.execute(json.dumps({
    "op": "batch",
    "puts": [
        {"key": "user:1", "value": json.dumps({"name": "Ada"})},
        {"key": "user:2", "value": json.dumps({"name": "Bob"})},
    ],
    "deletes": ["user:3"],
})))

print(result["applied"])  # 3
```

## API

| Method | Description |
|---|---|
| `Instance(config_json)` | Open a memory or persistent instance. |
| `id()` | Unique instance id. |
| `backend()` | `"memory"` or `"fjall"`. |
| `put(key, value)` | Upsert a UTF-8 value. |
| `get(key)` | Read a UTF-8 value, or `None`. |
| `execute(request)` | `ping`, `put`, `get`, `delete`, `scan`, `batch`, `flush`. |
| `version()` | Binding/engine version. |

Values are strings in this binding. Serialize JSON documents with
`json.dumps`; base64-encode binary data. Embedded mode is key/value only in
0.1.0 — DQL and GraphQL run against the standalone servers. The GIL is
released around every engine call.

## Build and test

```sh
uv venv .venv
uv pip install --python .venv/bin/python maturin pytest
.venv/bin/maturin develop
.venv/bin/pytest -q
```

Build a release wheel with `.venv/bin/maturin build --release`. The extension
uses `abi3-py310`, so one wheel covers Python 3.10 and newer.
