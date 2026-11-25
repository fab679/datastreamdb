# Quickstart (standalone servers)

Start a one-zero, one-alpha cluster, load data, and query it.

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

Defaults bind zero to `127.0.0.1:5080` and alpha to
`127.0.0.1:7080` (gRPC) plus HTTP on `127.0.0.1:8080`.
Expect `{"status":"ok",...}` from `/health`.

## 3. Schema, data, query

```sh
curl -XPOST localhost:8080/alter -d 'name: string @index(exact) .'
curl -XPOST 'localhost:8080/mutate?commitNow=true' \
  -H 'content-type: application/rdf' -d '_:a <name> "Ada" .'
curl -XPOST localhost:8080/query -d '{ q(func: has(name)) { uid name } }'
```

Expect Ada back with a `0x…` uid.

## 4. Backups and namespaces

```sh
curl -XPOST localhost:8080/admin -d \
  '{"query":"mutation { backup(input: {destination: \"/tmp/datastreamdb-bak\"}) { response { code message } } }"}'
curl -XPOST localhost:8080/admin -d \
  '{"query":"mutation { addNamespace(input: {}) { namespaceId message } }"}'
```

See [Admin API](../guides/admin.md), [Backups](../guides/backups.md), and
[Tenancy](../guides/tenancy.md) for the full surface.

## Next

- [HTTP API](../guides/http-api.md) — every endpoint and flag.
- [Operations](../guides/operations.md) — flags, loaders, ACL, audit, CDC.
- [Embedding](../embedding/index.md) — skip the servers entirely.
