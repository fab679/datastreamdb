# TODO - datastreamdb-filestore

Port checklist for `filestore`. Check items only with tests proving Go parity.

- [x] M0: crate scaffolded, compiles, docs render.
- [x] M6.5: `FileStore` trait + local backend + URL dispatch; chunk
  readers decode gzip + AES-CTR in Go order with magic sniffing
  (4 tests).
- [x] M6-s3: S3/MinIO client (`s3.rs` + `s3_auth.rs`): SigV4
  (openssl-oracle vector), HEAD/GET/PUT/DELETE, ListObjectsV2
  pagination, multipart streaming uploads, copy+delete rename,
  env credential chain (8 unit + 4 fake-server tests; HTTPS needs
  a TLS build; live-MinIO check still open).
- [x] M-doc: rustdoc + examples; update `docs/PROJECT_MAP.md` status.
- [x] M-done: `cargo test -p datastreamdb-filestore`, clippy `-D warnings`, fmt clean.
