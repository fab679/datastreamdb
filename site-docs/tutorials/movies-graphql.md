# Movies with GraphQL

Build the same movie graph through GraphQL SDL, mutations, and nested queries.

## Before you start

Pick the embedded binding you want to use:

=== "Rust"

    ```toml
    [dependencies]
    datastreamdb-core = "0.2"
    serde_json = "1"
    ```

=== "TypeScript"

    ```sh
    npm install @datastreamdb/core
    ```

=== "Python"

    ```sh
    pip install datastreamdb
    ```

=== "C"

    Download the `libdatastreamdb_ffi.a` / `datastreamdb.h` assets from the
    [v0.2.0 release](https://github.com/fab679/datastreamdb/releases/tag/v0.2.0).

The blocks below are GraphQL documents. In Rust, install the SDL once, then
send documents to the same `graphql` method:

```rust
use std::collections::HashMap;
use datastreamdb_core::{Instance, InstanceConfig};

let db = Instance::open(InstanceConfig::memory())?;
db.set_graphql_schema(SDL)?;

let headers = HashMap::new();
let result = db.graphql(MUTATION, None, serde_json::json!({}), &headers)?;
```

Here `SDL`, `MUTATION`, and `QUERY` name the blocks below. TypeScript and Python use `setGraphqlSchema` / `set_graphql_schema` and
`graphql`; C uses the `set_graphql_schema` and `graphql` envelope ops. GraphQL
execution errors arrive in `result["errors"]`, not as transport errors.

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
