//! Facet filter trees. Port of `preprocessFilter` / `applyFacetsTree` /
//! `filterOnStandardFn` from `worker/task.go`.

#![warn(missing_docs)]

use std::collections::HashMap;

use datastreamdb_proto::{FilterTree, api::Facet};
use datastreamdb_types::{TypeId, Val, Value};

use crate::context::{FuncType, parse_func_type_helper};
use crate::errors::{Result, TaskError};

/// Common facet types pre-converted at each function node.
const COMMON_TYPE_IDS: [TypeId; 6] = [
    TypeId::String,
    TypeId::Int,
    TypeId::Float,
    TypeId::DateTime,
    TypeId::Bool,
    TypeId::Default,
];

/// One leaf function over a facet key.
#[derive(Clone, Debug)]
pub struct FacetsFunc {
    /// Lowercased function name.
    pub name: String,
    /// Facet key.
    pub key: String,
    /// Raw arguments.
    pub args: Vec<String>,
    /// Tokenized arguments (`allofterms`/`anyofterms`).
    pub tokens: Vec<String>,
    /// String-typed argument value.
    pub val: Val,
    /// Function kind.
    pub fn_type: FuncType,
    /// Argument pre-converted per common type.
    pub types_to_val: HashMap<TypeId, Val>,
}

/// Boolean tree over facet leaves.
#[derive(Clone, Debug)]
pub struct FacetsTree {
    /// Operator (`and`/`or`/`not`; empty on leaves).
    pub op: String,
    /// Child subtrees.
    pub children: Vec<FacetsTree>,
    /// Leaf function.
    pub function: Option<FacetsFunc>,
}

/// Decodes a stored facet into a typed value.
///
/// # Errors
///
/// Returns [`TaskError`] on conversion failure.
fn facet_val(facet: &Facet) -> Result<Val> {
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
    let raw = Val {
        tid: TypeId::Binary,
        value: Value::Binary(facet.value.clone()),
    };
    Ok(datastreamdb_types::convert(&raw, tid)?)
}

/// Type tag of a stored facet.
fn facet_type_id(facet: &Facet) -> Result<TypeId> {
    match facet.val_type {
        0 => Ok(TypeId::String),
        1 => Ok(TypeId::Int),
        2 => Ok(TypeId::Float),
        3 => Ok(TypeId::Bool),
        4 => Ok(TypeId::DateTime),
        other => Err(TaskError::Invalid(format!(
            "unknown facet value type {other}"
        ))),
    }
}

/// Whether the facet's tokens satisfy an `allofterms`/`anyofterms` call.
fn filter_on_standard_fn(fname: &str, fc_tokens: &[String], arg_tokens: &[String]) -> Result<bool> {
    match fname {
        "allofterms" => {
            if arg_tokens.len() > fc_tokens.len() {
                return Ok(false);
            }
            let mut arg_idx = 0;
            let mut fc_idx = 0;
            while arg_idx < arg_tokens.len() && fc_idx < fc_tokens.len() {
                match fc_tokens[fc_idx].cmp(&arg_tokens[arg_idx]) {
                    std::cmp::Ordering::Less => fc_idx += 1,
                    std::cmp::Ordering::Equal => {
                        fc_idx += 1;
                        arg_idx += 1;
                    }
                    std::cmp::Ordering::Greater => break,
                }
            }
            Ok(arg_idx == arg_tokens.len())
        }
        "anyofterms" => {
            let mut arg_idx = 0;
            let mut fc_idx = 0;
            while arg_idx < arg_tokens.len() && fc_idx < fc_tokens.len() {
                match fc_tokens[fc_idx].cmp(&arg_tokens[arg_idx]) {
                    std::cmp::Ordering::Less => fc_idx += 1,
                    std::cmp::Ordering::Equal => return Ok(true),
                    std::cmp::Ordering::Greater => arg_idx += 1,
                }
            }
            Ok(false)
        }
        _ => Err(TaskError::Invalid(format!(
            "fn {fname} not supported in facets filtering"
        ))),
    }
}

