//! Value conversion matrix. Port of `types/conversion.go`.
//!
//! [`convert`] maps binary-tagged [`Val`]s across types exactly like Go's
//! `Convert`; [`marshal`] maps typed values to string/binary like `Marshal`;
//! [`object_value`] mirrors `ObjectValue` against a local [`ApiValue`].
//!
//! Deltas from Go: datetime binaries use our 12-byte layout instead of Go's
//! 15-byte `time` encoding; bigfloats are decimal instead of binary; and
//! float-to-datetime of out-of-range values errors instead of invoking Go's
//! wraparound.

#![warn(missing_docs)]

use bigdecimal::BigDecimal;
use chrono::{DateTime, FixedOffset, TimeZone};
use num_bigint::ToBigInt as _;
use num_traits::ToPrimitive as _;

use super::scalar::{NANOS_PER_SEC, TypeId, format_datetime, parse_time};
use super::strconv::{format_float_g, parse_go_bool, parse_go_float, parse_go_int};
use super::value::{Val, Value, bytes_as_float_array, float_array_as_bytes, float_array_as_string};

/// Length of our datetime binary encoding (seconds + nanos).
pub const DATETIME_BINARY_LEN: usize = 12;

/// Conversion error. Messages match Go's where tests pin them.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum ConversionError {
    /// Payload was not binary (Go requires `[]byte` input).
    #[error("invalid data to convert to {to}")]
    InvalidData {
        /// Requested target.
        to: TypeId,
    },
    /// Short/foreign int64 payload.
    #[error("invalid data for int64 {0}")]
    InvalidInt(String),
    /// Short/foreign float payload.
    #[error("invalid data for float {0}")]
    InvalidFloat(String),
    /// Bad bool byte.
    #[error("invalid value for bool {0}")]
    InvalidBool(String),
    /// Bad datetime binary (carries Go's prefix for compat).
    #[error("Time.UnmarshalBinary: {0}")]
    InvalidDateTime(String),
    /// Bad vfloat binary length.
    #[error("invalid data for vector of floats: {0}")]
    InvalidVFloat(String),
    /// Datetime out of representable range.
    #[error("datetime out of range: {0}")]
    DateTimeRange(String),
    /// Pair has no conversion.
    #[error("Cannot convert {from} to type {to}")]
    CantConvert {
        /// Source type.
        from: TypeId,
        /// Target type.
        to: TypeId,
    },
    /// Password shorter than 6 chars.
    #[error("Password too short, i.e. should have at least 6 chars")]
    PasswordTooShort,
    /// Float NaN where forbidden.
    #[error("Got invalid value: NaN")]
    Nan,
    /// Float outside `i64` range.
    #[error("Float out of int64 range")]
    FloatRange,
    /// String is not a vfloat list.
    #[error("cannot convert {0} to vfloat")]
    VFloatSyntax(String),
    /// Big-float text or math failure.
    #[error("bigfloat error: {0}")]
    BigFloat(String),
    /// Geo parse/encode failure.
    #[error("geo error: {0}")]
    Geo(String),
    /// Password hashing failure.
    #[error("password error: {0}")]
    Password(String),
    /// `marshal` got a mistyped payload (Go panics here; we error).
    #[error("value of type {got} does not match tag {tid}")]
    MismatchedType {
        /// Declared tag.
        tid: TypeId,
        /// Payload kind found.
        got: &'static str,
    },
    /// JSON cannot represent the float.
    #[error("invalid float for JSON: {0}")]
    JsonFloat(f64),
    /// Type has no JSON mapping.
    #[error("invalid type for JSON output: {0}")]
    JsonType(TypeId),
    /// `object_value` type mismatch (Go `%T`-style message).
    #[error("Expected value of type {expected}. Got{value}")]
    ObjectType {
        /// Expected kind.
        expected: &'static str,
        /// Actual value or type, per Go's per-branch format.
        value: String,
    },
    /// `object_value` has no mapping for the id.
    #[error("ObjectValue not available for: {0}")]
    ObjectUnavailable(i32),
    /// Verbatim Go `strconv` failure (exact message for test parity).
    #[error("{0}")]
    Strconv(String),
}

