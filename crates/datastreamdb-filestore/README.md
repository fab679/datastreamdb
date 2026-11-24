# datastreamdb-filestore

Local/remote file access for bulk load and backup.

- Go source: `dgraph-main/filestore` (reference clone at repo root; read-only).
- Status: M6-s3 done (trait + local backend + chunk readers + S3/MinIO client with multipart uploads; 12 tests). See [TODO.md](TODO.md) and [docs/PROJECT_MAP.md](../../docs/PROJECT_MAP.md).
- Embedding: this crate must stay usable from `datastreamdb-core` inside one process with many isolated instances.

## Layout

- `src/` - implementation (see [src/README.md](src/README.md)).
