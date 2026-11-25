---
title: Tutorials
---

# Tutorials

Complete, tested walkthroughs. Each tutorial is a standalone page with install
steps, schema, data, queries, updates, and expected results.

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

## Choose a tutorial

- [Movies with DQL](movies-dql.md): people, genres as nodes, relationships, cast facets.
- [Movies with GraphQL](movies-graphql.md): SDL, nested mutations, relationship queries, updates.
- [TODO app with DQL](todo-dql.md): projects, tasks, tags, dates, assignment facets.
- [TODO app with GraphQL](todo-graphql.md): GraphQL project/task mutations and queries.
- [Orders with numeric facets](orders-dql.md): line-item facets, nested relationships, reverse lookups.

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
    {"op":"dql_alter","schema":"name: string @index(exact) . acted_in: [uid] @reverse . type Movie { name\nacted_in }"}
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
- GraphQL subscriptions are poll-based; see the
  [DQL and GraphQL reference](../embedding/queries.md).
