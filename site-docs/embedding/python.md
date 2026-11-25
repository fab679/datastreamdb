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

## DQL

```python
import json
from datastreamdb import Instance

db = Instance("{}")

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

`dql_mutate` returns JSON text of blank-node assignments. `dql_query` returns
JSON text; parse it with `json.loads`.

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
print(response)
```

GraphQL execution errors arrive in `response["errors"]`; invalid SDL or
transport failures raise `RuntimeError`.

## Subscriptions

```python
subscription_id = db.graphql_subscribe("""
  subscription { queryPost { title } }
""")

event = json.loads(db.graphql_next(subscription_id, 0))  # initial response
db.graphql('mutation { addPost(input: [{title: "Hello again"}]) { numUids } }')
event = json.loads(db.graphql_next(subscription_id, 1000))
db.graphql_unsubscribe(subscription_id)
```

`graphql_next(id, wait_ms)` blocks up to `wait_ms` and returns
`{changed, closed, response?}`. Close subscriptions when done.

## Errors

The binding raises `RuntimeError` for malformed requests, invalid SDL, locked
directories, and engine failures:

```python
try:
    db.dql_query('{ q(func: has(name)) { name } }')
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
- DQL alters are additive/updating; drop operations and upserts are not exposed.
- GraphQL subscriptions are poll-based; WebSocket push is not exposed.

See [DQL and GraphQL](queries.md) for pattern filters, facets, and subscription
shapes. The separate [low-level key/value API](key-value.md) is available for
hosts that need raw storage.
