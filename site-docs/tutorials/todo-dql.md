# TODO app with DQL

Model projects, tasks, tags, assignments, dates, and relationship facets.

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
    }
    type Tag {
        name
    }
    type Person {
        name
    }
    """)

    # 2. Data
    db.dql_mutate("""
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
    """)

    # 3. Query
    result = json.loads(db.dql_query("""
    {
     tasks(func: type(Task)) {
      name status priority due estimate
      belongs_to { name }
      tagged { name }
      assigned_to @facets(role, since) { name }
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
    }
    type Tag {
      name
    }
    type Person {
      name
    }
    `)

    // 2. Data
    db.dqlMutate(`
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
    `)

    // 3. Query
    const result = JSON.parse(db.dqlQuery(`
    {
     tasks(func: type(Task)) {
      name status priority due estimate
      belongs_to { name }
      tagged { name }
      assigned_to @facets(role, since) { name }
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
    }
    type Tag {
        name
    }
    type Person {
        name
    }
    "#)?;

        // 2. Data
        db.dql_mutate(r#"
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
    "#)?;

        // 3. Query
        let result = db.dql_query(r#"
    {
     tasks(func: type(Task)) {
      name status priority due estimate
      belongs_to { name }
      tagged { name }
      assigned_to @facets(role, since) { name }
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

    /* Build these envelopes from the DQL strings in the steps below. */
    exec(db, "{\"op\":\"dql_alter\",\"schema\":\"...\"}");
    exec(db, "{\"op\":\"dql_mutate\",\"query\":\"...\"}");
    exec(db, "{\"op\":\"dql_query\",\"query\":\"...\"}");

    datastreamdb_close(db);
    ```

    The [C embedding guide](../embedding/c.md) has the complete `exec` helper,
    status codes, and buffer ownership rules.

## Step 1: schema

This is the schema used in the complete program above.

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
}
type Tag {
    name
}
type Person {
    name
}
```

## Step 2: add projects, tasks, tags, assignments, and facets

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

## Step 3: query tasks with relationships and facets

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

## Filter and reverse

Run these with the same `dql_query` call from step 3.

```text
{ t(func: eq(status, "doing")) { name priority } }
{ p(func: eq(name, "Website launch")) { name ~belongs_to { name } } }
```

## Add an indexed field later

Run the schema line with `dql_alter`, then query it with `dql_query`.

```text
estimate: int @index(int) .
```

```text
{ t(func: ge(estimate, 3)) { name estimate } }
```

## Datetime notes

- Use `datetime` for dates; there is no separate `date` type.
- RFC3339 literals: `"2026-11-30T17:00:00Z"^^<xs:dateTime>`.
- Datetime index tokenizers are `year`, `month`, `day`, and `hour` (there is no
  exact `datetime` tokenizer in 0.2.0).

---

## Next

See [all tutorials](index.md) and the [DQL & GraphQL reference](../embedding/queries.md).
