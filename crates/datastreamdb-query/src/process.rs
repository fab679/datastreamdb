//! Graph execution driver. Port of `ProcessGraph`, `createTaskQuery`,
//! pagination/ordering, and `expandSubgraph` from `query/query.go`.
//!
//! Delta from Go: children and filters run sequentially (Go fans out over
//! goroutines and channels); tasks execute against the local engine.

#![warn(missing_docs)]

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use datastreamdb_posting::{LocalCache, PostingEngine};
use datastreamdb_proto::{List as ProtoList, Order, SortMessage, SrcFunction, TaskQuery};

use crate::errors::{QueryError, Result};
use crate::subgraph::{SubGraph, is_inequality_fn};
use crate::vars::page_range;

/// Execution context for one request.
#[derive(Clone, Debug)]
pub struct ExecCtx<'a> {
    /// Posting engine.
    pub engine: &'a Arc<PostingEngine>,
    /// Posting cache (transactional or throwaway).
    pub cache: Arc<LocalCache>,
    /// Request namespace.
    pub namespace: u64,
    /// Debug mode (emits UIDs).
    pub debug: bool,
}

/// Executes one subgraph and its descendants.
///
/// # Errors
///
/// Returns [`QueryError`] on execution failure.
pub fn process_graph(ctx: &ExecCtx<'_>, sg: &mut SubGraph) -> Result<()> {
    process_graph_inner(ctx, sg, None)
}