/// Go `%v`-style byte rendering (`[1 2 3]`).
fn go_bytes(data: &[u8]) -> String {
    let mut out = String::from("[");
    for (i, b) in data.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(&b.to_string());
    }
    out.push(']');
    out
}

/// Parses `[1.0 2.0]` / `[1.0, 2.0]` float lists. Port of `ParseVFloat`.
///
/// # Errors
///
/// Returns [`ConversionError::VFloatSyntax`] on malformed lists.
pub fn parse_v_float(text: &str) -> Result<Vec<f32>, ConversionError> {
    let s = text.replace(['\n', '\t'], " ");
    let s = s.trim();
    // Go reports the processed string in errors.
    let fail = || ConversionError::VFloatSyntax(s.to_owned());
    if s.is_empty() {
        return Ok(Vec::new());
    }
    let Some(inner) = s.strip_prefix('[') else {
        return Err(fail());
    };
    // Go uses TrimRight: every trailing ']' is stripped.
    let trimmed = inner.trim_end_matches(']');
    if trimmed.len() == inner.len() {
        return Err(fail());
    }
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    if trimmed.contains(',') {
        let mut out = Vec::new();
        for part in trimmed.split(',') {
            out.push(parse_v_float_elem(part.trim(), &fail)?);
        }
        return Ok(out);
    }
    let mut out = Vec::new();
    for part in trimmed.split(' ') {
        if part.is_empty() {
            continue;
        }
        out.push(parse_v_float_elem(part, &fail)?);
    }
    Ok(out)
}

/// Parses one vfloat element like Go's `strconv.ParseFloat(s, 32)`.
fn parse_v_float_elem(
    text: &str,
    fail: &dyn Fn() -> ConversionError,
) -> Result<f32, ConversionError> {
    let value = parse_go_float(text).map_err(|_| fail())?;
    // Go bitSize 32 rejects finite literals that overflow f32 (literal
    // infinities and NaN stay valid).
    if value.is_finite() && (value as f32).is_infinite() {
        return Err(fail());
    }
    Ok(value as f32)
}

/// Encodes a datetime into our 12-byte binary layout.
#[must_use]
pub fn datetime_to_binary(value: &DateTime<FixedOffset>) -> Vec<u8> {
    let mut out = Vec::with_capacity(DATETIME_BINARY_LEN);
    out.extend_from_slice(&value.timestamp().to_le_bytes());
    out.extend_from_slice(&value.timestamp_subsec_nanos().to_le_bytes());
    out
}

/// Decodes our 12-byte datetime binary layout.
fn datetime_from_binary(data: &[u8]) -> Result<DateTime<FixedOffset>, ConversionError> {
    if data.is_empty() {
        return Err(ConversionError::InvalidDateTime("no data".to_owned()));
    }
    if data.len() != DATETIME_BINARY_LEN {
        return Err(ConversionError::InvalidDateTime(format!(
            "invalid length {}",
            data.len()
        )));
    }
    let secs = i64::from_le_bytes(data[..8].try_into().expect("8 bytes"));
    let nanos = u32::from_le_bytes(data[8..12].try_into().expect("4 bytes"));
    FixedOffset::east_opt(0)
        .expect("utc")
        .timestamp_opt(secs, nanos)
        .single()
        .ok_or_else(|| ConversionError::DateTimeRange(format!("{secs}s {nanos}ns")))
}

