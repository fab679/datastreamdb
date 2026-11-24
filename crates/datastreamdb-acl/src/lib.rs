//! Access control: JWT auth, HMAC secrets, rules.
//!
//! Rust port of the upstream Go `acl` package plus the JWT core it relies on
//! (`x/jwt_helper.go`, `x/authn.go`, `x/acl_enc_keys.go`,
//! `edgraph/access.go` login/mint paths).
//!
//! - [`entities`]: ACL users, groups, and rules as stored in the graph.
//! - [`keys`]: signing algorithms, secret validation, and key loading.
//! - [`jwt`]: access/refresh token minting and validation.
//! - [`nquads`]: user/group creation mutations.
//!
//! Password hashing itself lives in `datastreamdb-types::password` (bcrypt, Go
//! parity); login verifies via the `checkpwd` query function, and the
//! `datastreamdb acl` CLI plus frontend enforcement arrive with the M6 slices
//! that wire this crate into servers.

pub mod entities;
pub mod jwt;
pub mod keys;
pub mod nquads;
