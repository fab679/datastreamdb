# GraphQL data layer

Status in 0.2.0: the GraphQL engine ships as a library
(`datastreamdb-graphql`) and is exposed through embedded instances
(`set_graphql_schema` / `graphql` / `graphql_subscribe`). The standalone
server has no public `/graphql` route — the
[Admin API](admin.md) speaks GraphQL over HTTP. See
[DQL and GraphQL](../embedding/queries.md) for embedded examples.

What the crates do:

- **Schema generation**: ingest a GraphQL schema, complete it the
  way the upstream implementation does, and emit the generated
  DataStreamDB schema + SDL.
- **`@auth` rules**: JWT validation (HS/RS) plus static and RBAC
  rule evaluation for queries.
- **Resolvers**: GraphQL operations rewrite to DQL and execute
  against `datastreamdb-query` (queries, mutations with conditions).
- **Subscriptions**: a poll-based hub. Embedded instances expose it as a
  handle (`graphql_subscribe` / `graphql_next`); the WebSocket transport is
  not implemented.

Rustdoc on `datastreamdb-graphql` is the API reference.
