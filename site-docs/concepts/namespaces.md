# Namespaces and sharding

Two different partitions appear in a deployment: **namespaces** separate
tenants inside one database, and **groups** spread predicates across servers.
They solve different problems and can be used together.

## Namespaces (tenants)

A namespace partitions one instance into tenants. Predicate names carry the
namespace on the wire:

| Wire name | Means |
|---|---|
| `0-name` | `name` in namespace `0` (the galaxy) |
| `2-name` | `name` in tenant `2` |

Queries and mutations run inside one namespace; guardians manage all of them.
Namespace `0` is the default and can never be deleted. Creating a namespace
leases an id from zero, installs the initial schema and types on every group,
and bootstraps its guardians.

```graphql
mutation { addNamespace(input: {}) { namespaceId message } }
```

Login carries the tenant, and the resulting JWT scopes later requests:

```sh
curl -XPOST localhost:8080/login \
  -d '{"userid": "groot", "password": "password", "namespace": 2}'
```

Namespaces are a standalone feature. Embedded instances are isolated by handle
and have no namespace layer — open another instance instead.

See [Tenancy](../guides/tenancy.md) for management operations and
[Security](../guides/security.md) for the ACL side.

## Groups and tablets

Standalone deployments shard by predicate:

- A **tablet** assigns one predicate to a **group**.
- A **group** is an independent raft cluster of alphas with its own data.
- **Zero** owns the tablet map and tells alphas where each predicate lives.

`GET /state` on zero shows the current map. A single-alpha deployment is group
`1` serving every predicate.

Sharding is per predicate, not per node. A query that traverses `friend` and
`name` may touch two groups; the coordinator joins the results.

## Choosing a layout

| Situation | Layout |
|---|---|
| One application, one process | Embedded instance |
| One team, one server | Single alpha, group `1` |
| Several teams sharing a cluster | Namespaces |
| Dataset larger than one server | Multiple groups, tablets spread by predicate |
| Both | Namespaces inside each group's predicates |

## What embedded mode leaves out

Embedded instances have no zero, no raft, no groups, no tablets, and no
namespaces. That is the point: one process owns its data. When those features
become requirements, move to the [standalone servers](../get-started/quickstart.md);
the DQL and GraphQL surface stays the same.
