//! Typed values: [`Val`], zero values, type guessing, float vectors.
//! Port of `types/value.go` plus `Val`/`ValueForType` from `scalar_types.go`.

#![warn(missing_docs)]

use bigdecimal::BigDecimal;
use chrono::{DateTime, FixedOffset, TimeZone as _};

use super::scalar::{TypeId, parse_time};

/// A value with its type id. `Value::Null` mirrors Go's nil `Value`.
#[derive(Clone, Debug, PartialEq)]
pub struct Val {
    /// Type tag.
    pub tid: TypeId,
    /// Payload (see [`Value`]).
    pub value: Value,
}

/// Typed payload. Binary encodings: int/float are 8-byte LE, bool is one byte
/// (`0`/`1`), datetime is 12 bytes (LE `i64` seconds + LE `u32` nanos),
/// vfloat is LE `f32`s, geo is WKB.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// Missing value (Go nil).
    Null,
    /// Raw bytes.
    Binary(Vec<u8>),
    /// 64-bit integer.
    Int(i64),
    /// 64-bit float.
    Float(f64),
    /// Boolean.
    Bool(bool),
    /// UTF-8 string.
    String(String),
    /// Timestamp (offset-preserving).
    DateTime(DateTime<FixedOffset>),
    /// Arbitrary precision decimal (see precision note in `conversion`).
    BigFloat(BigDecimal),
    /// `GeoJSON` geometry.
    Geo(geojson::GeometryValue),
    /// Hashed password (opaque string).
    Password(String),
    /// UID reference.
    Uid(u64),
    /// Float vector.
    VFloat(Vec<f32>),
}

impl Val {
    /// Zero value for a type id (Go `ValueForType`, owned).
    ///
    /// # Panics
    ///
    /// Never panics; the year-1 date construction is infallible.
    #[must_use]
    pub fn zero(tid: TypeId) -> Self {
        let value = match tid {
            TypeId::Binary => Value::Binary(Vec::new()),
            TypeId::Int => Value::Int(0),
            TypeId::Float => Value::Float(0.0),
            TypeId::Bool => Value::Bool(false),
            TypeId::DateTime => Value::DateTime(
                FixedOffset::east_opt(0)
                    .expect("utc")
                    .with_ymd_and_hms(1, 1, 1, 0, 0, 0)
                    .single()
                    .expect("year 1"),
            ),
            TypeId::BigFloat => Value::BigFloat(BigDecimal::from(0)),
            TypeId::String | TypeId::Default | TypeId::Object => Value::String(String::new()),
            TypeId::Geo => Value::Geo(geojson::GeometryValue::GeometryCollection {
                geometries: Vec::new(),
            }),
            TypeId::Uid => Value::Uid(0),
            TypeId::Password => Value::Password(String::new()),
            TypeId::VFloat => Value::VFloat(Vec::new()),
            TypeId::Undefined => Value::Null,
        };
        Self { tid, value }
    }

    /// Payload with `Null` replaced by the zero value (Go `Val.Safe`).
    #[must_use]
    pub fn safe(&self) -> Value {
        if matches!(self.value, Value::Null) {
            return Self::zero(self.tid).value;
        }
        self.value.clone()
    }

    /// Short Go `%T`-style name for error messages.
    #[must_use]
    pub fn type_name(value: &Value) -> &'static str {
        match value {
            Value::Null => "nil",
            Value::Binary(_) => "[]byte",
            Value::Int(_) => "int64",
            Value::Float(_) => "float64",
            Value::Bool(_) => "bool",
            Value::String(_) | Value::Password(_) => "string",
            Value::DateTime(_) => "time.Time",
            Value::BigFloat(_) => "big.Float",
            Value::Geo(_) => "geom.T",
            Value::Uid(_) => "uint64",
            Value::VFloat(_) => "[]float32",
        }
    }
}

