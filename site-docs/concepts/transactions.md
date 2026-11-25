# Timestamps and transactions

Every committed write gets a timestamp. Reads pick a timestamp and see a
consistent snapshot as of that point.

## Standalone servers

A mutation is prepared at a `start_ts` leased from zero and becomes visible at
its `commit_ts`. The response carries both in `extensions.txn`:

```json
{
  "data": { "uids": { "ada": "0x2" } },
  "extensions": { "txn": { "start_ts": 41, "commit_ts": 42, "aborted": false } }
}
```

With `?commitNow=true`, the server commits before responding. Without it, the
mutation stays pending and you commit explicitly with the returned start
timestamp:

```sh
curl 'localhost:8080/commit?startTs=41'
```

### Conflicts abort

If two transactions write the same key, one commits and the other aborts with
`aborted: true` and HTTP 409. The aborted transaction writes nothing. Retry it
against fresh data instead of assuming the write landed.

### Best-effort reads

`?be=true` skips the timestamp lease and reads whatever is newest. It is faster
and can observe a partially applied state, so use it for caches and dashboards,
not for decisions that must be consistent.

## Embedded instances

Embedded mode has one writer and no raft, so there is no cross-node conflict to
resolve and no explicit transaction handle:

- Each `dql_mutate` or GraphQL mutation commits synchronously at a fresh
  timestamp before it returns.
- Each `dql_query` or GraphQL query reads at the current committed timestamp.
- The timestamp ceiling is persisted with the instance, so reopening a directory
  never reuses a timestamp.

There is no API to stage several calls into one transaction. If a group of
writes must be atomic, put them in one DQL mutation block:

```text
{
  set {
    _:order <name> "Order #1001" .
    _:order <ordered> _:laptop (quantity=1) .
  }
}
```

That block commits as one unit.

## What this buys you

| Property | Standalone | Embedded |
|---|---|---|
| Snapshot reads | Yes, at any leased timestamp | Yes, at the current committed timestamp |
| Deferred commit | Yes (`startTs` + `/commit`) | No, mutations commit on return |
| Conflict abort | Yes, on overlapping writes | Not applicable (single writer) |
| Best-effort reads | Yes (`?be=true`) | Not exposed |
| Multi-call transaction | Yes | No, use one mutation block |

Next: [namespaces and sharding](namespaces.md).
