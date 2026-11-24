//! Value comparison: ordering, equality, and filter operators.
//! Port of `types/compare.go` plus `Less`/`Equal` from `sort.go`.

#![warn(missing_docs)]

use super::scalar::TypeId;
use super::value::{Val, Value};

/// Comparison error (mirrors Go's messages).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CompareError {
    /// Operands have different tags.
    #[error("Arguments of different type can not be compared.")]
    MismatchedTypes,
    /// Tag has no ordering.
    #[error("Compare not supported for type: {0}")]
    NoOrder(TypeId),
    /// Tag has no equality.
    #[error("Equal not supported for type: {0}")]
    NoEqual(TypeId),
    /// Unknown filter operator (Go `Fatalf`s; we error).
    #[error("Unknown comparison operator: {0:?}")]
    UnknownOp(String),
}

/// Strict less-than, like Go's `Less` (same tag required).
///
/// # Errors
///
/// Returns [`CompareError`] on tag mismatch or unordered types.
pub fn less_than(a: &Val, b: &Val) -> Result<bool, CompareError> {
    if a.tid != b.tid {
        return Err(CompareError::MismatchedTypes);
    }
    match a.tid {
        TypeId::DateTime
        | TypeId::Uid
        | TypeId::Int
        | TypeId::Float
        | TypeId::String
        | TypeId::Default
        | TypeId::BigFloat => Ok(less_inner(a, b)),
        _ => Err(CompareError::NoOrder(a.tid)),
    }
}

/// Equality, like Go's `Equal` (same tag required).
///
/// # Errors
///
/// Returns [`CompareError`] on tag mismatch or non-comparable types.
pub fn equal_checked(a: &Val, b: &Val) -> Result<bool, CompareError> {
    if a.tid != b.tid {
        return Err(CompareError::MismatchedTypes);
    }
    match a.tid {
        TypeId::DateTime
        | TypeId::Int
        | TypeId::Float
        | TypeId::String
        | TypeId::Default
        | TypeId::Bool
        | TypeId::BigFloat => Ok(equal(a, b)),
        _ => Err(CompareError::NoEqual(a.tid)),
    }
}

/// Unchecked less-than for sort/filter paths (handles mixed tags).
/// `Null` and unsupported same-tag pairs compare as not-less.
#[must_use]
pub fn less_inner(a: &Val, b: &Val) -> bool {
    if a.tid != b.tid {
        return mismatched_less(a, b);
    }
    match (&a.value, &b.value) {
        (Value::DateTime(x), Value::DateTime(y)) => x < y,
        (Value::Int(x), Value::Int(y)) => x < y,
        (Value::Float(x), Value::Float(y)) => x < y,
        (Value::Uid(x), Value::Uid(y)) => x < y,
        (Value::String(x), Value::String(y)) => x < y,
        (Value::BigFloat(x), Value::BigFloat(y)) => x < y,
        _ => false,
    }
}

/// Cross-tag ordering: int/float mix numerically, anything else by tag id.
/// Port of `mismatchedLess`, including its >2^53 approximation caveat.
#[must_use]
pub fn mismatched_less(a: &Val, b: &Val) -> bool {
    let numeric = |v: &Val| matches!(v.tid, TypeId::Int | TypeId::Float);
    if !numeric(a) || !numeric(b) {
        return (a.tid as i32) < (b.tid as i32);
    }
    let num = |v: &Val| match &v.value {
        Value::Int(i) => *i as f64,
        Value::Float(f) => *f,
        _ => 0.0,
    };
    num(a) < num(b)
}

/// Equality across payloads; tag mismatch or `Null` is never equal.
/// Port of `equal` (comma-ok semantics: mistyped payloads compare false).
#[allow(clippy::float_cmp, reason = "Go compares floats with ==")]
#[must_use]
pub fn equal(a: &Val, b: &Val) -> bool {
    if a.tid != b.tid {
        return false;
    }
    match (&a.value, &b.value) {
        (Value::DateTime(x), Value::DateTime(y)) => x == y,
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::BigFloat(x), Value::BigFloat(y)) => x == y,
        _ => false,
    }
}

/// Filter comparison `op ∈ {ge, gt, le, lt, eq}` of `arg1` against `arg2`.
/// Port of `CompareVals`; errors collapse to `false` like Go's `noError`.
///
/// # Errors
///
/// Returns [`CompareError::UnknownOp`] for other operators (Go `Fatalf`s).
pub fn compare_vals(op: &str, arg1: &Val, arg2: &Val) -> Result<bool, CompareError> {
    let ok = |result: Result<bool, CompareError>| result.unwrap_or(false);
    match op {
        "ge" => Ok(!ok(less_than(arg1, arg2))),
        "gt" => Ok(ok(less_than(arg2, arg1))),
        "le" => Ok(!ok(less_than(arg2, arg1))),
        "lt" => Ok(ok(less_than(arg1, arg2))),
        "eq" => Ok(ok(equal_checked(arg1, arg2))),
        _ => Err(CompareError::UnknownOp(op.to_owned())),
    }
}

/// Whether `dst` lies within `[val1, val2]` (inclusive). Port of `CompareBetween`.
#[must_use]
pub fn compare_between(dst: &Val, val1: &Val, val2: &Val) -> bool {
    compare_vals("ge", dst, val1).unwrap_or(false) && compare_vals("le", dst, val2).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int(v: i64) -> Val {
        Val {
            tid: TypeId::Int,
            value: Value::Int(v),
        }
    }

    fn float(v: f64) -> Val {
        Val {
            tid: TypeId::Float,
            value: Value::Float(v),
        }
    }

    #[test]
    fn equal_vectors() {
        // Port of TestEqual.
        assert!(equal(&int(3), &int(3)));
        assert!(!equal(&int(3), &int(4)));
        assert!(!equal(&int(3), &float(3.0)));
        let null = Val {
            tid: TypeId::Int,
            value: Value::Null,
        };
        assert!(!equal(&int(3), &null));
        assert!(!equal(&null, &int(3)));
        assert!(!equal(&null, &null));
        let mistyped = Val {
            tid: TypeId::Int,
            value: Value::Float(3.0),
        };
        assert!(!equal(&mistyped, &float(3.0)));
        assert!(!equal(&float(3.0), &mistyped));
    }

    #[test]
    fn operators_behave() {
        assert_eq!(compare_vals("ge", &int(3), &int(3)), Ok(true));
        assert_eq!(compare_vals("gt", &int(3), &int(3)), Ok(false));
        assert_eq!(compare_vals("le", &int(2), &int(3)), Ok(true));
        assert_eq!(compare_vals("lt", &int(2), &int(3)), Ok(true));
        assert_eq!(compare_vals("eq", &int(3), &int(3)), Ok(true));
        // Errors collapse to false, except unknown operators.
        assert_eq!(compare_vals("lt", &int(3), &float(3.0)), Ok(false));
        assert!(compare_vals("ne", &int(3), &int(3)).is_err());
        assert!(compare_between(&int(2), &int(1), &int(3)));
        assert!(!compare_between(&int(4), &int(1), &int(3)));
    }

    #[test]
    fn checked_entry_points() {
        assert_eq!(less_than(&int(1), &int(2)), Ok(true));
        assert!(less_than(&int(1), &float(2.0)).is_err());
        assert!(
            less_than(
                &Val {
                    tid: TypeId::Bool,
                    value: Value::Bool(true)
                },
                &Val {
                    tid: TypeId::Bool,
                    value: Value::Bool(false)
                },
            )
            .is_err()
        );
        assert_eq!(equal_checked(&int(1), &int(1)), Ok(true));
    }
}
