# Admin API

`POST /admin` serves GraphQL for cluster operations. With `--acl`
configured the caller must present a guardian JWT
(`X-DataStreamDB-AccessJWT` with the `guardians` group); without ACL the
endpoint is open. Send `{"query": "..."}` and read `data`/`errors`.

## Health

```graphql
{ health { instance address status version uptime } }
```

## Backup

```graphql
mutation {
  backup(input: { destination: "/tmp/datastreamdb-bak" }) {
    response { code message }
  }
}
```

Runs synchronously and reports `Success` when every group is backed
up. `destination` is a directory or an `s3://`/`minio://` URL;
`accessKey`/`secretKey`/`sessionToken` (or `anonymous: true` for
public buckets) authenticate remote destinations. See
[Backups](backups.md).

## Export

```graphql
mutation {
  export(input: { format: "rdf", destination: "/tmp/datastreamdb-exp" }) {
    response { code message }
  }
}
```

`format` is `rdf` (default) or `json`. `namespace` selects one
namespace (`0`/omitted is the galaxy); all-namespaces export is not
supported yet.

## Restore

```graphql
mutation {
  restore(input: { location: "/tmp/datastreamdb-bak" }) {
    response { code message }
  }
}
```

Replays the newest backup chain into the live engine. `backupId`
picks a series (empty is latest), `backupNum` caps it, and
`encryptionKeyFile` decrypts encrypted chains.

## Namespaces

```graphql
mutation { addNamespace(input: { password: "…" }) { namespaceId message } }
mutation { deleteNamespace(input: { namespaceId: 2 }) { namespaceId message } }
```

`addNamespace` leases an id, installs the initial schema, and seeds
guardians/groot (default password `"password"` when omitted).
Deleting namespace `0` is rejected. Guardian-only under ACL, like the
equivalent gRPC calls. See [Tenancy](tenancy.md).