/// Tests stored facets against a filter tree.
///
/// # Errors
///
/// Returns [`TaskError`] on conversion failure or unsupported functions.
///
/// # Panics
///
/// Never in practice (child counts are validated by [`preprocess_filter`]).
pub fn apply_facets_tree(posting_facets: &[Facet], tree: Option<&FacetsTree>) -> Result<bool> {
    let Some(tree) = tree else {
        return Ok(true);
    };
    if let Some(function) = &tree.function {
        let facet = posting_facets.iter().find(|fc| fc.key == function.key);
        let Some(facet) = facet else {
            return Ok(false);
        };
        match function.fn_type {
            FuncType::CompareAttr => {
                let fval = facet_val(facet)?;
                let converted;
                let other = if let Some(val) = function.types_to_val.get(&fval.tid) {
                    val
                } else {
                    converted = datastreamdb_types::convert(&function.val, fval.tid);
                    match &converted {
                        Ok(val) => val,
                        Err(_) => return Ok(false),
                    }
                };
                Ok(datastreamdb_types::compare_vals(
                    &function.name,
                    &fval,
                    other,
                )?)
            }
            FuncType::Standard => {
                if facet_type_id(facet)? != TypeId::String {
                    return Ok(false);
                }
                filter_on_standard_fn(&function.name, &facet.tokens, &function.tokens)
            }
            _ => Err(TaskError::Invalid(format!(
                "fn {} not supported in facets filtering",
                function.name
            ))),
        }
    } else {
        let mut results = Vec::with_capacity(2);
        for child in &tree.children {
            results.push(apply_facets_tree(posting_facets, Some(child))?);
        }
        match tree.op.as_str() {
            "not" => Ok(!results[0]),
            "and" => Ok(results[0] && results[1]),
            "or" => Ok(results[0] || results[1]),
            _ => Err(TaskError::Invalid(
                "unexpected behavior in apply_facets_tree".to_owned(),
            )),
        }
    }
}

/// Compiles a wire filter tree (validating shape and pre-converting values).
///
/// # Errors
///
/// Returns [`TaskError`] on malformed trees or conversion failure.
pub fn preprocess_filter(tree: Option<&FilterTree>) -> Result<Option<FacetsTree>> {
    tree.map(compile_tree).transpose()
}

/// Compiles one present filter node.
fn compile_tree(tree: &FilterTree) -> Result<FacetsTree> {
    let mut compiled = FacetsTree {
        op: tree.op.to_lowercase(),
        children: Vec::new(),
        function: None,
    };
    if let Some(func) = &tree.func {
        if func.args.len() != 1 {
            return Err(TaskError::Invalid(format!(
                "one argument expected in {}, but got {}",
                func.name,
                func.args.len()
            )));
        }
        let (fn_type, fname) = parse_func_type_helper(&func.name);
        let mut function = FacetsFunc {
            name: fname,
            key: func.key.clone(),
            args: func.args.clone(),
            tokens: Vec::new(),
            val: Val {
                tid: TypeId::String,
                value: Value::String(String::new()),
            },
            fn_type,
            types_to_val: HashMap::new(),
        };
        match fn_type {
            FuncType::CompareAttr => {
                function.val = Val {
                    tid: TypeId::String,
                    value: Value::Binary(func.args[0].as_bytes().to_vec()),
                };
                for type_id in COMMON_TYPE_IDS {
                    if let Ok(converted) = datastreamdb_types::convert(&function.val, type_id) {
                        function.types_to_val.insert(type_id, converted);
                    }
                }
            }
            FuncType::Standard => {
                let arg = Val {
                    tid: TypeId::String,
                    value: Value::String(func.args[0].clone()),
                };
                let mut tokens: Vec<String> =
                    datastreamdb_tok::tok::get_term_tokens(std::slice::from_ref(&arg))?
                        .iter()
                        .map(|tok| String::from_utf8_lossy(tok).into_owned())
                        .collect();
                tokens.sort();
                function.tokens = tokens;
            }
            _ => {
                return Err(TaskError::Invalid(format!(
                    "fn {} not supported in preprocess_filter",
                    function.name
                )));
            }
        }
        compiled.function = Some(function);
        return Ok(compiled);
    }

    for child in &tree.children {
        compiled.children.push(compile_tree(child)?);
    }
    let count = tree.children.len();
    match compiled.op.as_str() {
        "not" => {
            if count != 1 {
                return Err(TaskError::Invalid(format!(
                    "expected 1 child for not but got {count}"
                )));
            }
        }
        "and" | "or" => {
            if count != 2 {
                return Err(TaskError::Invalid(format!(
                    "expected 2 child for not but got {count}"
                )));
            }
        }
        _ => {
            return Err(TaskError::Invalid(format!(
                "unsupported operation in facet filtering: {}",
                tree.op
            )));
        }
    }
    Ok(compiled)
}
