//! Backup coordinator and group runner (Go `ProcessBackupRequest`,
//! `BackupGroup`, `backupCurrentGroup`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use datastreamdb_backup::manifest::{MasterManifest, backup_name};
use datastreamdb_posting::PostingEngine;
use datastreamdb_proto::pb::{BackupRequest, BackupResponse, DropOperation as PbDropOperation};

use super::dest::{DestStore, read_master, write_master};
use crate::groups::GroupState;

/// Current UTC time in Go's `20060102.150405.000` layout.
fn now_unix_ts() -> String {
    chrono::Utc::now().format("%Y%m%d.%H%M%S%.3f").to_string()
}

/// Predicates this group serves (empty tablets mean "everything local",
/// the embedded single-group default).
fn group_predicates(engine: &PostingEngine, groups: &GroupState) -> Vec<String> {
    let mut preds: Vec<String> = engine
        .schema()
        .predicates()
        .into_iter()
        .filter(|pred| groups.serves_tablet(pred))
        .collect();
    if preds.is_empty() {
        preds = engine.schema().predicates();
    }
    preds.sort();
    preds
}

/// Validates the new backup against the chain tip (Go
/// `checkBackupReadTsAdvanced` + the encryption match checks) and
/// returns the incremental floor (`0` for full backups).
fn chain_floor(
    master: &MasterManifest,
    read_ts: u64,
    force_full: bool,
    key: Option<&datastreamdb_enc::keys::DataKey>,
) -> Result<u64, tonic::Status> {
    let latest = master.manifests.last();
    let latest_read = latest.map_or(0, |entry| entry.base.valid_read_ts());
    if force_full || latest.is_none_or(|entry| entry.base.backup_type.is_empty()) {
        return Ok(0);
    }
    if read_ts <= latest_read {
        return Err(tonic::Status::failed_precondition(format!(
            "backup read_ts ({read_ts}) is not greater than the latest manifest's \
             read_ts ({latest_read}); this usually means Zero state was reset or \
             rebuilt while this backup chain was active. Use \"forceFull\" to start \
             a new chain."
        )));
    }
    if key.is_some() {
        if latest.is_some_and(|entry| !entry.base.encrypted) {
            return Err(tonic::Status::failed_precondition(
                "latest manifest indicates the last backup was not encrypted but this \
                 instance has encryption turned on. Try \"forceFull\" flag.",
            ));
        }
    } else if latest.is_some_and(|entry| entry.base.encrypted) {
        return Err(tonic::Status::failed_precondition(
            "latest manifest indicates the last backup was encrypted but this \
             instance has encryption turned off. Try \"forceFull\" flag.",
        ));
    }
    Ok(latest_read)
}

/// Converts crate drop records to their proto form.
fn pb_drop_operations(
    ops: &[datastreamdb_backup::manifest::DropOperation],
) -> Vec<PbDropOperation> {
    ops.iter()
        .map(|op| PbDropOperation {
            drop_op: op.drop_op,
            drop_value: op.drop_value.clone(),
        })
        .collect()
}

/// Backs up this group only: writes
/// `datastreamdb.{unix_ts}/r{read}-g{gid}.backup` and returns its drop
/// operations (Go `backupCurrentGroup`). No manifest is touched; the
/// coordinator assembles it.
///
/// `key` encrypts the backup stream (Go `WorkerConfig.EncryptionKey`).
///
/// # Errors
///
/// Returns [`tonic::Status`] when the group is not served, the
/// destination is remote, or any write fails.
pub fn run_backup_group(
    engine: &Arc<PostingEngine>,
    groups: &GroupState,
    req: &BackupRequest,
    key: Option<&datastreamdb_enc::keys::DataKey>,
) -> Result<BackupResponse, tonic::Status> {
    if req.group_id != groups.group_id() {
        return Err(tonic::Status::failed_precondition(format!(
            "backup request group mismatch. Mine: {}. Requested: {}",
            groups.group_id(),
            req.group_id
        )));
    }
    let dest = DestStore::resolve(
        &req.destination,
        &req.access_key,
        &req.secret_key,
        &req.session_token,
        req.anonymous,
    )?;
    dest.create_dir("")?;
    let read_ts = if req.read_ts == 0 {
        engine.oracle().max_assigned()
    } else {
        req.read_ts
    };
    engine.oracle().wait_for_ts(read_ts);
    let unix_ts = if req.unix_ts.is_empty() {
        now_unix_ts()
    } else {
        req.unix_ts.clone()
    };
    let backup_dir = format!("datastreamdb.{unix_ts}");
    dest.create_dir(&backup_dir)?;

    let predicates = if req.predicates.is_empty() {
        group_predicates(engine, groups)
    } else {
        req.predicates.clone()
    };
    let backup_req = datastreamdb_backup::backup::BackupRequest {
        read_ts,
        since_ts: req.since_ts,
        group_id: req.group_id,
        predicates,
        encryption_key: key.cloned(),
    };
    let file_name = format!("{backup_dir}/{}", backup_name(read_ts, req.group_id));
    let mut writer = dest.create_file(&file_name)?;
    let stats = datastreamdb_backup::backup::write_backup(
        engine,
        engine.schema(),
        &backup_req,
        &mut writer,
    )
    .map_err(|err| tonic::Status::internal(format!("backup failed: {err}")))?;
    writer.finish()?;
    tracing::info!(
        "backup complete: group {} at {read_ts} ({} kvs)",
        req.group_id,
        stats.kvs
    );
    Ok(BackupResponse {
        drop_operations: pb_drop_operations(&stats.drop_operations),
    })
}

