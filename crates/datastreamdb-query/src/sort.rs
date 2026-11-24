//! Local sort. Port of the value-sort half of `worker/sort.go`
//! (`sortWithoutIndex`, `sortByValue`, `paginate`, `multiSort`).
//!
//! Delta from Go: sorting always fetches values (Go prefers an index walk
//! when the attribute is indexed); results are identical for single-group
//! data.

#![warn(missing_docs)]

use datastreamdb_proto::{List as ProtoList, SortMessage, SortResult, TaskQuery};
use datastreamdb_types::{TypeId, Val, Value};

use crate::errors::{QueryError, Result};

/// Sorts rows by the requested keys, then paginates.
///
/// # Errors
///
/// Returns [`QueryError`] on storage failure or unsortable types.
pub fn process_sort(
    engine: &std::sync::Arc<datastreamdb_posting::PostingEngine>,
    cache: &std::sync::Arc<datastreamdb_posting::LocalCache>,
    message: &SortMessage,
) -> Result<SortResult> {
    if message.order.is_empty() {
        return Err(QueryError::Invalid("no sort order".to_owned()));
    }
    let attr = &message.order[0].attr;
    let typ = engine
        .schema()
        .type_of(attr)
        .map_err(|_| QueryError::Invalid(format!("cannot sort attribute {attr} of type object")))?;
    if !typ.is_scalar() {
        return Err(QueryError::Invalid(format!(
            "cannot sort attribute {attr} of type object"
        )));
    }

    let mut out = SortResult {
        uid_matrix: Vec::new(),
    };
    let mut multi_offsets: Vec<i32> = Vec::new();
    for row in &message.uid_matrix {
        let mut uids = row.uids.clone();
        let vals = sort_by_value(engine, cache, message, &mut uids, typ)?;
        let (start, end) = paginate(message, &uids, &vals);
        if message.order.len() > 1 {
            let offset = if i32::try_from(start).unwrap_or(i32::MAX) < message.offset {
                message.offset - i32::try_from(start).unwrap_or(i32::MAX)
            } else {
                0
            };
            multi_offsets.push(offset);
        }
        uids = uids[start..end].to_vec();
        out.uid_matrix.push(ProtoList { uids });
    }

    if message.order.len() > 1 {
        multi_sort(engine, cache, message, &mut out, &multi_offsets)?;
    }
    Ok(out)
}

/// Fetches values, sorts one row on the first key, nulls last.
fn sort_by_value(
    engine: &std::sync::Arc<datastreamdb_posting::PostingEngine>,
    cache: &std::sync::Arc<datastreamdb_posting::LocalCache>,
    ts: &SortMessage,
    uids: &mut Vec<u64>,
    typ: TypeId,
) -> Result<Vec<Val>> {
    let order = &ts.order[0];
    let lang = match order.langs.len() {
        0 => String::new(),
        1 => order.langs[0].clone(),
        _ => {
            return Err(QueryError::Invalid(
                "sorting on multiple language is not supported".to_owned(),
            ));
        }
    };

    let mut valued_uids: Vec<u64> = Vec::new();
    let mut values: Vec<Vec<Val>> = Vec::new();
    let mut nulls: Vec<u64> = Vec::new();
    let mut null_vals: Vec<Vec<Val>> = Vec::new();
    let fetched = fetch_row_values(engine, cache, &order.attr, &order.langs, uids, ts.read_ts)?;
    for (uid, val) in uids.iter().zip(fetched) {
        if let Some(val) = val {
            valued_uids.push(*uid);
            values.push(vec![val]);
        } else {
            nulls.push(*uid);
            null_vals.push(vec![Val {
                tid: typ,
                value: Value::Null,
            }]);
        }
    }

    datastreamdb_types::sort_values(&mut values, &mut valued_uids, &[order.desc], &lang)?;
    valued_uids.extend_from_slice(&nulls);
    values.extend(null_vals);
    *uids = valued_uids;
    let mut multi = Vec::new();
    if ts.order.len() > 1 {
        for row in &values {
            multi.push(row[0].clone());
        }
    }
    Ok(multi)
}

