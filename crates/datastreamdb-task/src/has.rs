//! `has()` at the root. Port of `handleHasFunction` /
//! `handleHasWithOrderFunction` from `worker/task.go`: full-predicate scans
//! with first/offset and the ordered-index shortcut.

#![warn(missing_docs)]

use datastreamdb_posting::{ListOptions, PostingError};
use datastreamdb_proto::{List as ProtoList, TaskQuery, TaskResult, ValType};

use crate::context::{FunctionContext, QueryState};
use crate::errors::{Result, TaskError};
use crate::geo_string::needs_string_filtering;
use crate::parse::lang_for_func;

impl QueryState {
    /// Collects every UID carrying the predicate.
    pub(crate) fn handle_has_function(
        &self,
        query: &TaskQuery,
        out: &mut TaskResult,
        src_fn: &FunctionContext,
    ) -> Result<()> {
        let schema = self.engine.schema().clone();
        let has_orders = query
            .order
            .as_ref()
            .is_some_and(|order| !order.order.is_empty());
        if schema.is_indexed(&query.attr, false) && has_orders {
            if let Some(pred) = schema.get(&query.attr, false) {
                let sortable = matches!(
                    pred.value_type,
                    v if v == ValType::Int as i32
                        || v == ValType::Float as i32
                        || v == ValType::Datetime as i32
                );
                if sortable {
                    let order = query
                        .order
                        .as_ref()
                        .and_then(|order| order.order.iter().find(|item| item.attr == query.attr));
                    if let Some(order) = order {
                        let sort = query.order.as_ref().expect("order");
                        return self.handle_has_with_order_function(
                            query,
                            out,
                            order.desc,
                            i64::from(sort.offset),
                            i64::from(sort.count),
                            src_fn,
                        );
                    }
                }
            }
        }

        let parsed = datastreamdb_x::parse(&datastreamdb_x::data_key(&query.attr, 1))
            .map_err(datastreamdb_posting::PostingError::from)?;
        let (start_key, prefix) = if query.reverse {
            (
                datastreamdb_x::reverse_key(&query.attr, query.after_uid.saturating_add(1)),
                parsed.reverse_prefix(),
            )
        } else {
            (
                datastreamdb_x::data_key(&query.attr, query.after_uid.saturating_add(1)),
                parsed.data_prefix(),
            )
        };

        let lang = lang_for_func(&query.langs).to_owned();
        let need_filtering = needs_string_filtering(&schema, src_fn, &query.langs, &query.attr);
        let mut result = Vec::new();
        let mut skipped = 0i64;
        let mut failed: Result<()> = Ok(());
        let mut args = datastreamdb_posting::mem::IterateDiskArgs {
            prefix,
            read_ts: query.read_ts,
            reverse: false,
            start_key,
            check_inclusion: None,
            function: &mut |_, pk| {
                if need_filtering {
                    match self.get_vals_for_uid(&query.attr, &lang, pk.uid, query.read_ts) {
                        Ok(_) => {}
                        Err(TaskError::Posting(PostingError::NoValue)) => return Ok(()),
                        Err(err) => {
                            failed = Err(err);
                            return Err(PostingError::StopIteration);
                        }
                    }
                }
                if skipped < i64::from(query.offset) {
                    skipped += 1;
                    return Ok(());
                }
                result.push(pk.uid);
                if result.len() >= usize::try_from(query.first).unwrap_or(usize::MAX) {
                    return Err(PostingError::StopIteration);
                }
                Ok(())
            },
        };
        self.engine
            .memory()
            .iterate_disk(self.engine.store(), &mut args)?;
        failed?;
        out.uid_matrix.push(ProtoList { uids: result });
        Ok(())
    }

    /// Collects `has()` UIDs in index order.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn handle_has_with_order_function(
        &self,
        query: &TaskQuery,
        out: &mut TaskResult,
        reverse: bool,
        offset: i64,
        mut first: i64,
        src_fn: &FunctionContext,
    ) -> Result<()> {
        if first == 0 {
            first = i64::MAX;
        }
        let parsed = datastreamdb_x::parse(&datastreamdb_x::data_key(&query.attr, 1))
            .map_err(datastreamdb_posting::PostingError::from)?;
        let start_key = if reverse {
            datastreamdb_x::index_key_after_all_terms(&query.attr)
        } else {
            datastreamdb_x::index_key(&query.attr, &[])
        };
        let lang = lang_for_func(&query.langs).to_owned();
        let schema = self.engine.schema().clone();
        let need_filtering = needs_string_filtering(&schema, src_fn, &query.langs, &query.attr);
        let mut result = Vec::new();
        let mut skipped = 0i64;
        let mut args = datastreamdb_posting::mem::IterateDiskArgs {
            prefix: parsed.index_prefix(),
            read_ts: query.read_ts,
            reverse,
            start_key,
            check_inclusion: None,
            function: &mut |list, _| {
                let opts = ListOptions {
                    read_ts: query.read_ts,
                    ..ListOptions::default()
                };
                let found = list.uids(&opts)?;
                if skipped < i64::from(query.offset) {
                    skipped += 1;
                    return Ok(());
                }
                for uid in found {
                    if need_filtering
                        && self
                            .get_vals_for_uid(&query.attr, &lang, uid, query.read_ts)
                            .is_err()
                    {
                        continue;
                    }
                    result.push(uid);
                }
                if result.len() >= usize::try_from(query.first).unwrap_or(usize::MAX)
                    || i64::try_from(result.len()).unwrap_or(i64::MAX)
                        >= first.saturating_add(offset)
                {
                    return Err(PostingError::StopIteration);
                }
                Ok(())
            },
        };
        self.engine
            .memory()
            .iterate_disk(self.engine.store(), &mut args)?;
        result.sort_unstable();
        out.uid_matrix.push(ProtoList { uids: result });
        Ok(())
    }
}
