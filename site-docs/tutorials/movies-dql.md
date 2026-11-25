# Movies with DQL

Model a movie catalog with people, genres as nodes, relationships, and edge facets.

## Step by step

The Python, TypeScript, and Rust tabs are complete programs: install, open an
instance, apply the schema, add the data, run the query, and print the result.
The C tab shows the ABI lifecycle; the [C guide](../embedding/c.md) has the full
envelope example.

=== "Python"

    ```sh
    pip install datastreamdb
    ```

    ```python
    import json
    from datastreamdb import Instance

    db = Instance("{}")

    # 1. Schema
    db.dql_alter("""
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
    """)

    # 2. Data
    db.dql_mutate("""
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
    """)

    # 3. Query
    result = json.loads(db.dql_query("""
    {
     movie(func: eq(name, "The Matrix")) {
      name year rating budget
      genre { name }
      directed { name }
      cast: acted_in @facets(role, billing) { name }
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
    db.dqlAlter(`
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
    `)

    // 2. Data
    db.dqlMutate(`
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
    `)

    // 3. Query
    const result = JSON.parse(db.dqlQuery(`
    {
     movie(func: eq(name, "The Matrix")) {
      name year rating budget
      genre { name }
      directed { name }
      cast: acted_in @facets(role, billing) { name }
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
    use datastreamdb_core::{Instance, InstanceConfig};

    fn main() -> Result<(), Box<dyn std::error::Error>> {
        let db = Instance::open(InstanceConfig::memory())?;

        // 1. Schema
        db.dql_alter(r#"
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
    "#)?;

        // 2. Data
        db.dql_mutate(r#"
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
    "#)?;

        // 3. Query
        let result = db.dql_query(r#"
    {
     movie(func: eq(name, "The Matrix")) {
      name year rating budget
      genre { name }
      directed { name }
      cast: acted_in @facets(role, billing) { name }
     }
    }
    "#)?;
        println!("{result}");
        Ok(())
    }
    ```

    For a persistent instance, open with
    `InstanceConfig::from_json(r#"{"data_dir": "./data"}"#)?`.

=== "C"

    Download `libdatastreamdb_ffi.a` and `datastreamdb.h` from the
    [v0.2.0 release](https://github.com/fab679/datastreamdb/releases/tag/v0.2.0).

    ```c
    datastreamdb_handle_t *db = NULL;
    datastreamdb_open("{}", &db);

    /* Each request is one JSON envelope. Use a JSON library to build them
       from the DQL strings in the steps below. */
    exec(db, "{\"op\":\"dql_alter\",\"schema\":\"...\"}");
    exec(db, "{\"op\":\"dql_mutate\",\"query\":\"...\"}");
    exec(db, "{\"op\":\"dql_query\",\"query\":\"...\"}");

    datastreamdb_close(db);
    ```

    The [C embedding guide](../embedding/c.md) has the complete `exec` helper,
    status codes, and buffer ownership rules.

## Step 1: schema

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

This is the schema used in the complete program above. Genres are **nodes**,
not string properties: a genre can be shared by many movies, queried on its
own, and traversed in reverse. The `genre` predicate is a UID relationship;
`@reverse` maintains the incoming edge, so `~genre` traverses from a genre back
to its movies.

!!! note "Type fields need newlines"
    DQL type declarations require one field per line. `type Movie { name year }`
    is rejected; use the multiline form above.

## Step 2: add movies, people, relationships, and facets

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

## Step 3: query the movie, cast, and facets

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

Run this with the same `dql_query` call from step 3.

```text
{ p(func: eq(name, "Keanu Reeves")) { name ~acted_in { name } } }
```

Genres are nodes, so you can query the genre and traverse back to its movies:

```text
{ g(func: eq(name, "Sci-Fi")) { name ~genre { name year } } }
```

## Filter by a facet

Run this with the same `dql_query` call from step 3.

```text
{
 movie(func: eq(name, "The Matrix")) {
  acted_in @facets(gt(billing, 1)) { name }
 }
}
```

Only Carrie-Anne Moss (`billing=2`) is returned.

## Alter the schema after data exists

Run this with `dql_alter` on the same instance.

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
