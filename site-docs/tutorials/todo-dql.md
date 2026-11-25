# TODO app with DQL

Model projects, tasks, tags, assignments, dates, and relationship facets.

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

## Add projects, tasks, tags, assignments, and facets

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

## Query tasks with relationships and facets

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

```text
{ t(func: eq(status, "doing")) { name priority } }
{ p(func: eq(name, "Website launch")) { name ~belongs_to { name } } }
```

## Add an indexed field later

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