/// Executes one subgraph with an explicit parent.
#[allow(clippy::too_many_lines, reason = "exact port of Go processGraph")]
fn process_graph_inner(
    ctx: &ExecCtx<'_>,
    sg: &mut SubGraph,
    parent: Option<*const SubGraph>,
) -> Result<()> {
    if sg.attr == "uid" {
        sg.append_dummy_values();
        return Ok(());
    }

    if parent.is_none() && sg.src_func.as_ref().is_some_and(|func| func.name == "uid") {
        if sg.src_uids.as_ref().is_some_and(|src| !src.uids.is_empty()) {
            sg.dest_uids.clone_from(&sg.src_uids);
        } else {
            let dest = sg.dest_uids.clone().unwrap_or_default().uids;
            sg.uid_matrix = vec![ProtoList { uids: dest }];
            if let Some(dest) = &mut sg.dest_uids {
                dest.uids.sort_unstable();
            }
        }
        if sg.params.after_uid > 0 {
            if let Some(dest) = &mut sg.dest_uids {
                let at = dest.uids.partition_point(|uid| *uid <= sg.params.after_uid);
                dest.uids = dest.uids[at..].to_vec();
            }
        }
    } else if sg.attr.is_empty() {
        if sg.src_func.as_ref().is_some_and(|func| func.name == "uid") {
            sg.fill_vars(&sg.params.parent_vars.clone())?;
            let dest = sg.dest_uids.clone().unwrap_or_default();
            let src = sg.src_uids.clone().unwrap_or_default();
            let mut merged = Vec::new();
            datastreamdb_algo::uidlist::intersect_with(&dest.uids, &src.uids, &mut merged);
            sg.dest_uids = Some(ProtoList { uids: merged });
            return Ok(());
        }
        if sg.src_uids.is_none() {
            return Err(QueryError::Invalid("SrcUIDs shouldn't be nil.".to_owned()));
        }
        sg.dest_uids.clone_from(&sg.src_uids);
    } else {
        let is_ineq = sg
            .src_func
            .as_ref()
            .is_some_and(|func| is_inequality_fn(&func.name));
        let is_value_var = sg.src_func.as_ref().is_some_and(|func| func.is_value_var);
        let is_len_var = sg.src_func.as_ref().is_some_and(|func| func.is_len_var);
        if is_ineq && is_value_var {
            sg.apply_ineq_func()?;
            if parent.is_some() {
                return Ok(());
            }
        } else if is_ineq && is_len_var {
            let arg = sg
                .src_func
                .as_ref()
                .and_then(|func| func.args.first().map(|arg| arg.value.clone()))
                .unwrap_or_default();
            let src = datastreamdb_types::Val {
                tid: datastreamdb_types::TypeId::String,
                value: datastreamdb_types::Value::Binary(arg.as_bytes().to_vec()),
            };
            let dst = datastreamdb_types::convert(&src, datastreamdb_types::TypeId::Int).map_err(
                |err| {
                    QueryError::Invalid(format!(
                        "invalid argument {arg}. Comparing with different type: {err}"
                    ))
                },
            )?;
            let len = sg.dest_uids.as_ref().map_or(0, |dest| dest.uids.len());
            let current = datastreamdb_types::Val {
                tid: datastreamdb_types::TypeId::Int,
                value: datastreamdb_types::Value::Int(i64::try_from(len).unwrap_or(i64::MAX)),
            };
            let name = sg
                .src_func
                .as_ref()
                .map(|func| func.name.clone())
                .unwrap_or_default();
            if datastreamdb_types::compare_vals(&name, &current, &dst).unwrap_or(false) {
                sg.dest_uids.clone_from(&sg.src_uids);
            } else {
                sg.dest_uids = Some(ProtoList::default());
            }
        } else {
            process_task_query(ctx, sg)?;
            if parent.is_none() {
                let dest = sg.dest_uids.clone().unwrap_or_default().uids;
                sg.uid_matrix = vec![ProtoList { uids: dest }];
            }
        }
    }

    if !sg.filters.is_empty() {
        for idx in 0..sg.filters.len() {
            let is_uid_filter = sg.filters[idx].src_func.as_ref().is_some_and(|func| {
                func.name == "uid" && sg.filters[idx].params.needs_var.is_empty()
            });
            if is_uid_filter {
                let src = sg.filters[idx].src_uids.clone();
                sg.filters[idx].dest_uids = src;
                continue;
            }
            sg.filters[idx].src_uids.clone_from(&sg.dest_uids);
            if sg.filters[idx]
                .src_uids
                .as_ref()
                .is_none_or(|src| src.uids.is_empty())
            {
                continue;
            }
            let parent_vars = sg.params.parent_vars.clone();
            sg.filters[idx].params.parent_vars = parent_vars;
            let sg_ptr = std::ptr::from_mut::<SubGraph>(sg);
            // Borrow dance: process the filter while `sg` is reborrowed.
            let filter = &mut sg.filters[idx];
            // SAFETY: single-threaded; `sg` outlives the call.
            process_graph_inner(ctx, filter, Some(sg_ptr.cast_const()))?;
        }

        let mut lists: Vec<ProtoList> = Vec::new();
        for filter in &sg.filters {
            lists.push(filter.dest_uids.clone().unwrap_or_default());
        }
        match sg.filter_op.as_str() {
            "or" => {
                sg.dest_uids = Some(datastreamdb_algo::uidlist::merge_sorted(&lists));
            }
            "not" => {
                if sg.filters.len() != 1 {
                    return Err(QueryError::Invalid(
                        "not filter expects one child".to_owned(),
                    ));
                }
                let dest = sg.dest_uids.clone().unwrap_or_default();
                let other = sg.filters[0].dest_uids.clone().unwrap_or_default();
                sg.dest_uids = Some(datastreamdb_algo::uidlist::difference(&dest, &other));
            }
            "and" => {
                sg.dest_uids = Some(datastreamdb_algo::uidlist::intersect_sorted(&lists));
            }
            _ => {
                lists.push(sg.dest_uids.clone().unwrap_or_default());
                sg.dest_uids = Some(datastreamdb_algo::uidlist::intersect_sorted(&lists));
            }
        }
    }

    if sg.params.order.is_empty() && sg.params.facets_order.is_empty() {
        let skip_pagination =
            sg.filters.is_empty() && sg.src_func.as_ref().is_some_and(|func| func.name == "has");
        if !skip_pagination {
            apply_pagination(sg);
        }
    } else if !sg.params.do_count {
        apply_order_and_pagination(ctx, sg)?;
    }

    if sg.params.do_count {
        if sg.filters.is_empty() {
            return Err(QueryError::Invalid(
                "count with filters expected here".to_owned(),
            ));
        }
        sg.counts = vec![0; sg.uid_matrix.len()];
        sg.update_uid_matrix();
        for (i, row) in sg.uid_matrix.iter().enumerate() {
            sg.counts[i] = u32::try_from(row.uids.len()).unwrap_or(u32::MAX);
        }
        return Ok(());
    }

    sg.children = expand_subgraph(ctx, sg)?
        .into_iter()
        .map(Box::new)
        .collect();

    if sg.is_group_by() {
        for attr in sg.params.groupby_attrs.clone() {
            sg.children.push(Box::new(SubGraph {
                attr: attr.attr.clone(),
                read_ts: sg.read_ts,
                params: crate::subgraph::Params {
                    alias: attr.alias.clone(),
                    ignore_result: true,
                    langs: attr.langs.clone(),
                    ..crate::subgraph::Params::default()
                },
                ..SubGraph::default()
            }));
        }
    }

    if !sg.children.is_empty() {
        let mut parent_vars = sg.params.parent_vars.clone();
        sg.update_vars(&mut parent_vars, &Vec::new())?;
        sg.params.parent_vars = parent_vars;
    }

    for idx in 0..sg.children.len() {
        sg.children[idx].params.parent_vars = sg.params.parent_vars.clone();
        let dest = sg.dest_uids.clone();
        sg.children[idx].src_uids = dest;
        if sg.children[idx].is_internal() {
            continue;
        }
        let sg_ptr = std::ptr::from_mut::<SubGraph>(sg);
        let child = &mut sg.children[idx];
        process_graph_inner(ctx, child, Some(sg_ptr.cast_const()))?;
    }

    if sg
        .dest_uids
        .as_ref()
        .is_none_or(|dest| dest.uids.is_empty())
    {
        sg.children
            .retain(|child| !(child.is_internal() && child.attr == "expand"));
        return Ok(());
    }
    Ok(())
}

