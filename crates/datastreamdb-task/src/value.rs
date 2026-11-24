//! Value-posting tasks. Port of `handleValuePostings` plus the value
//! retrieve/count helpers from `worker/task.go`.
//!
//! Delta from Go: rows process sequentially (Go fans out over goroutines);
//! vector (`similar_to`) searches report [`TaskError::Unsupported`].

#![warn(missing_docs)]

use datastreamdb_posting::{List, PostingError};
use datastreamdb_proto::{
    Facets, FacetsList, LangList, List as ProtoList, Posting, TaskQuery, ValueList,
};
use datastreamdb_types::{TypeId, Val, Value};

use crate::context::{FuncType, FunctionContext, QueryState, convert_to_type};
use crate::errors::{Result, TaskError};
use crate::filter::{FacetsTree, apply_facets_tree, preprocess_filter};

/// Visits value postings selected by language breadth and the facet filter.
fn facets_filter_value_posting_list(
    query: &TaskQuery,
    list: &List,
    tree: Option<&FacetsTree>,
    list_type: bool,
    visit: &mut dyn FnMut(&Posting) -> Result<()>,
) -> Result<()> {
    let pick_multiple = query.expand_all || (list_type && query.langs.is_empty());
    let mut lang_match: Option<Posting> = None;
    if !pick_multiple {
        match list.posting_for(query.read_ts, &query.langs) {
            Ok(posting) => lang_match = Some(posting),
            Err(PostingError::NoValue) => {}
            Err(err) => return Err(err.into()),
        }
    }
    let mut failed: Result<()> = Ok(());
    list.iterate(query.read_ts, 0, &mut |posting| {
        let wanted = if query.expand_all {
            true
        } else if list_type && query.langs.is_empty() {
            posting.lang_tag.is_empty()
        } else {
            lang_match.as_ref() == Some(posting)
        };
        if !wanted {
            return Ok(());
        }
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
        if pick_multiple {
            return Ok(());
        }
        Err(PostingError::StopIteration)
    })?;
    failed
}

/// Counts value postings through the facet filter.
fn count_for_value_postings(
    query: &TaskQuery,
    list: &List,
    tree: Option<&FacetsTree>,
    list_type: bool,
) -> Result<usize> {
    let mut count = 0usize;
    facets_filter_value_posting_list(query, list, tree, list_type, &mut |_| {
        count += 1;
        Ok(())
    })?;
    Ok(count)
}

