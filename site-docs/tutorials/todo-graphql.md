# TODO app with GraphQL

Build a project/task graph with GraphQL mutations, nested queries, and updates.

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
    db.graphql(MUTATION, None, serde_json::json!({}), &headers)?;
    let result = db.graphql(QUERY, None, serde_json::json!({}), &headers)?;
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

    db.graphql(MUTATION)
    const result = JSON.parse(db.graphql(QUERY))
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

    db.graphql(MUTATION)
    result = json.loads(db.graphql(QUERY))
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