/// Runs the task layer for one subgraph and installs the matrices.
fn process_task_query(ctx: &ExecCtx<'_>, sg: &mut SubGraph) -> Result<()> {
    let query = create_task_query(ctx, sg)?;
    // Unknown attributes yield empty rows (no tablet routing embedded), so
    // downstream stages naturally skip them; no `UnknownAttr` sniffing needed.
    let result = datastreamdb_task::process_task(ctx.engine, &ctx.cache, query)?;

    sg.uid_matrix = result.uid_matrix;
    sg.value_matrix = result.value_matrix;
    sg.facets_matrix = result.facet_matrix;
    sg.counts = result.counts;
    sg.lang_tags = result.lang_matrix;
    sg.list = result.list;

    if sg.params.do_count && sg.filters.is_empty() {
        return Ok(());
    }
    if sg.params.do_count {
        sg.counts = vec![0; sg.uid_matrix.len()];
    }

    if result.intersect_dest {
        sg.dest_uids = Some(datastreamdb_algo::uidlist::intersect_sorted(&sg.uid_matrix));
    } else {
        sg.dest_uids = Some(datastreamdb_algo::uidlist::merge_sorted(&sg.uid_matrix));
    }
    Ok(())
}

/// Builds the task request for one subgraph.
fn create_task_query(ctx: &ExecCtx<'_>, sg: &mut SubGraph) -> Result<TaskQuery> {
    let mut attr = sg.attr.clone();
    let reverse = attr.starts_with('~');
    if reverse {
        attr = attr.trim_start_matches('~').to_owned();
    }
    let mut src_func = None;
    if let Some(func) = &sg.src_func {
        let mut task_func = SrcFunction {
            name: func.name.clone(),
            is_count: func.is_count,
            ..SrcFunction::default()
        };
        for arg in &func.args {
            if arg.is_value_var {
                return Err(QueryError::Invalid(
                    "Unsupported use of value var".to_owned(),
                ));
            }
            task_func.args.push(arg.value.clone());
        }
        src_func = Some(task_func);
    }

    if sg.params.langs.len() == 1 && sg.params.langs[0] == "*" {
        sg.params.expand_all = true;
    }

    let (first, offset) = calculate_pagination_params(sg);
    let order: Vec<Order> = sg
        .params
        .order
        .iter()
        .map(|item| Order {
            attr: datastreamdb_x::namespace_attr(ctx.namespace, &item.attr),
            desc: item.desc,
            langs: item.langs.clone(),
        })
        .collect();
    let sort_msg = SortMessage {
        order,
        uid_matrix: sg.uid_matrix.clone(),
        offset: sg.params.offset,
        count: sg.params.count,
        read_ts: sg.read_ts,
    };

    Ok(TaskQuery {
        read_ts: sg.read_ts,
        cache: sg.cache,
        attr: datastreamdb_x::namespace_attr(ctx.namespace, &attr),
        langs: sg.params.langs.clone(),
        reverse,
        src_func,
        after_uid: sg.params.after_uid,
        do_count: sg.filters.is_empty() && sg.params.do_count,
        facet_param: sg.params.facet.clone(),
        facets_filter: sg.facets_filter.clone(),
        expand_all: sg.params.expand_all,
        first,
        offset,
        order: Some(sort_msg),
        uid_list: sg.src_uids.clone(),
    })
}

