//! Facet values and parsing. Port of `types/facets` (tokenizer glue lives
//! here because it needs [`get_term_tokens`](super::tok::get_term_tokens)).
//!
//! Facet values are stored binary, exactly like Go: little-endian `i64`/`f64`,
//! one byte for bools, raw bytes for strings, and datetimes in
//! [`datastreamdb_types`]' 12-byte layout (Go uses its 15-byte `time` encoding).

#![warn(missing_docs)]

use chrono::{DateTime, FixedOffset};
use datastreamdb_types::strconv::{
    ParseFloatError, ParseIntError, parse_go_float, parse_go_int_base0,
};
use datastreamdb_types::{TypeId, Val, Value, convert, datetime_to_binary, parse_time};

/// Facet value type (discriminants match `api.Facet.ValType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum FacetValType {
    /// String facet.
    String = 0,
    /// Integer facet.
    Int = 1,
    /// Float facet.
    Float = 2,
    /// Boolean facet.
    Bool = 3,
    /// Datetime facet.
    Datetime = 4,
}

/// A parsed facet (mirrors `api.Facet`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Facet {
    /// Facet key.
    pub key: String,
    /// Binary value.
    pub value: Vec<u8>,
    /// Value type.
    pub val_type: FacetValType,
    /// Index tokens of the value (string facets only).
    pub tokens: Vec<String>,
    /// Query alias (never stored).
    pub alias: String,
}

/// Facet filter parameters (mirrors `pb.FacetParams`).
#[derive(Clone, Debug, Default)]
pub struct FacetParams {
    /// Return all keys.
    pub all_keys: bool,
    /// Allowed keys when `all_keys` is false.
    pub allowed_keys: Vec<String>,
    /// Per-key output aliases (Go `FacetParam.alias`); unmapped keys keep "".
    pub aliases: std::collections::HashMap<String, String>,
}

/// Facet error. Messages match Go's where pinned.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FacetError {
    /// Duplicate key in one edge.
    #[error("Repeated keys are not allowed in facets. But got {0}")]
    DuplicateKey(String),
    /// Tokenizer failure.
    #[error("facet tokens: {0}")]
    Tokens(String),
    /// Type conversion failure.
    #[error("facet value: {0}")]
    Convert(String),
}

/// Type id for a facet value type.
#[must_use]
pub const fn type_id_for(val_type: FacetValType) -> TypeId {
    match val_type {
        FacetValType::Int => TypeId::Int,
        FacetValType::Float => TypeId::Float,
        FacetValType::Bool => TypeId::Bool,
        FacetValType::Datetime => TypeId::DateTime,
        FacetValType::String => TypeId::String,
    }
}

/// Parsed facet value (mirrors Go's `interface{}` returns).
#[derive(Clone, Debug, PartialEq)]
pub enum FacetValue {
    /// Integer.
    Int(i64),
    /// Float.
    Float(f64),
    /// Boolean.
    Bool(bool),
    /// Datetime.
    DateTime(DateTime<FixedOffset>),
    /// String.
    String(String),
}

/// Parses binary facet bytes by type. Port of `ValFor` (`Convert` from binary).
///
/// # Errors
///
/// Returns [`FacetError`] on malformed values.
pub fn val_for(val_type: FacetValType, bytes: &[u8]) -> Result<FacetValue, FacetError> {
    let from = Val {
        tid: TypeId::Binary,
        value: Value::Binary(bytes.to_vec()),
    };
    let out =
        convert(&from, type_id_for(val_type)).map_err(|e| FacetError::Convert(e.to_string()))?;
    match out.value {
        Value::Int(v) => Ok(FacetValue::Int(v)),
        Value::Float(v) => Ok(FacetValue::Float(v)),
        Value::Bool(v) => Ok(FacetValue::Bool(v)),
        Value::DateTime(v) => Ok(FacetValue::DateTime(v)),
        Value::String(v) => Ok(FacetValue::String(v)),
        other => Err(FacetError::Convert(format!(
            "unexpected facet value {other:?}"
        ))),
    }
}

