# Tester quickstart (0.1.0)

Five-minute smoke test of `datastreamdb`: start a cluster, load data,
query it, back it up, and call the admin API.

## 1. Build

```sh
cargo build --release -p datastreamdb-server
```

Binaries land in `target/release/`: `zero`, `alpha`, `bulk`, `live`.

## 2. Start zero + alpha

```sh
./target/release/zero > /tmp/zero.log 2>&1 &
./target/release/alpha --zero 127.0.0.1:5080 \
  --postings /tmp/datastreamdb-p --wal /tmp/datastreamdb-w > /tmp/alpha.log 2>&1 &
sleep 5
curl localhost:8080/health
```

(Defaults bind zero to `127.0.0.1:5080` and alpha to
`127.0.0.1:7080`/HTTP `127.0.0.1:8080`; see `--help` for the full
flag list.)

Expect `{"status":"ok",...}`.

## 3. Schema, data, query

```sh
curl -XPOST localhost:8080/alter -d 'name: string @index(exact) .'
curl -XPOST 'localhost:8080/mutate?commitNow=true' \
  -H 'content-type: application/rdf' -d '_:a <name> "Ada" .'
curl -XPOST localhost:8080/query -d '{ q(func: has(name)) { uid name } }'
```

Expect Ada back with a `0x…` uid.

## 4. Backup, export, restore via the admin API

```sh
curl -XPOST localhost:8080/admin -d \
  '{"query":"mutation { backup(input: {destination: \"/tmp/datastreamdb-bak\"}) { response { code message } } }"}'
curl -XPOST localhost:8080/admin -d \
  '{"query":"mutation { export(input: {format: \"rdf\", destination: \"/tmp/datastreamdb-exp\"}) { response { code message } }"}'
ls /tmp/datastreamdb-bak /tmp/datastreamdb-exp
```

## 5. Tenancy

```sh
curl -XPOST localhost:8080/admin -d \
  '{"query":"mutation { addNamespace(input: {}) { namespaceId message } }"}'
```

Note the id, then delete it:

```sh
curl -XPOST localhost:8080/admin -d \
  '{"query":"mutation { deleteNamespace(input: {namespaceId: 1}) { message } }"}'
```

(Replace `1` with the id returned above.)

## 6. Embedded smoke (Rust)

```rust
// See datastreamdb-embed docs: open an in-process instance, run DQL,
// drop it — no servers, no ports.
```

TypeScript/Python/C entry points live under `bindings/` with their
own READMEs.

## What to report

- Any command above that errors: paste the command, the output, and
  the tail of `/tmp/zero.log` / `/tmp/alpha.log`.
- Anything marked out of scope in `docs/RELEASE-0.1.0.md` is a
  known gap, not a bug.
