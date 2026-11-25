# Changelog

## 0.2.0

Embedded graph mode:

- DQL relationship traversal and nested pattern/filter queries.
- DQL set/delete mutations with local UID allocation.
- Additive/updating schema alters with index rebuilds.
- GraphQL queries and mutations from user SDL.
- Poll-based GraphQL subscriptions with sync and async helpers.
- Schema history, GraphQL SDL, and timestamp recovery persisted for fjall
  directories.
- Rust, TypeScript, Python, and C-envelope APIs; expanded embedding docs.

Embedded graph mode is single-node. Clustering, ACL, namespaces, WebSocket
subscriptions, drop operations, and vector indexes remain server/future work.

## 0.1.0

First packaged release: DataStreamDB ported to Rust.

**Standalone servers** (`zero`, `alpha`):

- DQL queries, mutations (RDF/JSON), upserts over gRPC, schema
  alters, drops, commit/abort.
- GraphQL admin slice at `POST /admin`: `health`, `backup`,
  `export`, `restore`, `addNamespace`, `deleteNamespace`.
- Multi-tenancy: namespaces with isolated schemas and
  guardians/groot per tenant.
- ACL (login, JWT, predicate rules, guardian bootstrap), audit
  log, backup/export encryption.
- Backup/restore/export over local dirs and S3/MinIO (HTTP),
  with manifest chains and drop records.
- `bulk`/`live` loaders, file-sink CDC, multi-zero join.

**Embedded**:

- In-process isolated instances from Rust, TypeScript, Python,
  and C with `put`/`get`/`delete`/`scan`/`batch`/`flush`.
- In-memory and persistent (fjall) backends.

See [Limitations](limitations.md) for what 0.1.0 does not do.
