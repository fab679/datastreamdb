//! Audit flag parsing (Go `GetAuditConf` + `AuditDefaults`).
//!
//! Accepts Go's `--audit` superflag keys (`output`, `compress`, `days`,
//! `size`, `encrypt-file`, `dir`). Compression and encryption parse but
//! stay rejected until the encryption slice lands (silently dropping
//! either would lie about protection).

/// Default rotation age, days (Go `AuditDefaults`).
pub const DEFAULT_DAYS: u64 = 10;
/// Default rotation size, MiB (Go `AuditDefaults`).
pub const DEFAULT_SIZE_MB: u64 = 100;

/// Parsed audit settings (`None` output disables auditing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditSettings {
    /// Destination: file path or `"stdout"` (empty disables).
    pub output: String,
    /// Rotate past this many days.
    pub days: u64,
    /// Rotate past this many MiB.
    pub size_mb: u64,
}

impl Default for AuditSettings {
    fn default() -> Self {
        Self {
            output: String::new(),
            days: DEFAULT_DAYS,
            size_mb: DEFAULT_SIZE_MB,
        }
    }
}

/// Flag parsing failures.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuditConfigError {
    /// Malformed number.
    #[error("bad --audit value for {0}: {1:?}")]
    BadValue(String, String),
    /// Parsed but unsupported until the encryption slice.
    #[error("{0}")]
    Unsupported(String),
}

/// Parses a Go `--audit` superflag (`k=v`, `;`-separated).
///
/// # Errors
///
/// Returns [`AuditConfigError`] on malformed numbers, enabled
/// compression/encryption (deferred), or unknown keys (Go rejects
/// those too).
pub fn parse_audit_flag(flag: &str) -> Result<AuditSettings, AuditConfigError> {
    let mut settings = AuditSettings::default();
    if flag.trim().is_empty() {
        return Ok(settings);
    }
    for part in flag.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let Some((key, value)) = part.split_once('=') else {
            return Err(AuditConfigError::BadValue(part.to_owned(), String::new()));
        };
        let (key, value) = (key.trim(), value.trim());
        match key {
            "output" => value.clone_into(&mut settings.output),
            "days" => {
                settings.days = value
                    .parse()
                    .map_err(|_| AuditConfigError::BadValue(key.to_owned(), value.to_owned()))?;
            }
            "size" => {
                settings.size_mb = value
                    .parse()
                    .map_err(|_| AuditConfigError::BadValue(key.to_owned(), value.to_owned()))?;
            }
            "compress" => {
                let enabled: bool = value
                    .parse()
                    .map_err(|_| AuditConfigError::BadValue(key.to_owned(), value.to_owned()))?;
                if enabled {
                    return Err(AuditConfigError::Unsupported(
                        "audit compression lands with the encryption slice".to_owned(),
                    ));
                }
            }
            "encrypt-file" => {
                if !value.is_empty() {
                    return Err(AuditConfigError::Unsupported(
                        "audit encryption lands with the encryption slice".to_owned(),
                    ));
                }
            }
            "dir" => {}
            _ => {
                return Err(AuditConfigError::BadValue(key.to_owned(), value.to_owned()));
            }
        }
    }
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_flag_disables() {
        let settings = parse_audit_flag("").expect("parse");
        assert!(settings.output.is_empty());
        assert_eq!((settings.days, settings.size_mb), (10, 100));
    }

    #[test]
    fn parses_go_shape() {
        let settings = parse_audit_flag(
            "output=/var/log/audit.log; compress=false; days=7; size=50; dir=; encrypt-file=;",
        )
        .expect("parse");
        assert_eq!(settings.output, "/var/log/audit.log");
        assert_eq!((settings.days, settings.size_mb), (7, 50));
    }

    #[test]
    fn rejects_bad_and_deferred() {
        assert!(parse_audit_flag("days=abc").is_err());
        assert!(parse_audit_flag("bogus=1").is_err());
        assert!(matches!(
            parse_audit_flag("output=x; compress=true"),
            Err(AuditConfigError::Unsupported(_))
        ));
        assert!(matches!(
            parse_audit_flag("output=x; encrypt-file=k"),
            Err(AuditConfigError::Unsupported(_))
        ));
    }
}
