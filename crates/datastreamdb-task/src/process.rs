//! Task execution entry point. Port of `processTask` /
//! `helpProcessTask` from `worker/task.go` minus distribution: no tablet
//! routing, checksums, or network calls — every task runs against the local
//! engine.

#![warn(missing_docs)]

use std::sync::Arc;

use datastreamdb_posting::{ListOptions, LocalCache, PostingEngine};
use datastreamdb_proto::{TaskQuery, TaskResult};
use datastreamdb_types::TypeId;

use crate::context::{FuncType, QueryState, needs_index};
use crate::errors::{Result, TaskError};
use crate::geo_string::needs_string_filtering;
use crate::parse::parse_src_fn;

/// Read bound for UID fetches (positive bounded reads stop early).
pub(crate) fn uid_read_first(query: &TaskQuery) -> i64 {
    if query.first <= 0 || query.first == i32::MAX {
        return i64::from(query.first);
    }
    i64::from(query.first) + i64::from(query.offset.max(0))
}

/// Executes one task query against the local engine.
///
/// `cache` carries transactional reads (pass a throwaway cache for plain
/// read timestamps).
///
/// # Errors
///
/// Returns [`TaskError`] on storage, schema, or function failure.
pub fn process_task(
    engine: &Arc<PostingEngine>,
    cache: &Arc<LocalCache>,
    mut query: TaskQuery,
) -> Result<TaskResult> {
    engine.oracle().wait_for_ts(query.read_ts);
    let state = QueryState::new(cache.clone(), engine.clone());

    let mut src_fn = parse_src_fn(engine, &mut query)?;
    let schema = engine.schema().clone();
    if query.reverse && !schema.is_reversed(&query.attr, false) {
        return Err(TaskError::Invalid(format!(
            "predicate {} doesn't have reverse edge",
            datastreamdb_x::parse_attr(&query.attr)
        )));
    }
    if needs_index(src_fn.fn_type, query.uid_list.is_some())
        && !schema.is_indexed(&query.attr, false)
    {
        return Err(TaskError::Invalid(format!(
            "predicate {} is not indexed",
            datastreamdb_x::parse_attr(&query.attr)
        )));
    }
    if !query.langs.is_empty() && !schema.has_lang(&query.attr) {
        return Err(TaskError::Invalid(format!(
            "language tags can only be used with predicates of string type having @lang \
             directive in schema. Got: [{}]",
            datastreamdb_x::parse_attr(&query.attr)
        )));
    }
    if query.langs.len() == 1 && query.langs[0] == "*" {
        query.langs.clear();
    }

    let typ = schema.type_of(&query.attr).unwrap_or(TypeId::Default);
    let mut out = TaskResult {
        list: schema.is_list(&query.attr) || query.reverse,
        ..TaskResult::default()
    };
    src_fn.atype = typ;

    let mut opts = ListOptions {
        read_ts: query.read_ts,
        after_uid: query.after_uid,
        first: uid_read_first(&query),
        ..ListOptions::default()
    };
    if src_fn.fn_type != FuncType::NotAFunction {
        if let Some(uids) = query.uid_list.as_ref() {
            if !uids.uids.is_empty() {
                opts.intersect = Some(uids.uids.clone());
            }
        }
    }

    if src_fn.needs_value_postings(typ)? {
        state.handle_value_postings(&query, &src_fn, &mut out)?;
    } else {
        state.handle_uid_postings(&query, &src_fn, &opts, &mut out)?;
    }

    if src_fn.fn_type == FuncType::Has && src_fn.is_func_at_root {
        state.handle_has_function(&query, &mut out, &src_fn)?;
    }
    if src_fn.fn_type == FuncType::CompareScalar && src_fn.is_func_at_root {
        state.handle_compare_scalar_function(&query, &src_fn, &mut out)?;
    }
    if src_fn.fn_type == FuncType::Regex {
        state.handle_regex_function(&query, &src_fn, &mut out)?;
    }
    if src_fn.fn_type == FuncType::Match {
        state.handle_match_function(&query, &src_fn, &mut out)?;
    }
    if src_fn.fn_type == FuncType::CompareAttr && !src_fn.tokens.is_empty() {
        state.handle_compare_function(&query, &src_fn, &mut out)?;
    }
    if src_fn.geo_query.is_some() {
        state.filter_geo_function(&query, &src_fn, &mut out)?;
    }
    if src_fn.fn_type != FuncType::Has
        && needs_string_filtering(&schema, &src_fn, &query.langs, &query.attr)
    {
        state.filter_string_function(&query, &src_fn, &mut out)?;
    }

    out.intersect_dest = src_fn.intersect_dest;
    Ok(out)
}

/// Re-exported function classifier for the query layer.
pub use crate::context::parse_func_type as classify_func;