/// Converts a binary-tagged value to `to`, like Go's `Convert`.
/// The input payload must be [`Value::Binary`].
///
/// # Errors
///
/// Returns [`ConversionError`] on malformed payloads and impossible pairs.
pub fn convert(from: &Val, to: TypeId) -> Result<Val, ConversionError> {
    let Value::Binary(data) = &from.value else {
        return Err(ConversionError::InvalidData { to });
    };
    let value = match from.tid {
        TypeId::Binary => convert_from_binary(data, to)?,
        TypeId::String | TypeId::Default => convert_from_string(data, to)?,
        TypeId::Int => convert_from_int(data, to)?,
        TypeId::BigFloat => convert_from_bigfloat(data, to)?,
        TypeId::Float => convert_from_float(data, to)?,
        TypeId::Bool => convert_from_bool(data, to)?,
        TypeId::DateTime => convert_from_datetime(data, to)?,
        TypeId::Geo => convert_from_geo_bytes(data, to)?,
        TypeId::Password => convert_from_password(data, to)?,
        TypeId::VFloat => convert_from_vfloat(data, to)?,
        TypeId::Uid | TypeId::Object | TypeId::Undefined => {
            return Err(ConversionError::CantConvert { from: from.tid, to });
        }
    };
    Ok(Val { tid: to, value })
}

fn convert_from_binary(data: &[u8], to: TypeId) -> Result<Value, ConversionError> {
    match to {
        TypeId::Binary => Ok(Value::Binary(data.to_vec())),
        TypeId::String | TypeId::Default => {
            Ok(Value::String(String::from_utf8_lossy(data).into_owned()))
        }
        TypeId::Int => {
            if data.len() < 8 {
                return Err(ConversionError::InvalidInt(go_bytes(data)));
            }
            Ok(Value::Int(i64::from_le_bytes(
                data[..8].try_into().expect("8 bytes"),
            )))
        }
        TypeId::Float => {
            if data.len() < 8 {
                return Err(ConversionError::InvalidFloat(go_bytes(data)));
            }
            Ok(Value::Float(f64::from_le_bytes(
                data[..8].try_into().expect("8 bytes"),
            )))
        }
        TypeId::BigFloat => Ok(Value::BigFloat(parse_bigfloat(data)?)),
        TypeId::Bool => match data.first() {
            None | Some(0) => Ok(Value::Bool(false)),
            Some(1) => Ok(Value::Bool(true)),
            Some(b) => Err(ConversionError::InvalidBool(b.to_string())),
        },
        TypeId::DateTime => Ok(Value::DateTime(datetime_from_binary(data)?)),
        TypeId::Geo => Ok(Value::Geo(super::geo::geo_from_wkb(data)?)),
        TypeId::Password => Ok(Value::Password(String::from_utf8_lossy(data).into_owned())),
        TypeId::VFloat => {
            if data.len() % 4 != 0 {
                return Err(ConversionError::InvalidVFloat(go_bytes(data)));
            }
            Ok(Value::VFloat(bytes_as_float_array(data)))
        }
        _ => Err(ConversionError::CantConvert {
            from: TypeId::Binary,
            to,
        }),
    }
}

fn convert_from_string(data: &[u8], to: TypeId) -> Result<Value, ConversionError> {
    let text = String::from_utf8_lossy(data);
    let from = TypeId::String;
    match to {
        TypeId::Binary => Ok(Value::Binary(data.to_vec())),
        TypeId::Int => parse_go_int(&text)
            .map(Value::Int)
            .map_err(|e| ConversionError::Strconv(e.to_string())),
        TypeId::Float => {
            let value =
                parse_go_float(&text).map_err(|e| ConversionError::Strconv(e.to_string()))?;
            if value.is_nan() {
                return Err(ConversionError::Nan);
            }
            Ok(Value::Float(value))
        }
        TypeId::BigFloat => Ok(Value::BigFloat(parse_bigfloat(data)?)),
        TypeId::String | TypeId::Default => Ok(Value::String(text.into_owned())),
        TypeId::Bool => parse_go_bool(&text)
            .map(Value::Bool)
            .map_err(|e| ConversionError::Strconv(e.to_string())),
        TypeId::DateTime => {
            Ok(Value::DateTime(parse_time(&text).map_err(|e| {
                ConversionError::InvalidDateTime(e.to_string())
            })?))
        }
        TypeId::Geo => Ok(Value::Geo(super::geo::geo_from_json_text(&text)?)),
        TypeId::Password => {
            let hash = super::password::encrypt(&text).map_err(|e| match e {
                super::password::PasswordError::TooShort => ConversionError::PasswordTooShort,
                other => ConversionError::Password(other.to_string()),
            })?;
            Ok(Value::Password(hash))
        }
        TypeId::VFloat => Ok(Value::VFloat(parse_v_float(&text)?)),
        _ => Err(ConversionError::CantConvert { from, to }),
    }
}

