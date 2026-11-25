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

## How the tutorials are built

Each tutorial opens with a complete program for Python, TypeScript, and Rust:
install, create an instance, apply the schema, add data, run a query, and print
the result. The C tab shows the ABI lifecycle and links to the full C guide.

The per-language API is covered in [Embedding](../embedding/index.md); the
language and type reference is in [DQL and GraphQL](../embedding/queries.md).

## Common gotchas

- DQL type declarations need **one field per line**.
- Mutations need a newline or `#` after the final `.` before `}`.
- Scalar predicates auto-derive; UID relationships, reverse edges, indexes, and
  types need schema.
- `type(Task)` requires `datastreamdb.type` edges unless the data came from a
  GraphQL mutation.
- GraphQL subscriptions are poll-based; see the
  [DQL and GraphQL reference](../embedding/queries.md).
