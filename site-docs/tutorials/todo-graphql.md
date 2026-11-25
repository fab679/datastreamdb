# TODO app with GraphQL

Build a project/task graph with GraphQL mutations, nested queries, and updates.

## Step by step

The Python, TypeScript, and Rust tabs are complete programs: install, open an
instance, install the SDL, add a project, a person, and a task, query it,
update it, and query again. The C tab shows the ABI lifecycle; the
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
    """)

    # 2. Add a project and a person
    db.graphql('mutation { addProject(input: [{name: "Website launch"}]) { numUids } }')
    db.graphql('mutation { addPerson(input: [{name: "Ada"}]) { numUids } }')

    # 3. Add a task with nested relationships
    added = json.loads(db.graphql("""
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
    """))
    print(json.dumps(added, indent=2))

    # 4. Query it
    result = json.loads(db.graphql("""
    {
      queryTask(filter: {status: {eq: "todo"}}) {
        title status priority due
        project { name }
        assignee { name }
      }
    }
    """))
    print(json.dumps(result, indent=2))

    # 5. Update it and query again
    db.graphql("""
    mutation {
      updateTask(input: {
        filter: {title: {anyofterms: "release"}},
        set: {status: "doing"}
      }) { numUids }
    }
    """)
    done = json.loads(db.graphql("""
    { queryTask(filter: {status: {eq: "doing"}}) { title status } }
    """))
    print(json.dumps(done, indent=2))
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
    `)

    // 2. Add a project and a person
    db.graphql('mutation { addProject(input: [{name: "Website launch"}]) { numUids } }')
    db.graphql('mutation { addPerson(input: [{name: "Ada"}]) { numUids } }')

    // 3. Add a task with nested relationships
    const added = JSON.parse(db.graphql(`
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
    `))
    console.log(JSON.stringify(added, null, 2))

    // 4. Query it
    const result = JSON.parse(db.graphql(`
    {
      queryTask(filter: {status: {eq: "todo"}}) {
        title status priority due
        project { name }
        assignee { name }
      }
    }
    `))
    console.log(JSON.stringify(result, null, 2))

    // 5. Update it and query again
    db.graphql(`
    mutation {
      updateTask(input: {
        filter: {title: {anyofterms: "release"}},
        set: {status: "doing"}
      }) { numUids }
    }
    `)
    const done = JSON.parse(db.graphql(`
    { queryTask(filter: {status: {eq: "doing"}}) { title status } }
    `))
    console.log(JSON.stringify(done, null, 2))
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
    "#)?;

        let headers = HashMap::new();

        // 2. Add a project and a person
        db.graphql(
            r#"mutation { addProject(input: [{name: "Website launch"}]) { numUids } }"#,
            None,
            serde_json::json!({}),
            &headers,
        )?;
        db.graphql(
            r#"mutation { addPerson(input: [{name: "Ada"}]) { numUids } }"#,
            None,
            serde_json::json!({}),
            &headers,
        )?;

        // 3. Add a task with nested relationships
        let added = db.graphql(
            r#"mutation {
      addTask(input: [{
        title: "Write release notes", status: "todo", priority: 2,
        due: "2026-11-30T17:00:00Z",
        project: {name: "Website launch"},
        assignee: {name: "Ada"}
      }]) {
        task { title status priority due project { name } assignee { name } }
        numUids
      }
    }"#,
            None,
            serde_json::json!({}),
            &headers,
        )?;
        println!("{added}");

        // 4. Query it
        let result = db.graphql(
            r#"{
      queryTask(filter: {status: {eq: "todo"}}) {
        title status priority due
        project { name }
        assignee { name }
      }
    }"#,
            None,
            serde_json::json!({}),
            &headers,
        )?;
        println!("{result}");

        // 5. Update it and query again
        db.graphql(
            r#"mutation {
      updateTask(input: {
        filter: {title: {anyofterms: "release"}},
        set: {status: "doing"}
      }) { numUids }
    }"#,
            None,
            serde_json::json!({}),
            &headers,
        )?;
        let done = db.graphql(
            r#"{ queryTask(filter: {status: {eq: "doing"}}) { title status } }"#,
            None,
            serde_json::json!({}),
            &headers,
        )?;
        println!("{done}");
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

This is the SDL used in the complete program above.

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

## Step 2: add project, person, and task

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

## Steps 3 to 5: query and update

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
