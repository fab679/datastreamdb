//! Alpha proposal application (`worker/draft.go` apply path).
//!
//! [`AlphaApplier`] implements [`Applier`](datastreamdb_raft::Applier) for an alpha
//! group: it stages raft-committed mutations into explicit-timestamp posting
//! transactions, finalizes them on oracle deltas, applies schema/type/drops,
//! and snapshots the engine (postings, schema, oracle) for new members.
//!
//! Replay safety: raft replays committed suffixes after restarts. Re-staging
//! is idempotent (fresh caches), and re-finalize either commits (first run)
//! or reports a benign self-conflict (data already durable via the posting
//! layer's max-version guard). Deltas for unknown transactions only advance
//! the oracle maximum.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use datastreamdb_posting::{PostingEngine, Txn};
use datastreamdb_proto::pb::{self, Mutations, OracleDelta, Proposal, SchemaUpdate, TypeUpdate};
use datastreamdb_proto::{HintType, ValType};
use prost::Message as _;

use crate::groups::GroupState;

/// Partitions snapshotted for a full engine copy.
const SNAPSHOT_PARTITIONS: [&str; 2] = ["posting", datastreamdb_query::mutation::META_PARTITION];

/// Staged transaction plus the predicates it touches (schema-change aborts).
struct Staged {
    /// Posting transaction holding staged deltas.
    txn: Txn,
    /// Predicates mutated (namespaced).
    preds: HashSet<String>,
}

/// Alpha applier: raft proposals onto one embedded engine.
pub struct AlphaApplier {
    /// Posting engine receiving the proposals.
    engine: Arc<PostingEngine>,
    /// Owning group (schema reinstall scope after drop-all).
    group_id: u32,
    /// Membership view (for `Proposal.state` updates).
    groups: Option<Arc<GroupState>>,
    /// Staged transactions by start timestamp.
    staged: Arc<Mutex<HashMap<u64, Staged>>>,
    /// Fired when a finalized txn touched ACL data (rule-cache refresh).
    refresh_notify: Option<Arc<tokio::sync::Notify>>,
    /// Change-data-capture emitter (`None` disables CDC).
    cdc: Option<Arc<crate::cdc::Cdc>>,
}

impl Clone for AlphaApplier {
    fn clone(&self) -> Self {
        Self {
            engine: Arc::clone(&self.engine),
            group_id: self.group_id,
            groups: self.groups.clone(),
            staged: Arc::clone(&self.staged),
            refresh_notify: self.refresh_notify.clone(),
            cdc: self.cdc.clone(),
        }
    }
}

/// Applier failures: only corruption is fatal; apply outcomes ride in the
/// response bytes so proposers (not raft) see conflicts.
#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    /// Proposal bytes do not decode (corrupt log).
    #[error("bad proposal bytes: {0}")]
    Decode(String),
    /// Snapshot bytes do not decode (corrupt snapshot).
    #[error("bad snapshot bytes: {0}")]
    Snapshot(String),
    /// Storage failure during apply/snapshot/restore.
    #[error("storage failed: {0}")]
    Storage(String),
}

/// Encodes an apply outcome: empty means success.
fn ok_response() -> Vec<u8> {
    Vec::new()
}

/// Encodes an apply failure: `0x01` plus the message.
fn err_response(message: impl std::fmt::Display) -> Vec<u8> {
    let mut out = vec![1];
    out.extend_from_slice(message.to_string().as_bytes());
    out
}

/// Decodes an apply outcome.
///
/// # Errors
///
/// Returns the embedded message when the outcome reports failure.
pub fn parse_outcome(response: &[u8]) -> Result<(), String> {
    if response.is_empty() {
        return Ok(());
    }
    if response[0] == 1 {
        return Err(String::from_utf8_lossy(&response[1..]).into_owned());
    }
    Err(format!("bad outcome prefix {}", response[0]))
}

