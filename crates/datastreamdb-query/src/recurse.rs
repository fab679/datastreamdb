//! Recurse queries. Port of `query/recurse.go`: depth-bounded
//! expansion with edge dedup and an edge budget.
//!
//! Delta from Go: levels expand sequentially (Go fans out over goroutines).

#![warn(missing_docs)]

use std::collections::HashSet;

use crate::errors::{QueryError, Result};
use crate::subgraph::SubGraph;
use crate::vars::copy_filters_recurse;

/// Maximum edges traversed by one recurse query.
pub const LIMIT_QUERY_EDGE: u64 = 1_000_000;

/// Maximum depth when none is given.
pub const MAX_RECURSE_DEPTH: u64 = u64::MAX;

/// Runs recurse expansion to the depth bound.
///
/// # Errors
///
/// Returns [`QueryError`] on recurse failure.
pub fn run_recurse(ctx: &crate::process::ExecCtx<'_>, sg: &mut SubGraph) -> Result<()> {
    sg.recurse_query(ctx)
}

impl SubGraph {
    /// Runs recurse expansion to the depth bound.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] on recurse failure.
    pub fn recurse_query(&mut self, ctx: &crate::process::ExecCtx<'_>) -> Result<()> {
        if !self.params.recurse {
            return Err(QueryError::Invalid("Invalid recurse path query".to_owned()));
        }
        let mut depth = self.params.recurse_args.depth;
        if depth == 0 {
            if self.params.recurse_args.allow_loop {
                return Err(QueryError::Invalid(
                    "Depth must be > 0 when loop is true for recurse query".to_owned(),
                ));
            }
            depth = MAX_RECURSE_DEPTH;
        }
        for child in &self.children {
            if !child.children.is_empty() {
                return Err(QueryError::Invalid(
                    "recurse queries require that all predicates are specified in one level"
                        .to_owned(),
                ));
            }
        }
        self.expand_recurse(ctx, depth)
    }

    /// Expands the tree level by level.
    fn expand_recurse(&mut self, ctx: &crate::process::ExecCtx<'_>, max_depth: u64) -> Result<()> {
        let mut seen_edges: HashSet<String> = HashSet::new();
        let allow_loop = self.params.recurse_args.allow_loop;
        let mut num_edges = 0u64;

        let templates: Vec<SubGraph> = self
            .children
            .iter()
            .map(|child| child.as_ref().clone())
            .collect();
        self.children.clear();
        crate::process::process_graph(ctx, self)?;
        if self.unknown_attr {
            return Ok(());
        }

        // exec holds live nodes; raw pointers keep them addressable while the
        // tree grows (sequential use only).
        let mut exec: Vec<*mut SubGraph> = expand_children(ctx, self, &templates)?
            .into_iter()
            .map(std::ptr::from_mut::<SubGraph>)
            .collect();

        let mut depth = 0u64;
        loop {
            if depth >= max_depth {
                return Ok(());
            }
            depth += 1;

            if depth == max_depth {
                for node in &exec {
                    // SAFETY: nodes are live tree members (see above).
                    unsafe { &mut **node }.params.facet = None;
                }
            }

            for node in &exec {
                let node = unsafe { &mut **node };
                crate::process::process_graph(ctx, node)?;
            }

            for node in &exec {
                let node = unsafe { &mut **node };
                if node.uid_matrix.is_empty() || node.unknown_attr {
                    continue;
                }
                if !node.filters.is_empty() {
                    node.update_uid_matrix();
                }
                let src_uids = node.src_uids.clone().unwrap_or_default().uids;
                for (midx, from_uid) in src_uids.iter().enumerate() {
                    if allow_loop {
                        for row in &node.uid_matrix {
                            num_edges += row.uids.len() as u64;
                        }
                    } else {
                        let attr = node.attr.clone();
                        if let Some(row) = node.uid_matrix.get_mut(midx) {
                            let mut kept = Vec::new();
                            for uid in row.uids.drain(..) {
                                let key = format!("{attr}|{from_uid}|{uid}");
                                if seen_edges.insert(key) {
                                    num_edges += 1;
                                    kept.push(uid);
                                }
                            }
                            row.uids = kept;
                        }
                    }
                }
                if !node.params.order.is_empty() || !node.params.facets_order.is_empty() {
                    node.update_dest_uids();
                } else {
                    node.dest_uids =
                        Some(datastreamdb_algo::uidlist::merge_sorted(&node.uid_matrix));
                }
            }

            let mut out: Vec<*mut SubGraph> = Vec::new();
            for node in &exec {
                let node = unsafe { &mut **node };
                if node.unknown_attr {
                    continue;
                }
                if node
                    .dest_uids
                    .as_ref()
                    .is_none_or(|dest| dest.uids.is_empty())
                {
                    continue;
                }
                for child in expand_children(ctx, node, &templates)? {
                    out.push(std::ptr::from_mut::<SubGraph>(child));
                }
            }

            if num_edges > LIMIT_QUERY_EDGE {
                return Err(QueryError::Invalid(format!(
                    "Exceeded query edge limit = {LIMIT_QUERY_EDGE}. Found {num_edges} edges."
                )));
            }
            if out.is_empty() {
                return Ok(());
            }
            exec = out;
        }
    }
}

/// Attaches expanded children, returning the live nodes.
fn expand_children<'a>(
    ctx: &crate::process::ExecCtx<'_>,
    sg: &'a mut SubGraph,
    templates: &[SubGraph],
) -> Result<Vec<&'a mut SubGraph>> {
    if !sg.children.is_empty() {
        return Err(QueryError::Invalid(
            "Subgraph should not have any children".to_owned(),
        ));
    }
    for template in templates {
        sg.children.push(Box::new(template.clone()));
    }
    let expanded = crate::process::expand_subgraph(ctx, sg)?;
    sg.children.clear();
    for child in expanded {
        let mut fresh = SubGraph::default();
        copy_filters_recurse(&mut fresh, &child);
        fresh.src_uids.clone_from(&sg.dest_uids);
        fresh.params.var.clone_from(&child.params.var);
        sg.children.push(Box::new(fresh));
    }
    Ok(sg
        .children
        .iter_mut()
        .map(std::convert::AsMut::as_mut)
        .collect())
}
