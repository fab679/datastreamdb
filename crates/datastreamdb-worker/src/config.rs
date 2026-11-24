//! Server options (`worker/config.go`).
//!
//! [`Options`] mirrors Go's worker `Options`; ACL/audit/CDC fields arrive
//! with M6, and are absent (not stubbed) until then.

use std::path::PathBuf;

/// How mutation requests are handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MutationsMode {
    /// Allow all mutations.
    #[default]
    Allow,
    /// Reject all mutations.
    Disallow,
    /// Allow only mutations touching known (schema) predicates.
    Strict,
}

/// Options for one alpha worker.
#[derive(Debug, Clone)]
pub struct Options {
    /// Posting store directory.
    pub posting_dir: PathBuf,
    /// WAL directory (must differ from [`Options::posting_dir`]).
    pub wal_dir: PathBuf,
    /// Mutation handling mode.
    pub mutations_mode: MutationsMode,
    /// Token required by alter HTTP requests (empty disables the check).
    pub auth_token: String,
    /// Cache size in MiB shared by posting caches.
    pub cache_mb: i64,
    /// Drop mutated entries from cache instead of refreshing them.
    pub remove_on_update: bool,
    /// Type-filter UID threshold (direct vs index search cutoff).
    pub type_filter_uid_limit: u64,
    /// At-rest encryption key for backups/exports (`None` = plaintext,
    /// like Go's nil `WorkerConfig.EncryptionKey`).
    pub encryption_key: Option<datastreamdb_enc::keys::DataKey>,
    /// CDC superflag (`file=...`; empty disables CDC, like Go's
    /// empty `ChangeDataConf`).
    pub cdc_conf: String,
}

/// Option validation failures.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum OptionsError {
    /// Two directories resolve to the same path.
    #[error("{0} and {1} directories cannot be the same ({2})")]
    SameDir(&'static str, &'static str, String),
    /// A directory path cannot be absolutized.
    #[error("bad {0} directory {1:?}: {2}")]
    BadDir(&'static str, String, String),
}

impl Options {
    /// Validates directory layout (mirrors Go's `validate`).
    ///
    /// # Errors
    ///
    /// Returns [`OptionsError`] when directories clash or do not resolve.
    pub fn validate(&self) -> Result<(), OptionsError> {
        let posting = absolutize("posting", &self.posting_dir)?;
        let wal = absolutize("wal", &self.wal_dir)?;
        if posting == wal {
            return Err(OptionsError::SameDir(
                "Posting",
                "WAL",
                self.posting_dir.display().to_string(),
            ));
        }
        Ok(())
    }

    /// Redacted summary for logs (mirrors Go's `String`).
    #[must_use]
    pub fn redacted(&self) -> String {
        format!(
            "{{PostingDir:{} WALDir:{} MutationsMode:{:?} AuthToken:**** CacheMb:{} RemoveOnUpdate:{} TypeFilterUidLimit:{}}}",
            self.posting_dir.display(),
            self.wal_dir.display(),
            self.mutations_mode,
            self.cache_mb,
            self.remove_on_update,
            self.type_filter_uid_limit,
        )
    }
}

/// Resolves `dir` against the current directory for comparison.
fn absolutize(kind: &'static str, dir: &PathBuf) -> Result<PathBuf, OptionsError> {
    if dir.is_absolute() {
        return Ok(dir.clone());
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(dir))
        .map_err(|err| OptionsError::BadDir(kind, dir.display().to_string(), err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> Options {
        Options {
            posting_dir: "/tmp/datastreamdb-posting".into(),
            wal_dir: "/tmp/datastreamdb-wal".into(),
            mutations_mode: MutationsMode::default(),
            auth_token: String::new(),
            cache_mb: 1024,
            remove_on_update: false,
            type_filter_uid_limit: 1000,
            encryption_key: None,
            cdc_conf: String::new(),
        }
    }

    #[test]
    fn distinct_dirs_validate() {
        options().validate().expect("valid");
    }

    #[test]
    fn same_dirs_rejected() {
        let mut opts = options();
        opts.wal_dir = opts.posting_dir.clone();
        assert!(matches!(
            opts.validate(),
            Err(OptionsError::SameDir("Posting", "WAL", _))
        ));
    }

    #[test]
    fn redacted_hides_token() {
        let mut opts = options();
        opts.auth_token = "s3cret".to_owned();
        let summary = opts.redacted();
        assert!(summary.contains("AuthToken:****"));
        assert!(!summary.contains("s3cret"));
    }
}