impl AlphaApplier {
    /// Binds an engine to a group (`groups` receives membership proposals).
    pub fn new(engine: Arc<PostingEngine>, group_id: u32, groups: Option<Arc<GroupState>>) -> Self {
        Self {
            engine,
            group_id,
            groups,
            staged: Arc::new(Mutex::new(HashMap::new())),
            refresh_notify: None,
            cdc: None,
        }
    }

    /// Arms rule-cache refresh on ACL-data finalizes (server boot only).
    #[must_use]
    pub fn with_refresh_notify(mut self, notify: Arc<tokio::sync::Notify>) -> Self {
        self.refresh_notify = Some(notify);
        self
    }

    /// Arms the change-data-capture emitter (server boot only).
    #[must_use]
    pub fn with_cdc(mut self, cdc: Arc<crate::cdc::Cdc>) -> Self {
        self.cdc = Some(cdc);
        self
    }

    /// Conflict fingerprints + predicates staged under `start_ts`.
    ///
    /// Feeds the commit path (`TxnContext.keys`/`preds` for zero). Empty
    /// when nothing (or nothing anymore) is staged there.
    ///
    /// # Panics
    ///
    /// Panics when the staged lock is poisoned.
    #[must_use]
    pub fn staged_conflicts(&self, start_ts: u64) -> (HashSet<u64>, HashSet<String>) {
        self.staged
            .lock()
            .expect("staged lock")
            .get(&start_ts)
            .map_or_else(
                || (HashSet::new(), HashSet::new()),
                |staged| (staged.txn.conflicts(), staged.preds.clone()),
            )
    }

    /// Aborts staged transactions touching `pred` (schema/predicate drops).
    fn abort_staged_on(&self, pred: &str) {
        let mut staged = self.staged.lock().expect("staged lock");
        let victims: Vec<u64> = staged
            .iter()
            .filter(|(_, entry)| entry.preds.contains(pred))
            .map(|(start_ts, _)| *start_ts)
            .collect();
        for start_ts in victims {
            if let Some(entry) = staged.remove(&start_ts) {
                self.engine.abort(&entry.txn);
            }
        }
    }

    /// Aborts every staged transaction (drop-all/data flows).
    fn abort_all_staged(&self) {
        let mut staged = self.staged.lock().expect("staged lock");
        for (_, entry) in staged.drain() {
            self.engine.abort(&entry.txn);
        }
    }

    /// Stages a mutation batch: drops, schema/types, or edges.
    fn stage_mutations(&self, mutations: &Mutations) -> Result<(), String> {
        if let Some(cdc) = &self.cdc {
            cdc.on_mutations(mutations);
        }
        match pb::mutations::DropOp::try_from(mutations.drop_op)
            .unwrap_or(pb::mutations::DropOp::None)
        {
            pb::mutations::DropOp::None => {}
            pb::mutations::DropOp::All => return self.apply_drop_all(),
            pb::mutations::DropOp::Data => return self.apply_drop_data(&mutations.drop_value),
            pb::mutations::DropOp::Type => {
                self.apply_drop_type(&mutations.drop_value);
                return Ok(());
            }
            pb::mutations::DropOp::AllInNs => return self.apply_drop_ns(&mutations.drop_value),
        }
        if !mutations.schema.is_empty() || !mutations.types.is_empty() {
            // Go applies schema/types and returns, ignoring same-proposal
            // edges; coordinators send them separately.
            return self.apply_schema_types(&mutations.schema, &mutations.types);
        }
        self.stage_edges(mutations)
    }

    /// Drops everything and reinstalls the initial schema (drop-all).
    fn apply_drop_all(&self) -> Result<(), String> {
        self.abort_all_staged();
        self.engine.delete_all().map_err(|err| err.to_string())?;
        self.engine.schema().delete_all();
        if self.group_id == 1 {
            for update in datastreamdb_schema::initial_schema(datastreamdb_x::ROOT_NAMESPACE, false)
            {
                self.engine.schema().set(&update.predicate.clone(), update);
            }
        }
        for update in datastreamdb_schema::initial_types(datastreamdb_x::ROOT_NAMESPACE, false) {
            self.engine
                .schema()
                .set_type(&update.type_name.clone(), update);
        }
        Ok(())
    }