fn int_from_binary(data: &[u8]) -> Result<i64, ConversionError> {
    if data.len() < 8 {
        return Err(ConversionError::InvalidInt(go_bytes(data)));
    }
    Ok(i64::from_le_bytes(data[..8].try_into().expect("8 bytes")))
}

fn float_from_binary(data: &[u8]) -> Result<f64, ConversionError> {
    if data.len() < 8 {
        return Err(ConversionError::InvalidFloat(go_bytes(data)));
    }
    Ok(f64::from_le_bytes(data[..8].try_into().expect("8 bytes")))
}

fn convert_from_int(data: &[u8], to: TypeId) -> Result<Value, ConversionError> {
    let value = int_from_binary(data)?;
    match to {
        TypeId::Int => Ok(Value::Int(value)),
        TypeId::Binary => Ok(Value::Binary(value.to_le_bytes().to_vec())),
        TypeId::Float => Ok(Value::Float(value as f64)),
        TypeId::BigFloat => Ok(Value::BigFloat(BigDecimal::from(value))),
        TypeId::Bool => Ok(Value::Bool(value != 0)),
        TypeId::String | TypeId::Default => Ok(Value::String(value.to_string())),
        TypeId::DateTime => FixedOffset::east_opt(0)
            .expect("utc")
            .timestamp_opt(value, 0)
            .single()
            .map(Value::DateTime)
            .ok_or_else(|| ConversionError::DateTimeRange(value.to_string())),
        TypeId::VFloat => Ok(Value::VFloat(vec![value as f32])),
        _ => Err(ConversionError::CantConvert {
            from: TypeId::Int,
            to,
        }),
    }
}

fn parse_bigfloat(data: &[u8]) -> Result<BigDecimal, ConversionError> {
    let text = String::from_utf8_lossy(data);
    text.parse::<BigDecimal>()
        .map_err(|e| ConversionError::BigFloat(e.to_string()))
}

/// Truncates a big-float toward zero (low 64 bits on overflow, like Go).
/// Shared by conversion and the bigfloat tokenizer.
#[must_use]
pub fn bigfloat_to_i64(value: &BigDecimal) -> i64 {
    use num_bigint::BigInt;
    use num_traits::ToPrimitive;
    // Go's Int64() on overflow is undefined; truncate to low bits, documented.
    if let Some(int) = value.to_bigint().and_then(|i: BigInt| i.to_i64()) {
        return int;
    }
    value.to_bigint().map_or(0, |i: BigInt| {
        use num_traits::Signed as _;
        let bytes = i.to_signed_bytes_le();
        let mut buf = [0u8; 8];
        let n = bytes.len().min(8);
        buf[..n].copy_from_slice(&bytes[..n]);
        // Sign-extend negatives.
        if i.is_negative() {
            for b in buf.iter_mut().skip(n) {
                *b = 0xFF;
            }
        }
        i64::from_le_bytes(buf)
    })
}

fn bigfloat_to_f64(value: &BigDecimal) -> f64 {
    use num_traits::Signed as _;
    value.to_f64().unwrap_or(if value.is_negative() {
        f64::NEG_INFINITY
    } else {
        f64::INFINITY
    })
}

