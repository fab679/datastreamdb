# datastreamdb-acl/testdata

Test-only JWT key fixtures (never production secrets; Go keeps the same
shape under `enc/test-fixtures`).

- `hmac.key` — 64 fixed bytes (covers HS512 length rules).
- `es256.pem` (SEC1) + `es256-pkcs8.pem` — P-256 private keys in both
  accepted encodings; `es384.pem`, `es512.pem` likewise SEC1.
- `rsa.pem` (PKCS#8) + `rsa-pkcs1.pem` — 2048-bit RSA private keys.
- `eddsa.pem` — Ed25519 private key (PKCS#8).
