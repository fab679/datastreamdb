//! Request execution. Port of `Request.ProcessQuery` / `Process` from
//! `query/query.go`: block scheduling, variable threading, and metrics.
//!
//! Delta from Go: blocks run sequentially in dependency order (Go runs
//! ready blocks concurrently); schema queries read the local schema.

#![warn(missing_docs)]

use std::collections::{HashMap, HashSet};
use std::hash::BuildHasher;
use std::time::Instant;

use crate::errors::{QueryError, Result};
use crate::process::{ExecCtx, process_graph};
use crate::subgraph::{Latency, SubGraph, VarValue};
use crate::vars::{is_empty_ineq_fn_with_var, page_range};

/// Execution result of one request.
#[derive(Debug, Default)]
pub struct ExecutionResult {
    /// Executed subgraphs (shortest routes appended).
    pub subgraphs: Vec<SubGraph>,
    /// Schema nodes (schema queries).
    pub schema: Vec<datastreamdb_proto::SchemaUpdate>,
    /// Types (schema queries).
    pub types: Vec<datastreamdb_proto::TypeUpdate>,
    /// Predicate UID metrics.
    pub metrics: HashMap<String, u64>,
}

/// A parsed query request plus execution state.
pub struct QueryRequest<'a> {
    /// Execution context.
    pub ctx: ExecCtx<'a>,
    /// Parsed blocks.
    pub blocks: Vec<datastreamdb_dql::GraphQuery>,
    /// Schema query, if any.
    pub schema_query: Option<datastreamdb_proto::SchemaRequest>,
    /// Read timestamp.
    pub read_ts: u64,
    /// Latency breakdown.
    pub latency: Latency,
    /// Executed subgraphs.
    pub subgraphs: Vec<SubGraph>,
    /// Completed variables.
    pub vars: HashMap<String, VarValue>,
}

impl<'a> QueryRequest<'a> {
    /// Request over parsed blocks.
    #[must_use]
    pub fn new(ctx: ExecCtx<'a>, blocks: Vec<datastreamdb_dql::GraphQuery>, read_ts: u64) -> Self {
        Self {
            ctx,
            blocks,
            schema_query: None,
            read_ts,
            latency: Latency::default(),
            subgraphs: Vec::new(),
            vars: HashMap::new(),
        }
    }