/// Backs up a remote group over the network (Go `BackupGroup`).
async fn backup_group_remote(
    groups: &GroupState,
    peers: &Mutex<
        HashMap<
            String,
            datastreamdb_proto::pb::worker_client::WorkerClient<tonic::transport::Channel>,
            impl std::hash::BuildHasher,
        >,
    >,
    req: &BackupRequest,
) -> Result<BackupResponse, tonic::Status> {
    let addr = groups
        .leader_of(req.group_id)
        .map(|member| member.addr)
        .or_else(|| {
            groups
                .members(req.group_id)
                .into_values()
                .next()
                .map(|member| member.addr)
        })
        .ok_or_else(|| {
            tonic::Status::unavailable(format!("couldn't find a server in group {}", req.group_id))
        })?;
    let mut client = crate::predicate_move::dial(peers, &addr)
        .await
        .map_err(|err| tonic::Status::internal(err.to_string()))?;
    client
        .backup(req.clone())
        .await
        .map(tonic::Response::into_inner)
        .map_err(|err| {
            tonic::Status::internal(format!("backup error group {}: {err}", req.group_id))
        })
}

/// Builds one group's leg request plus its manifest predicate list.
fn leg_for(
    engine: &Arc<PostingEngine>,
    groups: &GroupState,
    req: &BackupRequest,
    gid: u32,
    read_ts: u64,
    since_ts: u64,
    unix_ts: &str,
) -> (Vec<String>, BackupRequest) {
    let mut preds = if req.group_id != 0 && !req.predicates.is_empty() {
        req.predicates.clone()
    } else {
        groups.predicates_of(gid)
    };
    if preds.is_empty() && gid == groups.group_id() {
        preds = group_predicates(engine, groups);
    }
    let mut leg = req.clone();
    leg.group_id = gid;
    leg.read_ts = read_ts;
    leg.since_ts = since_ts;
    unix_ts.clone_into(&mut leg.unix_ts);
    leg.predicates.clone_from(&preds);
    (preds, leg)
}

/// Runs one group's backup leg: blocking local write or a remote
/// group RPC (Go's per-group fan-out step).
async fn run_backup_leg(
    engine: &Arc<PostingEngine>,
    groups: &Arc<GroupState>,
    peers: &Mutex<
        HashMap<
            String,
            datastreamdb_proto::pb::worker_client::WorkerClient<tonic::transport::Channel>,
            impl std::hash::BuildHasher,
        >,
    >,
    leg: &BackupRequest,
    key: Option<&datastreamdb_enc::keys::DataKey>,
) -> Result<BackupResponse, tonic::Status> {
    if leg.group_id == groups.group_id() {
        let engine = engine.clone();
        let groups = groups.clone();
        let key = key.cloned();
        let leg = leg.clone();
        tokio::task::spawn_blocking(move || run_backup_group(&engine, &groups, &leg, key.as_ref()))
            .await
            .map_err(|err| tonic::Status::internal(format!("backup task failed: {err}")))?
            .map_err(|err| {
                tracing::error!("error received during backup: {err}");
                err
            })
    } else {
        backup_group_remote(groups, peers, leg).await
    }
}

