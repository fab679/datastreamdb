//! Query engine: one isolated executor over a posting engine.
//! Parses DQL, runs requests, renders JSON/RDF, and applies mutations.

#![warn(missing_docs)]

use std::collections::HashMap;
use std::sync::Arc;

use datastreamdb_posting::PostingEngine;

use crate::errors::{QueryError, Result};
use crate::mutation::UidAllocator;
use crate::process::ExecCtx;
use crate::request::{ExecutionResult, QueryRequest};
use crate::subgraph::Latency;

/// One isolated query engine.
#[derive(Debug)]
pub struct QueryEngine {
    /// Posting engine.
    posting: Arc<PostingEngine>,
    /// UID allocator.
    allocator: UidAllocator,
}

impl QueryEngine {
    /// Opens an engine over a posting engine.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] when the UID allocator cannot open.
    pub fn open(posting: Arc<PostingEngine>) -> Result<Arc<Self>> {
        let allocator = UidAllocator::open(posting.store().clone())?;
        Ok(Arc::new(Self { posting, allocator }))
    }

    /// Posting engine.
    #[must_use]
    pub fn posting(&self) -> &Arc<PostingEngine> {
        &self.posting
    }

    /// UID allocator (cluster leases install here).
    #[must_use]
    pub fn allocator(&self) -> &UidAllocator {
        &self.allocator
    }

    /// Executes a DQL query string into an execution result.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] on parse or execution failure.
    pub fn query(
        self: &Arc<Self>,
        namespace: u64,
        text: &str,
        read_ts: u64,
    ) -> Result<(ExecutionResult, Latency)> {
        let parsed = datastreamdb_dql::parse(datastreamdb_dql::Request {
            text: text.to_owned(),
            variables: HashMap::new(),
        })?;
        self.query_parsed(namespace, parsed.query, read_ts)
    }

    /// Executes parsed blocks into an execution result.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] on execution failure.
    pub fn query_parsed(
        self: &Arc<Self>,
        namespace: u64,
        blocks: Vec<datastreamdb_dql::GraphQuery>,
        read_ts: u64,
    ) -> Result<(ExecutionResult, Latency)> {
        let cache = Arc::new(datastreamdb_posting::LocalCache::new(
            self.posting.clone(),
            read_ts,
        ));
        let ctx = ExecCtx {
            engine: &self.posting,
            cache,
            namespace,
            debug: false,
        };
        let mut request = QueryRequest::new(ctx, blocks, read_ts);
        let result = request.process()?;
        Ok((result, request.latency))
    }

    /// Executes a DQL query string into JSON bytes.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] on parse, execution, or rendering failure.
    pub fn query_json(
        self: &Arc<Self>,
        namespace: u64,
        text: &str,
        read_ts: u64,
    ) -> Result<Vec<u8>> {
        let (mut result, _) = self.query(namespace, text, read_ts)?;
        crate::output::to_json(&mut result.subgraphs)
    }

    /// Applies a DQL mutation string, returning assigned blank-node UIDs.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] on parse or commit failure.
    pub fn mutate(self: &Arc<Self>, namespace: u64, text: &str) -> Result<HashMap<String, u64>> {
        let parsed = datastreamdb_dql::parse_mutation(text)?;
        let mut assigned = HashMap::new();
        for mutation in &parsed.mutations {
            let (set, _) = datastreamdb_chunker::parse_rdfs(&mutation.set_nquads)
                .map_err(|err| QueryError::Invalid(format!("bad set block: {err}")))?;
            let (del, _) = datastreamdb_chunker::parse_rdfs(&mutation.del_nquads)
                .map_err(|err| QueryError::Invalid(format!("bad del block: {err}")))?;
            assigned.extend(self.mutate_parsed(namespace, &set, &del)?);
        }
        Ok(assigned)
    }

    /// Applies parsed quads, returning assigned blank-node UIDs.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] on commit failure.
    pub fn mutate_parsed(
        self: &Arc<Self>,
        namespace: u64,
        set: &[datastreamdb_chunker::NQuad],
        del: &[datastreamdb_chunker::NQuad],
    ) -> Result<HashMap<String, u64>> {
        use datastreamdb_proto::EdgeOp;
        let mut assigned = HashMap::new();
        if !set.is_empty() {
            let edges = crate::mutation::to_directed_edges_shared(
                &self.posting,
                &self.allocator,
                namespace,
                set,
                EdgeOp::Set,
                &mut assigned,
            )?;
            crate::mutation::apply_mutations(&self.posting, &edges)?;
        }
        if !del.is_empty() {
            let edges = crate::mutation::to_directed_edges_shared(
                &self.posting,
                &self.allocator,
                namespace,
                del,
                EdgeOp::Del,
                &mut assigned,
            )?;
            crate::mutation::apply_mutations(&self.posting, &edges)?;
        }
        Ok(assigned)
    }
}
