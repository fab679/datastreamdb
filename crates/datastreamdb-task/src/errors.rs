//! Task-processor errors.

#![warn(missing_docs)]

/// Task failure.
#[derive(Debug, thiserror::Error)]
pub enum TaskError {
    /// Posting-layer failure.
    #[error(transparent)]
    Posting(#[from] datastreamdb_posting::PostingError),
    /// Type conversion/comparison failure.
    #[error(transparent)]
    Types(#[from] datastreamdb_types::ConversionError),
    /// Comparison failure.
    #[error(transparent)]
    Compare(#[from] datastreamdb_types::compare::CompareError),
    /// Tokenizer failure.
    #[error(transparent)]
    Tok(#[from] datastreamdb_tok::tok::TokError),
    /// Facet failure.
    #[error(transparent)]
    Facets(#[from] datastreamdb_tok::facets::FacetError),
    /// Password verification failure.
    #[error(transparent)]
    Password(#[from] datastreamdb_types::password::PasswordError),
    /// Schema failure.
    #[error(transparent)]
    Schema(#[from] datastreamdb_schema::SchemaError),
    /// Key codec failure.
    #[error(transparent)]
    Key(#[from] datastreamdb_x::KeyError),
    /// Regex compilation failure.
    #[error(transparent)]
    Regex(#[from] regex::Error),
    /// Malformed task or function arguments.
    #[error("{0}")]
    Invalid(String),
    /// Deferred feature (message names the milestone).
    #[error("unsupported: {0}")]
    Unsupported(String),
}

/// Short alias for task results.
pub type Result<T> = std::result::Result<T, TaskError>;
