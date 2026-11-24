//! Unit tests for the conversion matrix (split from `conversion.rs` to honor the 1000-line file rule).

use super::*;
use chrono::Timelike as _;

fn binary(tid: TypeId, bytes: &[u8]) -> Val {
    Val {
        tid,
        value: Value::Binary(bytes.to_vec()),
    }
}

fn int_bytes(v: i64) -> Vec<u8> {
    v.to_le_bytes().to_vec()
}

fn float_bytes(v: f64) -> Vec<u8> {
    v.to_le_bytes().to_vec()
}

fn time_bytes(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> Vec<u8> {
    let dt = FixedOffset::east_opt(0)
        .expect("utc")
        .with_ymd_and_hms(y, mo, d, h, mi, s)
        .single()
        .expect("valid test date");
    datetime_to_binary(&dt)
}

#[test]
fn edge_cases() {
    // Port of TestConversionEdgeCases (adapted to our datetime binary).
    assert_eq!(
        convert(
            &Val {
                tid: TypeId::Binary,
                value: Value::Null
            },
            TypeId::Binary
        )
        .expect_err("nil payload"),
        ConversionError::InvalidData { to: TypeId::Binary }
    );
    assert!(convert(&binary(TypeId::Binary, &[]), TypeId::Int).is_err());
    assert!(convert(&binary(TypeId::Binary, &[]), TypeId::Float).is_err());
    assert_eq!(
        convert(&binary(TypeId::Binary, &[]), TypeId::Bool)
            .expect("empty bool")
            .value,
        Value::Bool(false)
    );
    assert!(convert(&binary(TypeId::Binary, &[2]), TypeId::Bool).is_err());
    assert!(convert(&binary(TypeId::Binary, &[8]), TypeId::DateTime).is_err());
    assert!(convert(&binary(TypeId::String, &[]), TypeId::Int).is_err());
    assert!(convert(&binary(TypeId::String, &[]), TypeId::Float).is_err());
    assert!(convert(&binary(TypeId::String, &[]), TypeId::Bool).is_err());
    assert!(convert(&binary(TypeId::String, &[]), TypeId::DateTime).is_err());
    assert!(convert(&binary(TypeId::Int, &[]), TypeId::DateTime).is_err());
    assert_eq!(
        convert(&binary(TypeId::Int, &int_bytes(0)), TypeId::DateTime)
            .expect("epoch")
            .value,
        Value::DateTime(
            FixedOffset::east_opt(0)
                .expect("utc")
                .timestamp_opt(0, 0)
                .single()
                .expect("epoch")
        )
    );
    assert!(convert(&binary(TypeId::Float, &[]), TypeId::DateTime).is_err());
    assert!(convert(&binary(TypeId::Bool, &[]), TypeId::DateTime).is_err());
    assert!(convert(&binary(TypeId::Bool, &[8]), TypeId::DateTime).is_err());
}

#[test]
fn to_default_vectors() {
    // Port of TestConvertToDefault.
    for (from, expected) in [
        (binary(TypeId::String, b"a"), "a"),
        (binary(TypeId::String, b""), ""),
        (binary(TypeId::Default, b"abc"), "abc"),
        (binary(TypeId::Binary, b"2016"), "2016"),
        (binary(TypeId::Int, &int_bytes(3)), "3"),
        (binary(TypeId::Float, &float_bytes(-3.5)), "-3.5"),
        (
            binary(TypeId::DateTime, &time_bytes(2006, 1, 2, 15, 4, 5)),
            "2006-01-02T15:04:05Z",
        ),
    ] {
        let out = convert(&from, TypeId::Default).expect("convert");
        assert_eq!(out.value, Value::String(expected.to_owned()), "{from:?}");
    }
}

#[test]
fn from_default_vectors() {
    // Port of TestConvertFromDefault.
    let cases: &[(&str, TypeId, Value)] = &[
        ("1", TypeId::Int, Value::Int(1)),
        ("1.3", TypeId::Float, Value::Float(1.3)),
        ("true", TypeId::Bool, Value::Bool(true)),
        ("2016", TypeId::Binary, Value::Binary(b"2016".to_vec())),
        ("", TypeId::Binary, Value::Binary(Vec::new())),
        ("hello", TypeId::String, Value::String("hello".to_owned())),
        ("", TypeId::String, Value::String(String::new())),
    ];
    for (input, tid, expected) in cases {
        let out = convert(&binary(TypeId::Default, input.as_bytes()), *tid).expect("convert");
        assert_eq!(&out.value, expected, "{input}");
    }
    let out = convert(&binary(TypeId::Default, b"2016"), TypeId::DateTime).expect("convert");
    assert_eq!(
        out.value,
        Value::DateTime(
            FixedOffset::east_opt(0)
                .expect("utc")
                .with_ymd_and_hms(2016, 1, 1, 0, 0, 0)
                .single()
                .expect("date")
        )
    );
}

#[test]
fn password_matrix() {
    // Ports of TestConvertFromPassword / TestConvertToPassword.
    assert_eq!(
        convert(&binary(TypeId::Password, b""), TypeId::String)
            .expect("empty")
            .value,
        Value::String(String::new())
    );
    assert_eq!(
        convert(&binary(TypeId::Password, b"password"), TypeId::Password)
            .expect("pw")
            .value,
        Value::Password("password".to_owned())
    );
    for to in [TypeId::Default, TypeId::Int, TypeId::Float, TypeId::Bool] {
        let err = convert(&binary(TypeId::Password, b"password"), to).expect_err("impossible");
        assert_eq!(
            err.to_string(),
            format!("Cannot convert password to type {to}")
        );
    }
    let hashed = convert(&binary(TypeId::String, b"testing"), TypeId::Password).expect("hash");
    let Value::Password(hash) = &hashed.value else {
        panic!("expected password")
    };
    // Rust bcrypt emits $2b$ (same algorithm as Go's $2a$); hashes verify
    // cross-compatibly in both directions (see password::verify_vectors).
    assert!(hash.starts_with("$2b$10$"), "{hash}");
    assert_eq!(hash.len(), 60);
    crate::password::verify_password("testing", hash).expect("verify");
    let err = convert(&binary(TypeId::String, b""), TypeId::Password).expect_err("short");
    assert_eq!(
        err.to_string(),
        "Password too short, i.e. should have at least 6 chars"
    );
}

#[test]
fn bool_matrix() {
    // Ports of truthy/falsy + int/float/string to bool.
    for (from, expected) in [
        (binary(TypeId::String, b"true"), true),
        (binary(TypeId::Default, b"true"), true),
        (binary(TypeId::Int, &int_bytes(1)), true),
        (binary(TypeId::Int, &int_bytes(-1)), true),
        (binary(TypeId::Float, &float_bytes(1.0)), true),
        (binary(TypeId::Float, &float_bytes(-1.0)), true),
        (binary(TypeId::String, b"false"), false),
        (binary(TypeId::Default, b"false"), false),
        (binary(TypeId::Int, &int_bytes(0)), false),
        (binary(TypeId::Float, &float_bytes(0.0)), false),
        (binary(TypeId::Binary, b""), false),
    ] {
        let out = convert(&from, TypeId::Bool).expect("convert");
        assert_eq!(out.value, Value::Bool(expected), "{from:?}");
    }
    for nan in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let out =
            convert(&binary(TypeId::Float, &float_bytes(nan)), TypeId::Bool).expect("convert");
        assert_eq!(out.value, Value::Bool(true));
    }
    let err = convert(
        &binary(TypeId::DateTime, &time_bytes(2020, 1, 1, 0, 0, 0)),
        TypeId::Bool,
    )
    .expect_err("dt->bool");
    assert_eq!(err.to_string(), "Cannot convert datetime to type bool");
}

