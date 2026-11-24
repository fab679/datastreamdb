//! Password hashing with bcrypt. Port of `types/password.go`.
//!
//! Cost is fixed at 10 (`bcrypt::DEFAULT_COST` is 12; Go uses 10), so hashes
//! stay byte-compatible with Go's (`$2a$10$...`).

#![warn(missing_docs)]

/// Minimum accepted plain-text length.
pub const MIN_PASSWORD_LEN: usize = 6;
/// Bcrypt cost matching Go's `bcrypt.DefaultCost`.
pub const BCRYPT_COST: u32 = 10;

/// Password error. Messages match Go's; hashes never appear in them.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PasswordError {
    /// Plain text shorter than [`MIN_PASSWORD_LEN`].
    #[error("Password too short, i.e. should have at least 6 chars")]
    TooShort,
    /// Empty hash or short plain text at verify time.
    #[error("Invalid password/crypted string")]
    Invalid,
    /// Bcrypt failure (message only).
    #[error("bcrypt failure")]
    Hash,
}

/// Hashes plain text with bcrypt cost 10.
///
/// # Errors
///
/// Returns [`PasswordError`] when the input is too short or hashing fails.
pub fn encrypt(plain: &str) -> Result<String, PasswordError> {
    if plain.len() < MIN_PASSWORD_LEN {
        return Err(PasswordError::TooShort);
    }
    bcrypt::hash(plain, BCRYPT_COST).map_err(|_| PasswordError::Hash)
}

/// Verifies plain text against a bcrypt hash.
///
/// # Errors
///
/// Returns [`PasswordError`] on invalid inputs, mismatch, or bcrypt failure.
pub fn verify_password(plain: &str, encrypted: &str) -> Result<(), PasswordError> {
    if plain.len() < MIN_PASSWORD_LEN || encrypted.is_empty() {
        return Err(PasswordError::Invalid);
    }
    match bcrypt::verify(plain, encrypted) {
        Ok(true) => Ok(()),
        Ok(false) => Err(PasswordError::Invalid),
        Err(_) => Err(PasswordError::Hash),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_vectors() {
        // Port of TestEncrypt (lengths pin the $2a$10$ format).
        assert!(encrypt("").is_err());
        assert!(encrypt("12345").is_err());
        assert_eq!(encrypt("123456").expect("hash").len(), 60);
        assert_eq!(encrypt("1234567890").expect("hash").len(), 60);
    }

    #[test]
    fn verify_vectors() {
        // Port of TestVerifyPassword, including Go-generated hashes.
        assert!(verify_password("", "").is_err());
        assert!(
            verify_password(
                "",
                "$2a$10$LMGgvb5dOq4/YrWXjAy6W.tBfFQC4QDNFAuOCWGRk3f/Z1TMXswaC"
            )
            .is_err()
        );
        assert!(verify_password("1234567890", "").is_err());
        assert!(
            verify_password(
                "12345",
                "$2a$10$LMGgvb5dOq4/YrWXjAy6W.tBfFQC4QDNFAuOCWGRk3f/Z1TMXswaC"
            )
            .is_err()
        );
        assert!(
            verify_password(
                "12345678",
                "$2a$10$LMGgvb5dOq4/YrWXjAy6W.tBfFQC4QDNFAuOCWGRk3f/Z1TMXswaC"
            )
            .is_err()
        );
        verify_password(
            "123456",
            "$2a$10$LMGgvb5dOq4/YrWXjAy6W.tBfFQC4QDNFAuOCWGRk3f/Z1TMXswaC",
        )
        .expect("case 6");
        verify_password(
            "123456",
            "$2a$10$kXtSFCVmzsu0lwVqaWxo5OXlLGUakcY2t18QcqcVpvoTHsPqclOca",
        )
        .expect("case 7");
        verify_password(
            "1234567890",
            "$2a$10$WdCWNpOP6c4l7ECv3hEWKeD3oSiszlRJFmT4uRT1P/W9V9zUye8pS",
        )
        .expect("case 8");
    }
}
