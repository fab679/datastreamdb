# Backups & export

Backups are per-group Snappy-framed streams plus a manifest chain,
in the upstream Go on-disk layout, so Go-written and Rust-written backups
interoperate. Exports are per-group gzip files (RDF or JSON) plus
schema.

## Destinations

Every operation takes a local directory or an `s3://`/`minio://`
URL. Remote credentials come from the request itself:

```graphql
mutation {
  backup(input: {
    destination: "s3://backups/datastreamdb"
    accessKey: "…", secretKey: "…", sessionToken: ""
  }) { response { code message } }
}
```

Use `anonymous: true` for public buckets. Only HTTP endpoints are
supported in 0.1.0 (HTTPS needs a rustls build).

## Backup chains

Each run appends one manifest entry: the first run against a fresh
destination is a full backup, later runs are incremental. Drops
(`drop_all`, namespace deletes) are recorded in the manifest, so
restoring a chain reproduces drops exactly.

## Restore

```graphql
mutation {
  restore(input: { location: "/tmp/datastreamdb-bak", backupNum: 0 }) {
    response { code message }
  }
}
```

Restore replays newest-series-first back to its full backup, oldest
first. `backupId` selects a series id, `backupNum` caps how many
entries replay (`0` is all).

## Encryption

Pass `--encryption-key-file <16|24|32-byte key>` to `alpha` to
encrypt backup streams and export files (AES-CTR, Go wire
compatible). Restore reads the key back from the request's
`encryptionKeyFile` (or the server's configured key).
