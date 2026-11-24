//! Math-level variable transforms. Split from `vars.rs`: facet decoding,
//! level aggregation, path transforms, and int widening.
//!
//! Port of the level-walking half of the upstream Go `query/query.go`.

#![warn(missing_docs)]

use std::collections::HashMap;

use datastreamdb_types::{ShardedMap, TypeId, Val, Value, convert};

use crate::aggregator::Aggregator;
use crate::errors::{QueryError, Result};
use crate::subgraph::{SubGraph, VarValue};
use crate::vars::{SgPath, path_at};

/// Decodes a stored facet into a typed value.
pub(crate) fn facet_to_val(facet: &datastreamdb_proto::api::Facet) -> Result<Val> {
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
    Ok(convert(
        &Val {
            tid: TypeId::Binary,
            value: Value::Binary(facet.value.clone()),
        },
        tid,
    )?)
}

/// Reads a parent path entry's group-by flag.
pub(crate) fn path_at_parent(parent: *const SubGraph) -> &'static SubGraph {
    // SAFETY: parents outlive the aggregation walk.
    unsafe { &*parent }
}

/// Reads a parent path entry's attribute.
pub(crate) fn path_at_parent_attr(parent: *const SubGraph) -> &'static str {
    path_at_parent(parent).attr.as_str()
}

/// Aggregates one level for `min`/`max`/`sum`/`avg` over a variable.
pub(crate) fn eval_level_agg(
    done_vars: &HashMap<String, VarValue>,
    sg: &SubGraph,
    parent: Option<&SubGraph>,
) -> Result<ShardedMap> {
    let parent =
        parent.ok_or_else(|| QueryError::Invalid("Wrong level for var aggregation".to_owned()))?;
    let needs_var = sg
        .params
        .needs_var
        .first()
        .map(|needs| needs.name.clone())
        .unwrap_or_default();
    if parent.params.is_empty {
        let vals = done_vars.get(&needs_var).map(|var| &var.vals);
        let mut agg = Aggregator::new(
            &sg.src_func
                .as_ref()
                .map(|func| func.name.clone())
                .unwrap_or_default(),
        );
        if let Some(vals) = vals {
            vals.iterate(|_, val| {
                agg.apply(val)?;
                Ok::<(), QueryError>(())
            })?;
        }
        if let Ok(val) = agg.value() {
            if !matches!(val.value, Value::Null) {
                let mut mp = ShardedMap::new();
                mp.set(0, val);
                return Ok(mp);
            }
        }
        return Ok(ShardedMap::new());
    }

    let mut related: Option<&SubGraph> = None;
    for child in &parent.children {
        if std::ptr::eq(child.as_ref(), sg) {
            continue;
        }
        if child
            .params
            .facet_var
            .values()
            .any(|name| *name == needs_var)
        {
            related = Some(child);
        }
        for grand in &child.children {
            if grand.params.var == needs_var {
                related = Some(child);
            }
        }
    }
    let related = related.ok_or_else(|| {
        QueryError::Invalid("Invalid variable aggregation. Check the levels.".to_owned())
    })?;

    let vals = done_vars.get(&needs_var).map(|var| &var.vals);
    let mut mp = ShardedMap::new();
    for (i, row) in related.uid_matrix.iter().enumerate() {
        let mut agg = Aggregator::new(
            &sg.src_func
                .as_ref()
                .map(|func| func.name.clone())
                .unwrap_or_default(),
        );
        for uid in &row.uids {
            if let Some(val) = vals.and_then(|vals| vals.get(*uid)) {
                agg.apply(val).map_err(|err| {
                    QueryError::Invalid(format!("Error in applying aggregation {err}"))
                })?;
            }
        }
        if let Ok(val) = agg.value() {
            if !matches!(val.value, Value::Null) {
                let src = related
                    .src_uids
                    .as_ref()
                    .and_then(|src| src.uids.get(i).copied());
                if let Some(src) = src {
                    mp.set(src, val);
                }
            }
        }
    }
    Ok(mp)
}

/// Transforms variable values onto the target path level.
pub(crate) fn transform_to(from: &VarValue, to_path: &SgPath) -> Result<ShardedMap> {
    if to_path.len() < from.path.len() {
        return Ok(from.vals.clone());
    }
    let mut idx = 0;
    while idx < from.path.len() {
        if from.path[idx] != to_path[idx] {
            return Ok(from.vals.clone());
        }
        idx += 1;
    }
    if from.vals.is_empty() {
        return Ok(from.vals.clone());
    }

    let mut current = from.vals.clone();
    while idx < to_path.len() {
        let node = path_at(to_path, idx);
        idx += 1;
        if idx == 1 {
            continue;
        }
        let mut next = ShardedMap::new();
        for (i, row) in node.uid_matrix.iter().enumerate() {
            let Some(src_uid) = node
                .src_uids
                .as_ref()
                .and_then(|src| src.uids.get(i).copied())
            else {
                continue;
            };
            let Some(current_val) = current.get(src_uid) else {
                continue;
            };
            if matches!(current_val.value, Value::Null) {
                continue;
            }
            if current_val.tid != TypeId::Int && current_val.tid != TypeId::Float {
                return Ok(next);
            }
            for dst_uid in &row.uids {
                let mut agg = Aggregator::new("sum");
                agg.apply(current_val)?;
                if let Some(temp) = next.get(*dst_uid) {
                    agg.apply(temp)?;
                }
                if let Ok(val) = agg.value() {
                    next.set(*dst_uid, val);
                }
            }
        }
        current = next;
    }
    Ok(current)
}

/// Widens an integer exactly like Go's `float64(int64)`.
#[allow(clippy::cast_precision_loss, reason = "Go float64(int64) parity")]
pub(crate) fn int_to_float(num: i64) -> f64 {
    num as f64
}
