# GraphQL data layer

Status in 0.1.0: the GraphQL engine ships as **libraries**
(`datastreamdb-graphql`), not as a served endpoint. There is no
`/graphql` route yet — only the [Admin API](admin.md) speaks
GraphQL over HTTP today.

What the crates do:

- **Schema generation**: ingest a GraphQL schema, complete it the
  way the upstream implementation does, and emit the generated
  DataStreamDB schema + SDL.
- **`@auth` rules**: JWT validation (HS/RS) plus static and RBAC
  rule evaluation for queries.
- **Resolvers**: GraphQL operations rewrite to DQL and execute
  against `datastreamdb-query` (queries, mutations with conditions).
- **Subscriptions**: a poll-based hub core. The websocket
  transport is deferred, so nothing serves subscriptions yet.

Rustdoc on `datastreamdb-graphql` is the API reference. Serving the data
layer over HTTP/websocket is tracked for 0.2.0.
