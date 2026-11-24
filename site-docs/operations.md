# Operations

## Zero

```sh
zero --my 127.0.0.1:5080 --wal zw --http 127.0.0.1:6080
```

| Flag | Default | Purpose |
|---|---|---|
| `--my` | `127.0.0.1:5080` | Advertised gRPC address |
| `--wal` | `zw` | Zero group log directory |
| `--http` / `--grpc` | `127.0.0.1:6080` / `127.0.0.1:5080` | Bind addresses |
| `--bootstrap` | `true` | Fresh group vs join |
| `--join` | — | Peer zero addrs to join through (with `--no-bootstrap`) |
| `--replicas` | `1` | Alphas wanted per group |
| `--heartbeat-ms`, `--election-ms` | `100`, `1000,2000` | Raft timing |

Zero serves `/health`, `/state`, `/assign`, `/removeNode` over HTTP
plus the gRPC `Zero` service.

## Alpha

```sh
alpha --zero 127.0.0.1:5080 --postings p --wal w
```

| Flag | Default | Purpose |
|---|---|---|
| `--my` | `127.0.0.1:7080` | Advertised gRPC address |
| `--zero` | `127.0.0.1:5080` | Zero addrs (comma-separated) |
| `--postings` / `--wal` | `p` / `w` | Storage dirs (`:memory:` for ephemeral postings) |
| `--http` / `--grpc` | `127.0.0.1:8080` / `127.0.0.1:7080` | Bind addresses |
| `--group` | `0` | Raft group (`0` asks zero to place us) |
| `--bootstrap` | `true` | Fresh group vs join |
| `--mutations` | `allow` | Mutation mode |
| `--cache-mb` | `1024` | Posting cache size |
| `--acl` | `""` | `secret-file=…; access-ttl=6h; refresh-ttl=30d; jwt-alg=HS256`; empty disables ACL |
| `--audit` | `""` | `output=…; days=10; size=100`; empty disables auditing |
| `--encryption-key-file` | `""` | 16/24/32-byte key for backups/exports |
| `--cdc` | `""` | `file=…` change-data-capture sink; empty disables |

With `--acl`, first boot seeds guardians/groot in the galaxy
(password `password`); namespace creation seeds them per tenant.

## Loaders

```sh
bulk data.rdf.gz --schema schema.txt --data-dir p
live data.rdf.gz --data-dir p
```

Both take RDF/JSON files (plain, `.gz`, or encrypted + `.gz`),
`--batch` n-quads per commit, and `--encryption-key-file` for
encrypted inputs. `live --alpha` is reserved — remote mode is not
implemented; loaders write a local engine directly.

## CDC

`--cdc file=/tmp/cdc.json` appends one JSON event per committed
mutation (Go's event shape). Failed sends log and drop; consumers
must tolerate gaps after crashes.
