//! Backup manifests: on-disk JSON contract (Go `worker/backup*.go`).
//!
//! [`Manifest`] records one backup; [`MasterManifest`] chains them.
//! Field names and file layouts match Go byte-for-byte so either side
//! can list and verify the other's chains.

use std::collections::HashMap;

/// Backup file name shape: `r<read_ts>-g<group>.backup` (Go `backupNameFmt`).
#[must_use]
pub fn backup_name(read_ts: u64, group_id: u32) -> String {
    format!("r{read_ts}-g{group_id}.backup")
}

/// Manifest chain failures (messages mirror Go's).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManifestError {
    /// Chain must start at backup number 1.
    #[error("expected a BackupNum value of 1 for first manifest but got {0}")]
    BadFirstNum(u64),
    /// Mixed backup series in one chain.
    #[error("found a manifest with backup ID {0} but expected {1}")]
    MixedId(String, String),
    /// Non-contiguous backup numbers.
    #[error("found a manifest with backup number {0} but expected {1}")]
    SkippedNum(u64, u64),
    /// Fewer manifests than requested.
    #[error("not enough backups to restore manifest with backupNum {0}")]
    TooFew(u64),
    /// Malformed manifest JSON.
    #[error("malformed manifest: {0}")]
    Decode(String),
}

/// Shared per-backup fields (Go `ManifestBase`, flattened in JSON).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ManifestBase {
    /// `"full"` or `"incremental"`.
    #[serde(rename = "type")]
    pub backup_type: String,
    /// Previous backup's read timestamp (legacy `since` key kept).
    #[serde(rename = "since")]
    pub since_ts: u64,
    /// Read timestamp this backup was taken at.
    pub read_ts: u64,
    /// Series id shared from the full backup onward.
    pub backup_id: String,
    /// 1-based position in the series (1 = full backup).
    pub backup_num: u64,
    /// Predicate-encoding version (0 = pre-namespace).
    pub version: i64,
    /// Backup directory holding this backup's data files.
    pub path: String,
    /// Whether data files are encrypted.
    pub encrypted: bool,
    /// Compression codec for data files.
    pub compression: String,
}

impl ManifestBase {
    /// Effective read timestamp (`since` predates `read_ts`, like Go).
    #[must_use]
    pub fn valid_read_ts(&self) -> u64 {
        if self.read_ts == 0 {
            self.since_ts
        } else {
            self.read_ts
        }
    }
}

/// Drop operation recorded since the previous backup.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DropOperation {
    /// Drop kind (0 all, 1 data, 2 attr, 3 namespace).
    ///
    /// Go omits zero values (`omitempty`), so this defaults on decode.
    #[serde(default)]
    pub drop_op: i32,
    /// Predicate for attr drops, empty otherwise.
    #[serde(default)]
    pub drop_value: String,
}

/// Full per-backup record used during restore (Go `Manifest`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Manifest {
    /// Shared fields, flattened like Go's embedding.
    #[serde(flatten)]
    pub base: ManifestBase,
    /// Group id to predicate list at backup time.
    #[serde(default)]
    pub groups: HashMap<u32, Vec<String>>,
    /// Drops since the previous backup.
    #[serde(default)]
    pub drop_operations: Vec<DropOperation>,
}

/// Chain of manifests, oldest first (Go `MasterManifest`).
///
/// Go's struct has no JSON tag, so Go writes the key `Manifests`;
/// decoding accepts both casings for interop.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MasterManifest {
    /// Manifests, oldest first.
    #[serde(default, rename = "Manifests", alias = "manifests")]
    pub manifests: Vec<Manifest>,
}

/// Lightweight listing view without groups and drops (Go `ManifestSummary`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ManifestSummary {
    /// Shared fields, flattened like Go's embedding.
    #[serde(flatten)]
    pub base: ManifestBase,
}

/// Summary chain for `manifest_summary.json` (Go `MasterManifestSummary`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MasterManifestSummary {
    /// Summaries, oldest first.
    #[serde(default, rename = "Manifests", alias = "manifests")]
    pub manifests: Vec<ManifestSummary>,
}

impl MasterManifestSummary {
    /// Builds the summary view from a full manifest chain.
    #[must_use]
    pub fn summarize(manifest: &MasterManifest) -> Self {
        Self {
            manifests: manifest
                .manifests
                .iter()
                .map(|entry| ManifestSummary {
                    base: entry.base.clone(),
                })
                .collect(),
        }
    }

    /// Encodes canonically (Go-compatible field layout).
    #[must_use]
    pub fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

impl MasterManifest {
    /// Decodes a `manifest.json` document.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::Decode`] on malformed JSON.
    pub fn decode(text: &str) -> Result<Self, ManifestError> {
        serde_json::from_str(text).map_err(|err| ManifestError::Decode(err.to_string()))
    }

