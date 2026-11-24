//! Audit event records and skip tables (Go `audit/audit.go` constants).
//!
//! [`AuditEvent`] mirrors Go's struct field-for-field; JSON keys match
//! the `AuditI` attribute names Go emits.

use std::collections::HashMap;

/// Recorded when credentials are absent under ACL.
pub const UNAUTHORISED_USER: &str = "UnauthorisedUser";
/// Recorded when a presented token fails to parse.
pub const UNKNOWN_USER: &str = "UnknownUser";
/// Recorded when no namespace resolves.
pub const UNKNOWN_NAMESPACE: u64 = u64::MAX;
/// Recorded for poor-man's auth-token requests.
pub const POOR_MAN_AUTH: &str = "PoorManAuth";
/// gRPC request type tag.
pub const GRPC: &str = "Grpc";
/// HTTP request type tag.
pub const HTTP: &str = "Http";
/// WebSocket request type tag.
pub const WEBSOCKET: &str = "Websocket";

/// Maximum logged request length, bytes (4 KiB, like Go).
pub const MAX_REQ_LENGTH: usize = 4 << 10;

/// One audited request (Go `AuditEvent`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AuditEvent {
    /// Authenticated user (or a sentinel constant).
    pub user: String,
    /// Tenancy namespace (`UNKNOWN_NAMESPACE` when unresolved).
    pub namespace: u64,
    /// Serving alpha/zero address.
    #[serde(rename = "server")]
    pub server_host: String,
    /// Client peer address.
    #[serde(rename = "client")]
    pub client_host: String,
    /// Full RPC method or HTTP path.
    pub endpoint: String,
    /// [`GRPC`], [`HTTP`], or [`WEBSOCKET`].
    #[serde(rename = "req_type")]
    pub req_type: String,
    /// Masked, truncated request body.
    #[serde(rename = "req_body")]
    pub req: String,
    /// Status code name (`OK`, `PermissionDenied`, ...).
    pub status: String,
    /// HTTP query parameters (empty for gRPC).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub query_params: HashMap<String, Vec<String>>,
}

/// gRPC methods that bypass auditing (Go `skipApis`).
#[must_use]
pub fn skip_method(full_method: &str) -> bool {
    let name = full_method.rsplit('/').next().unwrap_or(full_method);
    matches!(
        name,
        "Heartbeat"
            | "RaftMessage"
            | "JoinCluster"
            | "IsPeer"
            | "StreamMembership"
            | "UpdateMembership"
            | "Oracle"
            | "Timestamps"
            | "ShouldServe"
            | "Connect"
            | "Check"
            | "Watch"
    )
}

/// HTTP paths that bypass auditing (Go `skipEPs`).
#[must_use]
pub fn skip_path(path: &str) -> bool {
    matches!(path, "/health" | "/state" | "/probe/graphql")
}

/// Truncates to [`MAX_REQ_LENGTH`] bytes (Go `truncate`).
#[must_use]
pub fn truncate(mut body: String) -> String {
    if body.len() > MAX_REQ_LENGTH {
        body.truncate(MAX_REQ_LENGTH);
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_tables_match_go() {
        assert!(!skip_method("/datastreamdb.DataStreamDB/Query"));
        assert!(!skip_method("/api.DataStreamDB/Login"));
        for skipped in [
            "/raft.Raft/Heartbeat",
            "/pb.Zero/Oracle",
            "/pb.Zero/Connect",
            "/grpc.health.v1.Health/Check",
        ] {
            assert!(skip_method(skipped), "{skipped}");
        }
        assert!(skip_path("/health"));
        assert!(skip_path("/state"));
        assert!(!skip_path("/query"));
        assert!(!skip_path("/login"));
    }

    #[test]
    fn truncate_caps_length() {
        assert_eq!(truncate("abc".to_owned()), "abc");
        assert_eq!(truncate("x".repeat(5000)).len(), MAX_REQ_LENGTH);
    }

    #[test]
    fn event_json_keys_match_go() {
        let event = AuditEvent {
            user: "groot".to_owned(),
            namespace: 0,
            server_host: "s".to_owned(),
            client_host: "c".to_owned(),
            endpoint: "/q".to_owned(),
            req_type: GRPC.to_owned(),
            req: "r".to_owned(),
            status: "OK".to_owned(),
            query_params: HashMap::new(),
        };
        let json = serde_json::to_value(&event).expect("json");
        for key in [
            "user",
            "namespace",
            "server",
            "client",
            "endpoint",
            "req_type",
            "req_body",
            "status",
        ] {
            assert!(json.get(key).is_some(), "{key}");
        }
        assert!(json.get("query_params").is_none());
    }
}
