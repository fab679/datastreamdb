# DQL guide

DQL (the graph query language) over [HTTP](http-api.md): schema in
`POST /alter`, reads in `POST /query`, writes in `POST /mutate`.
This page covers the core; the full language follows the upstream
semantics, and anything in [Limitations](../reference/limitations.md) is not
implemented.

## Schema

```sh
curl -XPOST localhost:8080/alter -d '
name: string @index(exact, trigram) .
age: int .
friend: [uid] .
datastreamdb.type: [string] @index(exact) .
'
```

Types group predicates for `@filter(type(...))`:

```sh
curl -XPOST localhost:8080/alter -d '
type Person {
  name
  age
  friend
}'
```

(Type fields go one per line.)

## Mutations (RDF)

```sh
curl -XPOST 'localhost:8080/mutate?commitNow=true' \
  -H 'content-type: application/rdf' -d '
_:ada <name> "Ada" .
_:ada <age> "36"^^<xs:int> .
_:ada <datastreamdb.type> "Person" .
_:ada <friend> _:grace .
_:grace <name> "Grace" .
_:grace <datastreamdb.type> "Person" .
'
```

Blanks (`_:ada`) come back mapped to uids in `data.uids`. Without
`commitNow`, commit later with the returned `start_ts`:

```sh
curl 'localhost:8080/commit?startTs=123'
```

## Mutations (JSON)

```sh
curl -XPOST 'localhost:8080/mutate?commitNow=true' \
  -H 'content-type: application/json' -d '
{"uid": "_:alan", "name": "Alan", "age": 41, "datastreamdb.type": "Person"}'
```

## Queries

```sh
curl -XPOST localhost:8080/query -d '{
  ada(func: eq(name, "Ada")) {
    uid name age
    friend { uid name }
  }
}'
```

Root functions include `uid(...)`, `has(pred)`, `eq/le/ge/gt/lt`,
`between`, `allofterms`/`anyofterms`, `regexp`, `type(...)`,
`uid_in`, and geo functions. Filters, facets, pagination
(`first:`/`offset:`), ordering (`orderasc:`/`orderdesc:`), and
`@cascade` follow upstream semantics.

## Directives

All six DQL directives are implemented in both standalone and embedded mode:

| Directive | Support | Notes |
|---|---|---|
| `@filter` | Yes | Functions and boolean operators; nested filters. |
| `@normalize` | Yes | Flattens the response to aliased predicates. |
| `@cascade` | Yes | Bare or `@cascade(pred, ...)`; checks nested paths. |
| `@recurse` | Yes | `depth` and `loop`; one level of predicates. |
| `@ignorereflex` | Yes | Block-level; removes a node that is its own ancestor. |
| `@groupby` | Yes | `count`, `min`, `max`, `sum`, `avg`; aliases. |

```text
{ q(func: has(name)) @cascade { name age } }
{ q(func: eq(name, "A")) @ignorereflex { name friend { name } } }
{ q(func: has(age)) @groupby(age) { count(uid) } }
```

`@ignorereflex` applies to the query block. Putting it on a predicate
(`friend @ignorereflex`) is rejected. Recursive traversal uses
`@recurse(depth: N, loop: false)` with every predicate in one block; `depth`
includes the root, and `loop: true` allows cycles and requires a positive depth.
See [recursive queries](../embedding/queries.md#recursive-queries) for the edge
budget and variable rules.

Variables cross blocks with `var()`:

```
{
  var(func: has(age)) { a as age }
  old(func: gt(age, 30)) @filter(uid(a)) { name }
}
```

## Upserts

`cond` guards a mutation on the request's query vars
(`eq/gt/lt/ge/le(len(v), n)`, `AND`-joined). It shines over gRPC,
where one `Request` carries both `query` and `mutations`: the query
runs first, its `var()` counts feed the guard, and failing
mutations are skipped without writing.

Note: `POST /mutate` carries no query, so every `len(v)` reads `0`
there — `@if(eq(len(v),0))` always passes and `@if(gt(len(v),0))`
always fails over plain HTTP. Real upserts need the gRPC
query+mutations form.

## Deletes

Deletes ride in `delete` blocks (bare bodies are always sets):

```sh
curl -XPOST 'localhost:8080/mutate?commitNow=true' \
  -H 'content-type: application/rdf' -d '{
    delete { <0x2> <age> * . }
  }'
```

Drop whole predicates via alter instead:

```sh
curl -XPOST localhost:8080/alter -d '{"drop_attr": "age"}'
```

`{"drop_all": true}` wipes everything (guardians only under ACL).