    /// Executes every block in dependency order.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] on malformed queries or execution failure.
    #[allow(clippy::too_many_lines, reason = "exact port of Go ProcessGraph")]
    pub fn process_query(&mut self) -> Result<()> {
        let loop_start = Instant::now();
        for block in &self.blocks {
            if block.uid.is_empty()
                && block.func.is_none()
                && block.needs_var.is_empty()
                && block.alias != "shortest"
                && !block.is_empty
            {
                return Err(QueryError::Invalid(
                    "Invalid query. No function used at root and no aggregation or math \
                     variables found in the body."
                        .to_owned(),
                ));
            }
            let mut root = crate::subgraph::to_sub_graph(block, self.ctx.debug).map_err(|err| {
                QueryError::Invalid(format!("while converting to subgraph: {err}"))
            })?;
            root.read_ts = self.read_ts;
            set_read_ts(&mut root, self.read_ts);
            self.subgraphs.push(root);
        }
        self.latency.parsing_ns +=
            u64::try_from(loop_start.elapsed().as_nanos()).unwrap_or(u64::MAX);

        let exec_start = Instant::now();
        let block_vars: Vec<(HashSet<String>, HashSet<String>)> =
            self.subgraphs.iter().map(block_variables).collect();
        let mut executed = vec![false; self.subgraphs.len()];
        let mut done = 0;
        let mut shortest: Vec<SubGraph> = Vec::new();

        for _ in 0..self.subgraphs.len() {
            if done >= self.subgraphs.len() {
                break;
            }
            let mut ready = Vec::new();
            for (idx, sg) in self.subgraphs.iter().enumerate() {
                if executed[idx] || !can_execute(&block_vars[idx], &self.vars) {
                    continue;
                }
                ready.push(idx);
                let _ = sg;
            }
            for idx in ready.clone() {
                self.subgraphs[idx].recursive_fill_vars(&self.vars.clone())?;
                executed[idx] = true;
                done += 1;
                let sg = &mut self.subgraphs[idx];
                if sg.params.is_empty || is_empty_ineq_fn_with_var(sg) {
                    continue;
                }
                if sg
                    .src_func
                    .as_ref()
                    .is_some_and(|func| func.name == "similar_to")
                    && sg
                        .src_func
                        .as_ref()
                        .is_some_and(|func| func.args.len() == 1)
                    && !sg.params.needs_var.is_empty()
                {
                    continue;
                }
                if sg.params.alias == "shortest" {
                    let mut routes = sg.shortest_path(&self.ctx)?;
                    shortest.append(&mut routes);
                } else if sg.params.recurse {
                    crate::recurse::run_recurse(&self.ctx, sg)?;
                } else {
                    process_graph(&self.ctx, sg)?;
                }
            }
            for idx in ready {
                let mut path = Vec::new();
                self.subgraphs[idx].populate_var_map(&mut self.vars, &mut path)?;
                let sg = &mut self.subgraphs[idx];
                if !sg.params.cascade.fields.is_empty()
                    && (sg.params.cascade.first != 0 || sg.params.cascade.offset != 0)
                {
                    sg.update_uid_matrix();
                    for row in &mut sg.uid_matrix {
                        let (start, end) = page_range(
                            sg.params.cascade.first,
                            sg.params.cascade.offset,
                            row.uids.len(),
                        );
                        row.uids = row.uids[start..end].to_vec();
                    }
                }
                self.subgraphs[idx].populate_post_aggregation(
                    &mut self.vars,
                    &mut Vec::new(),
                    None,
                )?;
            }
        }

        if executed.iter().any(|done| !done) {
            return Err(QueryError::Invalid("Query couldn't be executed".to_owned()));
        }
        self.latency.processing_ns +=
            u64::try_from(exec_start.elapsed().as_nanos()).unwrap_or(u64::MAX);
        self.subgraphs.append(&mut shortest);
        Ok(())
    }

    /// Executes the request and computes metrics.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] on execution failure.
    pub fn process(&mut self) -> Result<ExecutionResult> {
        self.process_query()?;
        let mut metrics = HashMap::new();
        for sg in &self.subgraphs {
            calculate_metrics(sg, &mut metrics);
        }
        let schema_start = Instant::now();
        let mut schema = Vec::new();
        let mut types = Vec::new();
        if let Some(request) = &self.schema_query {
            for pred in &request.predicates {
                let namespaced = datastreamdb_x::namespace_attr(self.ctx.namespace, pred);
                if let Some(update) = self.ctx.engine.schema().get(&namespaced, false) {
                    schema.push(update);
                }
            }
            for name in &request.types {
                let namespaced = datastreamdb_x::namespace_attr(self.ctx.namespace, name);
                if let Some(update) = self.ctx.engine.schema().get_type(&namespaced) {
                    types.push(update);
                }
            }
            if self.ctx.namespace != datastreamdb_x::ROOT_NAMESPACE {
                schema = filter_schema_for_namespace(self.ctx.namespace, schema);
                types = filter_types_for_namespace(self.ctx.namespace, types);
            }
        }
        self.latency.processing_ns +=
            u64::try_from(schema_start.elapsed().as_nanos()).unwrap_or(u64::MAX);
        Ok(ExecutionResult {
            subgraphs: std::mem::take(&mut self.subgraphs),
            schema,
            types,
            metrics,
        })
    }
}

/// Sets the read timestamp across a subtree.
fn set_read_ts(sg: &mut SubGraph, read_ts: u64) {
    sg.read_ts = read_ts;
    for child in &mut sg.children {
        set_read_ts(child, read_ts);
    }
    for filter in &mut sg.filters {
        set_read_ts(filter, read_ts);
    }
}

/// Defined and needed variables of one block.
fn block_variables(sg: &SubGraph) -> (HashSet<String>, HashSet<String>) {
    let mut defines = HashSet::new();
    let mut needs = HashSet::new();
    collect_block_vars(sg, &mut defines, &mut needs);
    (defines, needs)
}

