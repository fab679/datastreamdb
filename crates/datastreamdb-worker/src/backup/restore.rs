//! Manifest-chain restore driver (embedded online restore).

use std::path::Path;
use std::sync::Arc;

use datastreamdb_backup::manifest::{
    Manifest, backup_name, select_restore_chain, verify_manifests,
};
use datastreamdb_posting::PostingEngine;
use datastreamdb_proto::pb::{RestoreRequest, Status as PbStatus};

use super::dest::{DestStore, read_master};
use crate::groups::GroupState;

/// Replays a manifest chain into the live engine (embedded online
/// restore: newest series back to its full backup, oldest first).
///
/// `key` decrypts the backup streams; `req.encryption_key_file` wins
/// when both are set (Go reads the restore key from its flag).
///
/// # Errors
///
/// Returns [`tonic::Status`] when the location is remote or unreadable,
/// the chain breaks, a backup file is missing, or a write fails.
pub fn run_restore(
    engine: &Arc<PostingEngine>,
    groups: &GroupState,
    req: &RestoreRequest,
    key: Option<&datastreamdb_enc::keys::DataKey>,
) -> Result<PbStatus, tonic::Status> {
    if req.group_id != groups.group_id() {
        return Err(tonic::Status::failed_precondition(format!(
            "this server doesn't serve group id: {}",
            req.group_id
        )));
    }
    let dest = DestStore::resolve(
        &req.location,
        &req.access_key,
        &req.secret_key,
        &req.session_token,
        req.anonymous,
    )?;
    let master = read_master(&dest)?;
    if master.manifests.is_empty() {
        return Err(tonic::Status::not_found(format!(
            "no backup manifest at {}",
            req.location
        )));
    }
    let chain = select_restore_chain(&master.manifests, &req.backup_id, req.backup_num)
        .map_err(|err| tonic::Status::internal(format!("broken backup chain: {err}")))?;
    // Oldest first (the selector returns newest first).
    let ordered: Vec<&Manifest> = chain.into_iter().rev().collect();
    let mut ordered = ordered;
    // Drop empty-prefix series members whose files vanished (Go skips
    // manifests with missing files before verifying).
    ordered.retain(|entry| {
        let file = format!(
            "{}/{}",
            entry.base.path,
            backup_name(entry.base.valid_read_ts(), req.group_id)
        );
        if dest.exists(&file) {
            true
        } else {
            tracing::warn!(
                "backup file [{file}] missing for backupId [{}] and backupNum [{}]",
                entry.base.backup_id,
                entry.base.backup_num
            );
            false
        }
    });
    let owned: Vec<Manifest> = ordered.iter().map(|entry| (*entry).clone()).collect();
    verify_manifests(&owned.iter().rev().cloned().collect::<Vec<_>>())
        .map_err(|err| tonic::Status::internal(format!("broken backup chain: {err}")))?;

    let file_key = if req.encryption_key_file.is_empty() {
        None
    } else {
        Some(
            datastreamdb_enc::keys::load_key_file(Path::new(&req.encryption_key_file)).map_err(
                |err| tonic::Status::invalid_argument(format!("bad encryption key: {err}")),
            )?,
        )
    };
    let key = file_key.as_ref().or(key);
    for entry in &ordered {
        let file = format!(
            "{}/{}",
            entry.base.path,
            backup_name(entry.base.valid_read_ts(), req.group_id)
        );
        let input = dest.open_stream(&file)?;
        let restore_req = datastreamdb_backup::restore::RestoreRequest {
            restore_ts: req.restore_ts,
            predicates: Vec::new(),
            encryption_key: key.cloned(),
            compression: entry.base.compression.clone(),
        };
        let stats = datastreamdb_backup::restore::restore_stream(
            engine,
            engine.schema(),
            &restore_req,
            std::io::BufReader::new(input),
        )
        .map_err(|err| tonic::Status::internal(format!("restore failed: {err}")))?;
        tracing::info!("restored {file} ({} kvs)", stats.written);
        for op in &entry.drop_operations {
            datastreamdb_backup::restore::apply_drop_operation(engine, engine.schema(), op)
                .map_err(|err| tonic::Status::internal(format!("drop replay failed: {err}")))?;
        }
    }
    Ok(PbStatus {
        code: 0,
        msg: String::new(),
    })
}
