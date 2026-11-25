# Troubleshooting

## Servers

**`curl: couldn't connect` on `:8080`.**
Alpha is still joining. Wait for zero first (`curl
localhost:6080/health`), give alpha ~5s, then check `/tmp/alpha.log`.

**`group has no leader` / `cluster unavailable`.**
Zero is down or unreachable: every timestamp, commit, and tablet
lookup goes through it. Restart zero at the same `--wal` dir and
the cluster recovers.

**`transaction aborted`.**
A conflicting write committed first. Retry the whole
read-modify-write at a fresh timestamp; never reuse a `start_ts`
after an abort.

**`only guardians are allowed to drop all data`.**
`/alter` drop-all and namespace deletes need a guardian identity
under `--acl`. Log in as `groot`, or run without `--acl`.

**`admin requires guardian` (HTTP 403).**
`/admin` needs `X-DataStreamDB-AccessJWT` for a user in the `guardians`
group when ACL is on. Anonymous and non-guardian tokens are
rejected — see [Security](security.md).

**`Cannot delete default namespace` / `namespace [0] cannot be deleted`.**
Namespace `0` is permanent, by design.

**`exporting all namespaces is not supported yet`.**
Pass one `namespace` (or omit it) instead of a negative id.

## Embedded

**Second open of a directory fails.**
One live instance per directory: the `LOCK` file is exclusive.
Drop the first handle (after `flush`) before reopening.

**`invalid config JSON` / unknown field rejected.**
`InstanceConfig` rejects unknown fields so version skew fails fast.
Only `data_dir` and `tenant` exist in 0.2.0.

**`null` from `get`.**
Missing keys read as `null` (TypeScript), `None` (Python/Rust),
or `{"value": null}` (envelope) — not an error.

## Reporting bugs

Anything outside [Limitations](limitations.md) that misbehaves is a
bug. Report the exact command, its full output, and the tails of
the server logs.
