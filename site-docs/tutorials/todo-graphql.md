# TODO app with GraphQL

Build a project/task graph with GraphQL mutations, nested queries, and updates.

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
send mutations and queries to `graphql`:

```rust
use std::collections::HashMap;
use datastreamdb_core::{Instance, InstanceConfig};

let db = Instance::open(InstanceConfig::memory())?;
db.set_graphql_schema(SDL)?;

let headers = HashMap::new();
db.graphql(MUTATION, None, serde_json::json!({}), &headers)?;
let result = db.graphql(QUERY, None, serde_json::json!({}), &headers)?;
```

Here `SDL`, `MUTATION`, and `QUERY` name the blocks below. TypeScript and Python use `setGraphqlSchema` / `set_graphql_schema` and
`graphql`; C uses the envelope ops.

## Schema

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

## Add project, person, and task

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

## Query and update

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

## Next

See [all tutorials](index.md) and the [DQL & GraphQL reference](../embedding/queries.md).