/// Guesses the type (and parsed value) of schemaless bytes.
/// Port of `TypeForValue`; unparsed values come back as [`Value::Null`].
#[must_use]
pub fn type_for_value(bytes: &[u8]) -> (TypeId, Value) {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return (TypeId::Default, Value::Null);
    };
    if text.is_empty() {
        return (TypeId::Default, Value::Null);
    }
    let first = text.as_bytes()[0];
    if matches!(first, b't' | b'T' | b'f' | b'F') {
        return match text {
            "true" | "TRUE" | "True" => (TypeId::Bool, Value::Bool(true)),
            "false" | "FALSE" | "False" => (TypeId::Bool, Value::Bool(false)),
            _ => (TypeId::Default, Value::Null),
        };
    }
    if check_datetime(text) {
        if let Ok(time) = parse_time(text) {
            return (TypeId::DateTime, Value::DateTime(time));
        }
    }
    if check_int(text) {
        if let Ok(int) = super::strconv::parse_go_int(text) {
            return (TypeId::Int, Value::Int(int));
        }
    }
    if check_float(text) {
        if let Ok(float) = super::strconv::parse_go_float(text) {
            return (TypeId::Float, Value::Float(float));
        }
    }
    (TypeId::Default, Value::Null)
}

fn is_sign(byte: u8) -> bool {
    matches!(byte, b'-' | b'+')
}

fn is_digit(byte: u8) -> bool {
    byte.is_ascii_digit()
}

fn check_int(s: &str) -> bool {
    let bytes = s.as_bytes();
    let rest = if is_sign(bytes[0]) && bytes.len() > 1 {
        &bytes[1..]
    } else {
        bytes
    };
    !rest.is_empty()
        && is_digit(rest[0])
        && !rest[1..].iter().any(|&b| matches!(b, b'.' | b'E' | b'e'))
}

fn check_float(s: &str) -> bool {
    let bytes = s.as_bytes();
    let rest = if is_sign(bytes[0]) && bytes.len() > 1 {
        &bytes[1..]
    } else {
        bytes
    };
    if rest.is_empty() {
        return false;
    }
    if rest[0] == b'.' && rest.len() > 1 {
        return is_digit(rest[1]);
    }
    is_digit(rest[0]) && rest[1..].iter().any(|&b| matches!(b, b'.' | b'E' | b'e'))
}

fn check_datetime(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() >= 5
        && is_digit(bytes[0])
        && is_digit(bytes[1])
        && is_digit(bytes[2])
        && is_digit(bytes[3])
        && bytes[4] == b'-'
}

/// Decodes LE bytes into `f32`s, ignoring a trailing partial word.
/// Port of `BytesAsFloatArray`.
#[must_use]
pub fn bytes_as_float_array(encoded: &[u8]) -> Vec<f32> {
    encoded
        .chunks_exact(4)
        .map(|w| f32::from_le_bytes([w[0], w[1], w[2], w[3]]))
        .collect()
}

/// Encodes `f32`s as LE bytes. Port of `FloatArrayAsBytes`.
#[must_use]
pub fn float_array_as_bytes(values: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 * values.len());
    for &v in values {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// Renders `[1,2]` style (Go `%f` shortest per element, comma-joined).
/// Port of `FloatArrayAsString`.
#[must_use]
pub fn float_array_as_string(values: &[f32]) -> String {
    let mut out = String::from("[");
    for (i, &v) in values.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&super::strconv::format_float_f32(v));
    }
    out.push(']');
    out
}

