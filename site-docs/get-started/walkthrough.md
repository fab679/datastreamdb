# Movie graph walkthrough

Build a tiny movie database end to end: schema, data, queries, variables,
backup, and delete. Every command runs against the [Quickstart](quickstart.md)
cluster and is tested in CI against a local zero and alpha.

## 1. Schema

```sh
curl -XPOST localhost:8080/alter -d '
name: string @index(exact, trigram) .
year: int @index(int) .
starring: [uid] @reverse .
director: uid @reverse .
datastreamdb.type: [string] @index(exact) .
type Movie {
  name
  year
  starring
  director
}
type Person {
  name
}'
```

## 2. Load movies

```sh
curl -XPOST 'localhost:8080/mutate?commitNow=true' \
  -H 'content-type: application/rdf' -d '
_:m1 <name> "The Matrix" .
_:m1 <year> "1999"^^<xs:int> .
_:m1 <datastreamdb.type> "Movie" .
_:m1 <starring> _:k .
_:m1 <starring> _:l .
_:k <name> "Keanu Reeves" .
_:k <datastreamdb.type> "Person" .
_:l <name> "Laurence Fishburne" .
_:l <datastreamdb.type> "Person" .
_:m2 <name> "John Wick" .
_:m2 <year> "2014"^^<xs:int> .
_:m2 <datastreamdb.type> "Movie" .
_:m2 <starring> _:k .
'
```

## 3. Query it

Keanu's movies through the reverse edge:

```sh
curl -XPOST localhost:8080/query -d '{
  keanu(func: eq(name, "Keanu Reeves")) {
    name
    ~starring { name year }
  }
}'
```

Movies after 2000, ordered by year:

```sh
curl -XPOST localhost:8080/query -d '{
  recent(func: gt(year, 2000), orderasc: year) {
    name year
    starring { name }
  }
}'
```

## 4. Query with variables

Collect uids in one block, reuse them in another:

```sh
curl -XPOST localhost:8080/query -d '{
  var(func: has(year)) { m as uid }
  movies(func: uid(m)) @filter(type(Movie)) { name year }
}'
```

Conditional writes (`cond` upserts) guard on these same counts —
see [DQL upserts](../guides/dql.md#upserts).

## 5. Back up and export

```sh
curl -XPOST localhost:8080/admin -d \
  '{"query":"mutation { backup(input: {destination: \"/tmp/movie-bak\"}) { response { code message } } }"}'
curl -XPOST localhost:8080/admin -d \
  '{"query":"mutation { export(input: {format: \"rdf\", destination: \"/tmp/movie-exp\"}) { response { code message } }"}'
ls /tmp/movie-bak /tmp/movie-exp
```

## 6. Delete something

```sh
curl -XPOST 'localhost:8080/mutate?commitNow=true' \
  -H 'content-type: application/rdf' -d '{
    delete { <0x3> <year> * . }
  }'
```

(Use a real uid from step 3 — `0x3` is illustrative. Deletes need
the `delete` block; bare bodies are always sets.)

You now know the whole loop: [DQL](../guides/dql.md) for the language,
[Admin](../guides/admin.md) for operations, [Security](../guides/security.md) to lock
it down.
