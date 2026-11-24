//! Admin GraphQL API subset (Go `graphql/admin`).
//!
//! [`build_admin_schema`] serves `health` plus the `backup`, `export`,
//! `restore`, `addNamespace`, and `deleteNamespace` mutations through
//! an [`AdminBackend`]. Shapes mirror Go's admin SDL (`Response`,
//! `*Payload`, `*Input`); guardian authorization stays at the HTTP
//! layer like Go's admin middlewares. Schema/config/user/group
//! administration arrives with the full admin epic.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use async_graphql::dynamic::{
    Field, FieldFuture, FieldValue, InputObject, InputValue, Object, Schema, TypeRef,
};

/// Admin failures (surfaced as GraphQL errors).
#[derive(Debug, thiserror::Error)]
pub enum AdminError {
    /// Backend failure with a client-safe message.
    #[error("{0}")]
    Backend(String),
}

/// One node's health row (Go `NodeState`, served subset).
#[derive(Debug, Clone)]
pub struct AdminNodeState {
    /// Node type (`alpha`).
    pub instance: String,
    /// Advertised address.
    pub address: String,
    /// Health status (`healthy`).
    pub status: String,
    /// Build version.
    pub version: String,
    /// Uptime in seconds.
    pub uptime: u64,
}

/// Operation outcome (Go `Response`).
#[derive(Debug, Clone)]
pub struct AdminResponse {
    /// Machine code (`Success`, …).
    pub code: String,
    /// Human message.
    pub message: String,
}

/// `BackupInput` arguments.
#[derive(Debug, Clone, Default)]
pub struct BackupArgs {
    /// Destination URL (dir or `s3://`/`minio://`).
    pub destination: String,
    /// S3 access key.
    pub access_key: String,
    /// S3 secret key.
    pub secret_key: String,
    /// S3 session token.
    pub session_token: String,
    /// Public bucket (no credentials).
    pub anonymous: bool,
    /// Force a full backup.
    pub force_full: bool,
}

/// `ExportInput` arguments.
#[derive(Debug, Clone, Default)]
pub struct ExportArgs {
    /// `rdf` (default) or `json`.
    pub format: String,
    /// Destination directory or bucket URL.
    pub destination: String,
    /// Namespace (`None` = caller's; negative = all for guardians).
    pub namespace: Option<i64>,
    /// S3 access key.
    pub access_key: String,
    /// S3 secret key.
    pub secret_key: String,
    /// S3 session token.
    pub session_token: String,
    /// Public bucket (no credentials).
    pub anonymous: bool,
}

/// `RestoreInput` arguments.
#[derive(Debug, Clone, Default)]
pub struct RestoreArgs {
    /// Backup location (dir or `s3://`/`minio://`).
    pub location: String,
    /// Backup series id (empty = latest).
    pub backup_id: String,
    /// Restore at most this many series entries (0 = all).
    pub backup_num: u64,
    /// S3 access key.
    pub access_key: String,
    /// S3 secret key.
    pub secret_key: String,
    /// S3 session token.
    pub session_token: String,
    /// Public bucket (no credentials).
    pub anonymous: bool,
    /// Key file for encrypted backups.
    pub encryption_key_file: String,
}

/// Admin operation backend (frontend implements this over the live cluster).
#[async_trait::async_trait]
pub trait AdminBackend: Send + Sync {
    /// Cluster health rows.
    async fn health(&self) -> Vec<AdminNodeState>;
    /// Runs a backup.
    async fn backup(&self, input: BackupArgs) -> Result<AdminResponse, AdminError>;
    /// Runs an export.
    async fn export_data(&self, input: ExportArgs) -> Result<AdminResponse, AdminError>;
    /// Runs a restore.
    async fn restore(&self, input: RestoreArgs) -> Result<AdminResponse, AdminError>;
    /// Creates a namespace; returns its id and message.
    async fn add_namespace(&self, password: Option<String>) -> Result<(u64, String), AdminError>;
    /// Deletes a namespace; returns its id and message.
    async fn delete_namespace(&self, namespace: u64) -> Result<(u64, String), AdminError>;
}

