//! Math expression evaluation. Port of `processMath`,
//! `evalMathTree`, and the process helpers from `query/math.go`.

#![warn(missing_docs)]

use datastreamdb_types::{ShardedMap, TypeId, Val, Value};

use crate::aggregator::{
    compare_values, is_binary_boolean_fn, is_binary_fn, is_ternary_fn, is_unary_fn,
};
use crate::errors::{QueryError, Result};
use crate::subgraph::MathNode;

impl MathNode {
    /// Visits every variable leaf mutably.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] when the visitor fails.
    pub fn each_var_node(&mut self, f: &mut dyn FnMut(&mut MathNode) -> Result<()>) -> Result<()> {
        for child in &mut self.child {
            child.each_var_node(f)?;
        }
        if !self.var.is_empty() {
            f(self)?;
        }
        Ok(())
    }
}

/// Evaluates one math tree bottom-up.
///
/// # Errors
///
/// Returns [`QueryError`] on malformed trees or domain errors.
pub fn eval_math_tree(tree: &mut MathNode) -> Result<()> {
    if !matches!(tree.constant.value, Value::Null) {
        return Ok(());
    }
    if !tree.var.is_empty() {
        if tree.val.is_empty() {
            tracing::debug!("Variable {} not yet populated or missing.", tree.var);
        }
        return Ok(());
    }
    for child in &mut tree.child {
        eval_math_tree(child)?;
    }
    if is_unary_fn(&tree.op) {
        if tree.child.len() != 1 {
            return Err(QueryError::Invalid(format!(
                "Function {} expects 1 argument. But got: {}",
                tree.op,
                tree.child.len()
            )));
        }
        return process_unary(tree);
    }
    if is_binary_fn(&tree.op) {
        if tree.child.len() != 2 {
            return Err(QueryError::Invalid(format!(
                "Function {} expects 2 argument. But got: {}",
                tree.op,
                tree.child.len()
            )));
        }
        return process_binary(tree);
    }
    if is_binary_boolean_fn(&tree.op) {
        if tree.child.len() != 2 {
            return Err(QueryError::Invalid(format!(
                "Function {} expects 2 argument. But got: {}",
                tree.op,
                tree.child.len()
            )));
        }
        return process_binary_boolean(tree);
    }
    if is_ternary_fn(&tree.op) {
        if tree.child.len() != 3 {
            return Err(QueryError::Invalid(format!(
                "Function {} expects 3 argument. But got: {}",
                tree.op,
                tree.child.len()
            )));
        }
        return process_ternary(tree);
    }
    Err(QueryError::Invalid(format!(
        "invalid math function: {}",
        tree.op
    )))
}

/// Evaluates the math blocks of one subgraph.
///
/// # Errors
///
/// Returns [`QueryError`] on malformed trees or domain errors.
///
/// # Panics
///
/// Panics if the math expression vanished after the `None` check.
pub fn process_math(sg: &mut crate::subgraph::SubGraph) -> Result<()> {
    if sg.math_exp.is_none() {
        return Ok(());
    }
    let needs_var = !sg.params.needs_var.is_empty();
    if needs_var && sg.params.uid_to_val.is_empty() {
        return Ok(());
    }
    eval_math_tree(sg.math_exp.as_mut().expect("math"))
}

/// Applies a unary operator across the operand map.
fn process_unary(tree: &mut MathNode) -> Result<()> {
    let child = tree.child[0].clone();
    if !matches!(child.constant.value, Value::Null) {
        let mut agg = crate::aggregator::Aggregator::new(&tree.op);
        agg.apply_val(&child.constant)?;
        tree.constant = agg.value()?;
        return Ok(());
    }
    let operand = child.val;
    let op = tree.op.clone();
    let mut dest = ShardedMap::new();
    operand.iterate(|uid, val| {
        let mut agg = crate::aggregator::Aggregator::new(&op);
        agg.apply_val(val)?;
        dest.set(uid, agg.value()?);
        Ok::<(), QueryError>(())
    })?;
    tree.val = dest;
    Ok(())
}

