# TODO - datastreamdb-filestore/src

- [x] One module per ported Go file, each with unit tests + doc examples.
- [x] No `unwrap`/`expect` on user-controlled paths; errors via `thiserror`.
- [x] No global mutable state (multi-instance embedding requirement).
- [ ] TLS feature for `https://` S3 endpoints (needs network for rustls).
- [ ] Live-MinIO compatibility run (needs network + a minio binary).
