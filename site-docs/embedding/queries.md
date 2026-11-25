# DQL and GraphQL queries

Embedded 0.2.0 runs the same DQL executor and GraphQL layer as the standalone
servers, but with single-node semantics: no Raft, zero, worker, ACL, or
namespaces. Instances are isolated by handle, and graph data lives in the
instance's posting partition.

For complete movies, TODO, and orders walkthroughs, see
[Embedded tutorials](../tutorials/index.md).

## Schema: optional for scalars, required for relationships/indexes

Basic scalar predicates are auto-derived by mutations, Dgraph-style:

```text
{ set { _:a <name> "Ada" . } }
```

`name` becomes a scalar string predicate and `has(name)` works without any
schema. But declare schema when you need:

- **UID/relationship predicates** (`friend: [uid]`); otherwise the engine may
  infer a scalar and reject the UID value.
- **Reverse edges** (`@reverse`) for `~friend` traversal.
- **Indexes** for `eq`, `term`, `hash`, `int`, and similar root functions.
- **Type definitions** for `type(Person)` and GraphQL-generated schemas.

```text
name: string @index(exact, term) .
age: int @index(int) .
friend: [uid] @reverse .
type Person {
    name
    age
    friend
}
```

Apply it once per instance (it is persisted for fjall directories):

```rust
db.dql_alter(r#"
    name: string @index(exact, term) .
    age: int @index(int) .
    friend: [uid] @reverse .
    type Person {
        name
        age
        friend
    }
"#)?;
```

```js
db.dqlAlter(`
  name: string @index(exact, term) .
  age: int @index(int) .
  friend: [uid] @reverse .
  type Person {
    name
    age
    friend
  }
`)
```

```python
db.dql_alter("""
name: string @index(exact, term) .
age: int @index(int) .
friend: [uid] @reverse .
type Person {
  name
  age
  friend
}
""")
```

C uses the envelope:

```json
{"op":"dql_alter","schema":"name: string @index(exact) . friend: [uid] @reverse . type Person { name\nfriend }"}
```

## Relationship data

```rust
let assigned = db.dql_mutate(r#"{
    set {
        _:ada <name> "Ada" .
        _:ada <age> "36"^^<xs:int> .
        _:bob <name> "Bob" .
        _:bob <age> "25"^^<xs:int> .
        _:ada <friend> _:bob .
        _:ada <datastreamdb.type> "Person" .
        _:bob <datastreamdb.type> "Person" .
    }
}"#)?;
```

`dql_mutate` returns blank-node assignments (`_:ada -> uid`). GraphQL mutations
use the same engine and do not need a separate DQL mutation.

## Relationship traversal

```text
{
  people(func: eq(name, "Ada")) {
    name
    age
    friend {
      name
      age
    }
  }
}
```

```js
const result = JSON.parse(db.dqlQuery(`
  { people(func: eq(name, "Ada")) { name friend { name } } }
`))
```

Reverse edges work with `~friend` when the predicate has `@reverse`:

```text
{ q(func: eq(name, "Bob")) { name ~friend { name } } }
```

## Nested pattern filters

```text
{
  q(func: has(name)) @filter(eq(name, "Ada")) {
    name
    friend @filter(gt(age, 20)) {
      name
      age
    }
  }
}
```

This is a pattern query: the root selects candidates, `@filter` constrains the
root, and the nested `friend` block applies a second filter to related nodes.

Other useful roots and filters:

```text
{ q(func: has(friend)) { name } }
{ q(func: uid(0x2, 0x3)) { name friend { name } } }
{ q(func: eq(age, 36)) @filter(has(friend)) { name } }
{ q(func: has(name), orderasc: name, first: 10) { name } }
```

## GraphQL schema and queries

```js
db.setGraphqlSchema(`
  type Post {
    id: ID!
    title: String @search(by: [term])
    numLikes: Int @search
    author: Author @hasInverse(field: posts)
  }
  type Author {
    id: ID!
    name: String @id @search(by: [hash])
    posts: [Post]
  }
`)
```

Mutation and relationship query:

```graphql
mutation {
  addPost(input: [{title: "Hello", numLikes: 42, author: {name: "Ann"}}]) {
    post { id title numLikes author { name } }
    numUids
  }
}
```

```graphql
{
  queryPost(filter: {title: {anyofterms: "Hello"}}) {
    title
    numLikes
    author { name }
  }
}
```