/// Applies a binary operator across the operand maps.
fn process_binary(tree: &mut MathNode) -> Result<()> {
    let op = tree.op.clone();
    let (mut left_map, mut right_map) = (tree.child[0].val.clone(), tree.child[1].val.clone());
    let (mut left_const, mut right_const) = (
        tree.child[0].constant.clone(),
        tree.child[1].constant.clone(),
    );
    // Aggregation outputs (single key 0) act as constants.
    if left_map.len() == 1 {
        if let Some(val) = left_map.get(0) {
            left_const = val.clone();
            left_map = ShardedMap::new();
        }
    } else if right_map.len() == 1 {
        if let Some(val) = right_map.get(0) {
            right_const = val.clone();
            right_map = ShardedMap::new();
        }
    }

    if !left_map.is_empty() || !right_map.is_empty() {
        let mut keys = std::collections::HashSet::new();
        left_map.iterate(|uid, _| {
            keys.insert(uid);
            Ok::<(), QueryError>(())
        })?;
        right_map.iterate(|uid, _| {
            keys.insert(uid);
            Ok::<(), QueryError>(())
        })?;
        let mut dest = ShardedMap::new();
        for uid in keys {
            let left_val = if matches!(left_const.value, Value::Null) {
                left_map.get(uid).cloned().unwrap_or(null_val())
            } else {
                left_const.clone()
            };
            let right_val = if matches!(right_const.value, Value::Null) {
                right_map.get(uid).cloned().unwrap_or(null_val())
            } else {
                right_const.clone()
            };
            let mut agg = crate::aggregator::Aggregator::new(&op);
            agg.apply_val(&left_val)?;
            agg.apply_val(&right_val)?;
            dest.set(uid, agg.value()?);
        }
        tree.val = dest;
        return Ok(());
    }

    if !matches!(left_const.value, Value::Null) && !matches!(right_const.value, Value::Null) {
        let mut agg = crate::aggregator::Aggregator::new(&op);
        agg.apply_val(&left_const)?;
        agg.apply_val(&right_const)?;
        tree.constant = agg.value()?;
    }
    Ok(())
}

/// Applies a boolean operator across the operand maps.
fn process_binary_boolean(tree: &mut MathNode) -> Result<()> {
    let left = tree.child[0].val.clone();
    let right_child = tree.child[1].clone();
    let op = tree.op.clone();
    let mut dest = ShardedMap::new();
    left.iterate(|uid, lval| {
        let rval = if matches!(right_child.constant.value, Value::Null) {
            right_child.val.get(uid).cloned().unwrap_or(null_val())
        } else {
            right_child.constant.clone()
        };
        dest.set(
            uid,
            Val {
                tid: TypeId::Bool,
                value: Value::Bool(compare_values(&op, lval.clone(), rval.clone()).map_err(
                    |err| {
                        QueryError::Invalid(format!("Wrong values in comparison function: {err}"))
                    },
                )?),
            },
        );
        Ok::<(), QueryError>(())
    })?;
    tree.val = dest;
    Ok(())
}

/// Applies `cond(cond, then, else)` across the operand maps.
fn process_ternary(tree: &mut MathNode) -> Result<()> {
    let cond = tree.child[0].val.clone();
    if cond.is_empty() {
        return Err(QueryError::Invalid(format!(
            "Expected a value variable in {} but missing.",
            tree.op
        )));
    }
    let (then, otherwise) = (tree.child[1].clone(), tree.child[2].clone());
    let mut dest = ShardedMap::new();
    cond.iterate(|uid, guard| {
        let Value::Bool(take) = guard.value else {
            return Err(QueryError::Invalid(
                "First variable of conditional function not a bool value".to_owned(),
            ));
        };
        if take {
            if matches!(then.constant.value, Value::Null) {
                if let Some(val) = then.val.get(uid) {
                    dest.set(uid, val.clone());
                }
            } else {
                dest.set(uid, then.constant.clone());
            }
        } else if matches!(otherwise.constant.value, Value::Null) {
            if let Some(val) = otherwise.val.get(uid) {
                dest.set(uid, val.clone());
            }
        } else {
            dest.set(uid, otherwise.constant.clone());
        }
        Ok::<(), QueryError>(())
    })?;
    tree.val = dest;
    Ok(())
}

/// Null value (missing-operand marker).
fn null_val() -> Val {
    Val {
        tid: TypeId::Default,
        value: Value::Null,
    }
}