/// Task-level pagination bounds (skipped past filters/order/fan-out).
fn calculate_pagination_params(sg: &SubGraph) -> (i32, i32) {
    let excluded = sg.src_func.as_ref().is_some_and(|func| {
        matches!(
            func.name.as_str(),
            "regexp" | "alloftext" | "allofterms" | "match" | "ngram"
        )
    });
    if sg.filters.is_empty() && sg.params.order.is_empty() && !excluded && sg.params.count != 0 {
        return (sg.params.count, sg.params.offset);
    }
    (i32::MAX, 0)
}

/// Applies count/offset windows to every UID row.
fn apply_pagination(sg: &mut SubGraph) {
    if sg.params.count == 0 && sg.params.offset == 0 {
        return;
    }
    for row in &mut sg.uid_matrix {
        let (start, end) = page_range(sg.params.count, sg.params.offset, row.uids.len());
        row.uids = row.uids[start..end].to_vec();
    }
    sg.dest_uids = Some(datastreamdb_algo::uidlist::merge_sorted(&sg.uid_matrix));
}

/// Sorts (facet/var/value) then paginates.
fn apply_order_and_pagination(ctx: &ExecCtx<'_>, sg: &mut SubGraph) -> Result<()> {
    if sg.params.order.is_empty() && sg.params.facets_order.is_empty() {
        return Ok(());
    }
    sg.update_uid_matrix();

    if !sg.params.facets_order.is_empty() {
        return sort_and_paginate_using_facet(sg);
    }
    for needs in &sg.params.needs_var {
        if !sg.params.order.is_empty()
            && needs.name == sg.params.order[0].attr
            && needs.typ == datastreamdb_dql::VALUE_VAR
        {
            return sort_and_paginate_using_var(sg);
        }
    }

    if sg.params.count == 0 {
        sg.params.count = 1000;
    }
    if sg.params.order.is_empty() {
        return Err(QueryError::Invalid("missing sort order".to_owned()));
    }
    let order: Vec<Order> = sg
        .params
        .order
        .iter()
        .map(|item| Order {
            attr: datastreamdb_x::namespace_attr(ctx.namespace, &item.attr),
            desc: item.desc,
            langs: item.langs.clone(),
        })
        .collect();
    let message = SortMessage {
        order,
        uid_matrix: sg.uid_matrix.clone(),
        offset: sg.params.offset,
        count: sg.params.count,
        read_ts: sg.read_ts,
    };
    let result = crate::sort::process_sort(ctx.engine, &ctx.cache, &message)?;
    if result.uid_matrix.len() != sg.uid_matrix.len() {
        return Err(QueryError::Invalid("sort row mismatch".to_owned()));
    }
    if !sg.facets_matrix.is_empty() {
        for (idx, row) in result.uid_matrix.iter().enumerate() {
            let mut facets = Vec::with_capacity(sg.facets_matrix[idx].facets_list.len());
            for uid in &row.uids {
                let original = &sg.uid_matrix[idx].uids;
                let pos = original
                    .binary_search(uid)
                    .map_err(|_| QueryError::Invalid("sort facet mismatch".to_owned()))?;
                facets.push(sg.facets_matrix[idx].facets_list[pos].clone());
            }
            sg.facets_matrix[idx].facets_list = facets;
        }
    }
    sg.uid_matrix = result.uid_matrix;
    sg.update_dest_uids();
    Ok(())
}