/// Walks a block collecting variable definitions and uses.
fn collect_block_vars(sg: &SubGraph, defines: &mut HashSet<String>, needs: &mut HashSet<String>) {
    if !sg.params.var.is_empty() {
        defines.insert(sg.params.var.clone());
    }
    for name in sg.params.facet_var.values() {
        defines.insert(name.clone());
    }
    for need in &sg.params.needs_var {
        needs.insert(need.name.clone());
    }
    if let Some(math) = &sg.math_exp {
        collect_math_vars(math, needs);
    }
    for func in [
        &sg.params.shortest_path_args.from,
        &sg.params.shortest_path_args.to,
    ]
    .into_iter()
    .flatten()
    {
        for need in &func.needs_var {
            needs.insert(need.name.clone());
        }
    }
    for child in sg.children.iter().chain(sg.filters.iter()) {
        collect_block_vars(child, defines, needs);
    }
}

/// Collects math leaf variables.
fn collect_math_vars(node: &crate::subgraph::MathNode, needs: &mut HashSet<String>) {
    if !node.var.is_empty() {
        needs.insert(node.var.clone());
    }
    for child in &node.child {
        collect_math_vars(child, needs);
    }
}

/// Whether a block's dependencies are satisfied.
fn can_execute(
    block_vars: &(HashSet<String>, HashSet<String>),
    vars: &HashMap<String, VarValue>,
) -> bool {
    let (defines, needs) = block_vars;
    needs
        .iter()
        .all(|need| defines.contains(need) || vars.contains_key(need))
}

/// Tallies processed UIDs per predicate.
fn calculate_metrics(sg: &SubGraph, metrics: &mut HashMap<String, u64>) {
    if !sg.is_internal() {
        *metrics.entry(sg.attr.clone()).or_insert(0) +=
            sg.src_uids.as_ref().map_or(0, |src| src.uids.len() as u64);
    }
    for filter in &sg.filters {
        calculate_metrics(filter, metrics);
    }
    for child in &sg.children {
        calculate_metrics(child, metrics);
    }
}

/// Keeps schema nodes of one namespace (stripping the prefix).
fn filter_schema_for_namespace(
    namespace: u64,
    nodes: Vec<datastreamdb_proto::SchemaUpdate>,
) -> Vec<datastreamdb_proto::SchemaUpdate> {
    nodes
        .into_iter()
        .filter_map(|mut node| {
            let (ns, attr) = datastreamdb_x::parse_namespace_attr(&node.predicate);
            if ns != namespace {
                return None;
            }
            node.predicate = attr.to_owned();
            Some(node)
        })
        .collect()
}

/// Keeps types of one namespace (stripping prefixes).
fn filter_types_for_namespace(
    namespace: u64,
    types: Vec<datastreamdb_proto::TypeUpdate>,
) -> Vec<datastreamdb_proto::TypeUpdate> {
    types
        .into_iter()
        .filter_map(|mut update| {
            let (ns, name) = datastreamdb_x::parse_namespace_attr(&update.type_name);
            if ns != namespace {
                return None;
            }
            update.type_name = name.to_owned();
            for field in &mut update.fields {
                let (_, attr) = datastreamdb_x::parse_namespace_attr(&field.predicate);
                field.predicate = attr.to_owned();
            }
            Some(update)
        })
        .collect()
}

/// Converts assigned blank-node UIDs to hex strings.
#[must_use]
pub fn uids_to_hex<H: BuildHasher>(map: &HashMap<String, u64, H>) -> HashMap<String, String> {
    map.iter()
        .map(|(key, uid)| (key.clone(), format!("{uid:#x}")))
        .collect()
}

/// Strips blank-node prefixes from an assignment map.
#[must_use]
pub fn strip_blank_node<H: BuildHasher>(map: &HashMap<String, u64, H>) -> HashMap<String, u64> {
    map.iter()
        .filter_map(|(key, uid)| key.strip_prefix("_:").map(|key| (key.to_owned(), *uid)))
        .collect()
}
