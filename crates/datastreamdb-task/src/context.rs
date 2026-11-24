//! Task function types and per-task state. Port of `FuncType`,
//! `functionContext`, `queryState`, and the small `parseFuncType`/`convert`
//! helpers from `worker/task.go` (plus the `task/conversion.go` value
//! constructors).

#![warn(missing_docs)]

use std::sync::Arc;

use datastreamdb_posting::LocalCache;
use datastreamdb_proto::{SrcFunction, TaskValue, ValType};
use datastreamdb_types::{TypeId, Val, Value, convert, marshal};

use crate::errors::{Result, TaskError};

/// Query function kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FuncType {
    /// Plain UID/value fetch.
    #[default]
    NotAFunction,
    /// `min`/`max`/`sum`/`avg`.
    Aggregator,
    /// `eq`/`le`/… over an indexed attribute.
    CompareAttr,
    /// Comparison over `count()`.
    CompareScalar,
    /// Geo functions.
    Geo,
    /// `checkpwd`.
    Password,
    /// `regexp`.
    Regex,
    /// `ngram` (unused; tokenizes like full-text).
    Ngram,
    /// `alloftext`/`anyoftext`.
    FullTextSearch,
    /// `has`.
    Has,
    /// `uid_in`.
    UidIn,
    /// `anyof`/`allof` custom tokenizers.
    CustomIndex,
    /// `match` (fuzzy).
    Match,
    /// `similar_to` (vector; deferred).
    SimilarTo,
    /// `allofterms`/`anyofterms`/… term functions.
    Standard,
}

/// `eq` function name.
pub const FN_EQ: &str = "eq";
/// `between` function name.
pub const FN_BETWEEN: &str = "between";

/// Classifies a source function (lowercased name returned).
#[must_use]
pub fn parse_func_type(src_func: Option<&SrcFunction>) -> (FuncType, String) {
    let Some(src_func) = src_func else {
        return (FuncType::NotAFunction, String::new());
    };
    let (fn_type, name) = parse_func_type_helper(&src_func.name);
    if src_func.is_count && fn_type == FuncType::CompareAttr {
        return (FuncType::CompareScalar, name);
    }
    (fn_type, name)
}

/// Classifies a function name (lowercased name returned).
#[must_use]
pub fn parse_func_type_helper(name: &str) -> (FuncType, String) {
    if name.is_empty() {
        return (FuncType::NotAFunction, String::new());
    }
    let lower = name.to_lowercase();
    let fn_type = match lower.as_str() {
        "le" | "ge" | "lt" | "gt" | "eq" | "between" => FuncType::CompareAttr,
        "min" | "max" | "sum" | "avg" => FuncType::Aggregator,
        "checkpwd" => FuncType::Password,
        "regexp" => FuncType::Regex,
        "ngram" => FuncType::Ngram,
        "alloftext" | "anyoftext" => FuncType::FullTextSearch,
        "has" => FuncType::Has,
        "uid_in" => FuncType::UidIn,
        "similar_to" => FuncType::SimilarTo,
        "anyof" | "allof" => FuncType::CustomIndex,
        "match" => FuncType::Match,
        _ if is_geo_func(&lower) => FuncType::Geo,
        _ => FuncType::Standard,
    };
    (fn_type, lower)
}

/// Whether the name is a geo function.
fn is_geo_func(name: &str) -> bool {
    matches!(name, "near" | "within" | "contains" | "intersects")
}

/// Whether the function needs an index (false for uid-list filters).
#[must_use]
pub fn needs_index(fn_type: FuncType, has_uid_list: bool) -> bool {
    match fn_type {
        FuncType::CompareAttr => !has_uid_list,
        FuncType::Geo
        | FuncType::FullTextSearch
        | FuncType::Standard
        | FuncType::Match
        | FuncType::SimilarTo => true,
        FuncType::NotAFunction
        | FuncType::Aggregator
        | FuncType::CompareScalar
        | FuncType::Password
        | FuncType::Regex
        | FuncType::Ngram
        | FuncType::Has
        | FuncType::UidIn
        | FuncType::CustomIndex => false,
    }
}

/// Whether results need intersecting (`allof*` semantics).
#[must_use]
pub fn needs_intersect(fn_name: &str) -> bool {
    fn_name.starts_with("allof") || fn_name.ends_with("allof")
}

