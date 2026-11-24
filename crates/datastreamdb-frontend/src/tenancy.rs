//! Multi-tenancy (`edgraph/namespace.go` + `edgraph/multi_tenancy.go`).
//!
//! [`TxnCoordinator::create_namespace`] leases a namespace id from
//! zero, installs the initial schema/types, and bootstraps
//! guardians/groot; [`TxnCoordinator::drop_namespace`] drops group
//! data, tells zero, and records the drop for backups. Capability
//! checks (guardians only) stay with the callers, like Go's
//! `AuthorizeCapability` middlewares.

use std::collections::{BTreeMap, HashMap};

use datastreamdb_proto::pb::{Mutations, Num, num::LeaseType};

use crate::txn::{CoordError, TxnCoordinator};

impl TxnCoordinator {
    /// Creates a namespace: lease, initial schema, guardian/groot
    /// (Go `CreateNamespaceInternal`).
    ///
    /// `password` seeds groot's password (`"password"` when empty,
    /// like Go's GraphQL default).
    ///
    /// # Errors
    ///
    /// Returns [`CoordError`] when zero refuses the lease or the
    /// install writes fail.
    pub async fn create_namespace(&self, password: Option<String>) -> Result<u64, CoordError> {
        let leased = self
            .zero()
            .assign_ids(Num {
                val: 1,
                r#type: LeaseType::NsId as i32,
                ..Num::default()
            })
            .await?;
        let ns = leased.start_id;
        self.install_namespace_schema(ns).await?;
        let password = password.filter(|text| !text.is_empty());
        self.bootstrap_acl(ns, password.as_deref()).await?;
        Ok(ns)
    }

    /// Installs the initial schema/types for one namespace on every
    /// group (Go `query.ApplyMutations` with `InitialSchema(ns)`).
    async fn install_namespace_schema(&self, ns: u64) -> Result<(), CoordError> {
        let acl_enabled = self.auth().is_some();
        let preds = datastreamdb_schema::initial_schema(ns, acl_enabled);
        let types = datastreamdb_schema::initial_types(ns, acl_enabled);
        let start_ts = self.posting().oracle().max_assigned().max(1);
        let mut by_group: BTreeMap<u32, Mutations> = BTreeMap::new();
        for update in &preds {
            let gid = self.group_for(&update.predicate).await?;
            by_group
                .entry(gid)
                .or_insert_with(|| Mutations {
                    group_id: gid,
                    start_ts,
                    ..Mutations::default()
                })
                .schema
                .push(update.clone());
        }
        for gid in self.groups().known_groups() {
            if gid == 0 {
                continue;
            }
            by_group
                .entry(gid)
                .or_insert_with(|| Mutations {
                    group_id: gid,
                    start_ts,
                    ..Mutations::default()
                })
                .types
                .extend(types.clone());
        }
        for mutations in by_group.into_values() {
            self.propose_group(mutations).await?;
        }
        Ok(())
    }

    /// Drops a namespace: group data first, then zero, then the
    /// backup drop record (Go `DeleteNamespace` +
    /// `ProcessDeleteNsRequest` + `InsertDropRecord`).
    ///
    /// # Errors
    ///
    /// Returns [`CoordError`] for the root or an unknown namespace,
    /// or when the drop fails.
    pub async fn drop_namespace(&self, ns: u64) -> Result<(), CoordError> {
        if ns == datastreamdb_x::ROOT_NAMESPACE {
            return Err(CoordError::Invalid(format!(
                "namespace [{ns}] cannot be deleted"
            )));
        }
        if !self.posting().schema().namespaces().contains(&ns) {
            return Err(CoordError::Invalid(format!(
                "error deleting non-existing namespace {ns:#x}"
            )));
        }
        self.worker()
            .delete_namespace_cluster(ns)
            .await
            .map_err(CoordError::from)?;
        self.zero()
            .delete_namespace(datastreamdb_proto::pb::DeleteNsRequest {
                namespace: ns,
                ..datastreamdb_proto::pb::DeleteNsRequest::default()
            })
            .await?;
        self.insert_drop_record(&format!("DROP_NS;{ns:#x}")).await
    }

    /// Commits one `datastreamdb.drop.op` helper record (Go
    /// `InsertDropRecord`).
    async fn insert_drop_record(&self, drop_op: &str) -> Result<(), CoordError> {
        let guardian = datastreamdb_acl::jwt::Identity::guardian();
        let start_ts = self.begin(false).await?;
        let out = self
            .execute_mutations(
                datastreamdb_x::ROOT_NAMESPACE,
                &[datastreamdb_proto::api::Mutation {
                    set_nquads: format!("_:r <datastreamdb.drop.op> {drop_op:?} .").into_bytes(),
                    ..datastreamdb_proto::api::Mutation::default()
                }],
                start_ts,
                &HashMap::new(),
                Some(&guardian),
            )
            .await?;
        self.commit(out.ctx).await?;
        Ok(())
    }

    /// Lists namespaces known to the local schema (Go
    /// `ListNamespaces`).
    #[must_use]
    pub fn list_namespaces(&self) -> Vec<u64> {
        let mut namespaces: Vec<u64> = self.posting().schema().namespaces().into_iter().collect();
        namespaces.sort_unstable();
        namespaces
    }
}
