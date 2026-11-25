# datastreamdb

Python bindings for DataStreamDB — embedded graph database with DQL and
GraphQL in-process. This package ships an abi3 wheel for Python 3.10+ on Linux
x64 (glibc).

```sh
pip install datastreamdb
```

Full developer guide: <https://fab679.github.io/datastreamdb/embedding/python/>

## Quick start

```python
import json
from datastreamdb import Instance, version

print(version())  # "0.2.0"

db = Instance("{}")  # isolated in-memory instance

db.dql_alter("""
name: string @index(exact) .
friend: [uid] @reverse .
type Person {
  name
  friend
}
""")

db.dql_mutate("""
{ set {
    _:ada <name> "Ada" .
    _:bob <name> "Bob" .
    _:ada <friend> _:bob .
} }
""")

result = json.loads(db.dql_query("""
  { people(func: eq(name, "Ada")) { name friend { name } } }
"""))
print(result)
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

## GraphQL

```python
db.set_graphql_schema("""
type Post {
  id: ID!
  title: String @search(by: [term])
  author: Author @hasInverse(field: posts)
}
type Author {
  id: ID!
  name: String @id @search(by: [hash])
  posts: [Post]
}
""")

response = json.loads(db.graphql("""
  mutation {
    addPost(input: [{title: "Hello", author: {name: "Ann"}}]) {
      post { id title author { name } }
      numUids
    }
  }
"""))
```

Subscriptions are poll-based via `graphql_subscribe`, `graphql_next`, and
`graphql_unsubscribe`. Embedded graph mode is single-node; see the full guide
at <https://fab679.github.io/datastreamdb/embedding/queries/>.

## API

| Method | Description |
|---|---|
| `Instance(config_json)` | Open a memory or persistent instance. |
| `id()` | Unique instance id. |
| `backend()` | `"memory"` or `"fjall"`. |
| `dql_alter(schema)` | Apply additive/updating DQL schema. |
| `dql_mutate(mutation)` | Apply a DQL mutation; returns assigned uids as JSON. |
| `dql_query(query)` | Run a DQL query; returns JSON text. |
| `set_graphql_schema(sdl)` | Install/replace the GraphQL schema. |
| `graphql(query, variables_json=None, operation=None, headers_json=None)` | Run GraphQL; returns JSON text. |
| `graphql_subscribe(query, variables_json=None, headers_json=None, poll_ms=100)` | Create a poll subscription. |
| `graphql_next(id, wait_ms=0)` | Poll a subscription. |
| `graphql_unsubscribe(id)` | Cancel a subscription. |

The low-level `put`, `get`, `execute` (scan/batch), and `flush` methods remain
available for raw storage, but graph applications should use DQL and GraphQL.
See <https://fab679.github.io/datastreamdb/embedding/key-value/>.

## Build and test

```sh
uv venv .venv
uv pip install --python .venv/bin/python maturin pytest
.venv/bin/maturin develop
.venv/bin/pytest -q
```

Build a release wheel with `.venv/bin/maturin build --release`. The extension
uses `abi3-py310`, so one wheel covers Python 3.10 and newer.