/// Parsed function state for one task.
#[derive(Clone, Debug)]
pub struct FunctionContext {
    /// Index tokens (one row per token).
    pub tokens: Vec<Vec<u8>>,
    /// Geo filter data.
    pub geo_query: Option<datastreamdb_types::geofilter::GeoQueryData>,
    /// Intersect (rather than merge) destination UIDs.
    pub intersect_dest: bool,
    /// Converted comparison values.
    pub eq_tokens: Vec<Val>,
    /// Boundary token for inequalities.
    pub ineq_value_token: Vec<Vec<u8>>,
    /// Number of task rows.
    pub n: usize,
    /// Integer thresholds (`match` distance, scalar compares).
    pub threshold: Vec<i64>,
    /// Candidate UIDs (`uid_in`).
    pub uids_present: Vec<u64>,
    /// Lowercased function name.
    pub fname: String,
    /// Function kind.
    pub fn_type: FuncType,
    /// Compiled regex (`regexp`).
    pub regex: Option<regex::Regex>,
    /// Function runs at the query root.
    pub is_func_at_root: bool,
    /// Function runs over a string attribute.
    pub is_string_fn: bool,
    /// Schema type of the attribute.
    pub atype: TypeId,
}

impl Default for FunctionContext {
    fn default() -> Self {
        Self {
            tokens: Vec::new(),
            geo_query: None,
            intersect_dest: false,
            eq_tokens: Vec::new(),
            ineq_value_token: Vec::new(),
            n: 0,
            threshold: Vec::new(),
            uids_present: Vec::new(),
            fname: String::new(),
            fn_type: FuncType::NotAFunction,
            regex: None,
            is_func_at_root: false,
            is_string_fn: false,
            atype: TypeId::Default,
        }
    }
}

impl FunctionContext {
    /// Whether value (rather than UID) postings serve this function.
    ///
    /// # Errors
    ///
    /// Returns [`TaskError::Invalid`] for unhandled functions.
    pub fn needs_value_postings(&self, typ: TypeId) -> Result<bool> {
        match self.fn_type {
            FuncType::Aggregator | FuncType::Password | FuncType::SimilarTo => Ok(true),
            FuncType::CompareAttr => Ok(self.tokens.is_empty()),
            FuncType::Geo
            | FuncType::Regex
            | FuncType::FullTextSearch
            | FuncType::Standard
            | FuncType::Has
            | FuncType::CustomIndex
            | FuncType::Match
            | FuncType::Ngram
            | FuncType::UidIn
            | FuncType::CompareScalar => Ok(false),
            FuncType::NotAFunction => Ok(typ.is_scalar()),
        }
    }
}

/// Per-task query state (posting cache handle plus engine services).
#[derive(Clone, Debug)]
pub struct QueryState {
    /// Posting list cache (transactional or throwaway).
    pub cache: Arc<LocalCache>,
    /// Owning engine (schema, store, stats).
    pub engine: Arc<datastreamdb_posting::PostingEngine>,
}

impl QueryState {
    /// State over a cache and engine.
    #[must_use]
    pub fn new(cache: Arc<LocalCache>, engine: Arc<datastreamdb_posting::PostingEngine>) -> Self {
        Self { cache, engine }
    }
}

/// Converts query text into the attribute's schema type.
pub(crate) fn convert_value(
    schema: &datastreamdb_schema::SchemaState,
    attr: &str,
    data: &str,
) -> Result<Val> {
    let typ = schema.type_of(attr).map_err(|_| {
        TaskError::Invalid(format!(
            "attribute {} is not valid scalar type",
            datastreamdb_x::parse_attr(attr)
        ))
    })?;
    if !typ.is_scalar() {
        return Err(TaskError::Invalid(format!(
            "attribute {} is not valid scalar type",
            datastreamdb_x::parse_attr(attr)
        )));
    }
    let src = Val {
        tid: TypeId::String,
        value: Value::Binary(data.as_bytes().to_vec()),
    };
    Ok(convert(&src, typ)?)
}