/// Builds a facet from a JSON-ish value. Port of `ToBinary`.
///
/// Values are stored binary (never tokenized here): LE `i64`/`f64`, one byte
/// for bools, 12-byte datetimes, raw bytes for strings.
///
/// # Errors
///
/// Returns [`FacetError`] on mismatched value/type pairs.
pub fn to_binary(
    key: &str,
    json_value: &FacetJson,
    val_type: FacetValType,
) -> Result<Facet, FacetError> {
    let value = match (val_type, json_value) {
        (FacetValType::Int, FacetJson::Int(v)) => v.to_le_bytes().to_vec(),
        (FacetValType::Float, FacetJson::Float(v)) => v.to_le_bytes().to_vec(),
        (FacetValType::Bool, FacetJson::Bool(v)) => vec![u8::from(*v)],
        (FacetValType::Datetime, FacetJson::Datetime(v)) => datetime_to_binary(v),
        (FacetValType::String, FacetJson::String(v)) => v.as_bytes().to_vec(),
        _ => {
            return Err(FacetError::Convert(format!(
                "mismatched facet value for {val_type:?}"
            )));
        }
    };
    Ok(Facet {
        key: key.to_owned(),
        value,
        val_type,
        tokens: Vec::new(),
        alias: String::new(),
    })
}

/// JSON-ish facet input for [`to_binary`].
#[derive(Clone, Debug, PartialEq)]
pub enum FacetJson {
    /// Integer.
    Int(i64),
    /// Float.
    Float(f64),
    /// Boolean.
    Bool(bool),
    /// Datetime.
    Datetime(DateTime<FixedOffset>),
    /// String.
    String(String),
}

/// Term tokens for a raw string facet value.
///
/// Invalid UTF-8 (possible via `\x` escapes) is tokenized lossily; the stored
/// value bytes stay exact.
fn string_tokens(value: &[u8]) -> Result<Vec<String>, FacetError> {
    let val = Val {
        tid: TypeId::String,
        value: Value::String(String::from_utf8_lossy(value).into_owned()),
    };
    super::tok::get_term_tokens(std::slice::from_ref(&val))
        .map(|tokens| {
            tokens
                .into_iter()
                .map(|t| String::from_utf8_lossy(&t).into_owned())
                .collect()
        })
        .map_err(|e| FacetError::Tokens(e.to_string()))
}

/// Parses one `key=value` facet. Port of `facets.FacetFor`.
///
/// Empty keys and values are accepted (the RDF parser rejects empty keys
/// itself); only string facets get tokens, sorted ascending.
///
/// # Errors
///
/// Returns [`FacetError`] on unparseable literals.
pub fn facet_for(key: &str, val: &str) -> Result<Facet, FacetError> {
    let (value, val_type) = val_and_type(val)?;
    let mut facet = Facet {
        key: key.to_owned(),
        value,
        val_type,
        tokens: Vec::new(),
        alias: String::new(),
    };
    if val_type == FacetValType::String {
        let mut tokens = string_tokens(&facet.value)?;
        tokens.sort();
        facet.tokens = tokens;
    }
    Ok(facet)
}

/// Splits a facet literal into binary value + type. Port of `valAndValType`.
fn val_and_type(val: &str) -> Result<(Vec<u8>, FacetValType), FacetError> {
    if val.is_empty() {
        return Ok((Vec::new(), FacetValType::String));
    }
    // Strings must be double-quoted; a bad quote is an error, not a fallback.
    if val.len() >= 2 && val.starts_with('"') && val.ends_with('"') {
        let bytes = unquote(val)
            .map_err(|e| FacetError::Convert(format!("could not unquote {val:?}: {e}")))?;
        return Ok((bytes, FacetValType::String));
    }
    match parse_go_int_base0(val) {
        Ok(int) => return Ok((int.to_le_bytes().to_vec(), FacetValType::Int)),
        Err(e @ ParseIntError::Range { .. }) => {
            if val.bytes().all(|b| b.is_ascii_digit()) {
                return Err(FacetError::Convert(e.to_string()));
            }
            // Range error with non-digit characters: try float, like Go.
            // (Inputs reaching here are ASCII-only: anything else fails int
            // parsing with a syntax error, so the ASCII check is exact.)
        }
        Err(ParseIntError::Syntax { .. }) => {}
    }
    match parse_go_float(val) {
        Ok(float) => {
            // NaN would serialize into invalid JSON, so Go rejects it.
            if float.is_nan() {
                return Err(FacetError::Convert("Got invalid value: NaN".to_owned()));
            }
            return Ok((float.to_le_bytes().to_vec(), FacetValType::Float));
        }
        Err(e @ ParseFloatError::Range { .. }) => {
            return Err(FacetError::Convert(e.to_string()));
        }
        Err(ParseFloatError::Syntax { .. }) => {}
    }
    if val == "true" || val == "false" {
        return Ok((vec![u8::from(val == "true")], FacetValType::Bool));
    }
    if let Ok(time) = parse_time(val) {
        return Ok((datetime_to_binary(&time), FacetValType::Datetime));
    }
    Err(FacetError::Convert(format!(
        "Could not parse the facet value : [{val}]"
    )))
}

