# Movies with GraphQL

Build the same movie graph through GraphQL SDL, mutations, and nested queries.

## Step by step

The Python, TypeScript, and Rust tabs are complete programs: install, open an
instance, install the SDL, add a movie with nested people and genres, then
query it. The C tab shows the ABI lifecycle; the
[C guide](../embedding/c.md) has the full envelope example.

=== "Python"

    ```sh
    pip install datastreamdb
    ```

    ```python
    import json
    from datastreamdb import Instance

    db = Instance("{}")

    # 1. Schema
    db.set_graphql_schema("""
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
    """)

    # 2. Add a movie
    added = json.loads(db.graphql("""
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
    """))
    print(json.dumps(added, indent=2))

    # 3. Query it
    result = json.loads(db.graphql("""
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
    """))
    print(json.dumps(result, indent=2))
    ```

    For a persistent instance, replace `Instance("{}")` with
    `Instance(json.dumps({"data_dir": "./data"}))`.

=== "TypeScript"

    ```sh
    npm install @datastreamdb/core
    ```

    ```js
    import { Instance } from '@datastreamdb/core'

    const db = new Instance('{}')

    // 1. Schema
    db.setGraphqlSchema(`
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
    `)

    // 2. Add a movie
    const added = JSON.parse(db.graphql(`
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
    `))
    console.log(JSON.stringify(added, null, 2))

    // 3. Query it
    const result = JSON.parse(db.graphql(`
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
    `))
    console.log(JSON.stringify(result, null, 2))
    ```

    For a persistent instance, pass
    `JSON.stringify({ data_dir: './data' })` to `new Instance(...)`.

=== "Rust"

    ```toml
    [dependencies]
    datastreamdb-core = "0.2"
    serde_json = "1"
    ```

    ```rust
    use std::collections::HashMap;
    use datastreamdb_core::{Instance, InstanceConfig};

    fn main() -> Result<(), Box<dyn std::error::Error>> {
        let db = Instance::open(InstanceConfig::memory())?;

        // 1. Schema
        db.set_graphql_schema(r#"
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
    "#)?;

        let headers = HashMap::new();

        // 2. Add a movie
        let added = db.graphql(
            r#"mutation {
      addMovie(input: [{
        title: "The Matrix", year: 1999, rating: 8.7,
        director: {name: "Lana Wachowski"},
        actors: [{name: "Keanu Reeves"}, {name: "Carrie-Anne Moss"}],
        genres: [{name: "Sci-Fi"}]
      }]) {
        movie { id title year rating director { name } actors { name } genres { name } }
        numUids
      }
    }"#,
            None,
            serde_json::json!({}),
            &headers,
        )?;
        println!("{added}");

        // 3. Query it
        let result = db.graphql(
            r#"{
      queryMovie(filter: {title: {anyofterms: "Matrix"}}) {
        title
        year
        rating
        director { name }
        actors { name }
        genres { name }
      }
    }"#,
            None,
            serde_json::json!({}),
            &headers,
        )?;
        println!("{result}");
        Ok(())
    }
    ```

    For a persistent instance, open with
    `InstanceConfig::from_json(r#"{"data_dir": "./data"}"#)?`.

=== "C"

    Download `libdatastreamdb_ffi.a` and `datastreamdb.h` from the
    [v0.2.1 release](https://github.com/fab679/datastreamdb/releases/tag/v0.2.1).

    ```c
    datastreamdb_handle_t *db = NULL;
    datastreamdb_open("{}", &db);

    /* Build these envelopes from the GraphQL documents in the steps below. */
    exec(db, "{\"op\":\"set_graphql_schema\",\"sdl\":\"...\"}");
    exec(db, "{\"op\":\"graphql\",\"query\":\"...\"}");

    datastreamdb_close(db);
    ```

    GraphQL execution errors arrive in `response.errors`, not as a failed
    envelope. The [C embedding guide](../embedding/c.md) has the complete
    `exec` helper and buffer ownership rules.

## Step 1: schema

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

This is the SDL used in the complete program above.

## Step 2: add a movie with nested people

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

## Step 3: query relationships

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

## Step 4: update and filter

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