/// Reads an optional string field.
fn opt_string(input: &HashMap<String, async_graphql::Value>, name: &str) -> Option<String> {
    match input.get(name) {
        Some(async_graphql::Value::String(text)) => Some(text.clone()),
        _ => None,
    }
}

/// Reads an optional boolean field (absent means false).
fn opt_bool(input: &HashMap<String, async_graphql::Value>, name: &str) -> bool {
    matches!(input.get(name), Some(async_graphql::Value::Boolean(true)))
}

/// Reads an optional integer field.
fn opt_int(input: &HashMap<String, async_graphql::Value>, name: &str) -> Option<i64> {
    match input.get(name) {
        Some(async_graphql::Value::Number(number)) => number.as_i64(),
        _ => None,
    }
}

/// Unpacks the single `input` object argument into owned values.
fn input_map(
    ctx: &async_graphql::dynamic::ResolverContext<'_>,
) -> Result<HashMap<String, async_graphql::Value>, String> {
    let Some(value) = ctx.args.get("input") else {
        return Err("missing input".to_owned());
    };
    let input = value.object().map_err(|err| err.message)?;
    Ok(input
        .iter()
        .map(|(name, field)| (name.to_string(), field.as_value().clone()))
        .collect())
}

/// Fetches the backend from resolver data.
fn backend_of(
    ctx: &async_graphql::dynamic::ResolverContext<'_>,
) -> Result<Arc<dyn AdminBackend>, async_graphql::Error> {
    ctx.data::<Arc<dyn AdminBackend>>()
        .cloned()
        .map_err(|err| async_graphql::Error::new(err.message))
}

/// Converts JSON into a leaf field value.
fn json_leaf(value: &serde_json::Value) -> FieldValue<'static> {
    match value {
        serde_json::Value::Null => FieldValue::value(async_graphql::Value::Null),
        serde_json::Value::Bool(val) => FieldValue::value(async_graphql::Value::Boolean(*val)),
        serde_json::Value::Number(num) => {
            FieldValue::value(async_graphql::Value::Number(num.clone()))
        }
        serde_json::Value::String(text) => {
            FieldValue::value(async_graphql::Value::String(text.clone()))
        }
        serde_json::Value::Array(items) => FieldValue::list(items.iter().map(json_leaf)),
        serde_json::Value::Object(_) => FieldValue::owned_any(value.clone()),
    }
}

/// Nested resolver: plucks the response key from parent JSON.
fn json_field(name: &str, ty: TypeRef) -> Field {
    let name = name.to_owned();
    Field::new(name.clone(), ty, move |ctx| {
        let name = name.clone();
        FieldFuture::new(async move {
            let key = ctx.field().alias().unwrap_or_else(|| ctx.field().name());
            let Some(parent) = ctx.parent_value.downcast_ref::<serde_json::Value>() else {
                return Ok(None);
            };
            let _ = name;
            Ok(parent.get(key).map(json_leaf))
        })
    })
}

/// Builds a `{Backup,Export,Restore}Payload` object value (Go's
/// synchronous admin tasks report an empty `taskId`).
fn payload_value(response: &AdminResponse) -> FieldValue<'static> {
    FieldValue::owned_any(serde_json::json!({
        "response": {"code": response.code, "message": response.message},
        "taskId": "",
    }))
}

/// Named nullable type reference.
fn named(name: &str) -> TypeRef {
    TypeRef::Named(Cow::Owned(name.to_owned()))
}

/// Non-null named type reference.
fn required(name: &str) -> TypeRef {
    TypeRef::NonNull(Box::new(named(name)))
}