/// JSON text for a value (Go `Val.MarshalJSON`): bigfloat renders unquoted.
///
/// # Errors
///
/// Returns [`ConversionError`](super::conversion::ConversionError) for
/// mistyped payloads and unmappable types.
///
/// # Panics
///
/// Never panics; string quoting is infallible.
pub fn marshal_json(value: &Val) -> Result<String, super::conversion::ConversionError> {
    use super::conversion::ConversionError;
    match (&value.tid, &value.value) {
        (TypeId::Int, Value::Int(v)) => Ok(v.to_string()),
        (TypeId::Bool, Value::Bool(v)) => Ok(v.to_string()),
        (TypeId::Float, Value::Float(v)) => {
            serde_json::to_string(v).map_err(|_| ConversionError::JsonFloat(*v))
        }
        (TypeId::DateTime, Value::DateTime(v)) => {
            Ok(format!("{:?}", super::scalar::format_datetime(v)))
        }
        (TypeId::Geo, Value::Geo(v)) => Ok(super::geo::geo_to_json(v)),
        (TypeId::BigFloat, Value::BigFloat(v)) => Ok(v.to_string()),
        (TypeId::String | TypeId::Default, _) => {
            if let Value::String(s) = value.safe() {
                Ok(serde_json::to_string(&s).expect("quote string"))
            } else {
                Err(ConversionError::MismatchedType {
                    tid: value.tid,
                    got: Val::type_name(&value.value),
                })
            }
        }
        (TypeId::Password, Value::Password(v)) => {
            Ok(serde_json::to_string(&v).expect("quote string"))
        }
        _ => Err(ConversionError::JsonType(value.tid)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guesses_types() {
        // Port of TestTypeForValue (ids only; values spot-checked below).
        for (input, expected) in [
            ("true", TypeId::Bool),
            ("TRUE", TypeId::Bool),
            ("True", TypeId::Bool),
            ("t", TypeId::Default),
            ("false", TypeId::Bool),
            ("FALSE", TypeId::Bool),
            ("False", TypeId::Bool),
            ("f", TypeId::Default),
            ("2018", TypeId::Int),
            ("2018-10", TypeId::DateTime),
            ("2018-10-03", TypeId::DateTime),
            ("2018-10-03T20:47:53Z", TypeId::DateTime),
            ("123", TypeId::Int),
            ("-123", TypeId::Int),
            ("+123", TypeId::Int),
            ("0001", TypeId::Int),
            ("+0", TypeId::Int),
            ("-0", TypeId::Int),
            ("1World", TypeId::Default),
            ("3.14159", TypeId::Float),
            ("-273.15", TypeId::Float),
            ("2.99792e8", TypeId::Float),
            ("9.1095E-28", TypeId::Float),
            ("-.0", TypeId::Float),
            ("+.0", TypeId::Float),
            (".1", TypeId::Float),
            ("1.", TypeId::Float),
            ("1-800-4GOLANG", TypeId::Default),
            ("+1800-446-5264", TypeId::Default),
            ("212.555.9876", TypeId::Default),
            ("testing", TypeId::Default),
        ] {
            assert_eq!(type_for_value(input.as_bytes()).0, expected, "{input}");
        }
        assert_eq!(type_for_value(b"true").1, Value::Bool(true));
        assert_eq!(type_for_value(b"-123").1, Value::Int(-123));
        assert_eq!(type_for_value(b"nope").1, Value::Null);
    }

    #[test]
    #[allow(
        clippy::approx_constant,
        reason = "vector from Go's TestFloatArrayTranslation"
    )]
    fn float_arrays_roundtrip() {
        // Port of TestFloatArrayTranslation.
        for case in [
            vec![],
            vec![0.1],
            vec![0.0],
            vec![0.65433, 1.855, 3.141_592_7],
        ] {
            let back = bytes_as_float_array(&float_array_as_bytes(&case));
            assert_eq!(back.len(), case.len());
            for (a, b) in back.iter().zip(case.iter()) {
                assert_eq!(a.to_bits(), b.to_bits());
            }
        }
        assert!(bytes_as_float_array(&[1, 2, 3]).is_empty());
        assert_eq!(float_array_as_string(&[1.5, 2.0]), "[1.5,2]");
    }

    #[test]
    fn zero_and_safe() {
        for tid in TypeId::all() {
            let zero = Val::zero(*tid);
            assert_eq!(zero.tid, *tid);
            assert!(!matches!(zero.value, Value::Null) || *tid == TypeId::Undefined);
        }
        let null = Val {
            tid: TypeId::Int,
            value: Value::Null,
        };
        assert_eq!(null.safe(), Value::Int(0));
    }
}
