//! Request-body password masking (Go `checkRequestBody`).
//!
//! Login payloads must never reach the audit trail with secrets intact.
//! The patterns below match both Rust `Debug` rendering (`password:
//! "secret"`) and JSON bodies (`"password":"secret"`). GraphQL
//! variable indirection arrives with the `/admin` endpoint (the GQL
//! parser walk Go performs there); until then, literal passwords mask
//! and variable references pass through.

use regex::Regex;
use std::sync::OnceLock;

/// Matches `password` assignments with quoted values (Debug and JSON).
fn password_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN
        .get_or_init(|| Regex::new("password(\"?)(\\s*:\\s*)\"[^\"]*\"").expect("password regex"))
}

/// Replacement keeping the key and separator visible.
const MASKED: &str = "password$1$2\"*******\"";

/// Masks password values in a gRPC `Login` request rendering.
#[must_use]
pub fn mask_login_body(body: &str) -> String {
    password_pattern().replace_all(body, MASKED).into_owned()
}

/// Masks password values in an HTTP request body (login route).
#[must_use]
pub fn mask_http_body(path: &str, body: &str) -> String {
    if path == "/login" || path == "/admin" {
        password_pattern().replace_all(body, MASKED).into_owned()
    } else {
        body.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_debug_and_json_shapes() {
        let debug = "LoginRequest { userid: \"groot\", password: \"s3cret\" }";
        assert_eq!(
            mask_login_body(debug),
            "LoginRequest { userid: \"groot\", password: \"*******\" }"
        );
        let json = "{\"userid\":\"groot\",\"password\":\"s3cret\"}";
        assert_eq!(
            mask_http_body("/login", json),
            "{\"userid\":\"groot\",\"password\":\"*******\"}"
        );
    }

    #[test]
    fn leaves_other_paths_alone() {
        let body = "{ q(func: has(password)) { uid } }";
        assert_eq!(mask_http_body("/query", body), body);
    }
}
