//! Scalar comparisons and lossy-tokenizer verification. Port of
//! `handleCompareScalarFunction` / `evaluate` / `handleCompareFunction`
//! from `worker/task.go`.

#![warn(missing_docs)]

use datastreamdb_posting::ListOptions;
use datastreamdb_proto::{List as ProtoList, TaskQuery, TaskResult};
use datastreamdb_types::{TypeId, Val};

use crate::context::{FN_BETWEEN, FN_EQ, FunctionContext, QueryState};

use crate::errors::{Result, TaskError};
use crate::parse::{lang_for_func, pick_tokenizer};

impl QueryState {
    /// Compares `count()` results against thresholds at the root.
    pub(crate) fn handle_compare_scalar_function(
        &self,
        query: &TaskQuery,
        src_fn: &FunctionContext,
        out: &mut TaskResult,
    ) -> Result<()> {
        if !self.engine.schema().has_count(&query.attr, false) {
            return Err(TaskError::Invalid(format!(
                "need @count directive in schema for attr: {} for fn: {} at root",
                datastreamdb_x::parse_attr(&query.attr),
                src_fn.fname
            )));
        }
        self.evaluate(
            &query.attr,
            &src_fn.fname,
            &src_fn.threshold,
            query.read_ts,
            query.reverse,
            out,
        )
    }

    /// Evaluates a count comparison by scanning count buckets.
    fn evaluate(
        &self,
        attr: &str,
        fname: &str,
        counts: &[i64],
        read_ts: u64,
        reverse: bool,
        out: &mut TaskResult,
    ) -> Result<()> {
        let count_low = counts[0];
        let count_high = if fname == FN_BETWEEN { counts[1] } else { 0 };
        let illegal = match fname {
            "eq" | "le" | "ge" => count_low <= 0,
            "lt" => count_low <= 1,
            "gt" => count_low < 0,
            FN_BETWEEN => count_low <= 0 || count_high <= 0,
            _ => {
                return Err(TaskError::Invalid(format!(
                    "unhandled count comparison fn: {fname}"
                )));
            }
        };
        if illegal {
            return Err(TaskError::Invalid(
                "count(predicate) cannot be used to search for negative counts (nonsensical) \
                 or zero counts (not tracked)"
                    .to_owned(),
            ));
        }

        if fname == FN_EQ {
            let key =
                datastreamdb_x::count_key(attr, u32::try_from(count_low).unwrap_or(0), reverse);
            let list = self.cache.get_uids(&key)?;
            out.uid_matrix.push(ProtoList {
                uids: list.uids(&ListOptions {
                    read_ts,
                    ..ListOptions::default()
                })?,
            });
            return Ok(());
        }

        let mut bound = count_low;
        if fname == "lt" {
            bound -= 1;
        } else if fname == "gt" {
            bound += 1;
        }
        let seek = datastreamdb_x::count_key(attr, u32::try_from(bound).unwrap_or(0), reverse);
        let prefix = parsed(attr).count_prefix(reverse);
        let mut pairs = self
            .engine
            .store()
            .scan_prefix(datastreamdb_posting::store::PARTITION, &prefix)
            .map_err(datastreamdb_posting::PostingError::from)?;
        let forward = !(fname == "le" || fname == "lt");
        if forward {
            pairs.retain(|(key, _)| key.as_slice() >= seek.as_slice());
        } else {
            pairs.retain(|(key, _)| key.as_slice() <= seek.as_slice());
            pairs.reverse();
        }

        for (key, _) in &pairs {
            let Ok(parsed) = datastreamdb_x::parse(key) else {
                continue;
            };
            if parsed.has_start_uid {
                continue;
            }
            if fname == FN_BETWEEN && i64::from(parsed.count) > count_high {
                break;
            }
            let list = self.cache.get_uids(key)?;
            out.uid_matrix.push(ProtoList {
                uids: list.uids(&ListOptions {
                    read_ts,
                    ..ListOptions::default()
                })?,
            });
        }
        Ok(())
    }

