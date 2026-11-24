//! Backup/restore/export RPC drivers (Go `worker/backup*.go`, `worker/export.go`).
//!
//! [`process_backup_request`] coordinates a backup across groups,
//! [`run_backup_group`] writes one group's data file, [`run_export`]
//! writes the gzip data/schema/GraphQL files, and [`run_restore`]
//! replays a manifest chain into the live engine. Destinations are
//! local directories or S3/MinIO buckets; blocking legs run on
//! `spawn_blocking` threads.

mod dest;
mod driver;
mod export;
mod restore;

pub use driver::{process_backup_request, run_backup_group};
pub use export::run_export;
pub use restore::run_restore;

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use datastreamdb_backup::manifest::MasterManifest;
    use datastreamdb_posting::PostingEngine;
    use datastreamdb_proto::pb::{BackupRequest, ExportRequest, RestoreRequest};
    use datastreamdb_proto::{DirectedEdge, EdgeOp, SchemaUpdate, ValType};

    use super::dest::{MANIFEST_FILE, SUMMARY_FILE};
    use super::{process_backup_request, run_backup_group, run_export, run_restore};
    use crate::groups::GroupState;

    fn test_setup() -> (Arc<PostingEngine>, Arc<GroupState>) {
        let schema = Arc::new(datastreamdb_schema::SchemaState::new());
        schema.set(
            "0-name",
            SchemaUpdate {
                predicate: "0-name".to_owned(),
                value_type: ValType::String as i32,
                ..SchemaUpdate::default()
            },
        );
        let engine = PostingEngine::open(
            Arc::new(datastreamdb_store::MemoryBackend::new()),
            schema,
            64,
        );
        let txn = engine.begin();
        let mut edge = DirectedEdge {
            entity: 2,
            attr: "0-name".to_owned(),
            value: b"Alice".to_vec(),
            value_type: ValType::String as i32,
            op: EdgeOp::Set as i32,
            ..DirectedEdge::default()
        };
        let key = datastreamdb_x::data_key("0-name", 2);
        txn.get(&key)
            .expect("list")
            .add_mutation_with_index(&mut edge, &txn)
            .expect("mutate");
        engine.commit(&txn).expect("commit");
        let groups = Arc::new(GroupState::new(1, 1, "test".to_owned()));
        (engine, groups)
    }

    #[tokio::test]
    async fn backup_writes_manifest_chain() {
        let (engine, groups) = test_setup();
        let peers = Mutex::new(HashMap::new());
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().to_string_lossy().into_owned();
        let req = BackupRequest {
            group_id: 1,
            destination: dest.clone(),
            ..BackupRequest::default()
        };
        process_backup_request(&engine, &groups, &peers, &req, None)
            .await
            .expect("backup");
        let manifest_text =
            std::fs::read_to_string(dir.path().join(MANIFEST_FILE)).expect("manifest");
        let master = MasterManifest::decode(&manifest_text).expect("decode");
        assert_eq!(master.manifests.len(), 1);
        assert_eq!(master.manifests[0].base.backup_type, "full");
        assert_eq!(master.manifests[0].base.backup_num, 1);
        assert_eq!(master.manifests[0].base.compression, "snappy");
        assert!(dir.path().join(SUMMARY_FILE).exists());

        // Second backup chains incrementally.
        let txn = engine.begin();
        let mut edge = DirectedEdge {
            entity: 3,
            attr: "0-name".to_owned(),
            value: b"Bob".to_vec(),
            value_type: ValType::String as i32,
            op: EdgeOp::Set as i32,
            ..DirectedEdge::default()
        };
        let key = datastreamdb_x::data_key("0-name", 3);
        txn.get(&key)
            .expect("list")
            .add_mutation_with_index(&mut edge, &txn)
            .expect("mutate");
        engine.commit(&txn).expect("commit");
        // Distinct directory per backup (Go keys dirs by timestamp).
        std::thread::sleep(std::time::Duration::from_millis(5));
        process_backup_request(&engine, &groups, &peers, &req, None)
            .await
            .expect("backup2");
        let manifest_text =
            std::fs::read_to_string(dir.path().join(MANIFEST_FILE)).expect("manifest2");
        let master = MasterManifest::decode(&manifest_text).expect("decode2");
        assert_eq!(master.manifests.len(), 2);
        assert_eq!(master.manifests[1].base.backup_type, "incremental");
        assert_eq!(master.manifests[1].base.backup_num, 2);
        assert_eq!(
            master.manifests[1].base.backup_id,
            master.manifests[0].base.backup_id
        );
    }

    #[tokio::test]
    async fn coordinator_fans_out_to_known_groups() {
        use datastreamdb_proto::pb::{Group, Member, MembershipState};
        let (engine, groups) = test_setup();
        // Membership knows groups 1 (local) and 2 (unreachable).
        let mut membership = MembershipState::default();
        membership.groups.insert(
            1,
            Group {
                members: [(
                    1u64,
                    Member {
                        id: 1,
                        ..Member::default()
                    },
                )]
                .into_iter()
                .collect(),
                ..Group::default()
            },
        );
        membership.groups.insert(
            2,
            Group {
                members: [(
                    2u64,
                    Member {
                        id: 2,
                        addr: "127.0.0.1:1".to_owned(),
                        leader: true,
                        ..Member::default()
                    },
                )]
                .into_iter()
                .collect(),
                ..Group::default()
            },
        );
        groups.apply_state(&membership);
        assert_eq!(groups.known_groups(), vec![1, 2]);

        let peers = Mutex::new(HashMap::new());
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().to_string_lossy().into_owned();
        let req = BackupRequest {
            destination: dest.clone(),
            ..BackupRequest::default()
        };
        // Group 1's leg completes, then the unreachable group 2 fails.
        let err = process_backup_request(&engine, &groups, &peers, &req, None)
            .await
            .expect_err("group 2 unreachable");
        assert!(err.message().contains('2'), "wrong error: {err}");
        // The local leg still wrote its data file.
        let mut found_group1 = false;
        for entry in std::fs::read_dir(dir.path()).expect("readdir") {
            let entry = entry.expect("entry");
            if !entry.path().is_dir() {
                continue;
            }
            for file in std::fs::read_dir(entry.path()).expect("files") {
                let name = file
                    .expect("file")
                    .file_name()
                    .to_string_lossy()
                    .into_owned();
                found_group1 |= name.ends_with("-g1.backup");
            }
        }
        assert!(found_group1, "group 1 data file missing");
    }

    #[test]
    fn backup_rejects_mismatch_and_bad_remote() {
        let (engine, groups) = test_setup();
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().to_string_lossy().into_owned();
        let req = BackupRequest {
            group_id: 7,
            destination: dest.clone(),
            ..BackupRequest::default()
        };
        let err = run_backup_group(&engine, &groups, &req, None).expect_err("mismatch");
        assert_eq!(err.code(), tonic::Code::FailedPrecondition);
        // Secure S3 needs a TLS build (offline default is plain HTTP).
        let req = BackupRequest {
            group_id: 1,
            destination: "s3://bucket/backups".to_owned(),
            ..BackupRequest::default()
        };
        let err = run_backup_group(&engine, &groups, &req, None).expect_err("tls");
        assert_eq!(err.code(), tonic::Code::FailedPrecondition);
        // Unknown schemes never resolve.
        let req = BackupRequest {
            group_id: 1,
            destination: "ftp://host/backups".to_owned(),
            ..BackupRequest::default()
        };
        assert!(run_backup_group(&engine, &groups, &req, None).is_err());
    }

    #[test]
    fn export_writes_three_files() {
        let (engine, groups) = test_setup();
        let dir = tempfile::tempdir().expect("tempdir");
        for format in ["rdf", "json"] {
            let req = ExportRequest {
                group_id: 1,
                format: format.to_owned(),
                destination: dir.path().to_string_lossy().into_owned(),
                ..ExportRequest::default()
            };
            let response = run_export(&engine, &groups, &req, None).expect("export");
            assert_eq!(response.code, 0);
            assert_eq!(response.files.len(), 3);
            for file in &response.files {
                assert!(dir.path().join(file).exists(), "missing {file}");
            }
        }
        let req = ExportRequest {
            group_id: 1,
            format: "xml".to_owned(),
            destination: dir.path().to_string_lossy().into_owned(),
            ..ExportRequest::default()
        };
        let err = run_export(&engine, &groups, &req, None).expect_err("format");
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn encrypted_backup_restores_with_key() {
        let (engine, groups) = test_setup();
        let peers = Mutex::new(HashMap::new());
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().to_string_lossy().into_owned();
        let key = datastreamdb_enc::keys::DataKey::new(vec![9u8; 32]).expect("key");
        let req = BackupRequest {
            group_id: 1,
            destination: dest.clone(),
            ..BackupRequest::default()
        };
        process_backup_request(&engine, &groups, &peers, &req, Some(&key))
            .await
            .expect("backup");
        let manifest_text =
            std::fs::read_to_string(dir.path().join(MANIFEST_FILE)).expect("manifest");
        let master = MasterManifest::decode(&manifest_text).expect("decode");
        assert!(master.manifests[0].base.encrypted);

        // Keyless restore fails; keyed restore replays the data.
        let bare_schema = Arc::new(datastreamdb_schema::SchemaState::new());
        let bare = PostingEngine::open(
            Arc::new(datastreamdb_store::MemoryBackend::new()),
            bare_schema,
            64,
        );
        let restore_req = RestoreRequest {
            group_id: 1,
            location: dest.clone(),
            ..RestoreRequest::default()
        };
        assert!(run_restore(&bare, &groups, &restore_req, None).is_err());
        let fresh_schema = Arc::new(datastreamdb_schema::SchemaState::new());
        let fresh = PostingEngine::open(
            Arc::new(datastreamdb_store::MemoryBackend::new()),
            fresh_schema.clone(),
            64,
        );
        run_restore(&fresh, &groups, &restore_req, Some(&key)).expect("restore");
        assert_eq!(fresh_schema.predicates(), vec!["0-name".to_owned()]);

        // A plaintext follow-up to an encrypted chain is rejected like Go
        // (advance past the backup timestamp so the chain check runs).
        let txn = engine.begin();
        let mut edge = DirectedEdge {
            entity: 3,
            attr: "0-name".to_owned(),
            value: b"Bob".to_vec(),
            value_type: ValType::String as i32,
            op: EdgeOp::Set as i32,
            ..DirectedEdge::default()
        };
        let list_key = datastreamdb_x::data_key("0-name", 3);
        txn.get(&list_key)
            .expect("list")
            .add_mutation_with_index(&mut edge, &txn)
            .expect("mutate");
        engine.commit(&txn).expect("commit");
        let err = process_backup_request(&engine, &groups, &peers, &req, None)
            .await
            .expect_err("mixed chain");
        assert_eq!(err.code(), tonic::Code::FailedPrecondition);
        assert!(
            err.message().contains("encryption turned off"),
            "wrong error: {err}"
        );
    }

    #[test]
    fn encrypted_export_roundtrips() {
        use std::io::Read as _;
        let (engine, groups) = test_setup();
        let dir = tempfile::tempdir().expect("tempdir");
        let key = datastreamdb_enc::keys::DataKey::new(vec![5u8; 32]).expect("key");
        let req = ExportRequest {
            group_id: 1,
            format: "rdf".to_owned(),
            destination: dir.path().to_string_lossy().into_owned(),
            ..ExportRequest::default()
        };
        let response = run_export(&engine, &groups, &req, Some(&key)).expect("export");
        assert_eq!(response.files.len(), 3);
        let data_file = dir.path().join(&response.files[0]);
        // Ciphertext must not leak plaintext.
        let mut peek = vec![0u8; 256];
        let mut probe = std::fs::File::open(&data_file).expect("open");
        let read = probe.read(&mut peek).expect("peek");
        peek.truncate(read);
        assert!(!peek.windows(5).any(|window| window == b"Alice"));
        let raw = std::fs::File::open(&data_file).expect("reopen");
        let decrypted =
            datastreamdb_enc::stream::decrypting_reader(Some(&key), raw).expect("decrypt");
        let mut gunzip = flate2::read::GzDecoder::new(decrypted);
        let mut text = String::new();
        gunzip.read_to_string(&mut text).expect("gunzip");
        assert!(text.contains("Alice"), "missing Alice: {text}");
    }

    #[tokio::test]
    async fn restore_replays_chain() {
        let (engine, groups) = test_setup();
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().to_string_lossy().into_owned();
        let req = BackupRequest {
            group_id: 1,
            destination: dest.clone(),
            ..BackupRequest::default()
        };
        let peers = Mutex::new(HashMap::new());
        process_backup_request(&engine, &groups, &peers, &req, None)
            .await
            .expect("backup");

        let fresh_schema = Arc::new(datastreamdb_schema::SchemaState::new());
        let fresh = PostingEngine::open(
            Arc::new(datastreamdb_store::MemoryBackend::new()),
            fresh_schema.clone(),
            64,
        );
        let restore_req = RestoreRequest {
            group_id: 1,
            location: dest,
            ..RestoreRequest::default()
        };
        let status = run_restore(&fresh, &groups, &restore_req, None).expect("restore");
        assert_eq!(status.code, 0);
        assert_eq!(fresh_schema.predicates(), vec!["0-name".to_owned()]);
        let read_ts = fresh.oracle().max_assigned();
        let mut out = Vec::new();
        datastreamdb_backup::export::export_rdf(&fresh, 0, read_ts, &mut out).expect("export");
        let text = String::from_utf8(out).expect("utf8");
        assert!(text.contains("Alice"), "missing Alice: {text}");
    }
}