/// Sorts rows by facet keys, then paginates.
fn sort_and_paginate_using_facet(sg: &mut SubGraph) -> Result<()> {
    if sg.facets_matrix.is_empty() {
        return Ok(());
    }
    if sg.facets_matrix.len() != sg.uid_matrix.len() {
        return Err(QueryError::Invalid(format!(
            "Facet matrix and UID matrix mismatch: {} vs {}",
            sg.facets_matrix.len(),
            sg.uid_matrix.len()
        )));
    }

    let mut key_index: HashMap<String, usize> = HashMap::new();
    let mut desc = Vec::new();
    for (i, order) in sg.params.facets_order.iter().enumerate() {
        key_index.insert(order.key.clone(), i);
        desc.push(order.desc);
    }

    for i in 0..sg.uid_matrix.len() {
        let mut uids = std::mem::take(&mut sg.uid_matrix[i].uids);
        let mut facet_list = std::mem::take(&mut sg.facets_matrix[i].facets_list);
        let mut values: Vec<Vec<datastreamdb_types::Val>> = vec![Vec::new(); uids.len()];
        for (j, facets) in facet_list.iter().enumerate() {
            let mut row = vec![
                datastreamdb_types::Val {
                    tid: datastreamdb_types::TypeId::Default,
                    value: datastreamdb_types::Value::Null,
                };
                sg.params.facets_order.len()
            ];
            let mut remaining = key_index.len();
            for facet in &facets.facets {
                let Some(idx) = key_index.get(&facet.key).copied() else {
                    continue;
                };
                let val = facet_to_val(facet)?;
                if datastreamdb_types::is_sortable(val.tid) {
                    row[idx] = val;
                }
                remaining -= 1;
                if remaining == 0 {
                    break;
                }
            }
            values[j] = row;
        }
        if values.is_empty() {
            continue;
        }
        datastreamdb_types::sort_with_facet(&mut values, &mut uids, &mut facet_list, &desc, "")?;
        sg.uid_matrix[i].uids = uids;
        sg.facets_matrix[i].facets_list = facet_list;
    }

    if sg.params.count != 0 || sg.params.offset != 0 {
        for i in 0..sg.uid_matrix.len() {
            let (start, end) = page_range(
                sg.params.count,
                sg.params.offset,
                sg.uid_matrix[i].uids.len(),
            );
            sg.uid_matrix[i].uids = sg.uid_matrix[i].uids[start..end].to_vec();
            sg.facets_matrix[i].facets_list = sg.facets_matrix[i].facets_list[start..end].to_vec();
        }
    }
    sg.update_dest_uids();
    Ok(())
}

/// Sorts rows by a value variable, then paginates.
fn sort_and_paginate_using_var(sg: &mut SubGraph) -> Result<()> {
    for i in 0..sg.uid_matrix.len() {
        let mut uids = Vec::new();
        let mut values = Vec::new();
        for uid in sg.uid_matrix[i].uids.clone() {
            let Some(val) = sg.params.uid_to_val.get(uid) else {
                continue;
            };
            values.push(vec![val.clone()]);
            uids.push(uid);
        }
        if values.is_empty() {
            continue;
        }
        datastreamdb_types::sort_values(&mut values, &mut uids, &[sg.params.order[0].desc], "")?;
        sg.uid_matrix[i].uids = uids;
    }

    if sg.params.count != 0 || sg.params.offset != 0 {
        for i in 0..sg.uid_matrix.len() {
            let (start, end) = page_range(
                sg.params.count,
                sg.params.offset,
                sg.uid_matrix[i].uids.len(),
            );
            sg.uid_matrix[i].uids = sg.uid_matrix[i].uids[start..end].to_vec();
        }
    }
    sg.update_dest_uids();
    Ok(())
}

