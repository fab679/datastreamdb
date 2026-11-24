# Publishing the SDKs (0.1.0)

How to ship the three integration packages plus the Rust crates.
Do the crates first — the Node and Python builds compile against
the workspace, and downstream users resolve `datastreamdb-*` from
crates.io.

Prerequisites (all three): accounts and tokens for
[crates.io](https://crates.io), [npm](https://www.npmjs.com), and
[PyPI](https://pypi.org). Versions are aligned at `0.1.0`:
workspace `Cargo.toml`, `bindings/node/package.json`,
`bindings/python/pyproject.toml`.

## 1. Rust crates → crates.io (28 crates)

`cargo publish` has no workspace mode: publish leaf-first in
dependency order. The order below was topologically sorted from
`cargo metadata` — run it top to bottom, waiting ~30s between
publishes for the index to catch up:

```sh
cargo login  # once per machine
for c in \
  datastreamdb-audit datastreamdb-enc datastreamdb-filestore \
  datastreamdb-lex datastreamdb-proto datastreamdb-acl \
  datastreamdb-codec datastreamdb-algo datastreamdb-raft \
  datastreamdb-store datastreamdb-types datastreamdb-core \
  datastreamdb-ffi datastreamdb-tok datastreamdb-x \
  datastreamdb-chunker datastreamdb-dql datastreamdb-schema \
  datastreamdb-posting datastreamdb-backup datastreamdb-task \
  datastreamdb-query datastreamdb-graphql datastreamdb-worker \
  datastreamdb-frontend datastreamdb-xidmap datastreamdb-loader \
  datastreamdb-server
do cargo publish -p "$c"; sleep 30; done
```

Notes:

- `datastreamdb-node`, `datastreamdb-py`, and `xtask` are
  `publish = false` (they ship via npm/PyPI, not crates.io).
- If a publish fails halfway, just re-run the loop — already
  published crates error with "already exists" and you continue
  past them.
- Verify with `cargo package --workspace --offline --allow-dirty`
  first (see `docs/PUBLISH.md` for the cache-purge quirk).

## 2. TypeScript → npm (`@datastreamdb/core`)

The package ships one prebuilt addon today
(`index.linux-x64-gnu.node`, loaded by `index.js`).

```sh
cd bindings/node
npm run build        # napi release build for the host triple
npm test             # node --test test/
npm login            # once per machine (needs 2FA)
npm publish --access public
```

Multi-platform (later): add a `napi.targets` matrix plus per-OS
optional packages and build them in CI — the loader already falls
back to `@datastreamdb/core-<platform>` packages.

## 3. Python → PyPI (`datastreamdb`)

Built with maturin (mixed Rust/Python layout); the import name is
`datastreamdb`.

```sh
cd bindings/python
maturin build --release            # wheels into target/wheels
maturin publish                    # needs a PyPI token (or OIDC trusted publishing)
```

Test install from a clean venv first:

```sh
pip install target/wheels/datastreamdb-0.1.0-*.whl
python -c "import datastreamdb; print(datastreamdb.version())"
```

Per-platform wheels need per-OS CI runners; `cibuildwheel` with a
maturin backend is the standard setup.

## 4. C (`libdatastreamdb_ffi.a` + `datastreamdb.h`)

No registry — attach both files to the GitHub release:

```sh
cargo build --release -p datastreamdb-ffi
# upload: target/release/libdatastreamdb_ffi.a
#         crates/datastreamdb-ffi/datastreamdb.h
```

Consumers link with
`cc app.c -ldatastreamdb_ffi -o app` (plus `-lpthread -ldl -lm`
on Linux).

## Checklist

- [ ] `cargo publish` loop green (28 crates, in order above)
- [ ] `npm publish` green (`@datastreamdb/core@0.1.0`)
- [ ] `maturin publish` green (`datastreamdb==0.1.0`)
- [ ] GitHub release `v0.1.0` with the C assets attached
- [ ] Tag: `git tag v0.1.0 && git push origin v0.1.0`
