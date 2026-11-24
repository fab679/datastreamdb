# Changelog

## 0.1.0 (unreleased)

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