/// Go `strconv.Unquote` failure (always "invalid syntax", like Go).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnquoteError;

impl std::fmt::Display for UnquoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid syntax")
    }
}

impl std::error::Error for UnquoteError {}

/// Unquotes a double-quoted literal like Go's `strconv.Unquote`.
///
/// Returns raw bytes because `\x` escapes may produce invalid UTF-8 (which Go
/// strings tolerate). Handles `\a\b\f\n\r\t\v\\\"`, `\xNN`, `\uNNNN`,
/// `\UNNNNNNNN`, and octal escapes; `\'` is rejected, like Go.
///
/// # Errors
///
/// Returns [`UnquoteError`] on malformed literals.
pub fn unquote(text: &str) -> Result<Vec<u8>, UnquoteError> {
    let bytes = text.as_bytes();
    if bytes.len() < 2 || bytes[0] != b'"' || bytes[bytes.len() - 1] != b'"' {
        return Err(UnquoteError);
    }
    let inner = &bytes[1..bytes.len() - 1];
    let mut out = Vec::with_capacity(inner.len());
    let mut i = 0;
    while i < inner.len() {
        let c = inner[i];
        if c != b'\\' {
            if c == b'"' || c == b'\n' {
                return Err(UnquoteError);
            }
            out.push(c);
            i += 1;
            continue;
        }
        i += 1;
        if i >= inner.len() {
            return Err(UnquoteError);
        }
        match inner[i] {
            b'a' => {
                out.push(0x07);
                i += 1;
            }
            b'b' => {
                out.push(0x08);
                i += 1;
            }
            b'f' => {
                out.push(0x0C);
                i += 1;
            }
            b'n' => {
                out.push(b'\n');
                i += 1;
            }
            b'r' => {
                out.push(b'\r');
                i += 1;
            }
            b't' => {
                out.push(b'\t');
                i += 1;
            }
            b'v' => {
                out.push(0x0B);
                i += 1;
            }
            b'\\' => {
                out.push(b'\\');
                i += 1;
            }
            b'"' => {
                out.push(b'"');
                i += 1;
            }
            b'x' => {
                let hex = inner.get(i + 1..i + 3).ok_or(UnquoteError)?;
                out.push(hex_byte(hex)?);
                i += 3;
            }
            b'u' => {
                let hex = inner.get(i + 1..i + 5).ok_or(UnquoteError)?;
                push_rune(&mut out, hex)?;
                i += 5;
            }
            b'U' => {
                let hex = inner.get(i + 1..i + 9).ok_or(UnquoteError)?;
                push_rune(&mut out, hex)?;
                i += 9;
            }
            b'0'..=b'7' => {
                // The first digit is octal (matched above); take up to two more.
                let mut value = u32::from(inner[i] - b'0');
                i += 1;
                for _ in 0..2 {
                    match inner.get(i) {
                        Some(d) if matches!(d, b'0'..=b'7') => {
                            value = value * 8 + u32::from(d - b'0');
                            i += 1;
                        }
                        _ => break,
                    }
                }
                if value > 0xFF {
                    return Err(UnquoteError);
                }
                #[allow(clippy::cast_possible_truncation, reason = "range-checked above")]
                out.push(value as u8);
            }
            _ => return Err(UnquoteError),
        }
    }
    Ok(out)
}

fn hex_byte(hex: &[u8]) -> Result<u8, UnquoteError> {
    if hex.len() != 2 || !hex.iter().all(u8::is_ascii_hexdigit) {
        return Err(UnquoteError);
    }
    let mut value: u8 = 0;
    for b in hex {
        let digit = b.to_ascii_lowercase();
        value = value * 16 + digit
            - if digit.is_ascii_digit() {
                b'0'
            } else {
                b'a' - 10
            };
    }
    Ok(value)
}

fn push_rune(out: &mut Vec<u8>, hex: &[u8]) -> Result<(), UnquoteError> {
    if !hex.iter().all(u8::is_ascii_hexdigit) {
        return Err(UnquoteError);
    }
    let mut value: u32 = 0;
    for b in hex {
        let digit = u32::from(b.to_ascii_lowercase());
        let zero = u32::from(if b.is_ascii_digit() { b'0' } else { b'a' - 10 });
        value = value * 16 + digit - zero;
    }
    let c = char::from_u32(value).ok_or(UnquoteError)?;
    let mut buf = [0u8; 4];
    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    Ok(())
}