/// Values plus facets through the facet filter.
fn retrieve_values_and_facets(
    query: &TaskQuery,
    list: &List,
    tree: Option<&FacetsTree>,
    list_type: bool,
) -> Result<(Vec<Val>, FacetsList)> {
    let mut vals = Vec::new();
    let mut fcs = Vec::new();
    facets_filter_value_posting_list(query, list, tree, list_type, &mut |posting| {
        vals.push(Val {
            tid: TypeId::try_from(posting.val_type).unwrap_or(TypeId::Default),
            value: Value::Binary(posting.value.clone()),
        });
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
    Ok((vals, FacetsList { facets_list: fcs }))
}

impl QueryState {
    /// Fetches value postings row by row, comparing inline for filters.
    #[allow(
        clippy::too_many_lines,
        reason = "exact port of Go handleValuePostings"
    )]
    pub(crate) fn handle_value_postings(
        &self,
        query: &TaskQuery,
        src_fn: &FunctionContext,
        out: &mut datastreamdb_proto::TaskResult,
    ) -> Result<()> {
        let tree = preprocess_filter(query.facets_filter.as_ref())?;
        match src_fn.fn_type {
            FuncType::NotAFunction
            | FuncType::Aggregator
            | FuncType::Password
            | FuncType::CompareAttr
            | FuncType::SimilarTo => {}
            _ => {
                return Err(TaskError::Invalid(format!(
                    "unhandled function in handle_value_postings: {}",
                    src_fn.fname
                )));
            }
        }
        if src_fn.fn_type == FuncType::SimilarTo {
            return Err(TaskError::Unsupported(
                "similar_to (vector milestone)".to_owned(),
            ));
        }

        if src_fn.atype == TypeId::Password && src_fn.fn_type != FuncType::Password {
            return Ok(());
        }
        if src_fn.fn_type == FuncType::Password && src_fn.atype != TypeId::Password {
            return Err(TaskError::Invalid(format!(
                "checkpwd fn can only be used on attr: [{}] with schema type password",
                datastreamdb_x::parse_attr(&query.attr)
            )));
        }
        if src_fn.n == 0 {
            return Ok(());
        }
        let uids = query.uid_list.as_ref().map_or(&[][..], |list| &list.uids);
        if src_fn.n != uids.len() {
            return Err(TaskError::Invalid(format!(
                "src_fn.n {} != uid len {} in handle_value_postings",
                src_fn.n,
                uids.len()
            )));
        }

        let schema = self.engine.schema().clone();
        let list_type = schema.is_list(&query.attr);
        let has_lang = schema.has_lang(&query.attr);
        let multiple = query.do_count
            || query.expand_all
            || list_type
            || has_lang
            || query.facet_param.is_some();

        for uid in uids {
            let key = datastreamdb_x::data_key(&query.attr, *uid);
            let vals: Vec<Val>;
            let mut fcs = FacetsList {
                facets_list: Vec::new(),
            };

            if multiple {
                let list = self.cache.get(&key)?;
                if query.do_count {
                    match count_for_value_postings(query, &list, tree.as_ref(), list_type) {
                        Ok(count) => {
                            let count = u32::try_from(count).unwrap_or(u32::MAX);
                            out.counts.push(count);
                            out.uid_matrix.push(ProtoList { uids: Vec::new() });
                            continue;
                        }
                        Err(TaskError::Posting(PostingError::NoValue)) => continue,
                        Err(err) => return Err(err),
                    }
                }
                let (found, found_fcs) =
                    retrieve_values_and_facets(query, &list, tree.as_ref(), list_type)?;
                if found.is_empty() {
                    push_empty_value_row(out);
                    if query.expand_all {
                        out.lang_matrix.push(LangList { lang: Vec::new() });
                    }
                    continue;
                }
                vals = found;
                fcs = found_fcs;
                if query.expand_all {
                    out.lang_matrix.push(LangList {
                        lang: list.lang_tags(query.read_ts)?,
                    });
                }
            } else {
                let single = self.cache.get_single_posting(&key)?;
                let Some(single) = single.filter(|pl| !pl.postings.is_empty()) else {
                    push_empty_value_row(out);
                    continue;
                };
                vals = single
                    .postings
                    .iter()
                    .map(|p| Val {
                        tid: TypeId::try_from(p.val_type).unwrap_or(TypeId::Default),
                        value: Value::Binary(p.value.clone()),
                    })
                    .collect();
            }

            let mut uid_row = ProtoList { uids: Vec::new() };
            let mut value_row = ValueList { values: Vec::new() };
            for mut val in vals {
                let cell = convert_to_type(&val, src_fn.atype)?;
                if src_fn.fn_type == FuncType::CompareAttr {
                    val = datastreamdb_types::convert(&val, src_fn.atype)?;
                    match src_fn.fname.as_str() {
                        "eq" => {
                            for token in &src_fn.eq_tokens {
                                if datastreamdb_types::compare_vals(&src_fn.fname, &val, token)
                                    .unwrap_or(false)
                                {
                                    uid_row.uids.push(*uid);
                                    break;
                                }
                            }
                        }
                        "between" => {
                            if datastreamdb_types::compare_between(
                                &val,
                                &src_fn.eq_tokens[0],
                                &src_fn.eq_tokens[1],
                            ) {
                                uid_row.uids.push(*uid);
                            }
                        }
                        _ => {
                            if datastreamdb_types::compare_vals(
                                &src_fn.fname,
                                &val,
                                &src_fn.eq_tokens[0],
                            )
                            .unwrap_or(false)
                            {
                                uid_row.uids.push(*uid);
                            }
                        }
                    }
                } else {
                    value_row.values.push(cell);
                }
            }
            out.value_matrix.push(value_row);
            out.facet_matrix.push(fcs);

            match src_fn.fn_type {
                FuncType::Aggregator => {
                    out.uid_matrix.push(ProtoList { uids: Vec::new() });
                }
                FuncType::Password => {
                    let Some(last) = out.value_matrix.last_mut() else {
                        continue;
                    };
                    if last.values.is_empty() {
                        continue;
                    }
                    if last.values[0].val.is_empty() {
                        last.values[0] = crate::context::false_val();
                    }
                    let Some(src) = query.src_func.as_ref() else {
                        continue;
                    };
                    let password = String::from_utf8_lossy(&last.values[0].val).into_owned();
                    last.values[0] = match datastreamdb_types::password::verify_password(
                        &src.args[0],
                        &password,
                    ) {
                        Ok(()) => crate::context::true_val(),
                        Err(_) => crate::context::false_val(),
                    };
                    out.uid_matrix.push(ProtoList { uids: Vec::new() });
                }
                _ => out.uid_matrix.push(uid_row),
            }
        }
        Ok(())
    }
}

/// Empty value/facet rows keeping matrix cardinality predictable.
fn push_empty_value_row(out: &mut datastreamdb_proto::TaskResult) {
    out.uid_matrix.push(ProtoList { uids: Vec::new() });
    out.facet_matrix.push(FacetsList {
        facets_list: Vec::new(),
    });
    out.value_matrix.push(ValueList { values: Vec::new() });
}