fn convert_from_bigfloat(data: &[u8], to: TypeId) -> Result<Value, ConversionError> {
    let value = parse_bigfloat(data)?;
    match to {
        TypeId::BigFloat => Ok(Value::BigFloat(value.clone())),
        TypeId::Float => Ok(Value::Float(bigfloat_to_f64(&value))),
        TypeId::Binary => Ok(Value::Binary(value.to_string().into_bytes())),
        TypeId::Int => Ok(Value::Int(bigfloat_to_i64(&value))),
        TypeId::Bool => {
            use num_traits::Zero as _;
            Ok(Value::Bool(!value.is_zero()))
        }
        TypeId::String | TypeId::Default => Ok(Value::String(value.to_string())),
        TypeId::DateTime => {
            let secs = bigfloat_to_i64(&value);
            let frac = bigfloat_to_f64(&value) - secs as f64;
            #[allow(clippy::cast_possible_truncation)]
            let nanos = (frac * NANOS_PER_SEC) as i64;
            let (secs, nanos) = normalize_nanos(secs, nanos);
            FixedOffset::east_opt(0)
                .expect("utc")
                .timestamp_opt(secs, nanos)
                .single()
                .map(Value::DateTime)
                .ok_or_else(|| ConversionError::DateTimeRange(value.to_string()))
        }
        _ => Err(ConversionError::CantConvert {
            from: TypeId::BigFloat,
            to,
        }),
    }
}

fn convert_from_float(data: &[u8], to: TypeId) -> Result<Value, ConversionError> {
    let value = float_from_binary(data)?;
    match to {
        TypeId::Float => Ok(Value::Float(value)),
        TypeId::BigFloat => Ok(Value::BigFloat(
            BigDecimal::try_from(value).map_err(|e| ConversionError::BigFloat(e.to_string()))?,
        )),
        TypeId::Binary => Ok(Value::Binary(value.to_le_bytes().to_vec())),
        TypeId::Int => {
            if value > i64::MAX as f64 || value < i64::MIN as f64 || value.is_nan() {
                return Err(ConversionError::FloatRange);
            }
            Ok(Value::Int(value as i64))
        }
        TypeId::Bool => Ok(Value::Bool(value != 0.0)),
        TypeId::String | TypeId::Default => Ok(Value::String(format_float_g(value))),
        TypeId::DateTime => {
            if !(-9_223_372_036_854_776e18..9_223_372_036_854_776e18).contains(&value) {
                return Err(ConversionError::FloatRange);
            }
            #[allow(clippy::cast_possible_truncation)]
            let secs = value.trunc() as i64;
            #[allow(clippy::cast_possible_truncation)]
            let nanos = ((value - secs as f64) * NANOS_PER_SEC) as i64;
            let (secs, nanos) = normalize_nanos(secs, nanos);
            FixedOffset::east_opt(0)
                .expect("utc")
                .timestamp_opt(secs, nanos)
                .single()
                .map(Value::DateTime)
                .ok_or(ConversionError::FloatRange)
        }
        TypeId::VFloat => Ok(Value::VFloat(vec![value as f32])),
        _ => Err(ConversionError::CantConvert {
            from: TypeId::Float,
            to,
        }),
    }
}

/// Normalizes possibly-negative nanos into `(secs, 0..1e9 nanos)`,
/// mirroring Go's `time.Unix` normalization.
fn normalize_nanos(secs: i64, nanos: i64) -> (i64, u32) {
    if nanos < 0 {
        (secs - 1, (1_000_000_000 + nanos) as u32)
    } else {
        (secs, nanos as u32)
    }
}

fn convert_from_bool(data: &[u8], to: TypeId) -> Result<Value, ConversionError> {
    let byte = match data.first() {
        Some(0) => false,
        Some(1) => true,
        _ => return Err(ConversionError::InvalidBool(go_bytes(data))),
    };
    match to {
        TypeId::Bool => Ok(Value::Bool(byte)),
        TypeId::Binary => Ok(Value::Binary(vec![u8::from(byte)])),
        TypeId::Int => Ok(Value::Int(i64::from(byte))),
        TypeId::Float => Ok(Value::Float(f64::from(u8::from(byte)))),
        TypeId::VFloat => Ok(Value::VFloat(vec![f32::from(u8::from(byte))])),
        TypeId::BigFloat => Ok(Value::BigFloat(BigDecimal::from(u8::from(byte)))),
        TypeId::String | TypeId::Default => Ok(Value::String(byte.to_string())),
        _ => Err(ConversionError::CantConvert {
            from: TypeId::Bool,
            to,
        }),
    }
}

