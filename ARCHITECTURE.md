# ARCHITECTURE

## Hosting model

`datastreamdb-core::Instance` is the unit of embedding. A host process opens any number of
instances; each owns its storage backend, schema state, caches, and executors. Instances
share nothing except immutable code and the process-wide metrics registry (labeled per
instance). Dropping the last `Arc<Instance>` flushes and closes that instance only.

```text
host app (rust / node / python / c / go / ...)
  | napi / pyo3 / C ABI (datastreamdb-ffi)
  v
datastreamdb-core::Instance x N (isolated)
  |- datastreamdb-store::Backend (memory | fjall dir)
  |- datastreamdb-schema state
  |- datastreamdb-posting lists
  |- datastreamdb-query executors
  v
datastreamdb-raft (embedded single-node log; cluster mode later)
```

## Crate layers

1. **Foundation**: `datastreamdb-x`, `datastreamdb-lex`, `datastreamdb-types`, `datastreamdb-codec`, `datastreamdb-algo`, `datastreamdb-tok`. No I/O, no globals.
2. **Ingest**: `datastreamdb-chunker`, `datastreamdb-schema`.
3. **Frontends**: `datastreamdb-dql`, `datastreamdb-graphql` (parse only; no execution).
4. **Storage**: `datastreamdb-store` (trait + backends), `datastreamdb-posting`, `datastreamdb-xidmap`.
5. **Execution**: `datastreamdb-task`, `datastreamdb-query`, `datastreamdb-worker`, `datastreamdb-frontend`.
6. **Distribution**: `datastreamdb-proto`, `datastreamdb-raft`.
7. **Ops**: `datastreamdb-acl`, `datastreamdb-audit`, `datastreamdb-enc`, `datastreamdb-backup`, `datastreamdb-filestore`.
8. **Hosts**: `datastreamdb-core` (in-process), `datastreamdb-ffi` (C ABI), `datastreamdb-server` (binaries), `bindings/*`.

Dependencies point strictly downward except toward `datastreamdb-core`, which facades upward.

## Key decisions

- **Badger replacement**: `fjall` (pure-Rust LSM) behind the `datastreamdb-store::Backend` trait, with an in-memory backend for tests and ephemeral embeds. See [`docs/STORAGE.md`](docs/STORAGE.md).
- **Raft**: `openraft` for consensus over a ported `raftwal` log layout; embedded single-node log first.
- **Full-text**: `tantivy` replaces bleve behind the `datastreamdb-tok` registry.
- **GraphQL**: `async-graphql` for execution; upstream schema-gen/auth semantics ported on top.
- **Wire compat**: `datastreamdb-proto` keeps prost messages wire-compatible with `protos/pb.proto` so Rust and Go nodes can interoperate during migration.
- **No async in core traits**: storage traits are sync (fjall is sync); async lives at the serving edge (`frontend`, bindings) via `spawn_blocking` bridges.