/// Builds the executable admin schema over `backend`.
///
/// # Errors
///
/// Returns [`async_graphql::dynamic::SchemaError`] when the schema
/// fails to build (a programming error, not user input).
pub fn build_admin_schema(
    backend: Arc<dyn AdminBackend>,
) -> Result<Schema, async_graphql::dynamic::SchemaError> {
    let query = Object::new("Query").field(Field::new(
        "health",
        TypeRef::List(Box::new(named("NodeState"))),
        |ctx| {
            FieldFuture::new(async move {
                let backend = backend_of(&ctx)?;
                let rows = backend.health().await;
                let items = rows.into_iter().map(|row| {
                    FieldValue::owned_any(serde_json::json!({
                        "instance": row.instance,
                        "address": row.address,
                        "status": row.status,
                        "version": row.version,
                        "uptime": row.uptime,
                    }))
                });
                Ok(Some(FieldValue::list(items)))
            })
        },
    ));

    let mutation = Object::new("Mutation")
        .field(backup_field())
        .field(export_field())
        .field(restore_field())
        .field(add_namespace_field())
        .field(delete_namespace_field());

    Schema::build("Query", Some("Mutation"), None)
        .register(query)
        .register(mutation)
        .register(node_state_object())
        .register(response_object())
        .register(payload_object("BackupPayload"))
        .register(payload_object("ExportPayload"))
        .register(payload_object("RestorePayload"))
        .register(namespace_payload_object())
        .register(backup_input_object())
        .register(export_input_object())
        .register(restore_input_object())
        .register(add_namespace_input_object())
        .register(delete_namespace_input_object())
        .data(backend)
        .finish()
}

/// `NodeState` output object.
fn node_state_object() -> Object {
    Object::new("NodeState")
        .field(json_field("instance", named("String")))
        .field(json_field("address", named("String")))
        .field(json_field("status", named("String")))
        .field(json_field("version", named("String")))
        .field(json_field("uptime", named("Int")))
}

/// `Response` output object.
fn response_object() -> Object {
    Object::new("Response")
        .field(json_field("code", named("String")))
        .field(json_field("message", named("String")))
}

/// `{Backup,Export,Restore}Payload` output objects.
fn payload_object(name: &str) -> Object {
    Object::new(name)
        .field(json_field("response", named("Response")))
        .field(json_field("taskId", named("String")))
}

/// `NamespacePayload` output object.
fn namespace_payload_object() -> Object {
    Object::new("NamespacePayload")
        .field(json_field("namespaceId", named("Int")))
        .field(json_field("message", named("String")))
}

/// String field helper for input objects.
fn string_field(input: InputObject, name: &str) -> InputObject {
    input.field(InputValue::new(name.to_owned(), named("String")))
}

/// `BackupInput` object.
fn backup_input_object() -> InputObject {
    let input = InputObject::new("BackupInput").field(InputValue::new(
        "destination".to_owned(),
        required("String"),
    ));
    let input = string_field(input, "accessKey");
    let input = string_field(input, "secretKey");
    let input = string_field(input, "sessionToken");
    let input = input.field(InputValue::new("anonymous".to_owned(), named("Boolean")));
    input.field(InputValue::new("forceFull".to_owned(), named("Boolean")))
}

/// `ExportInput` object.
fn export_input_object() -> InputObject {
    let input = string_field(InputObject::new("ExportInput"), "format");
    let input = string_field(input, "destination");
    let input = input.field(InputValue::new("namespace".to_owned(), named("Int")));
    let input = string_field(input, "accessKey");
    let input = string_field(input, "secretKey");
    let input = string_field(input, "sessionToken");
    input.field(InputValue::new("anonymous".to_owned(), named("Boolean")))
}

/// `RestoreInput` object.
fn restore_input_object() -> InputObject {
    let input = InputObject::new("RestoreInput")
        .field(InputValue::new("location".to_owned(), required("String")));
    let input = string_field(input, "backupId");
    let input = input.field(InputValue::new("backupNum".to_owned(), named("Int")));
    let input = string_field(input, "accessKey");
    let input = string_field(input, "secretKey");
    let input = string_field(input, "sessionToken");
    let input = input.field(InputValue::new("anonymous".to_owned(), named("Boolean")));
    string_field(input, "encryptionKeyFile")
}

/// `AddNamespaceInput` object.
fn add_namespace_input_object() -> InputObject {
    string_field(InputObject::new("AddNamespaceInput"), "password")
}

/// `DeleteNamespaceInput` object.
fn delete_namespace_input_object() -> InputObject {
    InputObject::new("DeleteNamespaceInput")
        .field(InputValue::new("namespaceId".to_owned(), required("Int")))
}

