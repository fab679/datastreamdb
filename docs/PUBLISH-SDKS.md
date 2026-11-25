# Publishing the SDKs (0.2.0)

How to ship the three integration packages plus the Rust crates.
Do the crates first — the Node and Python builds compile against
the workspace, and downstream users resolve `datastreamdb-*` from
crates.io.

Prerequisites (all three): accounts and tokens for
[crates.io](https://crates.io), [npm](https://www.npmjs.com), and
[PyPI](https://pypi.org). Versions are aligned at `0.2.0`:
workspace `Cargo.toml`, `bindings/node/package.json`,
`bindings/python/pyproject.toml`.

## 1. Rust crates → crates.io (28 crates)

`cargo publish` has no workspace mode: publish leaf-first in
dependency order. crates.io allows a **burst of 5 new crates**, then
**1 new crate every 10 minutes**; a `429` response carries the exact
next-publish timestamp.

Use the canonical runner:

```sh
cargo login  # once per machine
tools/publish-crates.sh
```

It skips crates already visible on crates.io, verifies every path
dependency before publishing a parent, persists its next-allowed slot
across restarts, and honors `429` retry timestamps. The canonical
28-crate order is in `docs/PUBLISH.md`.

Notes:

- `datastreamdb-node`, `datastreamdb-py`, and `xtask` are
  `publish = false` (they ship via npm/PyPI, not crates.io).
- If a publish fails halfway, rerun the script — it is resumable and
  skips already-published crates.
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
pip install target/wheels/datastreamdb-0.2.0-*.whl
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

- [x] `cargo publish` loop green (28 crates, in order above)
- [x] `npm publish` green (`@datastreamdb/core@0.2.0`)
- [x] `maturin publish` green (`datastreamdb==0.2.0`)
- [ ] GitHub release `v0.2.0` with the C assets attached
- [ ] Tag: `git tag v0.2.0 && git push origin v0.2.0`
