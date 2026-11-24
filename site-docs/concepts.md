# Concepts

The five ideas the rest of these docs assume.

## Instance

An **instance** is one isolated database: its own storage backend,
its own data, its own handle. Embedded hosts open as many as they
like in one process ([Embedding](embedding/index.md)); the
standalone servers are single instances speaking HTTP/gRPC.

In-memory instances are ephemeral; directory instances persist
through the fjall backend (one live opener per directory, enforced
by a `LOCK` file).

## Namespace (tenant)

A **namespace** partitions one instance into tenants. Predicate
names carry the namespace on the wire: `0-name` is `name` in the
galaxy (namespace `0`), `2-name` is `name` in tenant `2`. Queries
and mutations run inside one namespace; guardians manage them all.
See [Tenancy](tenancy.md).

## Predicate and schema

A **predicate** is a named edge (`name`, `friend`, `datastreamdb.type`).
The **schema** declares each predicate's type and indexes
(`name: string @index(exact) .`). Undeclared predicates accept
writes but cannot be indexed or queried by function until declared.
Dropping uses the same vocabulary: drop one predicate, one type,
all data, or everything.

## Group and tablet

Standalone deployments shard by predicate: a **tablet** assigns one
predicate to a **group**, and each group is an independent raft
cluster of alphas. Zero owns the tablet map and tells alphas where
predicates live (`/state` shows the current map). Single-alpha
clusters are group `1` serving everything.

## Timestamps and transactions

Every write commits at a timestamp leased from zero; reads pick a
timestamp and see a consistent snapshot. The `extensions.txn`
envelope on every response carries `start_ts`, `commit_ts`, and
`aborted`. Conflicts abort instead of overwriting — retry the
transaction. Best-effort reads (`?be=true`) skip the lease and read
whatever is newest.
