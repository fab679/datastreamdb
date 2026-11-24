//! Per-instance audit entry point (Go `auditLogger` + interceptors).
//!
//! [`Auditor`] assembles [`AuditEvent`]s from
//! handler context (identity, peer, body, status) and fans out to
//! [`sinks`](crate::sink). Unlike Go's global, each server owns one,
//! so embedded instances never share a stream.

use std::collections::HashMap;
use std::sync::Arc;

use crate::event::{
    AuditEvent, GRPC, HTTP, POOR_MAN_AUTH, UNAUTHORISED_USER, UNKNOWN_NAMESPACE, UNKNOWN_USER,
    WEBSOCKET,
};
use crate::sink::SharedSink;

/// Credential shape behind a request (handlers already resolved it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credential {
    /// Authenticated identity (user id + groups resolved).
    Identity {
        /// User id.
        user: String,
    },
    /// Token presented but unverifiable.
    InvalidToken,
    /// Poor-man's `auth-token` header present.
    PoorMan,
    /// No credential presented.
    None,
}

impl Credential {
    /// Derives the recorded credential from handler context.
    #[must_use]
    pub fn from_outcome(user: Option<&str>, token_present: bool, poor_man: bool) -> Self {
        match (user, token_present, poor_man) {
            (Some(user), _, _) => Self::Identity {
                user: user.to_owned(),
            },
            (None, true, _) => Self::InvalidToken,
            (None, false, true) => Self::PoorMan,
            (None, false, false) => Self::None,
        }
    }
}

/// Per-instance auditor: skip checks, event assembly, fan-out.
#[derive(Clone)]
pub struct Auditor {
    /// Serving address recorded on every event.
    server_host: String,
    /// Whether ACL is on (shapes the anonymous user label).
    acl_on: bool,
    /// Event sinks.
    sinks: Vec<SharedSink>,
}

impl Auditor {
    /// Builds an auditor over `sinks`.
    #[must_use]
    pub fn new(server_host: String, acl_on: bool, sinks: Vec<SharedSink>) -> Self {
        Self {
            server_host,
            acl_on,
            sinks,
        }
    }

    /// Wraps one sink (tests, embedded callbacks).
    pub fn single(server_host: &str, acl_on: bool, sink: SharedSink) -> Self {
        Self::new(server_host.to_owned(), acl_on, vec![sink])
    }

    /// Records a gRPC call (Go `auditGrpc`, post-handler).
    pub fn grpc(
        &self,
        full_method: &str,
        client_host: &str,
        credential: &Credential,
        namespace: Option<u64>,
        req_body: &str,
        status: &str,
    ) {
        if crate::event::skip_method(full_method) {
            return;
        }
        let method = full_method.rsplit('/').next().unwrap_or(full_method);
        let body = if method == "Login" {
            crate::mask::mask_login_body(req_body)
        } else {
            req_body.to_owned()
        };
        self.emit(&AuditEvent {
            user: self.user_label(credential),
            namespace: namespace.unwrap_or(UNKNOWN_NAMESPACE),
            server_host: self.server_host.clone(),
            client_host: client_host.to_owned(),
            endpoint: full_method.to_owned(),
            req_type: GRPC.to_owned(),
            req: crate::event::truncate(body),
            status: status.to_owned(),
            query_params: HashMap::new(),
        });
    }

    /// Records a captured gRPC exchange (handler-integrated wiring).
    pub fn grpc_call(&self, call: &GrpcCall<'_>, user: Option<&str>, status: &str) {
        let credential = Credential::from_outcome(user, call.token_present, call.poor_man);
        self.grpc(
            call.method,
            &call.peer,
            &credential,
            call.namespace,
            &call.body,
            status,
        );
    }

    /// Records an HTTP call (Go `auditHttp`, post-handler).
    pub fn http(&self, call: &HttpCall<'_>) {
        if crate::event::skip_path(call.path) {
            return;
        }
        self.emit(&AuditEvent {
            user: self.user_label(call.credential),
            namespace: call.namespace,
            server_host: self.server_host.clone(),
            client_host: call.client_host.to_owned(),
            endpoint: call.path.to_owned(),
            req_type: HTTP.to_owned(),
            req: crate::event::truncate(crate::mask::mask_http_body(call.path, call.body)),
            status: call.status.to_owned(),
            query_params: call.query_params.clone(),
        });
    }

    /// Records a websocket subscription (Go `AuditWebSockets`).
    pub fn websocket(
        &self,
        client_host: &str,
        credential: &Credential,
        namespace: u64,
        query: &str,
    ) {
        self.emit(&AuditEvent {
            user: self.user_label(credential),
            namespace,
            server_host: self.server_host.clone(),
            client_host: client_host.to_owned(),
            endpoint: "/graphql".to_owned(),
            req_type: WEBSOCKET.to_owned(),
            req: crate::event::truncate(query.to_owned()),
            status: "OK".to_owned(),
            query_params: HashMap::new(),
        });
    }

    /// Maps credentials onto the recorded user label (Go `getUser`).
    fn user_label(&self, credential: &Credential) -> String {
        match credential {
            Credential::Identity { user } => user.clone(),
            Credential::InvalidToken => UNKNOWN_USER.to_owned(),
            Credential::PoorMan => POOR_MAN_AUTH.to_owned(),
            Credential::None => {
                if self.acl_on {
                    UNAUTHORISED_USER.to_owned()
                } else {
                    String::new()
                }
            }
        }
    }

