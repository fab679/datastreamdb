//! UID-posting tasks. Port of `handleUidPostings` plus the UID
//! retrieve/count helpers from `worker/task.go`.
//!
//! Delta from Go: rows process sequentially (Go fans out over goroutines).

#![warn(missing_docs)]

use datastreamdb_posting::{List, ListOptions, PostingError};
use datastreamdb_proto::{Facets, FacetsList, List as ProtoList, Posting, TaskQuery, TaskResult};

use crate::context::{FuncType, FunctionContext, QueryState, eval_compare};
use crate::errors::{Result, TaskError};
use crate::filter::{FacetsTree, apply_facets_tree, preprocess_filter};
use crate::geo_string::needs_string_filtering;
use crate::parse::lang_for_func;

/// Visits UID postings through the facet filter.
fn facets_filter_uid_posting_list(
    list: &List,
    tree: Option<&FacetsTree>,
    opts: &ListOptions,
    visit: &mut dyn FnMut(&Posting) -> Result<()>,
) -> Result<()> {
    let mut failed: Result<()> = Ok(());
    list.postings(opts, &mut |posting| {
        match apply_facets_tree(&posting.facets, tree) {
            Ok(true) => {}
            Ok(false) => return Ok(()),
            Err(err) => {
                failed = Err(err);
                return Err(PostingError::StopIteration);
            }
        }
        if let Err(err) = visit(posting) {
            failed = Err(err);
            return Err(PostingError::StopIteration);
        }
        Ok(())
    })?;
    failed
}

/// Counts UID postings through the facet filter.
fn count_for_uid_postings(
    list: &List,
    tree: Option<&FacetsTree>,
    opts: &ListOptions,
) -> Result<usize> {
    let mut count = 0usize;
    facets_filter_uid_posting_list(list, tree, opts, &mut |_| {
        count += 1;
        Ok(())
    })?;
    Ok(count)
}

/// UIDs plus facets through the facet filter.
fn retrieve_uids_and_facets(
    query: &TaskQuery,
    list: &List,
    tree: Option<&FacetsTree>,
    opts: &ListOptions,
) -> Result<(ProtoList, Vec<Facets>)> {
    let mut uids = ProtoList {
        uids: Vec::with_capacity(list.approx_len()),
    };
    let mut fcs = Vec::new();
    facets_filter_uid_posting_list(list, tree, opts, &mut |posting| {
        uids.uids.push(posting.uid);
        if query.facet_param.is_some() {
            fcs.push(Facets {
                facets: datastreamdb_posting::filter_facets(
                    &posting.facets,
                    query.facet_param.as_ref(),
                ),
            });
        }
        Ok(())
    })?;
    Ok((uids, fcs))
}

/// Whether UID warming pays off for this task.
fn should_precalculate_uids(
    query: &TaskQuery,
    src_fn: &FunctionContext,
    tree: Option<&FacetsTree>,
    opts: &ListOptions,
) -> bool {
    if query.do_count || query.facet_param.is_some() || tree.is_some() {
        return false;
    }
    if opts.intersect.is_some() || (opts.first > 0 && opts.first < i64::from(i32::MAX)) {
        return false;
    }
    !matches!(
        src_fn.fn_type,
        FuncType::CompareScalar | FuncType::Has | FuncType::UidIn
    )
}