    /// Encodes canonically (Go-compatible field layout).
    #[must_use]
    pub fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Verifies a restore chain: newest-first, one series, contiguous
/// numbers ending at 1 (Go `verifyManifests`).
///
/// # Errors
///
/// Returns [`ManifestError`] describing the first break found.
pub fn verify_manifests(manifests: &[Manifest]) -> Result<(), ManifestError> {
    let Some(last) = manifests.last() else {
        return Ok(());
    };
    if last.base.backup_num != 1 {
        return Err(ManifestError::BadFirstNum(last.base.backup_num));
    }
    let mut number = manifests.len() as u64;
    for manifest in manifests {
        if manifest.base.backup_id != last.base.backup_id {
            return Err(ManifestError::MixedId(
                manifest.base.backup_id.clone(),
                last.base.backup_id.clone(),
            ));
        }
        if manifest.base.backup_num != number {
            return Err(ManifestError::SkippedNum(manifest.base.backup_num, number));
        }
        number -= 1;
    }
    Ok(())
}

/// Selects the restore chain: newest series back to its full backup,
/// optionally filtered by `backup_id` and truncated to `backup_num`
/// entries (Go `getManifestsToRestore` minus the file-existence pass,
/// which the restore driver performs against its store).
///
/// # Errors
///
/// Returns [`ManifestError`] when the chain breaks or is short.
pub fn select_restore_chain<'a>(
    manifests: &'a [Manifest],
    backup_id: &str,
    backup_num: u64,
) -> Result<Vec<&'a Manifest>, ManifestError> {
    let mut picked = Vec::new();
    for (index, manifest) in manifests.iter().enumerate().rev() {
        if !backup_id.is_empty() && manifest.base.backup_id != backup_id {
            continue;
        }
        picked.push(index);
        if manifest.base.backup_type == "full" {
            break;
        }
    }
    let owned: Vec<Manifest> = picked
        .iter()
        .map(|&index| manifests[index].clone())
        .collect();
    verify_manifests(&owned)?;
    let mut refs: Vec<&Manifest> = picked.iter().map(|&index| &manifests[index]).collect();
    if backup_num > 0 {
        let want = usize::try_from(backup_num).unwrap_or(usize::MAX);
        if refs.len() < want {
            return Err(ManifestError::TooFew(backup_num));
        }
        refs = refs[refs.len() - want..].to_vec();
    }
    Ok(refs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(num: u64, typ: &str) -> Manifest {
        Manifest {
            base: ManifestBase {
                backup_type: typ.to_owned(),
                since_ts: 0,
                read_ts: 100 + num,
                backup_id: "chain-a".to_owned(),
                backup_num: num,
                version: 1,
                path: format!("datastreamdb.20260101.1{num}0000"),
                encrypted: false,
                compression: String::new(),
            },
            groups: HashMap::from([(1u32, vec!["name".to_owned()])]),
            drop_operations: Vec::new(),
        }
    }

    #[test]
    fn manifest_json_matches_go_layout() {
        let manifest = entry(1, "full");
        let text = serde_json::to_string(&manifest).expect("json");
        for key in [
            "\"type\":\"full\"",
            "\"since\":0",
            "\"read_ts\":101",
            "\"backup_id\":\"chain-a\"",
            "\"backup_num\":1",
            "\"version\":1",
            "\"encrypted\":false",
            "\"groups\":{\"1\":[\"name\"]}",
        ] {
            assert!(text.contains(key), "{key} in {text}");
        }
        let back: Manifest = serde_json::from_str(&text).expect("roundtrip");
        assert_eq!(back, manifest);
    }

    #[test]
    fn valid_read_ts_prefers_read_ts() {
        assert_eq!(entry(2, "incremental").base.valid_read_ts(), 102);
        let mut legacy = entry(1, "full");
        legacy.base.read_ts = 0;
        legacy.base.since_ts = 99;
        assert_eq!(legacy.base.valid_read_ts(), 99);
    }

    #[test]
    fn backup_names_match_go() {
        assert_eq!(backup_name(42, 1), "r42-g1.backup");
        assert_eq!(backup_name(100, 3), "r100-g3.backup");
    }

    #[test]
    fn verify_accepts_chains_rejects_breaks() {
        let good = vec![entry(2, "incremental"), entry(1, "full")];
        assert!(verify_manifests(&good).is_ok());
        assert!(verify_manifests(&[]).is_ok());
        let mut bad = good.clone();
        bad[1].base.backup_num = 2;
        assert!(matches!(
            verify_manifests(&bad),
            Err(ManifestError::BadFirstNum(2))
        ));
        let mut bad = good.clone();
        bad[0].base.backup_id = "other".to_owned();
        assert!(matches!(
            verify_manifests(&bad),
            Err(ManifestError::MixedId(_, _))
        ));
        let mut bad = good;
        bad[0].base.backup_num = 5;
        assert!(matches!(
            verify_manifests(&bad),
            Err(ManifestError::SkippedNum(5, 2))
        ));
    }

    #[test]
    fn select_stops_at_full_and_filters() {
        let chain = vec![
            entry(1, "full"),
            entry(2, "incremental"),
            entry(3, "incremental"),
        ];
        let picked = select_restore_chain(&chain, "", 0).expect("chain");
        assert_eq!(picked.len(), 3);
        let picked = select_restore_chain(&chain, "", 2).expect("chain");
        assert_eq!(picked.len(), 2);
        assert!(select_restore_chain(&chain, "", 9).is_err());
        assert!(
            select_restore_chain(&chain, "missing", 0)
                .expect("empty")
                .is_empty()
        );
    }
}