    /// Drops one namespace's data, keeping schema (drop-data).
    fn apply_drop_data(&self, ns: &str) -> Result<(), String> {
        let ns: u64 = ns
            .parse()
            .map_err(|err| format!("bad namespace {ns:?}: {err}"))?;
        self.abort_all_staged();
        self.engine.delete_data(ns).map_err(|err| err.to_string())
    }

    /// Drops one type definition (drop-type).
    fn apply_drop_type(&self, name: &str) {
        self.engine.schema().delete_type(name);
    }

    /// Drops one namespace entirely (drop-all-in-ns).
    fn apply_drop_ns(&self, ns: &str) -> Result<(), String> {
        let ns: u64 = ns
            .parse()
            .map_err(|err| format!("bad namespace {ns:?}: {err}"))?;
        self.abort_all_staged();
        self.engine
            .delete_namespace(ns)
            .map_err(|err| err.to_string())?;
        for update in datastreamdb_schema::initial_schema(ns, false) {
            self.engine.schema().set(&update.predicate.clone(), update);
        }
        for update in datastreamdb_schema::initial_types(ns, false) {
            self.engine
                .schema()
                .set_type(&update.type_name.clone(), update);
        }
        Ok(())
    }

    /// Applies schema/type updates with index rebuilds.
    fn apply_schema_types(
        &self,
        schema: &[SchemaUpdate],
        types: &[TypeUpdate],
    ) -> Result<(), String> {
        let read_ts = self.engine.oracle().max_assigned();
        for update in schema {
            self.abort_staged_on(&update.predicate);
            let old = self.engine.schema().get(&update.predicate, false);
            self.engine
                .schema()
                .set(&update.predicate.clone(), update.clone());
            let rebuild = datastreamdb_posting::IndexRebuild {
                attr: update.predicate.clone(),
                start_ts: read_ts,
                old_schema: old,
                current_schema: update.clone(),
            };
            rebuild
                .rebuild(&self.engine)
                .map_err(|err| err.to_string())?;
        }
        for update in types {
            self.engine
                .schema()
                .set_type(&update.type_name.clone(), update.clone());
        }
        Ok(())
    }

    /// Derives schema for unknown predicates, then stages every edge.
    fn stage_edges(&self, mutations: &Mutations) -> Result<(), String> {
        if mutations.start_ts == 0 {
            return Err("StartTs must be provided".to_owned());
        }
        // Schema derivation first (mirrors Go's schemaMap pass).
        let mut derived: HashMap<String, ValType> = HashMap::new();
        for edge in &mutations.edges {
            if edge.op == datastreamdb_proto::EdgeOp::Del as i32 {
                continue;
            }
            if self.engine.schema().type_of(&edge.attr).is_err() {
                let val_type = ValType::try_from(edge.value_type).unwrap_or(ValType::Default);
                derived.entry(edge.attr.clone()).or_insert(val_type);
            }
        }
        for (attr, val_type) in &derived {
            self.derive_schema(attr, *val_type, mutations);
        }

        let mut edges = mutations.edges.clone();
        edges.sort_by(|a, b| a.attr.cmp(&b.attr).then_with(|| a.entity.cmp(&b.entity)));
        // Stable order note: `sort_by` is stable, preserving the client's
        // edge order within (attr, entity) groups like Go's SliceStable.

        let txn = self.engine.begin_at(mutations.start_ts);
        let mut preds = HashSet::new();
        for edge in &mut edges {
            if edge.entity == 0 && edge.value == b"*" {
                // Drop-predicate edge: delete and stop (mirrors Go's return).
                self.abort_staged_on(&edge.attr);
                self.engine
                    .delete_predicate(&edge.attr)
                    .map_err(|err| err.to_string())?;
                self.engine.abort(&txn);
                return Ok(());
            }
            let mut owned = edge.clone();
            datastreamdb_query::mutation::validate_and_convert(&self.engine, &mut owned)
                .map_err(|err| err.to_string())?;
            datastreamdb_query::mutation::stage_edge(&txn, &owned)
                .map_err(|err| err.to_string())?;
            preds.insert(std::mem::take(&mut owned.attr));
        }
        // Surface write-write conflicts here (like Go's staging checks) so
        // the Mutate RPC fails instead of staging doomed data. The finalize
        // check stays as the backstop for interleaved commits.
        if self
            .engine
            .oracle()
            .has_conflict(mutations.start_ts, &txn.conflicts())
        {
            self.engine.abort(&txn);
            return Err("conflict with a committed transaction".to_owned());
        }
        self.staged
            .lock()
            .expect("staged lock")
            .insert(mutations.start_ts, Staged { txn, preds });
        Ok(())
    }

