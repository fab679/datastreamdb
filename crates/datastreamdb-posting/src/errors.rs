//! Posting-layer errors. Ports the `posting` package sentinels
//! (`ErrTsTooOld`, `ErrInvalidKey`, `ErrNoValue`, `ErrStopIteration`,
//! `ErrRetry`) plus the failure modes of the embedded version chain.

#![warn(missing_docs)]

/// Posting-layer failure.
#[derive(Debug, thiserror::Error)]
pub enum PostingError {
    /// Read timestamp older than the list's rolled-up base.
    #[error("transaction is too old")]
    TsTooOld,
    /// Tried to read a multi-part list through one of its split keys.
    #[error("cannot read posting list using multi-part list key")]
    InvalidKey,
    /// No value posting in the list.
    #[error("no value found")]
    NoValue,
    /// Iteration terminated early by the callback (control flow, not a failure).
    #[error("stop iteration")]
    StopIteration,
    /// List changed under the caller; retry the operation.
    #[error("temporary error, please retry")]
    Retry,
    /// Transaction conflicts with a committed one.
    #[error("transaction conflict")]
    Conflict,
    /// Corrupt version chain or posting bytes on disk.
    #[error("corrupt posting data: {0}")]
    Corrupt(String),
    /// Storage backend failure.
    #[error(transparent)]
    Store(#[from] datastreamdb_store::StoreError),
    /// Key codec failure.
    #[error(transparent)]
    Key(#[from] datastreamdb_x::KeyError),
    /// Schema failure.
    #[error(transparent)]
    Schema(#[from] datastreamdb_schema::SchemaError),
    /// Type conversion failure.
    #[error(transparent)]
    Types(#[from] datastreamdb_types::ConversionError),
    /// Facet failure.
    #[error(transparent)]
    Facets(#[from] datastreamdb_tok::facets::FacetError),
    /// Tokenizer failure.
    #[error(transparent)]
    Tok(#[from] datastreamdb_tok::tok::TokError),
    /// Deferred feature (message names the milestone).
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// Protobuf decode failure.
    #[error(transparent)]
    Proto(#[from] prost::DecodeError),
}

/// Short alias for posting results.
pub type Result<T> = std::result::Result<T, PostingError>;
