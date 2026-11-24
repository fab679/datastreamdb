# 0.1.0 release plan

First packaged release of `datastreamdb`: embeddable DataStreamDB in Rust.
Scope below is what is implemented and tested today; anything else is
explicitly out.

## In scope

- In-process engine with many isolated instances (`datastreamdb-embed`
  plus TypeScript/Python/C bindings).
- DQL queries, mutations (upserts, conditions), schema alters, drops.
- GraphQL data layer (schema-gen, auth rules, resolvers,
  subscriptions).
- Single-group raft plus multi-alpha groups, single zero with
  join/cluster membership (multi-zero join over the TCP transport).
- ACL (login, JWT, predicate rules, guardian/groot bootstrap),
  audit log, at-rest encryption flags.
- Backup/restore/export (local + S3 destinations, manifest chains,
  drop records, encryption), bulk + live loaders, CDC file sink.
- Tenancy (create/drop/list namespaces) and the admin slice:
  `POST /admin` with `health`, `backup`, `export`, `restore`,
  `addNamespace`, `deleteNamespace` (guardian-gated).

## Out of scope (tracked for 0.2.0)

- Full admin epic: schema/config/user/group mutations, drain,
  shutdown, async backup tasks with task ids, all-namespaces
  export, list-backups.
- Vector/HNSW search, encrypted WAL, Kafka CDC sink, S3 HTTPS
  (needs a rustls build), live MinIO verification, remote live
  load, sharded bulk, subscription websocket transport, JWK
  fetch, lambda/custom HTTP resolvers, Jepsen runs.

## Release gates (all must pass on a clean checkout)

```sh
export CARGO_HOME=/tmp/cargo-home NO_PROXY=127.0.0.1,localhost
cargo fmt --all --check
cargo clippy --workspace --offline --all-targets -- -D warnings
cargo test --workspace --offline
cargo doc --workspace --offline --no-deps
cargo xtask check-versions
```

Packaging (see `docs/PUBLISH.md` for the cache-purge quirk):

```sh
cargo clean
rm -rf target/package "$CARGO_HOME"/registry/src/*/datastreamdb-* \
  "$CARGO_HOME"/registry/cache/*/datastreamdb-*
cargo package --workspace --offline --allow-dirty
```

Bindings:

```sh
cd bindings/node && npm test
cd ../python && python -m pytest
cc bindings/c/smoke.c -Icrates/datastreamdb-ffi target/debug/libdatastreamdb_ffi.a -o /tmp/c-smoke -lpthread -ldl -lm && /tmp/c-smoke
```

## Publish order

1.crates.io (workspace members in dependency order; `cargo publish`
  walks it with `--workspace` skipped — publish leaf-first per the
  runbook).
2. npm (`bindings/node`, napi artifacts per target triple).
3. PyPI (`bindings/python`, maturin wheels).
4. C header + static lib as a GitHub release asset.
5. Tag `v0.1.0`; attach `docs/TESTING.md` as the tester quickstart.

## Needed before publish

- Repository URL + crates.io/npm/PyPI credentials and owners.
- First-ever commit of the working tree (packaging still uses
  `--allow-dirty`).
- Version strings: workspace `0.1.0` plus binding `package.json` /
  `pyproject.toml` alignment.
