# Movies with GraphQL

Build the same movie graph through GraphQL SDL, mutations, and nested queries.

## Before you start

Pick a binding. Each tab shows the install, how to open an instance, install
the SDL, and run a GraphQL document. `SDL`, `MUTATION`, and `QUERY` are the
blocks in the sections below.

=== "Rust"

    ```toml
    [dependencies]
    datastreamdb-core = "0.2"
    serde_json = "1"
    ```

    ```rust
    use std::collections::HashMap;
    use datastreamdb_core::{Instance, InstanceConfig};

    let db = Instance::open(InstanceConfig::memory())?;
    db.set_graphql_schema(SDL)?;

    let headers = HashMap::new();
    let result = db.graphql(MUTATION, None, serde_json::json!({}), &headers)?;
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
    db.setGraphqlSchema(SDL)

    const result = JSON.parse(db.graphql(MUTATION))
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
    db.set_graphql_schema(SDL)

    result = json.loads(db.graphql(MUTATION))
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

      exec(db, "{\"op\":\"set_graphql_schema\",\"sdl\":\"...\"}");
      exec(db, "{\"op\":\"graphql\",\"query\":\"...\"}");

      datastreamdb_close(db);
      return 0;
    }
    ```

    The `...` placeholders are the JSON-escaped blocks below. GraphQL execution
    errors arrive in `response.errors`, not as a failed envelope.

The blocks below are the GraphQL documents referenced as `SDL`, `MUTATION`, and
`QUERY`.

## Schema

```graphql
type Movie {
  id: ID!
  title: String @search(by: [term])
  year: Int @search
  rating: Float @search
  director: Person @hasInverse(field: directed)
  actors: [Person] @hasInverse(field: actedIn)
  genres: [Genre] @hasInverse(field: movies)
}
type Person {
  id: ID!
  name: String @id @search(by: [hash])
  directed: [Movie]
  actedIn: [Movie]
}
type Genre {
  id: ID!
  name: String @id @search(by: [hash])
  movies: [Movie]
}
```

```rust
db.set_graphql_schema(SDL)?;
```

## Add a movie with nested people

```graphql
mutation {
  addMovie(input: [{
    title: "The Matrix", year: 1999, rating: 8.7,
    director: {name: "Lana Wachowski"},
    actors: [{name: "Keanu Reeves"}, {name: "Carrie-Anne Moss"}],
    genres: [{name: "Sci-Fi"}]
  }]) {
    movie { id title year rating director { name } actors { name } genres { name } }
    numUids
  }
}
```

## Query relationships

```graphql
{
  queryMovie(filter: {title: {anyofterms: "Matrix"}}) {
    title
    year
    rating
    director { name }
    actors { name }
    genres { name }
  }
}
```

## Update and filter

```graphql
mutation {
  updateMovie(input: {
    filter: {title: {anyofterms: "Matrix"}},
    set: {rating: 8.8}
  }) { numUids }
}
```

```graphql
{ queryMovie(filter: {rating: {ge: 8.8}}) { title rating } }
```

---

## Next

See [all tutorials](index.md) and the [DQL & GraphQL reference](../embedding/queries.md).
