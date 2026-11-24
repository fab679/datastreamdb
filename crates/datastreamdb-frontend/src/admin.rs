//! Admin backend over the live coordinator (Go `graphql/admin`).
//!
//! [`FrontendAdmin`] implements
//! [`datastreamdb_graphql::admin::AdminBackend`]: health rows for this
//! alpha, synchronous `backup`/`export`/`restore` through the worker
//! cluster fan-out, and namespace create/delete through
//! [`TxnCoordinator`]. Operations run
//! synchronously (Go queues them and reports a task id); guardian
//! authorization stays at the HTTP layer.

use std::sync::Arc;
use std::time::Instant;

use datastreamdb_graphql::admin::{
    AdminBackend, AdminError, AdminNodeState, AdminResponse, BackupArgs, ExportArgs, RestoreArgs,
};

use crate::txn::TxnCoordinator;

/// Admin backend over one alpha's coordinator.
pub struct FrontendAdmin {
    /// Transaction coordinator (worker fan-out + tenancy).
    coordinator: Arc<TxnCoordinator>,
    /// Advertised address (health rows).
    addr: String,
    /// Process start (uptime).
    started: Instant,
}

impl FrontendAdmin {
    /// Serves admin operations over `coordinator`.
    #[must_use]
    pub fn new(coordinator: Arc<TxnCoordinator>, addr: String, started: Instant) -> Self {
        Self {
            coordinator,
            addr,
            started,
        }
    }
}

/// Maps a driver failure to a client-safe admin error.
fn backend(err: impl std::fmt::Display) -> AdminError {
    AdminError::Backend(err.to_string())
}

#[async_trait::async_trait]
impl AdminBackend for FrontendAdmin {
    async fn health(&self) -> Vec<AdminNodeState> {
        vec![AdminNodeState {
            instance: "alpha".to_owned(),
            address: self.addr.clone(),
            status: "healthy".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            uptime: self.started.elapsed().as_secs(),
        }]
    }

    async fn backup(&self, input: BackupArgs) -> Result<AdminResponse, AdminError> {
        let req = datastreamdb_proto::pb::BackupRequest {
            group_id: 0,
            destination: input.destination,
            access_key: input.access_key,
            secret_key: input.secret_key,
            session_token: input.session_token,
            anonymous: input.anonymous,
            ..datastreamdb_proto::pb::BackupRequest::default()
        };
        // `forceFull` is tracked for the async task epic; incremental
        // chains already restart when the destination is fresh.
        let _ = input.force_full;
        self.coordinator
            .worker()
            .backup_cluster(&req)
            .await
            .map_err(backend)?;
        Ok(AdminResponse {
            code: "Success".to_owned(),
            message: "Backup completed.".to_owned(),
        })
    }

    async fn export_data(&self, input: ExportArgs) -> Result<AdminResponse, AdminError> {
        if input.format != "rdf" && input.format != "json" {
            return Err(AdminError::Backend(format!(
                "invalid export format {:?}: want rdf or json",
                input.format
            )));
        }
        // Negative means every namespace (guardians only); the
        // exporter has no all-namespaces mode yet, so reject it
        // loudly instead of silently exporting the galaxy.
        let namespace = match input.namespace {
            None | Some(0) => datastreamdb_x::ROOT_NAMESPACE,
            Some(ns) => u64::try_from(ns).map_err(|_| {
                AdminError::Backend("exporting all namespaces is not supported yet".to_owned())
            })?,
        };
        let req = datastreamdb_proto::pb::ExportRequest {
            group_id: 0,
            format: if input.format.is_empty() {
                "rdf".to_owned()
            } else {
                input.format
            },
            destination: input.destination,
            access_key: input.access_key,
            secret_key: input.secret_key,
            session_token: input.session_token,
            anonymous: input.anonymous,
            namespace,
            ..datastreamdb_proto::pb::ExportRequest::default()
        };
        let response = self
            .coordinator
            .worker()
            .export_cluster(&req)
            .await
            .map_err(backend)?;
        if response.code != 0 {
            return Err(AdminError::Backend(response.msg));
        }
        Ok(AdminResponse {
            code: "Success".to_owned(),
            message: "Export completed.".to_owned(),
        })
    }

    async fn restore(&self, input: RestoreArgs) -> Result<AdminResponse, AdminError> {
        let req = datastreamdb_proto::pb::RestoreRequest {
            group_id: 0,
            location: input.location,
            backup_id: input.backup_id,
            backup_num: input.backup_num,
            access_key: input.access_key,
            secret_key: input.secret_key,
            session_token: input.session_token,
            anonymous: input.anonymous,
            encryption_key_file: input.encryption_key_file,
            ..datastreamdb_proto::pb::RestoreRequest::default()
        };
        let response = self
            .coordinator
            .worker()
            .restore_cluster(&req)
            .await
            .map_err(backend)?;
        if response.code != 0 {
            return Err(AdminError::Backend(response.msg));
        }
        Ok(AdminResponse {
            code: "Success".to_owned(),
            message: "Restore completed.".to_owned(),
        })
    }

    async fn add_namespace(&self, password: Option<String>) -> Result<(u64, String), AdminError> {
        let ns = self
            .coordinator
            .create_namespace(password)
            .await
            .map_err(backend)?;
        Ok((ns, "Created namespace successfully".to_owned()))
    }

    async fn delete_namespace(&self, namespace: u64) -> Result<(u64, String), AdminError> {
        if namespace == datastreamdb_x::ROOT_NAMESPACE {
            return Err(AdminError::Backend(
                "Cannot delete default namespace".to_owned(),
            ));
        }
        self.coordinator
            .drop_namespace(namespace)
            .await
            .map_err(backend)?;
        Ok((namespace, "Deleted namespace successfully".to_owned()))
    }
}
