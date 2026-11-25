# Movies with DQL

Model a movie catalog with people, genres as nodes, relationships, and edge facets.

## Before you start

Pick a binding. Each tab shows the install, how to open an instance, and the
three calls this tutorial uses. `SCHEMA`, `DATA`, and `QUERY` are the blocks in
the sections below.

=== "Rust"

    ```toml
    [dependencies]
    datastreamdb-core = "0.2"
    serde_json = "1"
    ```

    ```rust
    use datastreamdb_core::{Instance, InstanceConfig};

    let db = Instance::open(InstanceConfig::memory())?;

    db.dql_alter(SCHEMA)?;
    db.dql_mutate(DATA)?;
    let result = db.dql_query(QUERY)?;
    println!("{result}");
    ```

    For a persistent instance, open with
    `InstanceConfig::from_json(r#"{"data_dir": "./data"}"#)?`.

=== "TypeScript"

    ```sh
    npm install @datastreamdb/core
    ```

    ```js
    import { Instance } from '@datastreamdb/core'

    const db = new Instance('{}')

    db.dqlAlter(SCHEMA)
    db.dqlMutate(DATA)
    const result = JSON.parse(db.dqlQuery(QUERY))
    console.log(result)
    ```

    For a persistent instance, pass
    `JSON.stringify({ data_dir: './data' })` to `new Instance(...)`.

=== "Python"

    ```sh
    pip install datastreamdb
    ```

    ```python
    import json
    from datastreamdb import Instance

    db = Instance("{}")

    db.dql_alter(SCHEMA)
    db.dql_mutate(DATA)
    result = json.loads(db.dql_query(QUERY))
    print(result)
    ```

    For a persistent instance, pass
    `json.dumps({"data_dir": "./data"})` to `Instance(...)`.

=== "C"

    Download `libdatastreamdb_ffi.a` and `datastreamdb.h` from the
    [v0.2.0 release](https://github.com/fab679/datastreamdb/releases/tag/v0.2.0).

    ```c
    #include <stdio.h>
    #include <string.h>
    #include "datastreamdb.h"

    static int exec(datastreamdb_handle_t *db, const char *request) {
      uint8_t *out = NULL;
      size_t len = 0;
      int rc = datastreamdb_execute(db, (const uint8_t *)request,
                                    strlen(request), &out, &len);
      if (rc == DATASTREAMDB_OK) {
        fwrite(out, 1, len, stdout);
        datastreamdb_free_bytes(out, len);
      }
      return rc;
    }

    int main(void) {
      datastreamdb_handle_t *db = NULL;
      datastreamdb_open("{}", &db);

      exec(db, "{\"op\":\"dql_alter\",\"schema\":\"...\"}");
      exec(db, "{\"op\":\"dql_mutate\",\"query\":\"...\"}");
      exec(db, "{\"op\":\"dql_query\",\"query\":\"...\"}");

      datastreamdb_close(db);
      return 0;
    }
    ```

    The `...` placeholders are the JSON-escaped blocks below. Use a JSON
    library to build them rather than escaping by hand.

The blocks below are the DQL documents referenced as `SCHEMA`, `DATA`, and
`QUERY`.

## Schema

```text
name: string @index(exact, term) .
year: int @index(int) .
rating: float @index(float) .
budget: int .
directed: [uid] @reverse .
acted_in: [uid] @reverse .
genre: [uid] @reverse .
type Movie {
    name
    year
    rating
    budget
    directed
    acted_in
    genre
}
type Person {
    name
    directed
    acted_in
}
type Genre {
    name
}
```

Genres are **nodes**, not string properties: a genre can be shared by many
movies, queried on its own, and traversed in reverse. The `genre` predicate is
a UID relationship; `@reverse` maintains the incoming edge, so `~genre`
traverses from a genre back to its movies.

```rust
db.dql_alter(SCHEMA)?;
```

!!! note "Type fields need newlines"
    DQL type declarations require one field per line. `type Movie { name year }`
    is rejected; use the multiline form above.

## Add movies, people, relationships, and facets

```text
{
 set {
  _:matrix <name> "The Matrix" .
  _:matrix <year> "1999"^^<xs:int> .
  _:matrix <rating> "8.7"^^<xs:float> .
  _:matrix <budget> "63"^^<xs:int> .
  _:matrix <datastreamdb.type> "Movie" .
  _:scifi <name> "Sci-Fi" .
  _:scifi <datastreamdb.type> "Genre" .
  _:matrix <genre> _:scifi .
  _:keanu <name> "Keanu Reeves" .
  _:keanu <datastreamdb.type> "Person" .
  _:carrie <name> "Carrie-Anne Moss" .
  _:carrie <datastreamdb.type> "Person" .
  _:lana <name> "Lana Wachowski" .
  _:lana <datastreamdb.type> "Person" .
  _:matrix <directed> _:lana .
  _:matrix <acted_in> _:keanu (role="Neo", billing=1) .
  _:matrix <acted_in> _:carrie (role="Trinity", billing=2) .
 }
}
```

`datastreamdb.type` is required for `type(Movie)` roots. GraphQL mutations set
it automatically.

## Query the movie, cast, and facets

```text
{
 movie(func: eq(name, "The Matrix")) {
  name year rating budget
  genre { name }
  directed { name }
  cast: acted_in @facets(role, billing) { name }
 }
}
```

Response shape:

```json
{
  "movie": [{
    "name": "The Matrix",
    "year": 1999,
    "rating": 8.7,
    "genre": [{ "name": "Sci-Fi" }],
    "budget": 63,
    "directed": [{ "name": "Lana Wachowski" }],
    "cast": [
      { "name": "Keanu Reeves", "cast|role": "Neo", "cast|billing": 1 },
      { "name": "Carrie-Anne Moss", "cast|role": "Trinity", "cast|billing": 2 }
    ]
  }]
}
```

## Reverse traversal

```text
{ p(func: eq(name, "Keanu Reeves")) { name ~acted_in { name } } }
```

Genres are nodes, so you can query the genre and traverse back to its movies:

```text
{ g(func: eq(name, "Sci-Fi")) { name ~genre { name year } } }
```

## Filter by a facet

```text
{
 movie(func: eq(name, "The Matrix")) {
  acted_in @facets(gt(billing, 1)) { name }
 }
}
```

Only Carrie-Anne Moss (`billing=2`) is returned.

## Alter the schema after data exists

```text
budget: int @index(int) .
```

```text
{ m(func: ge(budget, 60)) { name budget } }
```

The alter rebuilds the index over existing data, so the new index works
immediately.

---

## Next

See [all tutorials](index.md) and the [DQL & GraphQL reference](../embedding/queries.md).