/// Coordinates one backup across groups (Go `ProcessBackupRequest`).
///
/// With `req.group_id == 0` every known group is backed up (falling
/// back to this group when membership is empty); otherwise only that
/// group. Local legs run on blocking threads, remote legs dial the
/// destination leader, and exactly one manifest entry is appended.
///
/// # Errors
///
/// Returns [`tonic::Status`] when the read timestamp regresses, a
/// group is unreachable, the destination is remote, or any write
/// fails.
pub async fn process_backup_request(
    engine: &Arc<PostingEngine>,
    groups: &Arc<GroupState>,
    peers: &Mutex<
        HashMap<
            String,
            datastreamdb_proto::pb::worker_client::WorkerClient<tonic::transport::Channel>,
            impl std::hash::BuildHasher,
        >,
    >,
    req: &BackupRequest,
    key: Option<&datastreamdb_enc::keys::DataKey>,
) -> Result<BackupResponse, tonic::Status> {
    // Blocking setup (destination resolve, directory, manifest read)
    // runs off the async runtime: the blocking HTTP client panics when
    // built, used, or dropped on a runtime thread. The destination is
    // resolved per phase so it never crosses an await.
    let setup_engine = engine.clone();
    let setup_req = req.clone();
    let setup_key = key.cloned();
    let (read_ts, master, latest, since_ts, unix_ts) =
        tokio::task::spawn_blocking(move || -> Result<_, tonic::Status> {
            let dest = DestStore::resolve(
                &setup_req.destination,
                &setup_req.access_key,
                &setup_req.secret_key,
                &setup_req.session_token,
                setup_req.anonymous,
            )?;
            dest.create_dir("")?;
            let read_ts = if setup_req.read_ts == 0 {
                setup_engine.oracle().max_assigned()
            } else {
                setup_req.read_ts
            };
            let master = read_master(&dest)?;
            let latest = master.manifests.last().cloned().unwrap_or_default();
            let since_ts = chain_floor(&master, read_ts, setup_req.force_full, setup_key.as_ref())?;
            let unix_ts = if setup_req.unix_ts.is_empty() {
                now_unix_ts()
            } else {
                setup_req.unix_ts.clone()
            };
            Ok((read_ts, master, latest, since_ts, unix_ts))
        })
        .await
        .map_err(|err| tonic::Status::internal(format!("backup setup failed: {err}")))??;
    let targets = if req.group_id != 0 {
        vec![req.group_id]
    } else {
        let known = groups.known_groups();
        if known.is_empty() {
            vec![groups.group_id()]
        } else {
            known
        }
    };
    let mut drop_operations = Vec::new();
    let mut pred_map = HashMap::new();
    for gid in targets {
        let (preds, leg) = leg_for(engine, groups, req, gid, read_ts, since_ts, &unix_ts);
        pred_map.insert(gid, preds);
        let response = run_backup_leg(engine, groups, peers, &leg, key).await?;
        drop_operations.extend(response.drop_operations);
    }

    let kind = if since_ts == 0 { "full" } else { "incremental" };
    let (backup_id, backup_num) = if since_ts == 0 {
        (format!("backup-{}-{read_ts}", unix_ts.replace('.', "")), 1)
    } else {
        (latest.base.backup_id.clone(), latest.base.backup_num + 1)
    };
    let manifest = datastreamdb_backup::backup::manifest_for_backup(
        &datastreamdb_backup::backup::BackupRequest {
            read_ts,
            since_ts,
            group_id: req.group_id,
            predicates: Vec::new(),
            encryption_key: key.cloned(),
        },
        &datastreamdb_backup::backup::BackupStats {
            drop_operations: manifest_drops(&drop_operations),
            ..datastreamdb_backup::backup::BackupStats::default()
        },
        kind,
        &backup_id,
        backup_num,
        &format!("datastreamdb.{unix_ts}"),
        pred_map,
    );
    let mut master = master;
    master.manifests.push(manifest);
    let write_req = req.clone();
    tokio::task::spawn_blocking(move || -> Result<(), tonic::Status> {
        let dest = DestStore::resolve(
            &write_req.destination,
            &write_req.access_key,
            &write_req.secret_key,
            &write_req.session_token,
            write_req.anonymous,
        )?;
        write_master(&dest, &master)
    })
    .await
    .map_err(|err| tonic::Status::internal(format!("manifest write failed: {err}")))??;
    tracing::info!("backup completed OK");

    Ok(BackupResponse { drop_operations })
}

/// Converts proto drop operations back to manifest records.
fn manifest_drops(ops: &[PbDropOperation]) -> Vec<datastreamdb_backup::manifest::DropOperation> {
    ops.iter()
        .map(|op| datastreamdb_backup::manifest::DropOperation {
            drop_op: op.drop_op,
            drop_value: op.drop_value.clone(),
        })
        .collect()
}