fn convert_from_datetime(data: &[u8], to: TypeId) -> Result<Value, ConversionError> {
    let value = datetime_from_binary(data)?;
    match to {
        TypeId::DateTime => Ok(Value::DateTime(value)),
        TypeId::Binary => Ok(Value::Binary(datetime_to_binary(&value))),
        TypeId::String | TypeId::Default => Ok(Value::String(format_datetime(&value))),
        TypeId::Int => Ok(Value::Int(value.timestamp())),
        TypeId::Float => {
            // Correct seconds+fraction math (Go's UnixNano wraps outside
            // 1678-2262; we do not replicate that undefined behavior).
            Ok(Value::Float(
                value.timestamp() as f64
                    + f64::from(value.timestamp_subsec_nanos()) / NANOS_PER_SEC,
            ))
        }
        TypeId::BigFloat => {
            let nanos = BigDecimal::from(value.timestamp()) * BigDecimal::from(1_000_000_000)
                + BigDecimal::from(value.timestamp_subsec_nanos());
            Ok(Value::BigFloat(nanos / BigDecimal::from(1_000_000_000)))
        }
        _ => Err(ConversionError::CantConvert {
            from: TypeId::DateTime,
            to,
        }),
    }
}

fn convert_from_geo_bytes(data: &[u8], to: TypeId) -> Result<Value, ConversionError> {
    let geom = super::geo::geo_from_wkb(data)?;
    convert_from_geo_value(&geom, to, TypeId::Geo)
}

fn convert_from_geo_value(
    geom: &geojson::GeometryValue,
    to: TypeId,
    from: TypeId,
) -> Result<Value, ConversionError> {
    match to {
        TypeId::Geo => Ok(Value::Geo(geom.clone())),
        TypeId::Binary => Ok(Value::Binary(super::geo::geo_to_wkb(geom))),
        TypeId::String | TypeId::Default => {
            Ok(Value::String(super::geo::geo_to_single_quoted_json(geom)))
        }
        _ => Err(ConversionError::CantConvert { from, to }),
    }
}

fn convert_from_password(data: &[u8], to: TypeId) -> Result<Value, ConversionError> {
    let text = String::from_utf8_lossy(data).into_owned();
    match to {
        TypeId::Binary => Ok(Value::Binary(text.into_bytes())),
        TypeId::String => Ok(Value::String(text)),
        TypeId::Password => Ok(Value::Password(text)),
        _ => Err(ConversionError::CantConvert {
            from: TypeId::Password,
            to,
        }),
    }
}

fn convert_from_vfloat(data: &[u8], to: TypeId) -> Result<Value, ConversionError> {
    match to {
        TypeId::Binary => Ok(Value::Binary(data.to_vec())),
        TypeId::VFloat => Ok(Value::VFloat(bytes_as_float_array(data))),
        TypeId::String => Ok(Value::String(float_array_as_string(&bytes_as_float_array(
            data,
        )))),
        _ => Err(ConversionError::CantConvert {
            from: TypeId::VFloat,
            to,
        }),
    }
}

