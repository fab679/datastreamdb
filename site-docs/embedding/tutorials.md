# Embedded tutorials

Complete, tested examples for DQL, GraphQL, and embedded instances. Every DQL
and GraphQL example on this page is exercised by
`crates/datastreamdb-core/tests/tutorials.rs`.

## Facets are relationship properties

A facet is a key/value property on an **edge**, not on a node:

```text
_:matrix <acted_in> _:keanu (role="Neo", billing=1) .
```

Here `acted_in` is the relationship, and `role`/`billing` describe that
specific movie→actor edge. Query them with `@facets(...)`:

```text
acted_in @facets(role, billing) { name }
```

Facets are a DQL feature. GraphQL in 0.2.0 does not expose facet syntax; model
relationship properties as nodes or separate predicates if you need them in
GraphQL.

---

## Tutorial 1 — Movies with DQL

### 1. Schema

```text
name: string @index(exact, term) .
year: int @index(int) .
rating: float @index(float) .
genre: [string] @index(exact) .
budget: int .
directed: [uid] @reverse .
acted_in: [uid] @reverse .
type Movie {
    name
    year
    rating
    genre
    budget
    directed
    acted_in
}
type Person {
    name
    directed
    acted_in
}
```

```rust
db.dql_alter(r#"
name: string @index(exact, term) .
year: int @index(int) .
rating: float @index(float) .
genre: [string] @index(exact) .
budget: int .
directed: [uid] @reverse .
acted_in: [uid] @reverse .
type Movie { name year rating genre budget directed acted_in }
type Person { name directed acted_in }
"#)?;
```

!!! note "Type fields need newlines"
    DQL type declarations require one field per line. `type Movie { name year }`
    is rejected; use the multiline form above.

### 2. Add movies, people, relationships, and facets