    /// Derives schema for a new predicate (Go `createSchema` rules).
    fn derive_schema(&self, attr: &str, val_type: ValType, mutations: &Mutations) {
        if let Some(mut update) = self.engine.schema().get(attr, true) {
            update.value_type = val_type as i32;
            self.engine.schema().set(attr, update);
            return;
        }
        let mut update = SchemaUpdate {
            predicate: attr.to_owned(),
            value_type: val_type as i32,
            ..Default::default()
        };
        if val_type == ValType::Uid {
            update.list = true;
        }
        match mutations
            .metadata
            .as_ref()
            .and_then(|meta| meta.pred_hints.get(attr))
            .and_then(|hint| HintType::try_from(*hint).ok())
            .unwrap_or(HintType::Default)
        {
            HintType::Single => update.list = false,
            HintType::List => update.list = true,
            HintType::Default => {}
        }
        self.engine.schema().set(attr, update);
    }

    /// Finalizes staged transactions from an oracle delta.
    fn finalize_delta(&self, delta: &OracleDelta) {
        if let Some(cdc) = &self.cdc {
            let txns: Vec<(u64, u64)> = delta
                .txns
                .iter()
                .map(|status| (status.start_ts, status.commit_ts))
                .collect();
            cdc.on_delta(&txns);
        }
        for status in &delta.txns {
            let staged = self
                .staged
                .lock()
                .expect("staged lock")
                .remove(&status.start_ts);
            let Some(staged) = staged else {
                continue;
            };
            if staged.preds.iter().any(|pred| {
                // Staged predicates may be namespaced or bare; the table
                // holds bare names, so match past any `ns-` prefix.
                let attr = pred.rsplit('-').next().unwrap_or(pred);
                datastreamdb_x::is_acl_predicate(attr)
            }) {
                if let Some(notify) = &self.refresh_notify {
                    notify.notify_one();
                }
            }
            if status.commit_ts == 0 {
                self.engine.abort(&staged.txn);
                continue;
            }
            if let Err(err) = self.engine.commit_at(&staged.txn, status.commit_ts) {
                // Replay self-conflicts are benign (data already durable);
                // anything else is unexpected but must not wedge the batch.
                tracing::warn!(
                    "finalize start_ts={} commit_ts={} failed: {err}",
                    status.start_ts,
                    status.commit_ts
                );
            }
        }
        // Advance the oracle past every decision (idempotent for known ones).
        self.engine.process_delta(delta);
    }
}

impl datastreamdb_raft::Applier for AlphaApplier {
    type Error = ApplyError;

