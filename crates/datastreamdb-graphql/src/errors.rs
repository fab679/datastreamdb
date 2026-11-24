//! GraphQL layer errors. Port of the `gqlerror` usage in
//! `dgraph-main/graphql/schema` plus crate-local failure modes.

#![warn(missing_docs)]

use thiserror::Error;

/// A single GraphQL schema/request error with optional source locations.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("{message}")]
pub struct GraphQlError {
    /// Human-readable message (Go-exact where ported).
    pub message: String,
    /// 1-based (line, column) source locations.
    pub locations: Vec<(usize, usize)>,
}

impl GraphQlError {
    /// Plain message without locations.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            locations: Vec::new(),
        }
    }

    /// Message with one source location.
    pub fn at(message: impl Into<String>, line: usize, column: usize) -> Self {
        Self {
            message: message.into(),
            locations: vec![(line, column)],
        }
    }
}

/// Crate result type.
pub type Result<T> = std::result::Result<T, GraphQlError>;

/// A list of schema errors (mirrors `gqlerror.List`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ErrorList(pub Vec<GraphQlError>);

impl ErrorList {
    /// Appends one error.
    pub fn push(&mut self, err: GraphQlError) {
        self.0.push(err);
    }

    /// Appends all errors from another list.
    pub fn extend(&mut self, other: ErrorList) {
        self.0.extend(other.0);
    }

    /// Whether any error was collected.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Number of collected errors.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Joins messages for display (one per line).
    #[must_use]
    pub fn messages(&self) -> Vec<String> {
        self.0.iter().map(|err| err.message.clone()).collect()
    }
}

impl std::fmt::Display for ErrorList {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, err) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str("\n")?;
            }
            write!(f, "{}", err.message)?;
        }
        Ok(())
    }
}

impl std::error::Error for ErrorList {}
