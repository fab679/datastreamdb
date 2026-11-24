# datastreamdb-filestore/tests

Integration suites.

- `s3_fake.rs` - S3 client roundtrips against an in-process fake
  object store (no network): signed CRUD, pagination, multipart
  streaming uploads, rename, missing-key behavior.