/// Expands `expand(...)` children into concrete predicates.
///
/// # Errors
///
/// Returns [`QueryError`] on schema or expansion failure.
pub fn expand_subgraph(ctx: &ExecCtx<'_>, sg: &SubGraph) -> Result<Vec<SubGraph>> {
    let mut out: Vec<SubGraph> = Vec::with_capacity(sg.children.len());
    for child in &sg.children {
        if child.params.expand.is_empty() {
            out.push(child.as_ref().clone());
            continue;
        }

        let type_names = get_node_types(ctx, sg)?;
        let mut preds: Vec<String> = Vec::new();
        if child.params.expand == "_all_" {
            if type_names.is_empty() {
                continue;
            }
            preds = get_predicates_from_types(ctx, &type_names);
            if !sg.params.allowed_preds.is_empty() {
                let allowed: HashSet<&str> =
                    sg.params.allowed_preds.iter().map(String::as_str).collect();
                preds.retain(|pred| allowed.contains(pred.as_str()));
            }
        } else if !child.expand_preds.is_empty() {
            for row in &child.expand_preds {
                for val in &row.values {
                    if !val.val.is_empty() {
                        preds.push(datastreamdb_x::namespace_attr(
                            ctx.namespace,
                            &String::from_utf8_lossy(&val.val),
                        ));
                    }
                }
            }
        } else {
            let names: Vec<String> = child.params.expand.split(',').map(str::to_owned).collect();
            preds = get_predicates_from_types(ctx, &names);
        }
        preds.sort();
        preds.dedup();

        preds = filter_uid_predicates(ctx, &preds);
        for pred in &preds {
            if pred == "datastreamdb.type" {
                continue;
            }
            let mut expanded = SubGraph {
                attr: pred.clone(),
                ..SubGraph::default()
            };
            crate::vars::copy_filters_recurse(&mut expanded, child);
            expanded.attr.clone_from(pred);
            out.push(expanded);
        }
    }
    Ok(out)
}

/// Type names of the subgraph's destination UIDs.
fn get_node_types(ctx: &ExecCtx<'_>, sg: &SubGraph) -> Result<Vec<String>> {
    let temp = SubGraph {
        attr: "datastreamdb.type".to_owned(),
        src_uids: sg.dest_uids.clone(),
        read_ts: sg.read_ts,
        ..SubGraph::default()
    };
    let query = TaskQuery {
        attr: datastreamdb_x::namespace_attr(ctx.namespace, "datastreamdb.type"),
        uid_list: temp.src_uids.clone(),
        read_ts: sg.read_ts,
        ..TaskQuery::default()
    };
    let result = datastreamdb_task::process_task(ctx.engine, &ctx.cache, query)?;
    let mut names = Vec::new();
    for row in &result.value_matrix {
        for val in &row.values {
            if !val.val.is_empty() {
                names.push(String::from_utf8_lossy(&val.val).into_owned());
            }
        }
    }
    Ok(names)
}

/// Predicates of the named types.
fn get_predicates_from_types(ctx: &ExecCtx<'_>, type_names: &[String]) -> Vec<String> {
    let mut preds = Vec::new();
    for name in type_names {
        let namespaced = datastreamdb_x::namespace_attr(ctx.namespace, name);
        if let Some(def) = ctx.engine.schema().get_type(&namespaced) {
            for field in &def.fields {
                preds.push(field.predicate.clone());
            }
        }
    }
    preds
}

/// Keeps UID-typed predicates.
fn filter_uid_predicates(ctx: &ExecCtx<'_>, preds: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for pred in preds {
        let is_uid = ctx
            .engine
            .schema()
            .type_of(pred)
            .is_ok_and(|typ| typ == datastreamdb_types::TypeId::Uid);
        if is_uid {
            out.push(pred.clone());
        }
    }
    out
}

/// Decodes a stored facet into a typed value.
fn facet_to_val(facet: &datastreamdb_proto::api::Facet) -> Result<datastreamdb_types::Val> {
    use datastreamdb_types::{TypeId, Val, Value};
    let tid = match facet.val_type {
        1 => TypeId::Int,
        2 => TypeId::Float,
        3 => TypeId::Bool,
        4 => TypeId::DateTime,
        _ => TypeId::String,
    };
    if tid == TypeId::String {
        return Ok(Val {
            tid,
            value: Value::String(String::from_utf8_lossy(&facet.value).into_owned()),
        });
    }
    Ok(datastreamdb_types::convert(
        &Val {
            tid: TypeId::Binary,
            value: Value::Binary(facet.value.clone()),
        },
        tid,
    )?)
}
