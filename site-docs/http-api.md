# HTTP API (alpha `:8080`)

| Method + path | Purpose |
|---|---|
| `GET /health` | Liveness: `status`, `version`, `instance`, `uptime` |
| `GET /state` | Membership snapshot (groups, members, tablets, leases) |
| `POST /query` | DQL query body; JSON `data` + `extensions` (txn, latency, metrics) |
| `POST /mutate` | RDF body (default) or JSON (by content type); `?commitNow=true` commits inline |
| `POST /alter` | Schema text, or JSON drop (`{"drop_all":true}`, `{"drop_attr":"name"}`, …) |
| `POST /login` | `{"userid","password"}` (or `refresh_token`) → `accessJWT` + `refreshJWT` |
| `GET /commit?startTs=…` | Commit one transaction (`keys`/`preds` repeat params) |
| `GET /abort?startTs=…` | Abort one transaction |
| `POST /admin` | Guardian-only admin GraphQL ([Admin API](admin.md)) |

## Query flags

`POST /query?ro=true` takes a read-only lease; `?be=true` reads
best-effort at the newest assigned timestamp without a lease;
`?startTs=…` continues an explicit transaction. Variables travel in
the `X-DataStreamDB-Vars` header as a JSON object.

## Auth headers

- `X-DataStreamDB-AccessJWT`: access token from `/login` (also accepted as
  `X-DataStreamDB-AccessToken`). Without `--acl` every request is open.
- `X-DataStreamDB-AuthToken`: legacy shared-secret mode.

## Mutations

Upsert guards ride on `?cond=@if(...)`; blank nodes (`_:a`) come back
mapped in `data.uids`. Responses always carry an `extensions.txn`
envelope (`start_ts`, `commit_ts`, `aborted`). See the
[Quickstart](quickstart.md) for a full round trip.