impl QueryState {
    /// Fetches UID postings row by row (data, reverse, or index keys).
    #[allow(clippy::too_many_lines, reason = "exact port of Go handleUidPostings")]
    pub(crate) fn handle_uid_postings(
        &self,
        query: &TaskQuery,
        src_fn: &FunctionContext,
        opts: &ListOptions,
        out: &mut TaskResult,
    ) -> Result<()> {
        let tree = preprocess_filter(query.facets_filter.as_ref())?;
        if src_fn.n == 0 {
            return Ok(());
        }
        if matches!(
            src_fn.fn_type,
            FuncType::NotAFunction | FuncType::CompareScalar | FuncType::Has | FuncType::UidIn
        ) {
            let len = query.uid_list.as_ref().map_or(0, |list| list.uids.len());
            if src_fn.n != len {
                return Err(TaskError::Invalid(format!(
                    "src_fn.n {} != uid len {len} in handle_uid_postings",
                    src_fn.n
                )));
            }
        }

        let schema = self.engine.schema().clone();
        let lang = lang_for_func(&query.langs).to_owned();
        let need_filtering = needs_string_filtering(&schema, src_fn, &query.langs, &query.attr);
        let is_list = schema.is_list(&query.attr);
        let precalculate = should_precalculate_uids(query, src_fn, tree.as_ref(), opts);

        for i in 0..src_fn.n {
            let key = match src_fn.fn_type {
                FuncType::NotAFunction
                | FuncType::CompareScalar
                | FuncType::Has
                | FuncType::UidIn => {
                    let uid = query.uid_list.as_ref().map_or(0, |list| list.uids[i]);
                    if query.reverse {
                        datastreamdb_x::reverse_key(&query.attr, uid)
                    } else {
                        datastreamdb_x::data_key(&query.attr, uid)
                    }
                }
                FuncType::Geo
                | FuncType::Regex
                | FuncType::FullTextSearch
                | FuncType::Standard
                | FuncType::CustomIndex
                | FuncType::Match
                | FuncType::Ngram
                | FuncType::CompareAttr => {
                    datastreamdb_x::index_key(&query.attr, &src_fn.tokens[i])
                }
                _ => {
                    return Err(TaskError::Invalid(format!(
                        "unhandled function in handle_uid_postings: {}",
                        src_fn.fname
                    )));
                }
            };

            let list = if precalculate {
                self.cache.get_uids(&key)?
            } else {
                self.cache.get(&key)?
            };

            if src_fn.fn_type == FuncType::CompareAttr {
                self.engine.memory().stats().insert_record(
                    &query.attr,
                    &src_fn.tokens[i],
                    list.approx_len() as u64,
                );
            }

            if query.do_count {
                let count = count_for_uid_postings(&list, tree.as_ref(), opts)?;
                out.counts.push(u32::try_from(count).unwrap_or(u32::MAX));
                out.uid_matrix.push(ProtoList { uids: Vec::new() });
            } else if src_fn.fn_type == FuncType::CompareScalar {
                let len = list.length(query.read_ts, 0);
                if len < 0 {
                    return Err(TaskError::Posting(PostingError::TsTooOld));
                }
                if eval_compare(&src_fn.fname, len, src_fn.threshold[0]) {
                    let uid = query.uid_list.as_ref().map_or(0, |list| list.uids[i]);
                    out.uid_matrix.push(ProtoList { uids: vec![uid] });
                }
            } else if src_fn.fn_type == FuncType::Has {
                let mut empty = false;
                if need_filtering {
                    let result: std::result::Result<(), PostingError> = if lang.is_empty() {
                        if is_list {
                            list.all_values(query.read_ts).map(|_| ())
                        } else {
                            list.value(query.read_ts).map(|_| ())
                        }
                    } else {
                        list.value_for_tag(query.read_ts, &lang).map(|_| ())
                    };
                    match result {
                        Ok(()) => {}
                        Err(PostingError::NoValue) => empty = true,
                        Err(err) => return Err(err.into()),
                    }
                } else {
                    empty = list.is_empty(query.read_ts, 0)?;
                }
                if !empty {
                    let uid = query.uid_list.as_ref().map_or(0, |list| list.uids[i]);
                    out.uid_matrix.push(ProtoList { uids: vec![uid] });
                }
            } else if src_fn.fn_type == FuncType::UidIn {
                let req = ListOptions {
                    read_ts: query.read_ts,
                    after_uid: 0,
                    intersect: Some(src_fn.uids_present.clone()),
                    first: crate::process::uid_read_first(query),
                };
                let found = list.uids(&req)?;
                if !found.is_empty() {
                    let uid = query.uid_list.as_ref().map_or(0, |list| list.uids[i]);
                    out.uid_matrix.push(ProtoList { uids: vec![uid] });
                }
            } else if query.facet_param.is_some() || tree.is_some() {
                let (uid_list, fcs_list) =
                    retrieve_uids_and_facets(query, &list, tree.as_ref(), opts)?;
                out.uid_matrix.push(uid_list);
                if query.facet_param.is_some() {
                    out.facet_matrix.push(FacetsList {
                        facets_list: fcs_list,
                    });
                }
            } else {
                out.uid_matrix.push(ProtoList {
                    uids: list.uids(opts)?,
                });
            }
        }
        Ok(())
    }
}