/// Marshals a typed value to string/binary/default, like Go's `Marshal`.
///
/// # Errors
///
/// Returns [`ConversionError`] on tag/payload mismatch or impossible targets.
pub fn marshal(from: &Val, to: TypeId) -> Result<Val, ConversionError> {
    let mismatch = || ConversionError::MismatchedType {
        tid: from.tid,
        got: Val::type_name(&from.value),
    };
    let value = match (&from.tid, &from.value) {
        (TypeId::Binary, Value::Binary(v)) => match to {
            TypeId::String | TypeId::Default => {
                Value::String(String::from_utf8_lossy(v).into_owned())
            }
            TypeId::Binary => Value::Binary(v.clone()),
            _ => return Err(ConversionError::CantConvert { from: from.tid, to }),
        },
        (TypeId::String | TypeId::Default, Value::String(v)) => match to {
            TypeId::String | TypeId::Default => Value::String(v.clone()),
            TypeId::Binary => Value::Binary(v.as_bytes().to_vec()),
            _ => return Err(ConversionError::CantConvert { from: from.tid, to }),
        },
        (TypeId::Int, Value::Int(v)) => match to {
            TypeId::String | TypeId::Default => Value::String(v.to_string()),
            TypeId::Binary => Value::Binary(v.to_le_bytes().to_vec()),
            _ => return Err(ConversionError::CantConvert { from: from.tid, to }),
        },
        (TypeId::Float, Value::Float(v)) => match to {
            TypeId::String | TypeId::Default => Value::String(format_float_g(*v)),
            TypeId::Binary => Value::Binary(v.to_le_bytes().to_vec()),
            _ => return Err(ConversionError::CantConvert { from: from.tid, to }),
        },
        (TypeId::BigFloat, Value::BigFloat(v)) => match to {
            TypeId::String | TypeId::Default => Value::String(v.to_string()),
            TypeId::Binary => Value::Binary(v.to_string().into_bytes()),
            _ => return Err(ConversionError::CantConvert { from: from.tid, to }),
        },
        (TypeId::Bool, Value::Bool(v)) => match to {
            TypeId::String | TypeId::Default => Value::String(v.to_string()),
            TypeId::Binary => Value::Binary(vec![u8::from(*v)]),
            _ => return Err(ConversionError::CantConvert { from: from.tid, to }),
        },
        (TypeId::DateTime, Value::DateTime(v)) => match to {
            TypeId::String | TypeId::Default => Value::String(format_datetime(v)),
            TypeId::Binary => Value::Binary(datetime_to_binary(v)),
            _ => return Err(ConversionError::CantConvert { from: from.tid, to }),
        },
        (TypeId::Geo, Value::Geo(v)) => match to {
            TypeId::Binary => Value::Binary(super::geo::geo_to_wkb(v)),
            TypeId::String | TypeId::Default => {
                Value::String(super::geo::geo_to_single_quoted_json(v))
            }
            _ => return Err(ConversionError::CantConvert { from: from.tid, to }),
        },
        (TypeId::Password, Value::Password(v)) => match to {
            TypeId::String => Value::String(v.clone()),
            TypeId::Binary => Value::Binary(v.as_bytes().to_vec()),
            _ => return Err(ConversionError::CantConvert { from: from.tid, to }),
        },
        (TypeId::VFloat, Value::VFloat(v)) => match to {
            TypeId::Binary => Value::Binary(float_array_as_bytes(v)),
            TypeId::String => Value::String(float_array_as_string(v)),
            _ => return Err(ConversionError::CantConvert { from: from.tid, to }),
        },
        _ => return Err(mismatch()),
    };
    Ok(Val { tid: to, value })
}

/// API value mirror for [`object_value`] (Go `api.Value` oneof subset).
#[derive(Clone, Debug, PartialEq)]
pub enum ApiValue {
    /// String cell.
    Str(String),
    /// Default-typed string cell.
    Default(String),
    /// Integer cell.
    Int(i64),
    /// Double cell.
    Double(f64),
    /// Boolean cell.
    Bool(bool),
    /// Bytes cell.
    Bytes(Vec<u8>),
    /// Big-float binary cell.
    Bigfloat(Vec<u8>),
    /// Geo binary cell.
    Geo(Vec<u8>),
    /// Datetime binary cell.
    Datetime(Vec<u8>),
    /// Password cell.
    Password(String),
    /// Float-vector binary cell.
    Vfloat(Vec<u8>),
}

