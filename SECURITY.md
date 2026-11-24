# SECURITY

## Posture

- Memory safety from safe Rust; `unsafe` is confined to the `datastreamdb-ffi` boundary and audited.
- Secrets (HMAC keys, encryption keys) are `zeroize`d after use; they never enter logs or error strings.
- Embedded mode runs fully in-process with no network listeners unless the host enables `datastreamdb-server` or `datastreamdb-frontend` serving.
- Dependencies: latest stable pins, `cargo audit`/`cargo deny` in CI (see `.github/TODO.md`).

## Reporting

Report vulnerabilities privately to the maintainers (see repository contact once published). Do not file public issues for suspected vulnerabilities. Expect acknowledgment within 3 business days.