/// Pagination window with multi-key tie expansion.
fn paginate(ts: &SortMessage, dest: &[u64], vals: &[Val]) -> (usize, usize) {
    let (mut start, mut end) = crate::vars::page_range(ts.count, ts.offset, dest.len());
    if ts.order.len() > 1 {
        while start < vals.len() && start > 0 {
            if !datastreamdb_types::equal(&vals[start], &vals[start - 1]) {
                break;
            }
            start -= 1;
        }
        while end < dest.len() {
            if end == 0 || !datastreamdb_types::equal(&vals[end - 1], &vals[end]) {
                break;
            }
            end += 1;
        }
    }
    (start, end)
}

/// Sorts every row on the remaining keys.
fn multi_sort(
    engine: &std::sync::Arc<datastreamdb_posting::PostingEngine>,
    cache: &std::sync::Arc<datastreamdb_posting::LocalCache>,
    ts: &SortMessage,
    out: &mut SortResult,
    offsets: &[i32],
) -> Result<()> {
    let mut dest: Vec<u64> = Vec::new();
    for row in &out.uid_matrix {
        dest.extend_from_slice(&row.uids);
    }
    dest.sort_unstable();
    dest.dedup();

    let mut columns: Vec<Vec<Vec<Val>>> = Vec::new();
    for (key_idx, order) in ts.order.iter().enumerate() {
        let fetched =
            fetch_row_values(engine, cache, &order.attr, &order.langs, &dest, ts.read_ts)?;
        let mut cells = Vec::with_capacity(dest.len());
        for val in fetched {
            cells.push(vec![val.unwrap_or(Val {
                tid: TypeId::Default,
                value: Value::Null,
            })]);
        }
        let _ = key_idx;
        columns.push(cells);
    }

    let descending: Vec<bool> = ts.order.iter().map(|order| order.desc).collect();
    for (i, row) in out.uid_matrix.iter_mut().enumerate() {
        let mut row_vals: Vec<Vec<Val>> = Vec::with_capacity(row.uids.len());
        for uid in &row.uids {
            let idx = dest
                .binary_search(uid)
                .map_err(|_| QueryError::Invalid("sort uid missing from destination".to_owned()))?;
            row_vals.push(columns.iter().map(|col| col[idx][0].clone()).collect());
        }
        let offset = offsets.get(i).copied().unwrap_or(0);
        let (start, end) = crate::vars::page_range(ts.count, offset, row.uids.len());
        if end < row.uids.len() / 2 {
            datastreamdb_types::sort_top_n(&mut row_vals, &mut row.uids, &descending, "", end)?;
        } else {
            datastreamdb_types::sort_values(&mut row_vals, &mut row.uids, &descending, "")?;
        }
        row.uids = row.uids[start..end].to_vec();
    }
    Ok(())
}

/// Fetches one value per UID (`None` for missing/unconvertible).
fn fetch_row_values(
    engine: &std::sync::Arc<datastreamdb_posting::PostingEngine>,
    cache: &std::sync::Arc<datastreamdb_posting::LocalCache>,
    attr: &str,
    langs: &[String],
    uids: &[u64],
    read_ts: u64,
) -> Result<Vec<Option<Val>>> {
    let query = TaskQuery {
        attr: attr.to_owned(),
        langs: langs.to_vec(),
        uid_list: Some(ProtoList {
            uids: uids.to_vec(),
        }),
        read_ts,
        ..TaskQuery::default()
    };
    let out = datastreamdb_task::process_task(engine, cache, query)?;
    let mut vals = Vec::with_capacity(uids.len());
    for (i, _) in uids.iter().enumerate() {
        let val = out
            .value_matrix
            .get(i)
            .and_then(|row| row.values.first())
            .and_then(|cell| crate::vars::convert_with_best_effort(cell, attr).ok());
        vals.push(val);
    }
    Ok(vals)
}