```text
{
 set {
  _:matrix <name> "The Matrix" .
  _:matrix <year> "1999"^^<xs:int> .
  _:matrix <rating> "8.7"^^<xs:float> .
  _:matrix <genre> "Sci-Fi" .
  _:matrix <budget> "63"^^<xs:int> .
  _:matrix <datastreamdb.type> "Movie" .
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

### 3. Query the movie, cast, and facets

```text
{
 movie(func: eq(name, "The Matrix")) {
  name year rating genre budget
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
    "genre": ["Sci-Fi"],
    "budget": 63,
    "directed": [{ "name": "Lana Wachowski" }],
    "cast": [
      { "name": "Keanu Reeves", "cast|role": "Neo", "cast|billing": 1 },
      { "name": "Carrie-Anne Moss", "cast|role": "Trinity", "cast|billing": 2 }
    ]
  }]
}
```

### 4. Reverse traversal

```text
{ p(func: eq(name, "Keanu Reeves")) { name ~acted_in { name } } }
```

### 5. Filter by a facet

```text
{
 movie(func: eq(name, "The Matrix")) {
  acted_in @facets(gt(billing, 1)) { name }
 }
}
```

Only Carrie-Anne Moss (`billing=2`) is returned.

### 6. Alter the schema after data exists

```text
budget: int @index(int) .
```

```text
{ m(func: ge(budget, 60)) { name budget } }
```

The alter rebuilds the index over existing data, so the new index works
immediately.

---

## Tutorial 2 — Movies with GraphQL

### 1. Schema

```graphql
type Movie {
  id: ID!
  title: String @search(by: [term])
  year: Int @search
  rating: Float @search
  director: Person @hasInverse(field: directed)
  actors: [Person] @hasInverse(field: actedIn)
}
type Person {
  id: ID!
  name: String @id @search(by: [hash])
  directed: [Movie]
  actedIn: [Movie]
}
```

```js
db.setGraphqlSchema(`...`)
```

### 2. Add a movie with nested people

```graphql
mutation {
  addMovie(input: [{
    title: "The Matrix", year: 1999, rating: 8.7,
    director: {name: "Lana Wachowski"},
    actors: [{name: "Keanu Reeves"}, {name: "Carrie-Anne Moss"}]
  }]) {
    movie { id title year rating director { name } actors { name } }
    numUids
  }
}
```

### 3. Query relationships

```graphql
{
  queryMovie(filter: {title: {anyofterms: "Matrix"}}) {
    title
    year
    rating
    director { name }
    actors { name }
  }
}
```

### 4. Update and filter

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

## Tutorial 3 — TODO app with DQL

### 1. Schema

```text
name: string @index(exact, term) .
status: string @index(exact) .
priority: int @index(int) .
due: datetime .
estimate: int @index(int) .
belongs_to: [uid] @reverse .
tagged: [uid] @reverse .
assigned_to: [uid] @reverse .
type Task {
    name
    status
    priority
    due
    estimate
    belongs_to
    tagged
    assigned_to
}
type Project {
    name
    tasks
}
type Tag {
    name
}
type Person {
    name
    assigned
}
```

### 2. Add projects, tasks, tags, assignments, and facets

```text
{
 set {
  _:proj <name> "Website launch" .
  _:proj <datastreamdb.type> "Project" .
  _:task1 <name> "Write release notes" .
  _:task1 <status> "todo" .
  _:task1 <priority> "2"^^<xs:int> .
  _:task1 <due> "2026-11-30T17:00:00Z"^^<xs:dateTime> .
  _:task1 <estimate> "3"^^<xs:int> .
  _:task1 <belongs_to> _:proj .
  _:task1 <datastreamdb.type> "Task" .
  _:task2 <name> "Deploy staging" .
  _:task2 <status> "doing" .
  _:task2 <priority> "1"^^<xs:int> .
  _:task2 <estimate> "2"^^<xs:int> .
  _:task2 <belongs_to> _:proj .
  _:task2 <datastreamdb.type> "Task" .
  _:ada <name> "Ada" .
  _:ada <datastreamdb.type> "Person" .
  _:task1 <assigned_to> _:ada (role="owner", since="2026-11-01") .
  _:task2 <assigned_to> _:ada (role="reviewer", since="2026-11-05") .
  _:urgent <name> "urgent" .
  _:urgent <datastreamdb.type> "Tag" .
  _:task1 <tagged> _:urgent .
 }
}
```

`assigned_to` has facets `role` and `since`; `since` is a string here, which is
fine for a facet.

### 3. Query tasks with relationships and facets

```text
{
 tasks(func: type(Task)) {
  name status priority due estimate
  belongs_to { name }
  tagged { name }
  assigned_to @facets(role, since) { name }
 }
}
```

### 4. Filter and reverse

```text
{ t(func: eq(status, "doing")) { name priority } }
{ p(func: eq(name, "Website launch")) { name ~belongs_to { name } } }
```

### 5. Add an indexed field later

```text
estimate: int @index(int) .
```

```text
{ t(func: ge(estimate, 3)) { name estimate } }
```

### Datetime notes

- Use `datetime` for dates; there is no separate `date` type.
- RFC3339 literals: `"2026-11-30T17:00:00Z"^^<xs:dateTime>`.
- Datetime index tokenizers are `year`, `month`, `day`, and `hour` (there is no
  exact `datetime` tokenizer in 0.2.0).

---

## Tutorial 4 — TODO app with GraphQL

### Schema

```graphql
type Task {
  id: ID!
  title: String @search(by: [term])
  status: String @search(by: [exact])
  priority: Int @search
  due: DateTime
  project: Project @hasInverse(field: tasks)
  assignee: Person @hasInverse(field: assigned)
}
type Project {
  id: ID!
  name: String @id @search(by: [hash])
  tasks: [Task]
}
type Person {
  id: ID!
  name: String @id @search(by: [hash])
  assigned: [Task]
}
```

### Add project, person, and task

```graphql
mutation { addProject(input: [{name: "Website launch"}]) { numUids } }
```

```graphql
mutation { addPerson(input: [{name: "Ada"}]) { numUids } }
```

```graphql
mutation {
  addTask(input: [{
    title: "Write release notes", status: "todo", priority: 2,
    due: "2026-11-30T17:00:00Z",
    project: {name: "Website launch"},
    assignee: {name: "Ada"}
  }]) {
    task { title status priority due project { name } assignee { name } }
    numUids
  }
}
```

### Query and update

```graphql
{
  queryTask(filter: {status: {eq: "todo"}}) {
    title status priority due
    project { name }
    assignee { name }
  }
}
```

```graphql
mutation {
  updateTask(input: {
    filter: {title: {anyofterms: "release"}},
    set: {status: "doing"}
  }) { numUids }
}
```

```graphql
{ queryTask(filter: {status: {eq: "doing"}}) { title status } }
```

---

## Tutorial 5 — Orders with numeric facets

This shows facets on a line-item edge: quantity and price belong to the
order→product relationship.

### Schema

```text
name: string @index(exact) .
email: string @index(exact) .
sku: string @index(exact) .
price: float @index(float) .
quantity: int @index(int) .
ordered: [uid] @reverse .
type Customer { name email orders }
type Product { name sku price }
type Order { name ordered }
```

### Data

```text
{
 set {
  _:ada <name> "Ada" .
  _:ada <email> "ada@example.com" .
  _:ada <datastreamdb.type> "Customer" .
  _:laptop <name> "Laptop" .
  _:laptop <sku> "LAP-1" .
  _:laptop <price> "999.00"^^<xs:float> .
  _:laptop <datastreamdb.type> "Product" .
  _:mouse <name> "Mouse" .
  _:mouse <sku> "MOU-1" .
  _:mouse <price> "25.00"^^<xs:float> .
  _:mouse <datastreamdb.type> "Product" .
  _:order <name> "Order #1001" .
  _:order <datastreamdb.type> "Order" .
  _:ada <ordered> _:order .
  _:order <ordered> _:laptop (quantity=1, price=999.00) .
  _:order <ordered> _:mouse (quantity=2, price=25.00) .
 }
}
```

### Query nested relationships and facets

```text
{
 c(func: eq(name, "Ada")) {
  name email
  ordered @facets(quantity, price) {
   name
   ordered @facets(quantity, price) { name sku price }
  }
 }
}
```

### Reverse lookup

```text
{ p(func: eq(sku, "LAP-1")) { name ~ordered { name } } }
```

---

## Embedded instance examples

The same DQL works through the embedded API:

=== "Rust"

    ```rust
    let db = datastreamdb_core::Instance::open(
        datastreamdb_core::InstanceConfig::memory()
    )?;
    db.dql_alter(SCHEMA)?;
    db.dql_mutate(MOVIES)?;
    let result = db.dql_query(r#"
      { movie(func: eq(name, "The Matrix")) {
          name cast: acted_in @facets(role, billing) { name }
      } }
    "#)?;
    ```

=== "TypeScript"

    ```js
    const db = new Instance('{}')
    db.dqlAlter(SCHEMA)
    db.dqlMutate(MOVIES)
    const result = JSON.parse(db.dqlQuery(`
      { movie(func: eq(name, "The Matrix")) {
          name cast: acted_in @facets(role, billing) { name }
      } }
    `))
    ```

=== "Python"

    ```python
    db = Instance("{}")
    db.dql_alter(SCHEMA)
    db.dql_mutate(MOVIES)
    result = json.loads(db.dql_query("""
      { movie(func: eq(name, "The Matrix")) {
          name cast: acted_in @facets(role, billing) { name }
      } }
    """))
    ```

=== "C"

    ```json
    {"op":"dql_alter","schema":"name: string @index(exact) . acted_in: [uid] @reverse . type Movie { name acted_in }"}
    {"op":"dql_mutate","query":"{ set { _:m <name> \"The Matrix\" .\n_:k <name> \"Keanu Reeves\" .\n_:m <acted_in> _:k (role=\"Neo\", billing=1) .\n} }"}
    {"op":"dql_query","query":"{ m(func: eq(name, \"The Matrix\")) { name acted_in @facets(role, billing) { name } } }"}
    ```

## Common gotchas

- DQL type declarations need **one field per line**.
- Mutations need a newline or `#` after the final `.` before `}`.
- Scalar predicates auto-derive; UID relationships, reverse edges, indexes, and
  types need schema.
- `type(Task)` requires `datastreamdb.type` edges unless the data came from a
  GraphQL mutation.
- GraphQL subscriptions are poll-based; see [DQL & GraphQL](queries.md).
