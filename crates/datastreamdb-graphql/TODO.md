# TODO - datastreamdb-graphql

Port checklist for `graphql`. Check items only with tests proving Go parity.

- [x] M0: crate scaffolded, compiles, docs render.
- [x] M1: schema generation (`gqlschema` ingest, `schemagen` completion, `genDgSchema`, SDL) onto async-graphql-parser — 16 schemagen tests.
- [x] M2: auth rules (`auth` JWT HS/RS + `auth_rules` `@auth` RBAC/static eval).
- [x] M3: resolvers (`op`/`rewrite_*`/`resolve*`) backed by `datastreamdb-query` — 8 rewrite + 6 mutation + 4 e2e tests.
- [x] M4: subscriptions (`subscription` poll-based hub) — 3 tests. Transport (`ws`) deferred to the server milestone.
- [x] M6-slice: admin API (`admin.rs`: health/backup/export/restore/add/deleteNamespace over `AdminBackend`; 4 tests).
- [ ] M5: remaining admin API (schema/config/users/groups) and e2e suite (`graphql/e2e`); JWK fetch; custom HTTP/lambda execution; remote schemas.
- [x] M-doc: rustdoc + examples; update `docs/PROJECT_MAP.md` status.
- [x] M-done: `cargo test -p datastreamdb-graphql` (56 green), clippy `-D warnings`, fmt clean.