    /// Verifies lossy-tokenizer matches against stored values.
    pub(crate) fn handle_compare_function(
        &self,
        query: &TaskQuery,
        src_fn: &FunctionContext,
        out: &mut TaskResult,
    ) -> Result<()> {
        let tokenizer = pick_tokenizer(self.engine.as_ref(), &query.attr, &src_fn.fname)?;
        if !tokenizer.is_lossy() {
            return Ok(());
        }
        let schema = self.engine.schema().clone();
        let typ = schema
            .type_of(&query.attr)
            .map_err(|_| TaskError::Invalid(format!("attribute not scalar: {}", query.attr)))?;
        if !typ.is_scalar() {
            return Err(TaskError::Invalid(format!(
                "attribute not scalar: {}",
                query.attr
            )));
        }
        if out.uid_matrix.is_empty() {
            return Err(TaskError::Invalid(
                "compare filter without uid rows".to_owned(),
            ));
        }
        let is_list = schema.is_list(&query.attr);
        let lang = lang_for_func(&query.langs).to_owned();

        let filter_row = |row: usize, out: &mut TaskResult, compare: &dyn Fn(&Val) -> bool| {
            let mut filter_err: Option<TaskError> = None;
            let mut row_list = std::mem::take(&mut out.uid_matrix[row]);
            datastreamdb_algo::uidlist::apply_filter(&mut row_list, |uid, _| {
                let keep = self.row_value_matches(query, typ, is_list, &lang, uid, compare);
                match keep {
                    Ok(keep) => keep,
                    Err(TaskError::Posting(datastreamdb_posting::PostingError::NoValue)) => false,
                    Err(err) => {
                        filter_err = Some(err);
                        false
                    }
                }
            });
            out.uid_matrix[row] = row_list;
            // Go returns a stale nil error here (`return err` on the outer
            // variable); we propagate the real filter failure.
            filter_err.map_or(Ok(()), Err)
        };

        match src_fn.fname.as_str() {
            FN_EQ => {
                for row in 0..src_fn.tokens.len() {
                    let token = src_fn.eq_tokens[row].clone();
                    filter_row(row, out, &|val| {
                        datastreamdb_types::compare_vals(&src_fn.fname, val, &token)
                            .unwrap_or(false)
                    })?;
                }
            }
            FN_BETWEEN => {
                let (low, high) = (src_fn.eq_tokens[0].clone(), src_fn.eq_tokens[1].clone());
                filter_row(0, out, &|val| {
                    datastreamdb_types::compare_between(val, &low, &high)
                })?;
                let last = out.uid_matrix.len() - 1;
                filter_row(last, out, &|val| {
                    datastreamdb_types::compare_between(val, &low, &high)
                })?;
            }
            _ => {
                if src_fn.tokens[0] == src_fn.ineq_value_token[0] {
                    let token = src_fn.eq_tokens[0].clone();
                    filter_row(0, out, &|val| {
                        datastreamdb_types::compare_vals(&src_fn.fname, val, &token)
                            .unwrap_or(false)
                    })?;
                }
            }
        }
        Ok(())
    }

    /// Tests one UID's stored values against a comparison.
    #[allow(clippy::too_many_arguments)]
    fn row_value_matches(
        &self,
        query: &TaskQuery,
        typ: TypeId,
        is_list: bool,
        lang: &str,
        uid: u64,
        compare: &dyn Fn(&Val) -> bool,
    ) -> Result<bool> {
        let key = datastreamdb_x::data_key(&query.attr, uid);
        let list = self.cache.get(&key)?;
        if lang.is_empty() && is_list {
            for posted in list.all_untagged_values(query.read_ts)? {
                if let Ok(value) = datastreamdb_types::convert(&posted, typ) {
                    if compare(&value) {
                        return Ok(true);
                    }
                }
            }
            return Ok(false);
        }
        if lang.is_empty() {
            let posted = list.value(query.read_ts)?;
            let value = datastreamdb_types::convert(&posted, typ)?;
            return Ok(compare(&value));
        }
        if lang == "." {
            for posted in list.all_values(query.read_ts)? {
                if let Ok(value) = datastreamdb_types::convert(&posted, typ) {
                    if compare(&value) {
                        return Ok(true);
                    }
                }
            }
            return Ok(false);
        }
        let posted = list.value_for(query.read_ts, &query.langs)?;
        let value = datastreamdb_types::convert(&posted, typ)
            .map_err(|_| datastreamdb_posting::PostingError::NoValue)?;
        if matches!(value.value, datastreamdb_types::Value::Null) {
            return Ok(false);
        }
        Ok(compare(&value))
    }
}

/// Parsed-key scaffold for one attribute.
fn parsed(attr: &str) -> datastreamdb_x::ParsedKey {
    datastreamdb_x::parse(&datastreamdb_x::data_key(attr, 1)).expect("data key parses")
}
