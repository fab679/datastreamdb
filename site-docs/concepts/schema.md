# Predicates and schema

Data is stored as **predicates**: named edges from a node to a value or to
another node. The schema declares each predicate's type and the indexes that
make it searchable.

## Predicates

A predicate is the smallest unit of structure. `name`, `year`, `friend`, and
`datastreamdb.type` are all predicates. Every fact is a triple:

```text
<subject> <predicate> <object> .
_:matrix  <name>      "The Matrix" .
_:matrix  <year>      "1999"^^<xs:int> .
_:matrix  <genre>     _:scifi .
```

Subjects are uids (`0x2`) or blank nodes (`_:matrix`) that the server maps to
uids on write.

## Schema syntax

```text
name: string @index(exact, term) .
year: int @index(int) .
rating: float @index(float) .
friend: [uid] @reverse .
datastreamdb.type: [string] @index(exact) .
```

- The type (`string`, `int`, `float`, `uid`, …) fixes what a predicate accepts.
- `[uid]` and `[string]` mark list predicates.
- `@index(...)` chooses tokenizers; without one, `eq`/`term`/range roots cannot
  use the predicate.
- `@reverse` maintains the incoming edge so `~friend` works.
- `@count`, `@upsert`, and `@lang` are also accepted where upstream DQL defines
  them.

The full type and tokenizer table lives in
[the embedded query reference](../embedding/queries.md#supported-types).

## What needs declaring

Mutations auto-derive **scalar** predicates, Dgraph-style:

```text
{ set { _:a <name> "Ada" . } }
```

`name` becomes a string predicate and `has(name)` works with no schema at all.
Declare the schema when you need any of the following:

| Need | Why |
|---|---|
| A UID relationship (`friend: [uid]`) | Otherwise the engine may infer a scalar and reject the uid value |
| Reverse traversal (`@reverse`) | `~friend` needs the reverse edge maintained |
| Indexed roots (`eq`, `term`, `int`, ranges) | No tokenizer means no index to consult |
| Type queries (`type(Person)`) | Types must exist in the schema |
| GraphQL-generated schemas | The SDL derives predicates, indexes, and types |

## Types

A type groups predicates so a node can be selected by what it is:

```text
type Person {
    name
    age
    friend
}
```

Query it with `func: type(Person)`. Membership is recorded as a
`datastreamdb.type` edge on the node. DQL mutations must set that edge
explicitly; GraphQL mutations set it automatically.

```text
_:ada <datastreamdb.type> "Person" .
```

Type declarations require **one field per line**. `type Person { name age }` is
rejected.

## Altering

Apply schema with `alter`; it is additive and idempotent. Adding an index to a
predicate that already holds data rebuilds the index over existing values, so
the new index is queryable immediately:

```text
budget: int @index(int) .
```

Removing schema is a separate operation. Standalone servers expose
`drop_attr`, `drop_all`, and the other drop forms through `POST /alter`; embedded
0.2.0 supports additive alters only (see [Limitations](../reference/limitations.md)).

## Where schema lives

Each instance owns its schema. Directory instances persist the alter history and
replay it on open, so predicates, indexes, and types survive a restart. Two
instances never share schema, even when they point at the same code path.

Next: [relationships and facets](relationships.md).
