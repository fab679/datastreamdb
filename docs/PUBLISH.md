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

Publish leaf-first. crates.io allows a **burst of 5 new crates**, then
**1 new crate every 10 minutes**; a `429` response carries the exact
next-publish timestamp. A 30-second sleep is too short and will hit
the limiter.

Use the canonical runner:

```sh
tools/publish-crates.sh
```

It publishes in the order below, skips crates already visible on
crates.io, verifies every path dependency is visible before a parent,
persists its next-allowed slot across restarts, and honors `429`
retry timestamps.

Canonical order:

```text
datastreamdb-proto datastreamdb-acl datastreamdb-codec datastreamdb-algo datastreamdb-audit
datastreamdb-lex datastreamdb-types datastreamdb-tok datastreamdb-x datastreamdb-chunker
datastreamdb-enc datastreamdb-schema datastreamdb-store datastreamdb-posting datastreamdb-backup
datastreamdb-core datastreamdb-dql datastreamdb-task datastreamdb-query datastreamdb-graphql
datastreamdb-raft datastreamdb-filestore datastreamdb-worker datastreamdb-frontend datastreamdb-ffi
datastreamdb-xidmap datastreamdb-loader datastreamdb-server
```

If publishing manually, run one `cargo publish -p <crate>` at a time and
wait for that crate to appear before publishing anything that depends
on it. Only new crates consume the 10-minute limiter; new versions of
an existing crate use a separate limit (burst 30, then 1 per minute).

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
