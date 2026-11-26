# Schema reference

Predicates, types, indexes, and the query functions each index enables. For the
conceptual model, see [Predicates and schema](../concepts/schema.md).

## Predicate declarations

```text
name: string @index(exact, term) .
age: int @index(int) .
friend: [uid] @reverse .
datastreamdb.type: [string] @index(exact) .
```

- One predicate per line, ending in `.`.
- `[T]` marks a list predicate.
- `@index(...)` attaches one or more tokenizers.
- `@reverse` maintains the incoming edge (`~friend`).
- `@count`, `@upsert`, and `@lang` are accepted where DQL defines them.

Indexing is not allowed on `uid`, `password`, or `default` predicates. `uid`
relationships use `@reverse` instead.

## Supported types

| Type | Stored value | Index tokenizers | Typical roots | GraphQL |
|---|---|---|---|---|
| `string` | UTF-8 text | `exact`, `term`, `fulltext`, `trigram`, `ngram`, `hash`, `sha256` | `eq`, `allofterms`, `anyofterms`, `alloftext`, `anyoftext`, `regexp`, `ngram` | `String` |
| `int` | 64-bit signed integer | `int` | `eq`, `lt`, `le`, `gt`, `ge`, `between` | `Int`, `Int64` |
| `float` | 64-bit float | `float` | `eq`, `lt`, `le`, `gt`, `ge`, `between` | `Float` |
| `bigfloat` | Arbitrary-precision decimal | `bigfloat` | `eq`, numeric comparisons | not exposed |
| `bool` | Boolean | `bool` | `eq` | `Boolean` |
| `datetime` | RFC3339 timestamp | `year`, `month`, `day`, `hour` | `eq`, comparisons, `between` | `DateTime` |
| `geo` | GeoJSON geometry | `geo` | `near`, `within`, `contains`, `intersects` | `Point`, `PointList`, `Polygon`, `MultiPolygon` |
| `uid` | Reference to another node | none; use `@reverse` | `uid(...)`, `uid_in`, traversal | object type |
| `password` | Hashed password | none | `checkpwd` | not exposed |
| `binary` | Raw bytes | none | not queryable in the DQL envelope | not exposed |
| `float32vector` | Float vector | `hnsw(...)` parses, but the vector engine is a stub in 0.2.1 | not queryable | not exposed |
| `default` | Auto-derived scalar | depends on the derived type | `has`, derived roots | mapped from the derived type |

There is **no separate `date` type**. Use `datetime` with an RFC3339 value such
as `"2026-11-30T17:00:00Z"^^<xs:dateTime>`.

## Mutating values

Every type has an RDF literal form. A mutation is a `set` block; the final `.`
needs a newline (or `#`) before the closing `}`.

| Type | Literal form | Example |
|---|---|---|
| `string` | quoted text | `"Ada"` |
| `int` | `xs:int` | `"30"^^<xs:int>` |
| `float` | `xs:float` or `xs:double` | `"8.7"^^<xs:float>` |
| `bool` | `xs:boolean` | `"true"^^<xs:boolean>` |
| `datetime` | `xs:dateTime` (RFC3339) | `"2026-11-30T17:00:00Z"^^<xs:dateTime>` |
| `geo` | `geo:geojson` | `"{\"type\":\"Point\",\"coordinates\":[-122.4194,37.7749]}"^^<geo:geojson>` |
| `uid` | node reference | `_:ada <friend> _:bob .` or `<0x2> <friend> <0x3> .` |
| `password` | plain text; hashed on write | `_:ada <secret> "hunter2" .` |
| `bigfloat` | `xs:decimal` | `"123.456"^^<xs:decimal>` |
| `binary` | `xs:base64Binary` | `"hello"^^<xs:base64Binary>` |
| `float32vector` | `xs:[]float32` | reserved; vector mutations are not available in 0.2.1 |
| `default` | any of the above; type is derived | `"Ada"` |

