# Python embedding

```sh
pip install datastreamdb
```

```python
from datastreamdb import Instance, version

print(version())  # "0.2.0"
```

## Open an instance

```python
import json
from datastreamdb import Instance

memory = Instance("{}")
persistent = Instance(json.dumps({
    "data_dir": "/var/lib/app/graph-a",
    "tenant": "app",
}))

print(memory.id(), memory.backend())        # uuid, "memory"
print(persistent.id(), persistent.backend()) # uuid, "fjall"
```

- `{}` opens an isolated in-memory instance.
- `data_dir` selects the persistent fjall backend and takes an exclusive lock.
- `tenant` is a label, not an isolation boundary.

## DQL and GraphQL

```python
db.dql_alter("""
name: string @index(exact) .
friend: [uid] @reverse .
type Person { name friend }
""")
db.dql_mutate('{ set { _:a <name> "Ada" . _:b <name> "Bob" . _:a <friend> _:b . } }')
print(db.dql_query('{ people(func: eq(name, "Ada")) { name friend { name } } }'))

db.set_graphql_schema("""
type Person {
  id: ID!
  name: String @search(by: [term])
}
""")
print(db.graphql("{ queryPerson { name } }"))
```

See [DQL & GraphQL](queries.md) for pattern filters, mutations, and
poll-based subscriptions.

## Supported types

| API | Type | Notes |
|---|---|---|
| `put(key, value)` | `str` | UTF-8 key and value. |
| `get(key)` | `str \| None` | `None` when missing. |
| `execute(request)` | JSON string | Envelope values are strings only. |
| `scan` result | list of `{key, value}` | Values are UTF-8 strings. |
| JSON document | `json.dumps` / `json.loads` | No native object values. |
| Binary | base64 string | The binding does not accept raw `bytes` values. |

## Store and read a JSON document

```python
user = {"name": "Ada", "age": 36, "active": True}

db.put("user:1", json.dumps(user))

raw = db.get("user:1")
decoded = None if raw is None else json.loads(raw)
print(decoded["name"], decoded["age"])  # Ada 36
```

Use a key prefix for collections: `user:1`, `user:2`, `post:1`.

## Query with prefix scans

```python
response = json.loads(db.execute(json.dumps({
    "op": "scan",
    "prefix": "user:",
    "limit": 100,
})))

for pair in response["pairs"]:
    user = json.loads(pair["value"])
    print(pair["key"], user["name"])
```

- Keys come back in lexicographic order.
- `limit` defaults to 1000 and is clamped to 100,000.
- There is no cursor; use a narrower prefix or a larger limit to page.
- Zero-pad numeric ids when order matters (`user:0001`).

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

The batch is atomic. `delete` is idempotent.

## Delete and flush

```python
db.execute(json.dumps({"op": "delete", "key": "user:1"}))
db.execute('{"op":"flush"}')
```

`flush` makes prior writes durable. Dropping the last reference also flushes,
but call `flush` explicitly when the result matters.

## Execute the envelope

```python
ping = json.loads(db.execute('{"op":"ping"}'))
print(ping["ok"], ping["backend"])  # True "memory"

value = json.loads(db.execute(json.dumps({
    "op": "get",
    "key": "user:1",
})))
print(value["value"])  # string or None
```

Success responses:

| `op` | Response |
|---|---|
| `ping` | `{ ok, instance, tenant, backend }` |
| `put`, `delete`, `flush` | `{ ok: true }` |
| `get` | `{ ok: true, value: string \| null }` |
| `scan` | `{ ok: true, pairs: [{ key, value }] }` |
| `batch` | `{ ok: true, applied: number }` |

## Errors

The binding raises `RuntimeError` for malformed requests, unknown ops, invalid
configs, locked directories, and storage failures:

```python
try:
    db.execute('{"op":"nope"}')
except RuntimeError as err:
    print("datastreamdb:", err)
```

## Threading

The GIL is released around every engine call, so one instance can be shared
across Python threads. Use a lock only for read-modify-write sequences that
must be atomic; the engine does not provide compare-and-swap.

## Limitations in 0.2.0

- Embedded graph mode is single-node; clustering, ACL, and namespaces require
  the standalone servers.
- Raw KV values are strings; store binary data as base64.
- Raw KV and graph data use separate partitions.
- GraphQL subscriptions are poll-based; WebSocket push is not exposed.
- Scans are bounded and have no continuation cursor.
