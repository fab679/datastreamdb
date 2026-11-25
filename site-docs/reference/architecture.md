# Architecture

One workspace, ~30 crates. Data flows: clients → `frontend`
(coordinator + HTTP/gRPC) → `worker` (groups, raft proposals) →
`posting` (storage engine) → `store` (memory/fjall backends).

| Area | Crates |
|---|---|
| Embedding | `datastreamdb-core`, `datastreamdb-ffi`, `bindings/{node,python,c}` |
| Serving | `datastreamdb-frontend`, `datastreamdb-server` (binaries), `datastreamdb-proto` |
| Query | `datastreamdb-dql`, `datastreamdb-query`, `datastreamdb-graphql`, `datastreamdb-task` |
| Storage | `datastreamdb-posting`, `datastreamdb-schema`, `datastreamdb-store`, `datastreamdb-codec`, `datastreamdb-tok`, `datastreamdb-types`, `datastreamdb-algo`, `datastreamdb-xidmap`, `datastreamdb-chunker`, `datastreamdb-lex` |
| Cluster | `datastreamdb-worker`, `datastreamdb-raft` |
| Ops | `datastreamdb-backup`, `datastreamdb-loader`, `datastreamdb-filestore`, `datastreamdb-acl`, `datastreamdb-audit`, `datastreamdb-enc`, `datastreamdb-x` |

Rustdocs for every crate publish to docs.rs. The `docs/PROJECT_MAP.md`
developer map in the repo tracks Go-package parity per crate.

## Design rules

- **Go parity, including limitations**, unless a concrete safety or
  parity reason diverges.
- **Embeddable core**: the engine never assumes a server; binaries
  are thin wrappers.
- **Fail closed**: auth failures deny; audit and encryption are
  explicit flags, never silent defaults.
- **Small files**: modules split before ~1000 lines.