    async fn apply(&self, data: &[u8]) -> Result<Vec<u8>, Self::Error> {
        let proposal = Proposal::decode(data).map_err(|err| ApplyError::Decode(err.to_string()))?;
        // Dispatch mirrors Go's applyCommitted order.
        if let Some(mutations) = &proposal.mutations {
            return Ok(match self.stage_mutations(mutations) {
                Ok(()) => ok_response(),
                Err(message) => err_response(message),
            });
        }
        if proposal.kv.is_empty()
            && proposal.state.is_none()
            && proposal.clean_predicate.is_empty()
            && proposal.delta.is_none()
            && proposal.ext_snapshot_state.is_none()
            && proposal.snapshot.is_none()
            && proposal.restore.is_none()
            && proposal.delete_ns.is_none()
            && proposal.cdc_state.is_none()
        {
            return Ok(ok_response());
        }
        if !proposal.kv.is_empty() {
            // Predicate-move batches (Go `populateKeyValues`).
            for kv in &proposal.kv {
                if let Err(err) = datastreamdb_backup::restore::apply_store_kv(
                    &self.engine,
                    self.engine.schema(),
                    &kv.key,
                    &kv.value,
                    &kv.user_meta,
                    kv.version,
                ) {
                    return Err(ApplyError::Storage(err.to_string()));
                }
            }
            return Ok(ok_response());
        }
        if let Some(state) = &proposal.state {
            if let Some(groups) = &self.groups {
                groups.apply_state(state);
            }
            return Ok(ok_response());
        }
        if !proposal.clean_predicate.is_empty() {
            // Predicate-move cleanup (checksum gating is zero's job and
            // stays deferred; the delete applies immediately).
            if let Err(err) = crate::predicate_move::clean_predicate_local(
                &self.engine,
                &proposal.clean_predicate,
            ) {
                return Err(ApplyError::Storage(err.to_string()));
            }
            return Ok(ok_response());
        }
        if let Some(delta) = &proposal.delta {
            self.finalize_delta(delta);
            return Ok(ok_response());
        }
        if proposal.ext_snapshot_state.is_some() {
            // External snapshot streaming (bulk import) is M6 scope.
            return Ok(ok_response());
        }
        if proposal.snapshot.is_some() {
            // Snapshots are policy-driven (openraft); nothing to do on apply.
            return Ok(ok_response());
        }
        if proposal.restore.is_some() {
            return Ok(err_response("restore proposals need M6 backup support"));
        }
        if let Some(delete_ns) = &proposal.delete_ns {
            if delete_ns.namespace == datastreamdb_x::ROOT_NAMESPACE {
                return Ok(err_response("cannot delete the root namespace"));
            }
            return Ok(match self.engine.delete_namespace(delete_ns.namespace) {
                Ok(()) => ok_response(),
                Err(err) => err_response(err),
            });
        }
        if let Some(state) = &proposal.cdc_state {
            // Replicated flush mark (Go `updateCDCState`).
            if let Some(cdc) = &self.cdc {
                cdc.set_sent_ts(state.sent_ts);
            }
            return Ok(ok_response());
        }
        Ok(ok_response())
    }

    async fn snapshot(&self) -> Result<Vec<u8>, Self::Error> {
        let mut pairs = Vec::new();
        for partition in SNAPSHOT_PARTITIONS {
            let kvs = self
                .engine
                .store()
                .scan_prefix(partition, b"")
                .map_err(|err| ApplyError::Storage(err.to_string()))?;
            pairs.extend(kvs.into_iter().map(|(key, value)| SnapKv {
                partition: partition.to_owned(),
                key,
                value,
            }));
        }
        let schema_state = self.engine.schema();
        let mut schema = Vec::new();
        for pred in schema_state.predicates() {
            if let Some(update) = schema_state.get(&pred, false) {
                schema.push(update.encode_to_vec());
            }
        }
        let mut types = Vec::new();
        for name in schema_state.types() {
            if let Some(update) = schema_state.get_type(&name) {
                types.push(update.encode_to_vec());
            }
        }
        let snap = AlphaSnapshot {
            pairs,
            schema,
            types,
            max_assigned: self.engine.oracle().max_assigned(),
            committed: self.engine.oracle().export_committed(),
        };
        serde_json::to_vec(&snap).map_err(|err| ApplyError::Snapshot(err.to_string()))
    }

