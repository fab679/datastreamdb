# Python embedding

```sh
pip install datastreamdb
```

```python
from datastreamdb import Instance, version

print(version())  # "0.1.0"

a = Instance("{}")
b = Instance('{"tenant": "b"}')
assert a.id() != b.id()

a.put("name", "Ada")
assert a.get("name") == "Ada"
assert b.get("name") is None  # isolated

import json
res = json.loads(a.execute('{"op":"ping"}'))
assert res["ok"] and res["backend"] == "memory"
```

## API

- `Instance(config_json)` — raises on invalid config or backend failure.
- `id()` — unique id assigned at open.
- `backend()` — `"memory"` or `"fjall"`.
- `execute(request)` — JSON envelope in, JSON string out.
- `put(key, value)` / `get(key)` — raw key access (`None` when missing).
- `version()` — engine version.

The GIL is released around every engine call, so instances can be
shared across threads. Config is the same JSON as
[Embedding overview](index.md#configuration).
