# Tenancy

Namespaces isolate tenants: predicate names are
`namespace-predicate` on the wire (`0-name` is `name` in the galaxy).
Guardian-only under ACL.

## Manage namespaces

Via [Admin API](admin.md):

```graphql
mutation { addNamespace(input: {}) { namespaceId message } }
mutation { deleteNamespace(input: { namespaceId: 2 }) { message } }
```

Or via gRPC on `api.DataStreamDB`: `CreateNamespace`, `DropNamespace`,
`ListNamespaces` (a `map<uint64, Namespace>`).

Creating a namespace leases the id from zero, installs the initial
schema/types on every group, and bootstraps guardians/groot inside
it. Deleting drops group data first, then tells zero, then records
`DROP_NS` for backups. Namespace `0` (galaxy) can never be deleted.

## Authenticate into a namespace

Login carries the tenant:

```sh
curl -XPOST localhost:8080/login \
  -d '{"userid": "groot", "password": "password", "namespace": 2}'
```

The access JWT's `namespace` claim scopes subsequent requests.