#[test]
fn float_int_matrix() {
    // Ports of TestConvertFloatToInt + TestConvertIntToFloat + TestConvertBoolToInt.
    assert_eq!(
        convert(
            &binary(TypeId::Float, &float_bytes(522_638_295_213.324_3)),
            TypeId::Int
        )
        .expect("convert")
        .value,
        Value::Int(522_638_295_213)
    );
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let err =
            convert(&binary(TypeId::Float, &float_bytes(bad)), TypeId::Int).expect_err("range");
        assert_eq!(err.to_string(), "Float out of int64 range");
    }
    assert_eq!(
        convert(&binary(TypeId::Int, &int_bytes(-3)), TypeId::Float)
            .expect("convert")
            .value,
        Value::Float(-3.0)
    );
    assert_eq!(
        convert(&binary(TypeId::Bool, &[1]), TypeId::Int)
            .expect("convert")
            .value,
        Value::Int(1)
    );
}

#[test]
fn datetime_math_vectors() {
    // Ports of float/int <-> datetime vectors.
    type DateCase = (f64, (i32, u32, u32, u32, u32, u32, u32));
    let cases: &[DateCase] = &[
        (1_257_894_000.0, (2009, 11, 10, 23, 0, 0, 0)),
        (-4_410_000.0, (1969, 11, 10, 23, 0, 0, 0)),
        (2_204_578_800.0, (2039, 11, 10, 23, 0, 0, 0)),
        (-2_150_326_800.0, (1901, 11, 10, 23, 0, 0, 0)),
    ];
    for (input, (y, mo, d, h, mi, s, nanos)) in cases {
        let expected = FixedOffset::east_opt(0)
            .expect("utc")
            .with_ymd_and_hms(*y, *mo, *d, *h, *mi, *s)
            .single()
            .and_then(|dt| dt.with_nanosecond(*nanos))
            .expect("valid test date");
        let out = convert(
            &binary(TypeId::Float, &float_bytes(*input)),
            TypeId::DateTime,
        )
        .expect("convert");
        assert_eq!(out.value, Value::DateTime(expected), "{input}");
        // Roundtrip back to float is exact for whole seconds.
        let back = convert(
            &Val {
                tid: TypeId::DateTime,
                value: Value::Binary(datetime_to_binary(&expected)),
            },
            TypeId::Float,
        )
        .expect("convert");
        assert_eq!(back.value, Value::Float(*input));
    }
    // Fractional vectors (float arithmetic mirrors Go's formula).
    let out = convert(
        &binary(TypeId::Float, &float_bytes(1_257_894_000.001)),
        TypeId::DateTime,
    )
    .expect("convert");
    let Value::DateTime(dt) = out.value else {
        panic!("expected datetime")
    };
    assert_eq!(
        (dt.timestamp(), dt.timestamp_subsec_nanos()),
        (1_257_894_000, 999_927)
    );
}