## GraphQL subscriptions

Subscriptions are **poll-based**. The embedded handle captures an initial
response, then returns a changed response when the query result changes:

```js
const id = db.graphqlSubscribe(`
  subscription {
    queryPost(filter: {title: {anyofterms: "Hello"}}) {
      title
      numLikes
    }
  }
`)

let event = JSON.parse(db.graphqlNext(id, 0)) // initial: changed=true
db.graphql('mutation { addPost(input: [{title: "Hello again"}]) { numUids } }')
event = JSON.parse(db.graphqlNext(id, 1000))  // changed=true after the mutation
db.graphqlUnsubscribe(id)
```

`graphqlNext(id, wait_ms)` blocks up to `wait_ms`, polling at the subscription
interval. It returns `{changed, closed, response?}`; `closed` becomes true when
the subscription is cancelled or the GraphQL schema is replaced.

In Rust, use the `Subscription` handle directly:

```rust
let sub = db.graphql_subscribe(query, json!({}), &headers, Duration::from_millis(100))?;
assert!(matches!(sub.next()?, SubscriptionEvent::Changed(_)));
let changed = sub.next_timeout(Duration::from_secs(2))?;
sub.close();
```

## Supported types

### DQL predicate types

| Type | Meaning | Index tokenizers |
|---|---|---|
| `string` | UTF-8 text | `exact`, `term`, `fulltext`, `trigram`, `hash` |
| `int` | 64-bit signed integer | `int` |
| `float` | 64-bit floating point | `float` |
| `bool` | Boolean | `bool` |
| `datetime` | RFC3339 date/time; use this for calendar dates | `year`, `month`, `day`, `hour` |
| `geo` | GeoJSON geometry: Point, LineString, Polygon, MultiPolygon | `geo` |
| `uid` | Relationship to another node | Not indexable; supports `@reverse` |
| `password` | Hashed password | Not indexable |
| `bigfloat` | Arbitrary-precision decimal | No dedicated index tokenizer |
| `binary` | Raw bytes | Schema-supported; not exposed in GraphQL |
| `float32vector` | Float vector | Vector indexes are not supported in embedded 0.2.0 |
| `default` | Untyped/auto-derived scalar | Depends on the derived type |

There is **no separate `date` type**: use `datetime` with an RFC3339 value such
as `"2026-11-25T00:00:00Z"`.

### Spatial types

DQL uses `geo` and stores GeoJSON geometries. GraphQL exposes them as:

| GraphQL type | Geometry |
|---|---|
| `Point` | Point |
| `PointList` | Line string / point list |
| `Polygon` | Polygon |
| `MultiPolygon` | Multi-polygon |

### GraphQL type mapping

| GraphQL type | DQL type |
|---|---|
| `ID` | `uid` |
| `String` | `string` |
| `Int` / `Int64` | `int` |
| `Float` | `float` |
| `Boolean` | `bool` |
| `DateTime` | `datetime` |
| `Point`, `PointList`, `Polygon`, `MultiPolygon` | `geo` |
| `[T]` | list predicate |
| Object type | `uid` relationship, usually with `@hasInverse` |

### Other data

| Item | Embedded graph support |
|---|---|
| Relationships | `uid` predicates, lists, `@reverse`, `@hasInverse` in GraphQL SDL |
| DQL values | RDF literals in mutations; JSON output from queries |
| GraphQL values | GraphQL scalar/object/list types from the user SDL |
| Raw KV values | UTF-8 strings in bindings; arbitrary bytes in Rust |
| Binary graph values | Not exposed in the 0.2.0 GraphQL/DQL envelope |

## Errors

- Rust: `Result<_, CoreError>` (`Query`, `Schema`, `Posting`, `GraphQl`).
- Node: thrown `Error`.
- Python: `RuntimeError`.
- C: non-zero status + `datastreamdb_last_error()`.

GraphQL execution errors are returned inside the GraphQL response
(`response.errors`), not as transport errors.

## 0.2.0 boundaries

- Single-node only; no Raft/zero/worker, ACL, or namespaces.
- DQL alters are additive/updating; drop operations are not exposed.
- DQL upserts/conditional mutations and query variables are not exposed yet.
- GraphQL subscriptions are poll-based; no WebSocket transport.
- `@auth`, `@custom` HTTP/lambda resolvers, remote schemas, and vector indexes
  are not supported in embedded mode.
