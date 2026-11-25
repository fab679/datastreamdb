# Limitations (0.2.1)

What is out of scope and tracked for later releases:

- **Embedded graph mode is single-node**: no Raft/zero/worker, ACL, or
  namespaces. Use the standalone servers for those.
- **Embedded DQL scope**: relationship/pattern queries, set/delete mutations,
  and additive schema alters are supported. Drop operations, upserts, and
  query variables are not exposed yet.
- **DQL deletes**: `S P *` and exact-value deletes work. The `S * *` wildcard is
  accepted but not applied in 0.2.1; delete predicates individually. Facets are
  replaced when the edge is set again — there is no separate facet delete.
- **Embedded GraphQL scope**: queries, mutations, and poll-based subscriptions.
  No WebSocket push, `@auth`, `@custom` HTTP/lambda resolvers, remote schemas,
  or vector indexes.
- **Admin epic remainder**: schema/config/user/group mutations,
  drain, shutdown, async backup tasks with task ids,
  all-namespaces export, list-backups.
- **S3 HTTPS**: HTTP endpoints only; TLS needs a rustls build.
  Live-MinIO compatibility is unverified.
- **Search**: no vector/HNSW indexes.
- **CDC**: file sink only (no Kafka); no subscription websocket
  transport.
- **Storage**: no encrypted WAL.
- **Loaders**: no remote live-load, no sharded bulk.
- **GraphQL server extras**: no JWK fetch, lambda/custom HTTP resolvers, or
  remote schemas.
- **Testing**: no Jepsen runs yet.

Anything else that misbehaves is a bug — report it with the command,
its output, and the server logs.