    async fn restore(&self, snapshot: &[u8]) -> Result<(), Self::Error> {
        let snap: AlphaSnapshot = serde_json::from_slice(snapshot)
            .map_err(|err| ApplyError::Snapshot(err.to_string()))?;
        self.abort_all_staged();
        // Replace store content partition by partition (clear, then write).
        for partition in SNAPSHOT_PARTITIONS {
            let stale = self
                .engine
                .store()
                .scan_prefix(partition, b"")
                .map_err(|err| ApplyError::Storage(err.to_string()))?;
            let mut batch = datastreamdb_store::WriteBatch::default();
            for (key, _) in &stale {
                batch.delete(partition, key);
            }
            self.engine
                .store()
                .write_batch(&batch)
                .map_err(|err| ApplyError::Storage(err.to_string()))?;
        }
        let mut batch = datastreamdb_store::WriteBatch::default();
        for pair in &snap.pairs {
            batch.put(&pair.partition, &pair.key, &pair.value);
        }
        self.engine
            .store()
            .write_batch(&batch)
            .map_err(|err| ApplyError::Storage(err.to_string()))?;
        // Replace schema and oracle state.
        let schema_state = self.engine.schema();
        schema_state.delete_all();
        for bytes in &snap.schema {
            let update = SchemaUpdate::decode(bytes.as_slice())
                .map_err(|err| ApplyError::Snapshot(err.to_string()))?;
            schema_state.set(&update.predicate.clone(), update);
        }
        for bytes in &snap.types {
            let update = TypeUpdate::decode(bytes.as_slice())
                .map_err(|err| ApplyError::Snapshot(err.to_string()))?;
            schema_state.set_type(&update.type_name.clone(), update);
        }
        self.engine.oracle().reset_txns();
        self.engine.process_delta(&OracleDelta {
            txns: Vec::new(),
            max_assigned: snap.max_assigned,
            ..Default::default()
        });
        self.engine.oracle().import_committed(&snap.committed);
        Ok(())
    }
}

/// One stored key/value with its partition.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct SnapKv {
    /// Partition name.
    partition: String,
    /// Key bytes.
    key: Vec<u8>,
    /// Value bytes.
    value: Vec<u8>,
}

/// Full engine snapshot: postings, schema, oracle.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct AlphaSnapshot {
    /// Every key/value in every snapshotted partition.
    pairs: Vec<SnapKv>,
    /// Encoded [`SchemaUpdate`] protos.
    schema: Vec<Vec<u8>>,
    /// Encoded [`TypeUpdate`] protos.
    types: Vec<Vec<u8>>,
    /// Highest assigned timestamp.
    max_assigned: u64,
    /// Conflict fingerprints to commit timestamps.
    committed: HashMap<u64, u64>,
}

/// Formats a conflict fingerprint the way Go's `FillContext` does.
///
/// Go renders the `u64` in base 36 so every language client can carry it
/// in `TxnContext.keys`; zero parses it back for conflict detection.
///
/// # Panics
///
/// Panics only on ASCII violation (impossible by construction).
#[must_use]
pub fn conflict_key_string(fingerprint: u64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if fingerprint == 0 {
        return "0".to_owned();
    }
    let mut out = Vec::new();
    let mut rest = fingerprint;
    while rest > 0 {
        out.push(DIGITS[(rest % 36) as usize]);
        rest /= 36;
    }
    out.reverse();
    String::from_utf8(out).expect("base36 is ASCII")
}

/// Formats staged predicates the way zero expects (`{gid}-{predicate}`).
#[must_use]
pub fn txn_pred_keys(
    group_id: u32,
    preds: &HashSet<String, impl std::hash::BuildHasher>,
) -> Vec<String> {
    let mut keys: Vec<String> = preds
        .iter()
        .map(|pred| format!("{group_id}-{pred}"))
        .collect();
    keys.sort();
    keys
}