/// Sorts facets by key, rejecting duplicates. Port of `SortAndValidate`.
///
/// # Errors
///
/// Returns [`FacetError::DuplicateKey`] on repeated keys.
pub fn sort_and_validate(mut facets: Vec<Facet>) -> Result<Vec<Facet>, FacetError> {
    facets.sort_by(|a, b| a.key.cmp(&b.key));
    let mut last = "";
    for facet in &facets {
        if facet.key == last {
            return Err(FacetError::DuplicateKey(facet.key.clone()));
        }
        last = &facet.key;
    }
    Ok(facets)
}

/// Filters facets by params. Port of `CopyFacets`, aliases included.
#[must_use]
pub fn copy_facets(facets: &[Facet], params: &FacetParams) -> Vec<Facet> {
    if params.all_keys {
        // Go rebuilds fresh copies (aliases cleared) in this mode.
        return facets
            .iter()
            .map(|f| Facet {
                alias: String::new(),
                ..f.clone()
            })
            .collect();
    }
    facets
        .iter()
        .filter(|f| params.allowed_keys.iter().any(|k| k == &f.key))
        .map(|f| Facet {
            alias: params.aliases.get(&f.key).cloned().unwrap_or_default(),
            ..f.clone()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Go `rdf_parser_test.go` facet vectors (values are binary, like Go).
    #[test]
    fn facet_for_go_vectors() {
        // (friend="hatter")
        let facet = facet_for("friend", "\"hatter\"").expect("string");
        assert_eq!(facet.val_type, FacetValType::String);
        assert_eq!(facet.value, b"hatter");
        assert_eq!(facet.tokens, ["\u{1}hatter"]);
        // (friend="hatter \u0045") unquotes to "hatter E", tokens sorted.
        let facet = facet_for("friend", "\"hatter \\u0045\"").expect("unquote");
        assert_eq!(facet.value, b"hatter E");
        assert_eq!(facet.tokens, ["\u{1}e", "\u{1}hatter"]);
        // (key2=13): little-endian i64, no tokens.
        let facet = facet_for("key2", "13").expect("int");
        assert_eq!(facet.val_type, FacetValType::Int);
        assert_eq!(facet.value, 13i64.to_le_bytes());
        assert!(facet.tokens.is_empty());
        // Integers accept Go base-0 formats.
        let facet = facet_for("k", "0x0D").expect("hex");
        assert_eq!(facet.value, 13i64.to_le_bytes());
        // Empty value stays an empty string with the empty-term token.
        let facet = facet_for("key1", "").expect("empty");
        assert_eq!(facet.val_type, FacetValType::String);
        assert!(facet.value.is_empty());
        assert_eq!(facet.tokens, ["\u{1}"]);
        // Empty keys are accepted (callers validate).
        assert!(facet_for("", "1").is_ok());
        // Floats are little-endian f64.
        let facet = facet_for("key3", "1.2").expect("float");
        assert_eq!(facet.value, 1.2f64.to_le_bytes());
        // Huge integers fall back to float.
        let facet = facet_for("k", "111111111111111111888888.23").expect("big");
        assert_eq!(facet.val_type, FacetValType::Float);
        // Booleans are a single byte; only literals count.
        let facet = facet_for("key5", "true").expect("bool");
        assert_eq!(facet.value, [1]);
        assert_eq!(
            facet_for("k", "1").expect("one").val_type,
            FacetValType::Int
        );
        // Datetimes use the 12-byte layout.
        let facet = facet_for("key4", "2006-01-02T15:04:05").expect("datetime");
        assert_eq!(facet.val_type, FacetValType::Datetime);
        assert_eq!(facet.value.len(), 12);
        let FacetValue::DateTime(back) = val_for(FacetValType::Datetime, &facet.value).expect("rt")
        else {
            panic!("datetime roundtrip");
        };
        assert_eq!(
            datastreamdb_types::format_datetime(&back),
            "2006-01-02T15:04:05Z"
        );
    }

    #[test]
    fn facet_for_go_errors() {
        // Quoted but invalid: Go reports the unquote failure.
        let err = facet_for("k", "\"a\\qb\"").expect_err("bad escape");
        assert_eq!(
            err.to_string(),
            "facet value: could not unquote \"\\\"a\\\\qb\\\"\": invalid syntax"
        );
        // NaN facets are rejected (invalid JSON).
        assert!(facet_for("salary", "NaN").is_err());
        // Pure-digit overflow stays an integer error.
        let err = facet_for("k", "111111111111111111888888").expect_err("bigint");
        assert!(err.to_string().contains("value out of range"), "{err}");
        // Hex overflow is not a float either.
        assert!(facet_for("k", "0x1787586C4FA8A0284FF8").is_err());
        // Unparseable literals name the value.
        let err = facet_for("k", "not a value!").expect_err("junk");
        assert_eq!(
            err.to_string(),
            "facet value: Could not parse the facet value : [not a value!]"
        );
    }

    #[test]
    fn unquote_go_vectors() {
        assert_eq!(unquote("\"abc\"").expect("plain"), b"abc");
        assert_eq!(unquote("\"a\\nb\"").expect("nl"), b"a\nb");
        assert_eq!(
            unquote("\"\\v\\t\\b\\n\\r\\f\\\"\\\\\"").expect("echars"),
            b"\x0b\t\x08\n\r\x0c\"\\"
        );
        assert_eq!(unquote("\"\\x02\"").expect("hex"), [0x02]);
        assert_eq!(unquote("\"\\u0045\"").expect("u"), b"E");
        assert_eq!(unquote("\"\\U0001F600\"").expect("U"), "😀".as_bytes());
        assert_eq!(unquote("\"\\0\\12\\123\"").expect("octal"), [0, 0x0A, 0x53]);
        // Go rejects \' inside double-quoted strings (RDF test pins this).
        assert!(unquote("\"\\'\"").is_err());
        assert_eq!(unquote("\"\\a\"").expect("bell"), [0x07]);
        assert!(unquote("\"\n\"").is_err());
        assert!(unquote("\"abc").is_err());
        assert!(unquote("\"\\x2\"").is_err());
        assert!(unquote("\"\\u004\"").is_err());
        assert!(unquote("\"\\ud800\"").is_err());
        assert!(unquote("\"\\400\"").is_err());
        assert!(unquote("abc").is_err());
    }

    #[test]
    fn to_binary_go_vectors() {
        // JSON facet bytes match Go's Marshal: LE ints/floats, 1-byte bools.
        let facet = to_binary("age", &FacetJson::Int(3), FacetValType::Int).expect("int");
        assert_eq!(facet.value, 3i64.to_le_bytes());
        assert!(facet.tokens.is_empty());
        let facet =
            to_binary("price", &FacetJson::Float(30000.56), FacetValType::Float).expect("float");
        assert_eq!(facet.value, 30000.56f64.to_le_bytes());
        let facet = to_binary("first", &FacetJson::Bool(true), FacetValType::Bool).expect("bool");
        assert_eq!(facet.value, [1]);
        let facet = to_binary(
            "op",
            &FacetJson::String("READ WRITE".into()),
            FacetValType::String,
        )
        .expect("str");
        assert_eq!(facet.value, b"READ WRITE");
        assert!(to_binary("k", &FacetJson::Int(1), FacetValType::Bool).is_err());
    }

    #[test]
    fn sorts_and_validates() {
        let a = facet_for("a", "1").expect("a");
        let b = facet_for("b", "2").expect("b");
        let sorted = sort_and_validate(vec![b.clone(), a.clone()]).expect("sort");
        assert_eq!(sorted, [a.clone(), b]);
        let err = sort_and_validate(vec![a.clone(), a]).expect_err("dup");
        assert_eq!(
            err.to_string(),
            "Repeated keys are not allowed in facets. But got a"
        );
    }

    #[test]
    fn copies_by_params() {
        let facets = vec![
            facet_for("a", "1").expect("a"),
            facet_for("b", "2").expect("b"),
        ];
        assert_eq!(copy_facets(&facets, &FacetParams::default()).len(), 0);
        let params = FacetParams {
            all_keys: true,
            ..FacetParams::default()
        };
        let out = copy_facets(&facets, &params);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|f| f.alias.is_empty()));
        let params = FacetParams {
            all_keys: false,
            allowed_keys: vec!["b".to_owned()],
            aliases: [("b".to_owned(), "beta".to_owned())].into_iter().collect(),
        };
        let out = copy_facets(&facets, &params);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].key, "b");
        assert_eq!(out[0].alias, "beta");
    }

    #[test]
    fn val_for_roundtrips() {
        assert_eq!(
            val_for(FacetValType::Int, &42i64.to_le_bytes()).expect("int"),
            FacetValue::Int(42)
        );
        assert_eq!(
            val_for(FacetValType::Float, &1.5f64.to_le_bytes()).expect("float"),
            FacetValue::Float(1.5)
        );
        assert_eq!(
            val_for(FacetValType::Bool, &[1]).expect("bool"),
            FacetValue::Bool(true)
        );
        assert!(val_for(FacetValType::Bool, &[8]).is_err());
        assert_eq!(
            val_for(FacetValType::String, b"hi").expect("str"),
            FacetValue::String("hi".to_owned())
        );
        let facet = facet_for("n", "-7").expect("n");
        assert_eq!(
            val_for(FacetValType::Int, &facet.value).expect("neg"),
            FacetValue::Int(-7)
        );
    }
}