/// `backup(input:)` mutation field.
fn backup_field() -> Field {
    Field::new("backup", named("BackupPayload"), |ctx| {
        FieldFuture::new(async move {
            let backend = backend_of(&ctx)?;
            let args = input_map(&ctx).map_err(async_graphql::Error::new)?;
            let input = BackupArgs {
                destination: opt_string(&args, "destination").unwrap_or_default(),
                access_key: opt_string(&args, "accessKey").unwrap_or_default(),
                secret_key: opt_string(&args, "secretKey").unwrap_or_default(),
                session_token: opt_string(&args, "sessionToken").unwrap_or_default(),
                anonymous: opt_bool(&args, "anonymous"),
                force_full: opt_bool(&args, "forceFull"),
            };
            let response = backend
                .backup(input)
                .await
                .map_err(|err| async_graphql::Error::new(err.to_string()))?;
            Ok(Some(payload_value(&response)))
        })
    })
    .argument(InputValue::new("input".to_owned(), required("BackupInput")))
}

/// `export(input:)` mutation field.
fn export_field() -> Field {
    Field::new("export", named("ExportPayload"), |ctx| {
        FieldFuture::new(async move {
            let backend = backend_of(&ctx)?;
            let args = input_map(&ctx).map_err(async_graphql::Error::new)?;
            let input = ExportArgs {
                format: opt_string(&args, "format").unwrap_or_else(|| "rdf".to_owned()),
                destination: opt_string(&args, "destination").unwrap_or_default(),
                namespace: opt_int(&args, "namespace"),
                access_key: opt_string(&args, "accessKey").unwrap_or_default(),
                secret_key: opt_string(&args, "secretKey").unwrap_or_default(),
                session_token: opt_string(&args, "sessionToken").unwrap_or_default(),
                anonymous: opt_bool(&args, "anonymous"),
            };
            let response = backend
                .export_data(input)
                .await
                .map_err(|err| async_graphql::Error::new(err.to_string()))?;
            Ok(Some(payload_value(&response)))
        })
    })
    .argument(InputValue::new("input".to_owned(), required("ExportInput")))
}

/// `restore(input:)` mutation field.
fn restore_field() -> Field {
    Field::new("restore", named("RestorePayload"), |ctx| {
        FieldFuture::new(async move {
            let backend = backend_of(&ctx)?;
            let args = input_map(&ctx).map_err(async_graphql::Error::new)?;
            let input = RestoreArgs {
                location: opt_string(&args, "location").unwrap_or_default(),
                backup_id: opt_string(&args, "backupId").unwrap_or_default(),
                backup_num: u64::try_from(opt_int(&args, "backupNum").unwrap_or(0)).unwrap_or(0),
                access_key: opt_string(&args, "accessKey").unwrap_or_default(),
                secret_key: opt_string(&args, "secretKey").unwrap_or_default(),
                session_token: opt_string(&args, "sessionToken").unwrap_or_default(),
                anonymous: opt_bool(&args, "anonymous"),
                encryption_key_file: opt_string(&args, "encryptionKeyFile").unwrap_or_default(),
            };
            let response = backend
                .restore(input)
                .await
                .map_err(|err| async_graphql::Error::new(err.to_string()))?;
            Ok(Some(payload_value(&response)))
        })
    })
    .argument(InputValue::new(
        "input".to_owned(),
        required("RestoreInput"),
    ))
}

/// `addNamespace(input:)` mutation field.
fn add_namespace_field() -> Field {
    Field::new("addNamespace", named("NamespacePayload"), |ctx| {
        FieldFuture::new(async move {
            let backend = backend_of(&ctx)?;
            let password = input_map(&ctx)
                .ok()
                .as_ref()
                .and_then(|args| opt_string(args, "password"));
            let (namespace, message) = backend
                .add_namespace(password)
                .await
                .map_err(|err| async_graphql::Error::new(err.to_string()))?;
            Ok(Some(FieldValue::owned_any(serde_json::json!({
                "namespaceId": namespace,
                "message": message,
            }))))
        })
    })
    .argument(InputValue::new(
        "input".to_owned(),
        named("AddNamespaceInput"),
    ))
}