    /// Fans one event out to every sink.
    fn emit(&self, event: &AuditEvent) {
        for sink in &self.sinks {
            sink.emit(event);
        }
    }
}

/// One gRPC exchange to audit.
#[derive(Debug, Clone)]
pub struct GrpcCall<'a> {
    /// Full method path (`/api.DataStreamDB/Query`).
    pub method: &'a str,
    /// Client peer address.
    pub peer: String,
    /// Rendered request body.
    pub body: String,
    /// Access token presented (valid or not).
    pub token_present: bool,
    /// Poor-man's `auth-token` presented.
    pub poor_man: bool,
    /// Tenancy namespace.
    pub namespace: Option<u64>,
}

/// One HTTP exchange to audit.
#[derive(Debug, Clone)]
pub struct HttpCall<'a> {
    /// Request path.
    pub path: &'a str,
    /// Client peer address.
    pub client_host: &'a str,
    /// Resolved credential.
    pub credential: &'a Credential,
    /// Tenancy namespace.
    pub namespace: u64,
    /// Raw request body.
    pub body: &'a str,
    /// Response status text.
    pub status: &'a str,
    /// Query parameters.
    pub query_params: HashMap<String, Vec<String>>,
}

/// Shared auditor handle.
pub type SharedAuditor = Arc<Auditor>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sink::{ChannelSink, FnSink};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn auditor() -> (Auditor, std::sync::mpsc::Receiver<AuditEvent>) {
        let (sender, receiver) = std::sync::mpsc::sync_channel(16);
        let sink: SharedSink = Arc::new(ChannelSink::new(sender));
        (Auditor::single("alpha:7080", true, sink), receiver)
    }

    #[test]
    fn grpc_skips_and_labels() {
        let (auditor, receiver) = auditor();
        auditor.grpc(
            "/raft.Raft/Heartbeat",
            "c",
            &Credential::None,
            None,
            "body",
            "OK",
        );
        assert!(receiver.try_recv().is_err());
        auditor.grpc(
            "/api.DataStreamDB/Query",
            "c",
            &Credential::Identity {
                user: "groot".to_owned(),
            },
            Some(3),
            "body",
            "OK",
        );
        let event = receiver.recv().expect("event");
        assert_eq!(event.user, "groot");
        assert_eq!(event.namespace, 3);
        assert_eq!(event.req_type, "Grpc");
    }

    #[test]
    fn anonymous_labels_follow_acl() {
        let (auditor, receiver) = auditor();
        auditor.grpc(
            "/api.DataStreamDB/Query",
            "c",
            &Credential::None,
            None,
            "",
            "OK",
        );
        assert_eq!(receiver.recv().expect("e").user, UNAUTHORISED_USER);
        auditor.grpc(
            "/api.DataStreamDB/Query",
            "c",
            &Credential::InvalidToken,
            None,
            "",
            "OK",
        );
        assert_eq!(receiver.recv().expect("e").user, UNKNOWN_USER);
        auditor.grpc(
            "/api.DataStreamDB/Query",
            "c",
            &Credential::PoorMan,
            None,
            "",
            "OK",
        );
        assert_eq!(receiver.recv().expect("e").user, POOR_MAN_AUTH);

        let open = Auditor::single("s", false, Arc::new(FnSink::new(|_| {})));
        assert_eq!(open.user_label(&Credential::None), "");
    }

    #[test]
    fn login_bodies_mask() {
        let (auditor, receiver) = auditor();
        auditor.grpc(
            "/api.DataStreamDB/Login",
            "c",
            &Credential::None,
            None,
            "LoginRequest { userid: \"groot\", password: \"s3cret\" }",
            "OK",
        );
        let event = receiver.recv().expect("event");
        assert!(event.req.contains("groot"));
        assert!(!event.req.contains("s3cret"));
    }

    #[test]
    fn http_paths_skip_and_mask() {
        let (auditor, receiver) = auditor();
        let anon = Credential::None;
        auditor.http(&HttpCall {
            path: "/health",
            client_host: "c",
            credential: &anon,
            namespace: 0,
            body: "",
            status: "OK",
            query_params: HashMap::new(),
        });
        assert!(receiver.try_recv().is_err());
        auditor.http(&HttpCall {
            path: "/login",
            client_host: "c",
            credential: &anon,
            namespace: 0,
            body: "{\"userid\":\"groot\",\"password\":\"s3cret\"}",
            status: "OK",
            query_params: HashMap::new(),
        });
        let event = receiver.recv().expect("event");
        assert!(!event.req.contains("s3cret"));
        assert_eq!(event.req_type, "Http");
    }

    #[test]
    fn events_fan_out_to_every_sink() {
        let count = Arc::new(AtomicUsize::new(0));
        let moved = count.clone();
        let (sender, receiver) = std::sync::mpsc::sync_channel(8);
        let auditor = Auditor::new(
            "s".to_owned(),
            false,
            vec![
                Arc::new(FnSink::new(move |_| {
                    moved.fetch_add(1, Ordering::SeqCst);
                })) as SharedSink,
                Arc::new(ChannelSink::new(sender)) as SharedSink,
            ],
        );
        auditor.websocket("c", &Credential::None, 0, "{ q }");
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(receiver.recv().expect("e").req_type, "Websocket");
    }
}