```text
{
  set {
    _:a <name> "Ada" .
    _:a <i> "30"^^<xs:int> .
    _:a <f> "8.7"^^<xs:float> .
    _:a <b> "true"^^<xs:boolean> .
    _:a <d> "2026-11-30T17:00:00Z"^^<xs:dateTime> .
    _:a <g> "{\"type\":\"Point\",\"coordinates\":[-122.4194,37.7749]}"^^<geo:geojson> .
    _:a <big> "123.456"^^<xs:decimal> .
    _:a <bin> "hello"^^<xs:base64Binary> .
  }
}
```

Pass the block to `dql_mutate` (Rust), `dqlMutate` (TypeScript), or
`dql_mutate` (Python), or post it to `/mutate` on a standalone server.

A Polygon uses the same `geo:geojson` form:

```text
_:a <g> "{\"type\":\"Polygon\",\"coordinates\":[[[-122.42,37.77],[-122.41,37.77],[-122.41,37.78],[-122.42,37.77]]]}"^^<geo:geojson> .
```

`binary` literals are stored as their bytes and returned as a byte array in DQL
JSON. `password` values are stored hashed; verify them with `checkpwd`.

## Index examples

### String

```text
name: string @index(exact) .
bio: string @index(fulltext) .
tags: [string] @index(term) .
code: string @index(hash) .
digest: string @index(sha256) .
blob: string @index(trigram) .
```

```text
{ q(func: eq(name, "Ada")) { name } }
{ q(func: anyofterms(tags, "rust graph")) { tags } }
{ q(func: alloftext(bio, "graph database")) { bio } }
{ q(func: regexp(blob, /matrix/)) { blob } }
```

- `exact` and `hash`/`sha256` are equality indexes.
- `term` splits into words; use `allofterms`/`anyofterms`.
- `fulltext` applies stemming/stop words; use `alloftext`/`anyoftext`.
- `trigram` supports substring/regexp matching.
- `ngram` supports the `ngram(...)` root.

### Numeric and boolean

```text
age: int @index(int) .
rating: float @index(float) .
active: bool @index(bool) .
```

```text
{ q(func: ge(age, 30)) { age } }
{ q(func: between(rating, 7.0, 9.0)) { rating } }
{ q(func: eq(active, true)) { active } }
```

### Datetime

```text
due: datetime @index(year, month, day, hour) .
```

```text
{ q(func: ge(due, "2026-01-01T00:00:00Z"^^<xs:dateTime>)) { due } }
```

Use the date-part tokenizers you need; there is no exact `datetime` tokenizer.

### Geo

```text
location: geo @index(geo) .
```

```text
{ q(func: near(location, [-122.4194, 37.7749], 1000)) { location } }
```

### UID relationships

```text
friend: [uid] @reverse .
directed: uid @reverse .
```

```text
{ q(func: eq(name, "Ada")) { friend { name } } }
{ q(func: eq(name, "Bob")) { name ~friend { name } } }
```

`uid` predicates cannot be indexed; `@reverse` maintains the incoming edge.

### Password

```text
secret: password .
```

```text
{ q(func: eq(name, "Ada")) { checkpwd(secret, "guess") } }
```

### Vector

```text
embedding: float32vector @index(hnsw(metric: "cosine")) .
```

The type and directive parse, but the HNSW engine is a stub in 0.2.1: vector
mutations and vector search are not available. See
[Limitations](limitations.md).

## Type definitions

```text
type Person {
    name
    age
    friend
}
```

Type fields go one per line. A type groups predicates for schema-aware
expansion/cascade; `type(Person)` roots match `datastreamdb.type` edges.
DQL mutations must set that edge explicitly:

```text
_:ada <datastreamdb.type> "Person" .
```

GraphQL mutations set it automatically.

## GraphQL type mapping

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
| Object type | `uid` relationship |

## Alters and drops

Schema changes are additive by default. Adding an index to a predicate with
existing data rebuilds it over that data:

```text
budget: int @index(int) .
```

Standalone servers also accept drop forms through `POST /alter`:

```json
{"drop_attr": "budget"}
{"drop_all": true}
```

Embedded 0.2.1 supports additive alters only.
