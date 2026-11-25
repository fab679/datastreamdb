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

The full type and tokenizer table, with examples for every index, lives in the
[schema reference](../reference/schema.md).

## What needs declaring

Mutations auto-derive **scalar** predicates, Dgraph-style:

```text
{ set { _:a <name> "Ada" . } }
```

`name` becomes a scalar predicate: the value is stored and `has(name)` works
with no schema. But it has no index, no UID type, and no reverse edge. Schema is
therefore **incremental** — declare what a query needs, when it needs it.

| You want | Declare first? | Without it |
|---|---|---|
| Store/read a scalar, `has(pred)` | No | The value is stored; `has` works |
| `eq`, `term`, or range roots | Yes (`@index`) | The mutation stores; the query fails with `attribute ... is not valid scalar type` |
| A UID relationship (`friend: [uid]`) | Yes | The mutation fails: the predicate is treated as scalar and the UID value is rejected |
| Reverse traversal (`~friend`) | Yes (`@reverse`) | There is no incoming edge to traverse |
| `type(Name)` root | No | Matches `datastreamdb.type` edges; a `type` declaration adds schema-aware expansion/cascade |
| GraphQL schema | Yes (SDL) | `graphql` has no schema; the SDL derives predicates, indexes, and types |

You can add schema later. `alter` is additive, and an index added after data
exists is rebuilt over the existing values, so it is queryable immediately.

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
`datastreamdb.type` edge on the node, so the root query works even without a
`type` declaration. The declaration is what makes type-aware expansion and
cascade know the type's fields. DQL mutations must set the `datastreamdb.type`
edge explicitly; GraphQL mutations set it automatically.

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
0.2.1 supports additive alters only (see [Limitations](../reference/limitations.md)).

## Where schema lives

Each instance owns its schema. Directory instances persist the alter history and
replay it on open, so predicates, indexes, and types survive a restart. Two
instances never share schema, even when they point at the same code path.

Next: [relationships and facets](relationships.md).
