# Limitations (0.1.0)

What is out of scope and tracked for 0.2.0:

- **Embedded DQL**: the embedded handle speaks key-value ops only.
  Graph queries need the standalone servers for now.
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
- **GraphQL**: no JWK fetch, lambda/custom HTTP resolvers, or
  remote schemas.
- **Testing**: no Jepsen runs yet.

Anything else that misbehaves is a bug — report it with the command,
its output, and the server logs.