#[test]
fn to_string_vectors() {
    // Port of TestConvertToString.
    for (from, expected) in [
        (binary(TypeId::Float, &float_bytes(13816.251)), "13816.251"),
        (binary(TypeId::Int, &int_bytes(-1221)), "-1221"),
        (binary(TypeId::Bool, &[1]), "true"),
        (binary(TypeId::String, b"srfrog"), "srfrog"),
        (binary(TypeId::Password, b"password"), "password"),
        (
            binary(TypeId::DateTime, &time_bytes(2006, 1, 2, 15, 4, 5)),
            "2006-01-02T15:04:05Z",
        ),
    ] {
        let out = convert(&from, TypeId::String).expect("convert");
        assert_eq!(out.value, Value::String(expected.to_owned()), "{from:?}");
    }
}

#[test]
fn object_value_hides_passwords() {
    // Port of TestObjectValuePasswordErrorDoesNotLeakValue.
    let err = object_value(TypeId::Password, &Value::Int(123_456_789)).expect_err("mismatch");
    let message = err.to_string();
    assert!(
        message.contains("Expected value of type password"),
        "{message}"
    );
    assert!(message.contains("int64"), "{message}");
    assert!(!message.contains("123456789"), "{message}");
    assert_eq!(
        object_value(TypeId::Int, &Value::Int(7)).expect("int"),
        ApiValue::Int(7)
    );
    assert!(object_value(TypeId::Uid, &Value::Uid(1)).is_err());
}

#[test]
fn vfloat_text_vectors() {
    assert_eq!(parse_v_float("").expect("empty"), Vec::<f32>::new());
    assert_eq!(parse_v_float("[]").expect("brackets"), Vec::<f32>::new());
    assert_eq!(parse_v_float("[1.0, 2.0]").expect("csv"), [1.0, 2.0]);
    assert_eq!(
        parse_v_float("[1.1, 2.2, 3.3]").expect("json-vector"),
        [1.1f32, 2.2, 3.3]
    );
    assert_eq!(parse_v_float("[1.0     2.0]").expect("spaces"), [1.0, 2.0]);
    // Go strips every trailing ']' (TrimRight).
    assert_eq!(parse_v_float("[1.0]]").expect("double-bracket"), [1.0]);
    // Go accepts a trailing-dot mantissa via ParseFloat.
    assert_eq!(parse_v_float("[1.]").expect("trailing-dot"), [1.0]);
    assert!(parse_v_float("nope").is_err());
    assert!(parse_v_float("[1.0").is_err());
    // Go bitSize 32 rejects f32 overflow (literal infinities stay valid).
    assert!(parse_v_float("[1e100]").is_err());
    assert_eq!(parse_v_float("[Inf]").expect("inf"), [f32::INFINITY]);
    // Errors name the processed string (newlines/tabs folded, trimmed).
    assert_eq!(
        parse_v_float("  nope\n").expect_err("msg").to_string(),
        "cannot convert nope to vfloat"
    );
}