/// Converts a typed value into an [`ApiValue`], like Go's `ObjectValue`.
/// Password errors name the type but never the value.
///
/// # Errors
///
/// Returns [`ConversionError`] on tag/payload mismatch.
pub fn object_value(id: TypeId, value: &Value) -> Result<ApiValue, ConversionError> {
    let binary_of = |tid: TypeId| -> Result<Vec<u8>, ConversionError> {
        match marshal(
            &Val {
                tid,
                value: value.clone(),
            },
            TypeId::Binary,
        )?
        .value
        {
            Value::Binary(bytes) => Ok(bytes),
            _ => Err(ConversionError::MismatchedType {
                tid,
                got: Val::type_name(value),
            }),
        }
    };
    match (id, value) {
        (TypeId::String, Value::String(v)) => Ok(ApiValue::Str(v.clone())),
        (TypeId::Default, Value::String(v)) => Ok(ApiValue::Default(v.clone())),
        (TypeId::Int, Value::Int(v)) => Ok(ApiValue::Int(*v)),
        (TypeId::Float, Value::Float(v)) => Ok(ApiValue::Double(*v)),
        (TypeId::Bool, Value::Bool(v)) => Ok(ApiValue::Bool(*v)),
        (TypeId::Binary, Value::Binary(v)) => Ok(ApiValue::Bytes(v.clone())),
        (TypeId::BigFloat, _) => Ok(ApiValue::Bigfloat(binary_of(TypeId::BigFloat)?)),
        (TypeId::Geo, _) => Ok(ApiValue::Geo(binary_of(TypeId::Geo)?)),
        (TypeId::DateTime, _) => Ok(ApiValue::Datetime(binary_of(TypeId::DateTime)?)),
        (TypeId::Password, Value::Password(v)) => Ok(ApiValue::Password(v.clone())),
        (TypeId::Password, other) => Err(ConversionError::ObjectType {
            expected: "password",
            value: format!(". Got type: {}", Val::type_name(other)),
        }),
        (TypeId::VFloat, _) => Ok(ApiValue::Vfloat(binary_of(TypeId::VFloat)?)),
        (TypeId::String | TypeId::Default, other) => Err(ConversionError::ObjectType {
            expected: "string",
            value: format!(". Got : {}", go_value_fmt(other)),
        }),
        (TypeId::Int, other) => Err(ConversionError::ObjectType {
            expected: "int64",
            value: format!(". Got : {}", go_value_fmt(other)),
        }),
        (TypeId::Float, other) => Err(ConversionError::ObjectType {
            expected: "float64",
            value: format!(". Got : {}", go_value_fmt(other)),
        }),
        (TypeId::Bool, other) => Err(ConversionError::ObjectType {
            expected: "bool",
            value: format!(". Got : {}", go_value_fmt(other)),
        }),
        (TypeId::Binary, other) => Err(ConversionError::ObjectType {
            expected: "[]byte",
            value: format!(". Got : {}", go_value_fmt(other)),
        }),
        (other, _) => Err(ConversionError::ObjectUnavailable(other as i32)),
    }
}

/// Go `%v` rendering for error messages.
fn go_value_fmt(value: &Value) -> String {
    match value {
        Value::Null => "<nil>".to_owned(),
        Value::Binary(v) => go_bytes(v),
        Value::Int(v) => v.to_string(),
        Value::Float(v) => format_float_g(*v),
        Value::Bool(v) => v.to_string(),
        Value::String(v) | Value::Password(v) => v.clone(),
        Value::DateTime(v) => format_datetime(v),
        Value::BigFloat(v) => v.to_string(),
        Value::Geo(_) => "<geom>".to_owned(),
        Value::Uid(v) => v.to_string(),
        Value::VFloat(v) => float_array_as_string(v),
    }
}

#[cfg(test)]
#[path = "conversion_tests.rs"]
mod tests;
