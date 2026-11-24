//! Aggregations and math primitives. Port of `query/aggregator.go`:
//! the running aggregator, scalar/vector/bigfloat arithmetic, comparisons,
//! and the group-by value helpers.

#![warn(missing_docs)]
// Every numeric conversion below mirrors a Go `float64()`/`float32()` cast
// in the same expression, including its precision behavior.
#![allow(clippy::cast_precision_loss, reason = "Go-parity float casts")]
#![allow(clippy::cast_possible_truncation, reason = "Go-parity float casts")]
// Exact zero/NaN checks mirror Go (`== 0`, `math.IsNaN`).
#![allow(clippy::float_cmp, reason = "Go-parity float comparisons")]

use bigdecimal::{BigDecimal, FromPrimitive, ToPrimitive, Zero};
use datastreamdb_proto::TaskValue;
use datastreamdb_types::{TypeId, Val, Value, equal};

use crate::errors::{QueryError, Result};

/// Whether the name is a value aggregator.
#[must_use]
pub fn is_aggregator_fn(name: &str) -> bool {
    matches!(name, "min" | "max" | "sum" | "avg")
}

/// Whether the name is a unary math function.
#[must_use]
pub fn is_unary_fn(name: &str) -> bool {
    matches!(
        name,
        "ln" | "exp" | "u-" | "sqrt" | "floor" | "ceil" | "since"
    )
}

/// Whether the name is a boolean binary operator.
#[must_use]
pub fn is_binary_boolean_fn(name: &str) -> bool {
    matches!(name, "<" | ">" | "<=" | ">=" | "==" | "!=")
}

/// Whether the name is the ternary operator.
#[must_use]
pub fn is_ternary_fn(name: &str) -> bool {
    name == "cond"
}

/// Whether the name is a binary math function.
#[must_use]
pub fn is_binary_fn(name: &str) -> bool {
    matches!(
        name,
        "+" | "*" | "-" | "/" | "%" | "max" | "min" | "logbase" | "pow" | "dot"
    )
}

/// Value category for type coercion (discriminants match Go's `valType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BaseType {
    /// Integers.
    Int,
    /// Floats.
    Float,
    /// Float vectors.
    VFloat,
    /// Big floats.
    BigFloat,
    /// Anything else.
    Default,
}

impl BaseType {
    /// Go `valType` discriminant (`invalid types` messages).
    fn discriminant(self) -> u8 {
        match self {
            BaseType::Int => 0,
            BaseType::Float => 1,
            BaseType::VFloat => 2,
            BaseType::Default => 3,
            BaseType::BigFloat => 4,
        }
    }
}

/// `invalid types` error (Go `invalidTypeError`).
fn invalid_type_error(left: BaseType, right: BaseType, name: &str) -> QueryError {
    QueryError::Invalid(format!(
        "invalid types {}, {} for func {}",
        left.discriminant(),
        right.discriminant(),
        name
    ))
}

/// `Wrong type` error (Go `errors.Errorf`, tid as discriminant).
fn wrong_type_error(tid: TypeId, name: &str) -> QueryError {
    QueryError::Invalid(format!(
        "Wrong type {} encountered for func {}",
        tid as i32, name
    ))
}

/// Classifies a value for coercion.
fn base_type(val: &Val) -> BaseType {
    match val.tid {
        TypeId::Int => BaseType::Int,
        TypeId::Float => BaseType::Float,
        TypeId::VFloat => BaseType::VFloat,
        TypeId::BigFloat => BaseType::BigFloat,
        _ => BaseType::Default,
    }
}

/// Binary functions allowing a vector on the right.
fn allows_vector_on_right(name: &str) -> bool {
    matches!(name, "u-" | "+" | "-" | "*" | "dot")
}

/// Binary functions mixing one vector with one scalar.
fn mixes_scalar_vector(name: &str) -> bool {
    matches!(name, "*" | "/")
}

/// Running `min`/`max`/`sum`/`avg` aggregation.
#[derive(Clone, Debug, Default)]
pub struct Aggregator {
    /// Aggregation name.
    name: String,
    /// Running result.
    result: Option<Val>,
    /// Applied value count (`avg` divisor).
    count: usize,
}