/// Converts a value into a task cell of type `typ`.
pub(crate) fn convert_to_type(val: &Val, typ: TypeId) -> Result<TaskValue> {
    let val_type = match typ {
        TypeId::Default | TypeId::Undefined => ValType::Default,
        TypeId::Binary => ValType::Binary,
        TypeId::Int => ValType::Int,
        TypeId::Float => ValType::Float,
        TypeId::Bool => ValType::Bool,
        TypeId::DateTime => ValType::Datetime,
        TypeId::Geo => ValType::Geo,
        TypeId::Uid => ValType::Uid,
        TypeId::Password => ValType::Password,
        TypeId::String => ValType::String,
        TypeId::Object => ValType::Object,
        TypeId::BigFloat => ValType::Bigfloat,
        TypeId::VFloat => ValType::Vfloat,
    };
    if val.tid == typ {
        if let Value::Binary(bytes) = &val.value {
            return Ok(TaskValue {
                val: bytes.clone(),
                val_type: val_type as i32,
            });
        }
    }
    let converted = convert(val, typ)?;
    let data = marshal(&converted, TypeId::Binary)?;
    let Value::Binary(bytes) = data.value else {
        return Err(TaskError::Invalid(
            "failed convert_to_type during marshal".to_owned(),
        ));
    };
    Ok(TaskValue {
        val: bytes,
        val_type: val_type as i32,
    })
}

/// Whether an aggregator applies to a type.
#[must_use]
pub fn could_apply_aggregator_on(aggregator: &str, typ: TypeId) -> bool {
    if !typ.is_scalar() {
        return false;
    }
    match aggregator {
        "min" | "max" => matches!(
            typ,
            TypeId::Int | TypeId::Float | TypeId::DateTime | TypeId::String | TypeId::Default
        ),
        "sum" | "avg" => matches!(typ, TypeId::Int | TypeId::Float | TypeId::VFloat),
        _ => false,
    }
}

/// Scalar integer comparison.
#[must_use]
pub fn eval_compare(cmp: &str, left: i64, right: i64) -> bool {
    match cmp {
        "le" => left <= right,
        "ge" => left >= right,
        "lt" => left < right,
        "gt" => left > right,
        "eq" => left == right,
        _ => unreachable!("eval_compare: unreachable"),
    }
}

/// Task cell holding a boolean.
#[must_use]
pub fn task_value_bool(val: bool) -> TaskValue {
    task_value_int(i64::from(val))
}

/// Task cell holding an integer (little-endian, like Go).
#[must_use]
pub fn task_value_int(val: i64) -> TaskValue {
    TaskValue {
        val: val.to_le_bytes().to_vec(),
        val_type: ValType::Int as i32,
    }
}

/// Reads an integer task cell.
#[must_use]
pub fn task_value_to_int(val: &TaskValue) -> i64 {
    let mut buf = [0u8; 8];
    let len = val.val.len().min(8);
    buf[..len].copy_from_slice(&val.val[..len]);
    i64::from_le_bytes(buf)
}

/// Reads a boolean task cell.
#[must_use]
pub fn task_value_to_bool(val: &TaskValue) -> bool {
    !val.val.is_empty() && task_value_to_int(val) != 0
}

/// Task cell holding a string.
#[must_use]
pub fn task_value_string(val: &str) -> TaskValue {
    TaskValue {
        val: val.as_bytes().to_vec(),
        val_type: ValType::String as i32,
    }
}

/// Reads a string task cell.
#[must_use]
pub fn task_value_to_string(val: &TaskValue) -> String {
    String::from_utf8_lossy(&val.val).into_owned()
}

/// Task cell holding a float (little-endian, like Go).
#[must_use]
pub fn task_value_float(val: f64) -> TaskValue {
    TaskValue {
        val: val.to_le_bytes().to_vec(),
        val_type: ValType::Float as i32,
    }
}

/// Reads a float task cell.
#[must_use]
pub fn task_value_to_float(val: &TaskValue) -> f64 {
    let mut buf = [0u8; 8];
    let len = val.val.len().min(8);
    buf[..len].copy_from_slice(&val.val[..len]);
    f64::from_le_bytes(buf)
}

/// `true` task cell.
#[must_use]
pub fn true_val() -> TaskValue {
    task_value_bool(true)
}

/// `false` task cell.
#[must_use]
pub fn false_val() -> TaskValue {
    task_value_bool(false)
}
