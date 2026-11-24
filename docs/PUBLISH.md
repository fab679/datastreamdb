# Publishing datastreamdb

Release runbook for crates.io, npm, and PyPI. All 28 workspace crates
verify with `cargo package --workspace --offline --allow-dirty`
(31 packages incl. `publish = false` members; zero errors).

## Prerequisites

- [ ] Repository URL decided; add `repository = "..."` to
  `[workspace.package]` in `Cargo.toml` (blocked: no URL yet).
- [ ] Working tree committed (`cargo package` needs `--allow-dirty`
  until then; `cargo publish` does not check git status, but release
  from a clean tag).
- [ ] Credentials: `cargo login` (crates.io token), `npm login`,
  PyPI/Trusted Publishing for `datastreamdb-py`.
- [ ] Gates green: `cargo fmt --all --check`, `cargo clippy
  --workspace --all-targets -- -D warnings`, `cargo test
  --workspace`, `RUSTDOCFLAGS="-D warnings" cargo doc --workspace
  --no-deps`.

## crates.io order

Publish strictly in this dependency order (one command per crate;
wait for each to appear on crates.io before the next, or use
`--wait-for-publish` via a loop):

```sh
for c in \
  datastreamdb-proto datastreamdb-acl datastreamdb-codec datastreamdb-algo datastreamdb-audit \
  datastreamdb-lex datastreamdb-types datastreamdb-tok datastreamdb-x datastreamdb-chunker \
  datastreamdb-enc datastreamdb-schema datastreamdb-store datastreamdb-posting datastreamdb-backup \
  datastreamdb-core datastreamdb-dql datastreamdb-task datastreamdb-query datastreamdb-graphql \
  datastreamdb-raft datastreamdb-worker datastreamdb-frontend datastreamdb-ffi datastreamdb-filestore \
  datastreamdb-xidmap datastreamdb-loader datastreamdb-server; do
  cargo publish -p "$c"
done
```

Skipped (`publish = false`): `datastreamdb-node`, `datastreamdb-py`, `xtask`.

Dry-run per crate (needs network unless deps are already published):

```sh
cargo publish -p datastreamdb-proto --dry-run
```

## Re-verification quirk

`cargo package --workspace` verifies siblings against a temporary
registry whose tarball mtimes are normalized, so cargo's fingerprint
reuse can pin a *stale* sibling build on repeat runs (observed:
`datastreamdb-worker` failing against an older `datastreamdb-backup` even though
every source on disk was fresh). Before re-running verification after
code changes, clear both caches:

```sh
cargo clean
rm -rf target/package "$CARGO_HOME"/registry/src/*/datastreamdb-* \
  "$CARGO_HOME"/registry/cache/*/datastreamdb-*
cargo package --workspace --offline --allow-dirty
```

## npm (`datastreamdb-node`)

`bindings/node` ships the prebuilt Node-API binary plus
`index.js`/`index.d.ts`:

```sh
cd bindings/node
npm run build   # napi build for the host triple
npm test
npm publish --access public
```

## PyPI (`datastreamdb-py`)

`bindings/python` builds with maturin:

```sh
cd bindings/python
maturin build --release
maturin publish
```

## Post-release

- Tag `v0.1.0` and push.
- `cargo xtask check-versions` verifies the lockfile is fresh and
  reports duplicated pins; `--online` also diffs against the
  registry (needs network).
- Update `docs/PROJECT_MAP.md` and the root `README.md` status.