impl Aggregator {
    /// Aggregator for `name`.
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            result: None,
            count: 0,
        }
    }

    /// Applies one value.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] on vector-length mismatch or unknown names.
    /// Delta from Go: unknown names error instead of crashing the process.
    pub fn apply(&mut self, val: &Val) -> Result<()> {
        let Some(current) = self.result.clone() else {
            self.result = Some(val.clone());
            self.count += 1;
            return Ok(());
        };
        let next = match self.name.as_str() {
            "min" => {
                if less_than(val, &current).unwrap_or(true) {
                    val.clone()
                } else {
                    current
                }
            }
            "max" => {
                if less_than(&current, val).unwrap_or(true) {
                    val.clone()
                } else {
                    current
                }
            }
            "sum" | "avg" => add_vals(&current, val)?,
            name => {
                return Err(QueryError::Invalid(format!(
                    "unhandled aggregator function {name}"
                )));
            }
        };
        self.count += 1;
        self.result = Some(next);
        Ok(())
    }

    /// Applies one value through math coercion (math expressions).
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] on type mismatch or unknown names.
    pub fn apply_val(&mut self, val: &Val) -> Result<()> {
        let mut val = val.clone();
        if matches!(val.value, Value::Null) {
            val = Val {
                tid: TypeId::Int,
                value: Value::Int(0),
            };
        }
        if is_unary_fn(&self.name) {
            self.result = Some(apply_unary(&self.name, &val)?);
            return Ok(());
        }
        let Some(current) = self.result.clone() else {
            self.result = Some(val);
            return Ok(());
        };
        let (mut left, mut right) = (current, val);
        match_types(&self.name, &mut left, &mut right)?;
        if !is_binary_fn(&self.name) {
            return Err(QueryError::Invalid(format!(
                "unhandled aggregator function {:?}",
                self.name
            )));
        }
        self.result = Some(apply_binary(&self.name, &left, &right)?);
        Ok(())
    }

    /// Current value (`avg` divides by the count; floats clamped).
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::EmptyAggregation`] without values.
    ///
    /// # Panics
    ///
    /// Never in practice (the result is checked `Some` above).
    pub fn value(&mut self) -> Result<Val> {
        let Some(result) = self.result.clone() else {
            return Err(QueryError::EmptyAggregation);
        };
        if self.name == "avg" && self.count != 0 {
            self.result = Some(divide_by_count(&result, self.count));
        }
        let result = self.result.clone().expect("aggregator result");
        if result.tid == TypeId::Float {
            if let Value::Float(num) = result.value {
                if num.is_infinite() {
                    let clamped = if num.is_sign_positive() {
                        f64::MAX
                    } else {
                        -f64::MAX
                    };
                    let val = Val {
                        tid: TypeId::Float,
                        value: Value::Float(clamped),
                    };
                    self.result = Some(val.clone());
                    return Ok(val);
                }
                if num.is_nan() {
                    let val = float_val(0.0);
                    self.result = Some(val.clone());
                    return Ok(val);
                }
            }
        }
        Ok(result)
    }

    /// Running count (tests).
    #[must_use]
    pub fn count(&self) -> usize {
        self.count
    }
}

/// Divides an `avg` sum by the count.
fn divide_by_count(sum: &Val, count: usize) -> Val {
    match &sum.value {
        Value::BigFloat(num) => Val {
            tid: TypeId::BigFloat,
            value: Value::BigFloat(
                num / BigDecimal::from(u64::try_from(count).unwrap_or(u64::MAX)),
            ),
        },
        Value::Int(num) => float_val(*num as f64 / count as f64),
        Value::Float(num) => float_val(num / count as f64),
        Value::VFloat(vec) => Val {
            tid: TypeId::VFloat,
            value: Value::VFloat(vec.iter().map(|v| v / count as f32).collect()),
        },
        _ => sum.clone(),
    }
}

/// Coerces math operands to a shared type.
fn match_types(name: &str, left: &mut Val, right: &mut Val) -> Result<()> {
    let (left_base, right_base) = (base_type(left), base_type(right));
    let invalid = || invalid_type_error(left_base, right_base, name);

    if right_base == BaseType::VFloat && !allows_vector_on_right(name) {
        return Err(invalid());
    }
    if left_base == right_base {
        if left_base == BaseType::VFloat && mixes_scalar_vector(name) {
            return Err(invalid_type_error(left_base, right_base, name));
        }
        return Ok(());
    }
    if left_base == BaseType::Default || right_base == BaseType::Default {
        return Err(invalid());
    }

    match left_base {
        BaseType::Int => {
            if right_base == BaseType::VFloat && !mixes_scalar_vector(name) {
                return Err(invalid());
            }
            if right_base == BaseType::BigFloat {
                if let Value::Int(num) = left.value {
                    *left = Val {
                        tid: TypeId::BigFloat,
                        value: Value::BigFloat(BigDecimal::from(num)),
                    };
                }
            } else if let Value::Int(num) = left.value {
                *left = float_val(num as f64);
            }
        }
        BaseType::Float => {
            if right_base == BaseType::VFloat && !mixes_scalar_vector(name) {
                return Err(invalid());
            }
            if right_base == BaseType::BigFloat {
                if let Value::Float(num) = left.value {
                    *left = Val {
                        tid: TypeId::BigFloat,
                        value: Value::BigFloat(BigDecimal::from_f64(num).unwrap_or_default()),
                    };
                }
            } else if let Value::Int(num) = right.value {
                *right = float_val(num as f64);
            }
        }
        BaseType::VFloat => {
            if right_base == BaseType::Int {
                if let Value::Int(num) = right.value {
                    *right = float_val(num as f64);
                }
            }
        }
        BaseType::BigFloat => match &right.value {
            Value::Float(num) => {
                *right = Val {
                    tid: TypeId::BigFloat,
                    value: Value::BigFloat(BigDecimal::from_f64(*num).unwrap_or_default()),
                };
            }
            Value::Int(num) => {
                *right = Val {
                    tid: TypeId::BigFloat,
                    value: Value::BigFloat(BigDecimal::from(*num)),
                };
            }
            _ => {}
        },
        BaseType::Default => return Err(invalid()),
    }
    Ok(())
}

/// Converts a task cell into a typed value (empty cells stay empty).
///
/// # Errors
///
/// Returns [`QueryError`] on conversion failure.
pub fn convert_to(cell: &TaskValue) -> Result<Val> {
    let tid = TypeId::try_from(cell.val_type).unwrap_or(TypeId::Default);
    let raw = Val {
        tid: TypeId::Binary,
        value: Value::Binary(cell.val.clone()),
    };
    if cell.val.is_empty() {
        return Ok(raw);
    }
    datastreamdb_types::convert(&raw, tid).map_err(|err| {
        QueryError::Invalid(format!(
            "fail to convert from api.Value to types.Val: {err}"
        ))
    })
}

/// Compares two values with a boolean operator.
///
/// # Errors
///
/// Returns [`QueryError`] on unknown operators or incompatible types.
pub fn compare_values(op: &str, mut left: Val, mut right: Val) -> Result<bool> {
    if !is_binary_boolean_fn(op) {
        return Err(QueryError::Invalid(format!(
            "function {op} is not binary boolean"
        )));
    }
    if less_than(&left, &right).is_err() {
        if left.tid == TypeId::Int {
            if let Value::Int(num) = left.value {
                left = float_val(num as f64);
            }
        } else if right.tid == TypeId::Int {
            if let Value::Int(num) = right.value {
                right = float_val(num as f64);
            }
        } else {
            return Err(QueryError::Invalid(
                "comparison of incompatible types".to_owned(),
            ));
        }
    }
    let is_less = less_than(&left, &right).unwrap_or(false);
    let is_more = less_than(&right, &left).unwrap_or(false);
    let is_equal = equal(&left, &right);
    match op {
        "<" => Ok(is_less),
        ">" => Ok(is_more),
        "<=" => Ok(is_less || is_equal),
        ">=" => Ok(is_more || is_equal),
        "==" => Ok(is_equal),
        "!=" => Ok(!is_equal),
        _ => Err(QueryError::Invalid(format!(
            "invalid compare function {op:?}"
        ))),
    }
}

/// Extracts the aggregation value from a task result row.
///
/// # Errors
///
/// Returns [`QueryError`] on missing cells or conversion failure.
pub fn get_agg_val(result: &datastreamdb_proto::TaskResult, i: usize, j: usize) -> Result<Val> {
    let cell = result
        .value_matrix
        .get(i)
        .and_then(|row| row.values.get(j))
        .ok_or_else(|| QueryError::Invalid("missing aggregation value".to_owned()))?;
    convert_to(cell)
}

/// Fresh aggregator (group-by helper).
#[must_use]
pub fn new_aggregator(name: &str) -> Aggregator {
    Aggregator::new(name)
}

/// Adds two aggregation values (same types, like Go).
fn add_vals(left: &Val, right: &Val) -> Result<Val> {
    match (&left.value, &right.value) {
        (Value::Int(a), Value::Int(b)) => a
            .checked_add(*b)
            .map(|sum| Val {
                tid: TypeId::Int,
                value: Value::Int(sum),
            })
            .ok_or_else(|| QueryError::Invalid("Integer overflow".to_owned())),
        (Value::Float(a), Value::Float(b)) => Ok(float_val(a + b)),
        (Value::VFloat(a), Value::VFloat(b)) => {
            if a.len() != b.len() {
                return Err(QueryError::Invalid(
                    "The length of vectors must match".to_owned(),
                ));
            }
            Ok(Val {
                tid: TypeId::VFloat,
                value: Value::VFloat(a.iter().zip(b.iter()).map(|(x, y)| x + y).collect()),
            })
        }
        (Value::BigFloat(a), Value::BigFloat(b)) => Ok(Val {
            tid: TypeId::BigFloat,
            value: Value::BigFloat(a + b),
        }),
        // Go panics on the unchecked assertion here; we error instead.
        _ => Err(QueryError::Invalid(
            "Left and right arguments must match".to_owned(),
        )),
    }
}

/// Unary math application.
///
/// # Errors
///
/// Returns [`QueryError`] on unknown functions or domain errors.
pub fn apply_unary(name: &str, arg: &Val) -> Result<Val> {
    match name {
        "ln" => apply_ln(arg),
        "exp" => apply_exp(arg),
        "u-" => apply_neg(arg),
        "sqrt" => apply_sqrt(arg),
        "floor" => apply_floor(arg),
        "ceil" => apply_ceil(arg),
        "since" => apply_since(arg),
        _ => Err(QueryError::Invalid(format!(
            "unknown unary function: {name}"
        ))),
    }
}

/// Binary math application.
///
/// # Errors
///
/// Returns [`QueryError`] on unknown functions or domain errors.
pub fn apply_binary(name: &str, left: &Val, right: &Val) -> Result<Val> {
    match name {
        "+" => apply_add(left, right),
        "-" => apply_sub(left, right),
        "*" => apply_mul(left, right),
        "/" => apply_div(left, right),
        "%" => apply_mod(left, right),
        "pow" => apply_pow(left, right),
        "logbase" => apply_log(left, right),
        "min" | "max" => apply_min_max(name, left, right),
        "dot" => apply_dot(left, right),
        _ => Err(QueryError::Invalid(format!(
            "unknown binary function: {name}"
        ))),
    }
}

/// Natural logarithm.
fn apply_ln(arg: &Val) -> Result<Val> {
    let num = to_f64(arg, "ln")?;
    if num <= 0.0 {
        return Err(QueryError::Invalid("Log of negative number".to_owned()));
    }
    Ok(float_val(num.ln()))
}

/// Exponential.
fn apply_exp(arg: &Val) -> Result<Val> {
    Ok(float_val(to_f64(arg, "exp")?.exp()))
}

/// Negation.
fn apply_neg(arg: &Val) -> Result<Val> {
    match &arg.value {
        Value::Int(num) => num
            .checked_neg()
            .map(|neg| Val {
                tid: TypeId::Int,
                value: Value::Int(neg),
            })
            .ok_or_else(|| QueryError::Invalid("Integer overflow".to_owned())),
        Value::Float(num) => Ok(float_val(-num)),
        Value::VFloat(vec) => Ok(Val {
            tid: TypeId::VFloat,
            value: Value::VFloat(vec.iter().map(|v| -v).collect()),
        }),
        Value::BigFloat(num) => Ok(Val {
            tid: TypeId::BigFloat,
            value: Value::BigFloat(-num),
        }),
        _ => Err(wrong_type_error(arg.tid, "u-")),
    }
}

/// Square root.
fn apply_sqrt(arg: &Val) -> Result<Val> {
    match &arg.value {
        Value::Int(num) => {
            if *num < 0 {
                return Err(QueryError::Invalid("Root of negative number".to_owned()));
            }
            Ok(float_val((*num as f64).sqrt()))
        }
        Value::Float(num) => {
            if *num < 0.0 {
                return Err(QueryError::Invalid("Root of negative number".to_owned()));
            }
            Ok(float_val(num.sqrt()))
        }
        Value::BigFloat(num) => num
            .sqrt()
            .map(|root| Val {
                tid: TypeId::BigFloat,
                value: Value::BigFloat(root),
            })
            .ok_or_else(|| QueryError::Invalid("Root of negative number".to_owned())),
        _ => Err(wrong_type_error(arg.tid, "sqrt")),
    }
}

/// Floor.
fn apply_floor(arg: &Val) -> Result<Val> {
    match &arg.value {
        Value::Int(num) => Ok(Val {
            tid: TypeId::Int,
            value: Value::Int(*num),
        }),
        Value::Float(num) => Ok(float_val(num.floor())),
        Value::BigFloat(num) => Ok(Val {
            tid: TypeId::BigFloat,
            value: Value::BigFloat(
                BigDecimal::from_f64(num.to_f64().unwrap_or(0.0).floor()).unwrap_or_default(),
            ),
        }),
        _ => Err(wrong_type_error(arg.tid, "floor")),
    }
}

/// Ceiling (including Go's `fun ceil` typo, kept for message parity).
fn apply_ceil(arg: &Val) -> Result<Val> {
    match &arg.value {
        Value::Int(num) => Ok(Val {
            tid: TypeId::Int,
            value: Value::Int(*num),
        }),
        Value::Float(num) => Ok(float_val(num.ceil())),
        Value::BigFloat(num) => Ok(Val {
            tid: TypeId::BigFloat,
            value: Value::BigFloat(
                BigDecimal::from_f64(num.to_f64().unwrap_or(0.0).ceil()).unwrap_or_default(),
            ),
        }),
        _ => Err(wrong_type_error(arg.tid, "fun ceil")),
    }
}

/// Seconds since a datetime.
fn apply_since(arg: &Val) -> Result<Val> {
    match &arg.value {
        Value::DateTime(when) => {
            let elapsed = chrono::Utc::now().signed_duration_since(*when);
            let seconds =
                elapsed.num_seconds() as f64 + f64::from(elapsed.subsec_nanos()) / 1_000_000_000.0;
            Ok(float_val(seconds))
        }
        _ => Err(wrong_type_error(arg.tid, "since")),
    }
}

/// Addition.
fn apply_add(left: &Val, right: &Val) -> Result<Val> {
    add_vals(left, right)
}

/// Subtraction.
fn apply_sub(left: &Val, right: &Val) -> Result<Val> {
    match base_type(left) {
        BaseType::Int => {
            let (Value::Int(a), Value::Int(b)) = (&left.value, &right.value) else {
                return Err(invalid_type_error(base_type(left), base_type(right), "-"));
            };
            a.checked_sub(*b)
                .map(|diff| Val {
                    tid: TypeId::Int,
                    value: Value::Int(diff),
                })
                .ok_or_else(|| QueryError::Invalid("Integer overflow".to_owned()))
        }
        BaseType::Float => {
            let (Value::Float(a), Value::Float(b)) = (&left.value, &right.value) else {
                return Err(invalid_type_error(base_type(left), base_type(right), "-"));
            };
            Ok(float_val(a - b))
        }
        BaseType::VFloat => {
            let (Value::VFloat(a), Value::VFloat(b)) = (&left.value, &right.value) else {
                return Err(QueryError::Invalid(
                    "Left and right arguments must match".to_owned(),
                ));
            };
            if a.len() != b.len() {
                return Err(QueryError::Invalid(
                    "The length of vectors must match".to_owned(),
                ));
            }
            Ok(Val {
                tid: TypeId::VFloat,
                value: Value::VFloat(a.iter().zip(b.iter()).map(|(x, y)| x - y).collect()),
            })
        }
        BaseType::BigFloat => {
            let (Value::BigFloat(a), Value::BigFloat(b)) = (&left.value, &right.value) else {
                return Err(invalid_type_error(base_type(left), base_type(right), "-"));
            };
            Ok(Val {
                tid: TypeId::BigFloat,
                value: Value::BigFloat(a - b),
            })
        }
        BaseType::Default => Err(wrong_type_error(left.tid, "-")),
    }
}

/// Multiplication (scalars, scalar-vector mixes, big floats).
fn apply_mul(left: &Val, right: &Val) -> Result<Val> {
    let (left_base, right_base) = (base_type(left), base_type(right));
    match left_base {
        BaseType::Int => {
            let (Value::Int(a), Value::Int(b)) = (&left.value, &right.value) else {
                return Err(invalid_type_error(left_base, right_base, "*"));
            };
            a.checked_mul(*b)
                .map(|prod| Val {
                    tid: TypeId::Int,
                    value: Value::Int(prod),
                })
                .ok_or_else(|| QueryError::Invalid("Integer overflow".to_owned()))
        }
        BaseType::Float => {
            let Value::Float(a) = left.value else {
                return Err(invalid_type_error(left_base, right_base, "*"));
            };
            match right_base {
                BaseType::Float => {
                    let Value::Float(b) = right.value else {
                        return Err(invalid_type_error(left_base, right_base, "*"));
                    };
                    Ok(float_val(a * b))
                }
                BaseType::VFloat => {
                    let Value::VFloat(b) = &right.value else {
                        return Err(invalid_type_error(left_base, right_base, "*"));
                    };
                    Ok(Val {
                        tid: TypeId::VFloat,
                        value: Value::VFloat(b.iter().map(|v| a as f32 * v).collect()),
                    })
                }
                _ => Err(invalid_type_error(left_base, right_base, "*")),
            }
        }
        BaseType::VFloat => {
            if right_base != BaseType::Float {
                return Err(invalid_type_error(left_base, right_base, "*"));
            }
            let (Value::VFloat(a), Value::Float(b)) = (&left.value, &right.value) else {
                return Err(invalid_type_error(left_base, right_base, "*"));
            };
            if f64::from(*b as f32).is_infinite() {
                return Err(QueryError::Invalid("Float32 overflow".to_owned()));
            }
            Ok(Val {
                tid: TypeId::VFloat,
                value: Value::VFloat(a.iter().map(|v| v * *b as f32).collect()),
            })
        }
        BaseType::BigFloat => {
            let (Value::BigFloat(a), Value::BigFloat(b)) = (&left.value, &right.value) else {
                return Err(invalid_type_error(left_base, right_base, "*"));
            };
            Ok(Val {
                tid: TypeId::BigFloat,
                value: Value::BigFloat(a * b),
            })
        }
        BaseType::Default => Err(invalid_type_error(left_base, right_base, "*")),
    }
}

/// Division.
fn apply_div(left: &Val, right: &Val) -> Result<Val> {
    match (&left.value, &right.value) {
        (Value::Int(a), Value::Int(b)) => {
            if *b == 0 {
                return Err(QueryError::Invalid("Division by zero".to_owned()));
            }
            a.checked_div(*b)
                .map(|quot| Val {
                    tid: TypeId::Int,
                    value: Value::Int(quot),
                })
                .ok_or_else(|| QueryError::Invalid("Integer overflow".to_owned()))
        }
        (Value::Float(a), Value::Float(b)) => {
            if *b == 0.0 {
                return Err(QueryError::Invalid("Division by zero".to_owned()));
            }
            Ok(float_val(a / b))
        }
        (Value::VFloat(a), Value::Float(b)) => {
            if *b == 0.0 {
                return Err(QueryError::Invalid("Division by zero".to_owned()));
            }
            if f64::from(*b as f32).is_infinite() {
                return Err(QueryError::Invalid("Float32 overflow".to_owned()));
            }
            Ok(Val {
                tid: TypeId::VFloat,
                value: Value::VFloat(a.iter().map(|v| v / *b as f32).collect()),
            })
        }
        (Value::BigFloat(a), Value::BigFloat(b)) => {
            if b.is_zero() {
                return Err(QueryError::Invalid("Division by zero".to_owned()));
            }
            Ok(Val {
                tid: TypeId::BigFloat,
                value: Value::BigFloat(a / b),
            })
        }
        _ => Err(invalid_type_error(base_type(left), base_type(right), "/")),
    }
}

/// Remainder.
fn apply_mod(left: &Val, right: &Val) -> Result<Val> {
    match (&left.value, &right.value) {
        (Value::Int(a), Value::Int(b)) => {
            if *b == 0 {
                return Err(QueryError::Invalid("Division by zero".to_owned()));
            }
            Ok(Val {
                tid: TypeId::Int,
                value: Value::Int(a % b),
            })
        }
        (Value::Float(a), Value::Float(b)) => {
            if *b == 0.0 {
                return Err(QueryError::Invalid("Division by zero".to_owned()));
            }
            Ok(float_val(a % b))
        }
        _ => Err(wrong_type_error(left.tid, "%%")),
    }
}

/// Power (float result, like Go).
fn apply_pow(left: &Val, right: &Val) -> Result<Val> {
    match (&left.value, &right.value) {
        (Value::Int(a), Value::Int(b)) => Ok(float_val((*a as f64).powf(*b as f64))),
        (Value::Float(a), Value::Float(b)) => {
            if *a < 0.0 && (b.ceil() - b).abs() > 0.0 {
                return Err(QueryError::Invalid(
                    "Fractional power of negative number".to_owned(),
                ));
            }
            Ok(float_val(a.powf(*b)))
        }
        _ => Err(wrong_type_error(left.tid, "^")),
    }
}

/// Logarithm of `left` in base `right`.
fn apply_log(left: &Val, right: &Val) -> Result<Val> {
    match (&left.value, &right.value) {
        (Value::Int(a), Value::Int(b)) => {
            if *a < 0 || *b < 0 {
                return Err(QueryError::Invalid("Log of negative number".to_owned()));
            }
            if *b == 1 {
                return Err(QueryError::Invalid("Division by zero".to_owned()));
            }
            Ok(float_val((*a as f64).ln() / (*b as f64).ln()))
        }
        (Value::Float(a), Value::Float(b)) => {
            if *a < 0.0 || *b < 0.0 {
                return Err(QueryError::Invalid("Log of negative number".to_owned()));
            }
            if *b == 1.0 {
                return Err(QueryError::Invalid("Division by zero".to_owned()));
            }
            Ok(float_val(a.ln() / b.ln()))
        }
        _ => Err(wrong_type_error(left.tid, "log")),
    }
}

/// Element-wise min/max (comparison errors propagate, like Go).
fn apply_min_max(name: &str, left: &Val, right: &Val) -> Result<Val> {
    if less_than(left, right)? {
        return Ok(if name == "min" {
            left.clone()
        } else {
            right.clone()
        });
    }
    Ok(if name == "min" {
        right.clone()
    } else {
        left.clone()
    })
}

/// Dot product of two float vectors.
fn apply_dot(left: &Val, right: &Val) -> Result<Val> {
    if base_type(left) != BaseType::VFloat || base_type(right) != BaseType::VFloat {
        return Err(invalid_type_error(base_type(left), base_type(right), "dot"));
    }
    match (&left.value, &right.value) {
        (Value::VFloat(a), Value::VFloat(b)) => {
            if a.len() != b.len() {
                return Err(QueryError::Invalid(
                    "The length of vectors must match".to_owned(),
                ));
            }
            Ok(float_val(
                a.iter()
                    .zip(b.iter())
                    .map(|(x, y)| f64::from(*x) * f64::from(*y))
                    .sum(),
            ))
        }
        _ => Err(invalid_type_error(base_type(left), base_type(right), "dot")),
    }
}

/// Coerces int/float/bigfloat to f64.
fn to_f64(val: &Val, name: &str) -> Result<f64> {
    match &val.value {
        Value::Int(num) => Ok(*num as f64),
        Value::Float(num) => Ok(*num),
        Value::BigFloat(num) => Ok(num.to_f64().unwrap_or(0.0)),
        _ => Err(wrong_type_error(val.tid, name)),
    }
}

/// Float value constructor.
fn float_val(num: f64) -> Val {
    Val {
        tid: TypeId::Float,
        value: Value::Float(num),
    }
}

/// Less-than with int/float coercion (mirrors `compareValues` prep).
fn less_than(
    left: &Val,
    right: &Val,
) -> std::result::Result<bool, datastreamdb_types::compare::CompareError> {
    datastreamdb_types::compare::less_than(left, right)
}
