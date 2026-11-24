//! Query-executor errors.

#![warn(missing_docs)]

/// Query failure.
#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    /// Task-processor failure.
    #[error(transparent)]
    Task(#[from] datastreamdb_task::TaskError),
    /// Posting-layer failure.
    #[error(transparent)]
    Posting(#[from] datastreamdb_posting::PostingError),
    /// DQL failure.
    #[error(transparent)]
    Dql(#[from] datastreamdb_dql::DqlError),
    /// Type conversion failure.
    #[error(transparent)]
    Types(#[from] datastreamdb_types::ConversionError),
    /// Sort failure.
    #[error(transparent)]
    Sort(#[from] datastreamdb_types::SortError),
    /// Comparison failure.
    #[error(transparent)]
    Compare(#[from] datastreamdb_types::compare::CompareError),
    /// Schema failure.
    #[error(transparent)]
    Schema(#[from] datastreamdb_schema::SchemaError),
    /// Storage failure.
    #[error(transparent)]
    Store(#[from] datastreamdb_store::StoreError),
    /// Key codec failure.
    #[error(transparent)]
    Key(#[from] datastreamdb_x::KeyError),
    /// JSON rendering failure.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// Malformed query or executor invariant.
    #[error("{0}")]
    Invalid(String),
    /// Cascade rejection marker (control flow, like Go's `_INV_`).
    #[error("_INV_")]
    InvalidCascade,
    /// Aggregation over no values (control flow, like Go's `ErrEmptyVal`).
    #[error("empty aggregation")]
    EmptyAggregation,
    /// Deferred feature (message names the milestone).
    #[error("unsupported: {0}")]
    Unsupported(String),
}

/// Short alias for query results.
pub type Result<T> = std::result::Result<T, QueryError>;
