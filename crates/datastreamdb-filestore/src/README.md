# datastreamdb-filestore/src

Implementation of `datastreamdb-filestore`. Public API lives in `lib.rs`; add one module per Go source file being ported and name it after that file.

- `lib.rs` - `FileStore` trait, URL dispatch, `open_input`.
- `local.rs` - disk backend + shared decrypt/gunzip decoding.
- `s3.rs` - S3/MinIO REST client + multipart streaming writer.
- `s3_auth.rs` - S3 credentials, URL parsing, SigV4 signing.