/// `deleteNamespace(input:)` mutation field.
fn delete_namespace_field() -> Field {
    Field::new("deleteNamespace", named("NamespacePayload"), |ctx| {
        FieldFuture::new(async move {
            let backend = backend_of(&ctx)?;
            let args = input_map(&ctx).map_err(async_graphql::Error::new)?;
            let namespace = u64::try_from(opt_int(&args, "namespaceId").unwrap_or(-1)).unwrap_or(0);
            let (namespace, message) = backend
                .delete_namespace(namespace)
                .await
                .map_err(|err| async_graphql::Error::new(err.to_string()))?;
            Ok(Some(FieldValue::owned_any(serde_json::json!({
                "namespaceId": namespace,
                "message": message,
            }))))
        })
    })
    .argument(InputValue::new(
        "input".to_owned(),
        required("DeleteNamespaceInput"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Recording stub backend.
    struct Stub {
        /// Seen backup destinations.
        pub backups: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl AdminBackend for Stub {
        async fn health(&self) -> Vec<AdminNodeState> {
            vec![AdminNodeState {
                instance: "alpha".to_owned(),
                address: "127.0.0.1:7080".to_owned(),
                status: "healthy".to_owned(),
                version: "0.1.0".to_owned(),
                uptime: 7,
            }]
        }

        async fn backup(&self, input: BackupArgs) -> Result<AdminResponse, AdminError> {
            self.backups.lock().expect("lock").push(input.destination);
            Ok(AdminResponse {
                code: "Success".to_owned(),
                message: "backup done".to_owned(),
            })
        }

        async fn export_data(&self, input: ExportArgs) -> Result<AdminResponse, AdminError> {
            if input.format != "rdf" && input.format != "json" {
                return Err(AdminError::Backend("bad format".to_owned()));
            }
            Ok(AdminResponse {
                code: "Success".to_owned(),
                message: format!("exported {}", input.format),
            })
        }

        async fn restore(&self, input: RestoreArgs) -> Result<AdminResponse, AdminError> {
            Ok(AdminResponse {
                code: "Success".to_owned(),
                message: format!("restored {}", input.location),
            })
        }

        async fn add_namespace(
            &self,
            password: Option<String>,
        ) -> Result<(u64, String), AdminError> {
            Ok((2, format!("pw len {}", password.map_or(0, |pw| pw.len()))))
        }

        async fn delete_namespace(&self, namespace: u64) -> Result<(u64, String), AdminError> {
            if namespace == 0 {
                return Err(AdminError::Backend("cannot delete default".to_owned()));
            }
            Ok((namespace, "deleted".to_owned()))
        }
    }

    /// Executes one admin operation against the stub.
    async fn execute(query: &str) -> serde_json::Value {
        let stub = Arc::new(Stub {
            backups: Mutex::new(Vec::new()),
        });
        let schema = build_admin_schema(stub).expect("schema");
        let response = schema.execute(async_graphql::Request::new(query)).await;
        serde_json::to_value(&response).expect("json")
    }

    #[tokio::test]
    async fn health_lists_nodes() {
        let value = execute("{ health { instance address status version uptime } }").await;
        assert_eq!(value["data"]["health"][0]["instance"], "alpha");
        assert_eq!(value["data"]["health"][0]["uptime"], 7);
    }

    #[tokio::test]
    async fn backup_passes_input_through() {
        let value = execute(
            r#"mutation { backup(input: {destination: "/tmp/b", forceFull: true}) { response { code message } } }"#,
        )
        .await;
        assert_eq!(value["data"]["backup"]["response"]["code"], "Success");
    }

    #[tokio::test]
    async fn export_rejects_bad_format() {
        let value =
            execute(r#"mutation { export(input: {format: "xml"}) { response { code } } }"#).await;
        assert!(value.get("errors").is_some());
    }

    #[tokio::test]
    async fn namespaces_roundtrip() {
        let value = execute(
            r#"mutation { addNamespace(input: {password: "pw"}) { namespaceId message } }"#,
        )
        .await;
        assert_eq!(value["data"]["addNamespace"]["namespaceId"], 2);
        let value = execute(
            "mutation { deleteNamespace(input: {namespaceId: 2}) { namespaceId message } }",
        )
        .await;
        assert_eq!(value["data"]["deleteNamespace"]["message"], "deleted");
        let value =
            execute("mutation { deleteNamespace(input: {namespaceId: 0}) { namespaceId } }").await;
        assert!(value.get("errors").is_some());
    }
}
