//! Tokenizer registry, scalar tokenizers, and builders. Port of `tok/tok.go`
//! (non-text half; string tokenizers live in [`text`](super::text)).
//!
//! Tokens are byte strings (`Vec<u8>`): Go strings are byte bags and index
//! keys carry arbitrary bytes (sign-prefixed integers, collation keys), which
//! Rust `String` cannot hold. Text tokenizers emit UTF-8 bytes; use
//! [`String::from_utf8`] to view them.

#![warn(missing_docs)]

use std::{collections::HashMap, sync::RwLock};

use datastreamdb_types::{TypeId, Val, Value};

/// Tokenizer identifiers (frozen; stored in index keys).
pub const IDENT_NONE: u8 = 0x0;
/// Term tokens.
pub const IDENT_TERM: u8 = 0x1;
/// Exact tokens.
pub const IDENT_EXACT: u8 = 0x2;
/// Exact tokens with language collation.
pub const IDENT_EXACT_LANG: u8 = 0x3;
/// Year tokens.
pub const IDENT_YEAR: u8 = 0x4;
/// Month tokens.
pub const IDENT_MONTH: u8 = 0x41;
/// Day tokens.
pub const IDENT_DAY: u8 = 0x42;
/// Hour tokens.
pub const IDENT_HOUR: u8 = 0x43;
/// Geo tokens.
pub const IDENT_GEO: u8 = 0x5;
/// Integer tokens.
pub const IDENT_INT: u8 = 0x6;
/// Float tokens.
pub const IDENT_FLOAT: u8 = 0x7;
/// Full-text tokens.
pub const IDENT_FULLTEXT: u8 = 0x8;
/// Boolean tokens.
pub const IDENT_BOOL: u8 = 0x9;
/// Trigram tokens.
pub const IDENT_TRIGRAM: u8 = 0xA;
/// Hash tokens.
pub const IDENT_HASH: u8 = 0xB;
/// SHA tokens.
pub const IDENT_SHA: u8 = 0xC;
/// Big-float tokens.
pub const IDENT_BIGFLOAT: u8 = 0xD;
/// Float-vector tokens.
pub const IDENT_VFLOAT: u8 = 0xE;
/// N-gram tokens.
pub const IDENT_NGRAM: u8 = 0xF;
/// First identifier byte for custom tokenizers.
pub const IDENT_CUSTOM: u8 = 0x80;
/// Language delimiter inside exact-lang tokens.
pub const IDENT_DELIMITER: u8 = 0x1f;

/// Tokenizer error. Messages match Go's where pinned.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TokError {
    /// Wrong number of function arguments.
    #[error("Function requires 1 arguments, but got {0}")]
    Arity(usize),
    /// No tokenizer with the id.
    #[error("No tokenizer was found with id {0}")]
    UnknownId(u8),
    /// No tokenizer with the name.
    #[error("Invalid tokenizer {0}")]
    UnknownName(String),
    /// Value type mismatch (Go panics here; we error).
    #[error("{tokenizer} tokenizer cannot tokenize {got}")]
    Mistyped {
        /// Tokenizer name.
        tokenizer: &'static str,
        /// Payload kind found.
        got: &'static str,
    },
    /// Exact indices need strings.
    #[error("Exact indices only supported for string types")]
    ExactNonString,
    /// Trigram indices need strings.
    #[error("Trigram indices only supported for string types")]
    TrigramNonString,
    /// Hash tokenizer needs strings.
    #[error("Hash tokenizer only supported for string types")]
    HashNonString,
    /// Duplicate custom tokenizer name.
    #[error("Duplicate tokenizer: {0}")]
    Duplicate(String),
    /// Custom tokenizer id below `0x80`.
    #[error("custom tokenizer identifier byte must be >= 0x80, but was {0:#x}")]
    CustomIdTooLow(u8),
    /// Custom tokenizer type is not a known type id.
    #[error("Invalid type {0:?} for tokenizer {1}")]
    CustomType(String, String),
    /// Geo conversion failure.
    #[error("geo tokenizer: {0}")]
    Geo(String),
}

/// A tokenizer: names, raw (unencoded) tokens, index properties.
pub trait Tokenizer: Send + Sync {
    /// Unique name (`int`, `term`, ...).
    fn name(&self) -> &'static str;
    /// Type id this tokenizer serves.
    fn tokenizer_type(&self) -> &'static str;
    /// Raw tokens (callers add the identifier byte).
    ///
    /// # Errors
    ///
    /// Returns [`TokError`] on mistyped values.
    fn tokens(&self, value: &Val) -> Result<Vec<Vec<u8>>, TokError>;
    /// Unique identifier byte.
    fn identifier(&self) -> u8;
    /// Whether the tokenizer supports ordering.
    fn is_sortable(&self) -> bool;
    /// Whether index keys need value comparison.
    fn is_lossy(&self) -> bool;
}

/// Big-endian order-preserving integer encoding (sign byte + 8 bytes).
#[must_use]
pub fn encode_int(val: i64) -> Vec<u8> {
    let mut buf = vec![u8::from(val >= 0)];
    buf.extend_from_slice(&val.to_be_bytes());
    buf
}

/// Prefixes a raw token with the identifier byte.
#[must_use]
pub fn encode_token(token: &[u8], typ: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(token.len() + 1);
    out.push(typ);
    out.extend_from_slice(token);
    out
}

/// Tokenizes `value`, prefixing every token with the identifier.
///
/// # Errors
///
/// Returns [`TokError`] on tokenizer failure.
pub fn build_tokens(value: &Val, tokenizer: &dyn Tokenizer) -> Result<Vec<Vec<u8>>, TokError> {
    let id = tokenizer.identifier();
    Ok(tokenizer
        .tokens(value)?
        .into_iter()
        .map(|t| encode_token(&t, id))
        .collect())
}

/// Tokens for a tokenizer id + single argument. Port of `GetTokens`.
///
/// # Errors
///
/// Returns [`TokError`] on wrong arity, unknown ids, or tokenizer failure.
pub fn get_tokens(id: u8, args: &[Val]) -> Result<Vec<Vec<u8>>, TokError> {
    if args.len() != 1 {
        return Err(TokError::Arity(args.len()));
    }
    let tokenizer = get_tokenizer_by_id(id).ok_or(TokError::UnknownId(id))?;
    build_tokens(&args[0], tokenizer.as_ref())
}

/// Term tokens for one value. Port of `GetTermTokens`.
///
/// # Errors
///
/// Returns [`TokError`] on wrong arity or tokenizer failure.
pub fn get_term_tokens(args: &[Val]) -> Result<Vec<Vec<u8>>, TokError> {
    get_tokens(IDENT_TERM, args)
}

/// Geo tokens with the geo identifier. Port of `EncodeGeoTokens`.
pub fn encode_geo_tokens(tokens: &mut [Vec<u8>]) {
    for token in tokens.iter_mut() {
        *token = encode_token(token, IDENT_GEO);
    }
}

/// Regex tokens with the trigram identifier. Port of `EncodeRegexTokens`.
pub fn encode_regex_tokens(tokens: &mut [Vec<u8>]) {
    for token in tokens.iter_mut() {
        *token = encode_token(token, IDENT_TRIGRAM);
    }
}

fn mistyped(tokenizer: &'static str, value: &Val) -> TokError {
    TokError::Mistyped {
        tokenizer,
        got: Val::type_name(&value.value),
    }
}

/// Integer tokenizer.
#[derive(Debug, Default)]
pub struct IntTokenizer;
/// Float tokenizer (truncates toward zero like Go).
#[derive(Debug, Default)]
pub struct FloatTokenizer;
/// Big-float tokenizer (truncates like Go's `Int64`).
#[derive(Debug, Default)]
pub struct BigFloatTokenizer;
/// Boolean tokenizer.
#[derive(Debug, Default)]
pub struct BoolTokenizer;
/// Year tokenizer.
#[derive(Debug, Default)]
pub struct YearTokenizer;
/// Month tokenizer.
#[derive(Debug, Default)]
pub struct MonthTokenizer;
/// Day tokenizer.
#[derive(Debug, Default)]
pub struct DayTokenizer;
/// Hour tokenizer.
#[derive(Debug, Default)]
pub struct HourTokenizer;
/// Geo tokenizer.
#[derive(Debug, Default)]
pub struct GeoTokenizer;

impl Tokenizer for IntTokenizer {
    fn name(&self) -> &'static str {
        "int"
    }
    fn tokenizer_type(&self) -> &'static str {
        "int"
    }
    fn tokens(&self, value: &Val) -> Result<Vec<Vec<u8>>, TokError> {
        match &value.value {
            Value::Int(v) => Ok(vec![encode_int(*v)]),
            _ => Err(mistyped("int", value)),
        }
    }
    fn identifier(&self) -> u8 {
        IDENT_INT
    }
    fn is_sortable(&self) -> bool {
        true
    }
    fn is_lossy(&self) -> bool {
        false
    }
}

impl Tokenizer for FloatTokenizer {
    fn name(&self) -> &'static str {
        "float"
    }
    fn tokenizer_type(&self) -> &'static str {
        "float"
    }
    fn tokens(&self, value: &Val) -> Result<Vec<Vec<u8>>, TokError> {
        match &value.value {
            // Go's int64(float64) is undefined on overflow; saturate instead.
            Value::Float(v) => Ok(vec![encode_int(
                v.trunc().clamp(i64::MIN as f64, i64::MAX as f64) as i64,
            )]),
            _ => Err(mistyped("float", value)),
        }
    }
    fn identifier(&self) -> u8 {
        IDENT_FLOAT
    }
    fn is_sortable(&self) -> bool {
        true
    }
    fn is_lossy(&self) -> bool {
        true
    }
}

impl Tokenizer for BigFloatTokenizer {
    fn name(&self) -> &'static str {
        "bigfloat"
    }
    fn tokenizer_type(&self) -> &'static str {
        "bigfloat"
    }
    fn tokens(&self, value: &Val) -> Result<Vec<Vec<u8>>, TokError> {
        match &value.value {
            Value::BigFloat(v) => Ok(vec![encode_int(
                datastreamdb_types::conversion::bigfloat_to_i64(v),
            )]),
            _ => Err(mistyped("bigfloat", value)),
        }
    }
    fn identifier(&self) -> u8 {
        IDENT_BIGFLOAT
    }
    fn is_sortable(&self) -> bool {
        true
    }
    fn is_lossy(&self) -> bool {
        true
    }
}

impl Tokenizer for BoolTokenizer {
    fn name(&self) -> &'static str {
        "bool"
    }
    fn tokenizer_type(&self) -> &'static str {
        "bool"
    }
    fn tokens(&self, value: &Val) -> Result<Vec<Vec<u8>>, TokError> {
        match &value.value {
            Value::Bool(v) => Ok(vec![encode_int(i64::from(*v))]),
            _ => Err(mistyped("bool", value)),
        }
    }
    fn identifier(&self) -> u8 {
        IDENT_BOOL
    }
    fn is_sortable(&self) -> bool {
        false
    }
    fn is_lossy(&self) -> bool {
        false
    }
}

fn datetime_parts(value: &Val, tokenizer: &'static str) -> Result<(i32, u32, u32, u32), TokError> {
    use chrono::{Datelike as _, Timelike as _};
    match &value.value {
        Value::DateTime(v) => {
            let utc = v.to_utc();
            Ok((utc.year(), utc.month(), utc.day(), utc.hour()))
        }
        _ => Err(mistyped(tokenizer, value)),
    }
}

impl Tokenizer for YearTokenizer {
    fn name(&self) -> &'static str {
        "year"
    }
    fn tokenizer_type(&self) -> &'static str {
        "datetime"
    }
    fn tokens(&self, value: &Val) -> Result<Vec<Vec<u8>>, TokError> {
        let (year, _, _, _) = datetime_parts(value, "year")?;
        Ok(vec![(year as u16).to_be_bytes().to_vec()])
    }
    fn identifier(&self) -> u8 {
        IDENT_YEAR
    }
    fn is_sortable(&self) -> bool {
        true
    }
    fn is_lossy(&self) -> bool {
        true
    }
}

impl Tokenizer for MonthTokenizer {
    fn name(&self) -> &'static str {
        "month"
    }
    fn tokenizer_type(&self) -> &'static str {
        "datetime"
    }
    fn tokens(&self, value: &Val) -> Result<Vec<Vec<u8>>, TokError> {
        let (year, month, _, _) = datetime_parts(value, "month")?;
        let mut out = (year as u16).to_be_bytes().to_vec();
        out.extend_from_slice(&(month as u16).to_be_bytes());
        Ok(vec![out])
    }
    fn identifier(&self) -> u8 {
        IDENT_MONTH
    }
    fn is_sortable(&self) -> bool {
        true
    }
    fn is_lossy(&self) -> bool {
        true
    }
}

impl Tokenizer for DayTokenizer {
    fn name(&self) -> &'static str {
        "day"
    }
    fn tokenizer_type(&self) -> &'static str {
        "datetime"
    }
    fn tokens(&self, value: &Val) -> Result<Vec<Vec<u8>>, TokError> {
        let (year, month, day, _) = datetime_parts(value, "day")?;
        let mut out = (year as u16).to_be_bytes().to_vec();
        out.extend_from_slice(&(month as u16).to_be_bytes());
        out.extend_from_slice(&(day as u16).to_be_bytes());
        Ok(vec![out])
    }
    fn identifier(&self) -> u8 {
        IDENT_DAY
    }
    fn is_sortable(&self) -> bool {
        true
    }
    fn is_lossy(&self) -> bool {
        true
    }
}

impl Tokenizer for HourTokenizer {
    fn name(&self) -> &'static str {
        "hour"
    }
    fn tokenizer_type(&self) -> &'static str {
        "datetime"
    }
    fn tokens(&self, value: &Val) -> Result<Vec<Vec<u8>>, TokError> {
        let (year, month, day, hour) = datetime_parts(value, "hour")?;
        let mut out = (year as u16).to_be_bytes().to_vec();
        out.extend_from_slice(&(month as u16).to_be_bytes());
        out.extend_from_slice(&(day as u16).to_be_bytes());
        out.extend_from_slice(&(hour as u16).to_be_bytes());
        Ok(vec![out])
    }
    fn identifier(&self) -> u8 {
        IDENT_HOUR
    }
    fn is_sortable(&self) -> bool {
        true
    }
    fn is_lossy(&self) -> bool {
        true
    }
}

impl Tokenizer for GeoTokenizer {
    fn name(&self) -> &'static str {
        "geo"
    }
    fn tokenizer_type(&self) -> &'static str {
        "geo"
    }
    fn tokens(&self, value: &Val) -> Result<Vec<Vec<u8>>, TokError> {
        match &value.value {
            Value::Geo(geom) => datastreamdb_types::geo_index::index_geo_tokens(geom)
                .map(|tokens| tokens.into_iter().map(String::into_bytes).collect())
                .map_err(|e| TokError::Geo(e.to_string())),
            _ => Err(mistyped("geo", value)),
        }
    }
    fn identifier(&self) -> u8 {
        IDENT_GEO
    }
    fn is_sortable(&self) -> bool {
        false
    }
    fn is_lossy(&self) -> bool {
        true
    }
}

/// External plugin tokenizer. Rust has no Go-style `.so` plugins; implement
/// this trait in-process and register it (ids must be `>= 0x80`).
pub trait CustomTokenizer: Send + Sync {
    /// Unique name.
    fn name(&self) -> &'static str;
    /// Type id served (must be a known [`TypeId`] name).
    fn tokenizer_type(&self) -> &'static str;
    /// Raw tokens.
    ///
    /// # Errors
    ///
    /// Returns [`TokError`] on duplicates, low ids, or unknown types.
    fn tokens(&self, value: &Val) -> Result<Vec<Vec<u8>>, TokError>;
    /// Identifier byte (`>= 0x80`).
    fn identifier(&self) -> u8;
}

/// Adapter giving custom tokenizers fixed flags (Go `CustomTokenizer`).
pub struct CustomAdapter {
    inner: std::sync::Arc<dyn CustomTokenizer>,
}

impl Tokenizer for CustomAdapter {
    fn name(&self) -> &'static str {
        self.inner.name()
    }
    fn tokenizer_type(&self) -> &'static str {
        self.inner.tokenizer_type()
    }
    fn tokens(&self, value: &Val) -> Result<Vec<Vec<u8>>, TokError> {
        self.inner.tokens(value)
    }
    fn identifier(&self) -> u8 {
        self.inner.identifier()
    }
    fn is_sortable(&self) -> bool {
        false
    }
    fn is_lossy(&self) -> bool {
        true
    }
}

type Registry = HashMap<String, std::sync::Arc<dyn Tokenizer>>;

fn registry() -> &'static RwLock<Registry> {
    static REGISTRY: std::sync::OnceLock<RwLock<Registry>> = std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut map: Registry = HashMap::new();
        let builtins: Vec<std::sync::Arc<dyn Tokenizer>> = vec![
            std::sync::Arc::new(BigFloatTokenizer),
            std::sync::Arc::new(GeoTokenizer),
            std::sync::Arc::new(IntTokenizer),
            std::sync::Arc::new(FloatTokenizer),
            std::sync::Arc::new(YearTokenizer),
            std::sync::Arc::new(HourTokenizer),
            std::sync::Arc::new(MonthTokenizer),
            std::sync::Arc::new(DayTokenizer),
            std::sync::Arc::new(super::text::ExactTokenizer::default()),
            std::sync::Arc::new(BoolTokenizer),
            std::sync::Arc::new(super::text::TrigramTokenizer),
            std::sync::Arc::new(super::text::HashTokenizer),
            std::sync::Arc::new(super::text::TermTokenizer::default()),
            std::sync::Arc::new(super::text::FullTextTokenizer::default()),
            std::sync::Arc::new(super::text::NGramTokenizer::default()),
            std::sync::Arc::new(super::text::Sha256Tokenizer),
        ];
        for tokenizer in builtins {
            assert!(TypeId::for_name(tokenizer.tokenizer_type()).is_some());
            map.insert(tokenizer.name().to_owned(), tokenizer);
        }
        RwLock::new(map)
    })
}

/// Looks up a tokenizer by name.
///
/// # Panics
///
/// Panics if the tokenizer registry lock is poisoned.
#[must_use]
pub fn get_tokenizer(name: &str) -> Option<std::sync::Arc<dyn Tokenizer>> {
    registry().read().expect("registry").get(name).cloned()
}

/// Looks up a tokenizer by identifier byte.
///
/// # Panics
///
/// Panics if the tokenizer registry lock is poisoned.
#[must_use]
pub fn get_tokenizer_by_id(id: u8) -> Option<std::sync::Arc<dyn Tokenizer>> {
    registry()
        .read()
        .expect("registry")
        .values()
        .find(|t| t.identifier() == id)
        .cloned()
}

/// Looks up tokenizers by names.
///
/// # Errors
///
/// Returns [`TokError::UnknownName`] for the first missing name.
pub fn get_tokenizers(names: &[&str]) -> Result<Vec<std::sync::Arc<dyn Tokenizer>>, TokError> {
    names
        .iter()
        .map(|name| get_tokenizer(name).ok_or_else(|| TokError::UnknownName((*name).to_owned())))
        .collect()
}

/// Registers a custom tokenizer (replaces Go's `.so` plugin loading).
///
/// # Errors
///
/// Returns [`TokError`] on duplicate names, low ids, or unknown types.
///
/// # Panics
///
/// Panics if the tokenizer registry lock is poisoned.
pub fn register_custom_tokenizer(
    tokenizer: std::sync::Arc<dyn CustomTokenizer>,
) -> Result<(), TokError> {
    if tokenizer.identifier() < IDENT_CUSTOM {
        return Err(TokError::CustomIdTooLow(tokenizer.identifier()));
    }
    if TypeId::for_name(tokenizer.tokenizer_type()).is_none() {
        return Err(TokError::CustomType(
            tokenizer.tokenizer_type().to_owned(),
            tokenizer.name().to_owned(),
        ));
    }
    let mut registry = registry().write().expect("registry");
    if registry.contains_key(tokenizer.name()) {
        return Err(TokError::Duplicate(tokenizer.name().to_owned()));
    }
    let name = tokenizer.name().to_owned();
    registry.insert(
        name,
        std::sync::Arc::new(CustomAdapter { inner: tokenizer }),
    );
    Ok(())
}

/// Is `value` a string for the given tokenizer, else `TokError::Mistyped`?
///
/// # Errors
///
/// Returns [`TokError::Mistyped`] for non-string values.
pub fn require_string<'a>(tokenizer: &'static str, value: &'a Val) -> Result<&'a str, TokError> {
    match &value.value {
        Value::String(s) => Ok(s),
        _ => Err(mistyped(tokenizer, value)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use datastreamdb_types::Value;

    fn val(tid: TypeId, value: Value) -> Val {
        Val { tid, value }
    }

    #[test]
    fn int_encoding_vectors() {
        // Port of TestIntEncoding.
        assert_eq!(
            encode_int(0x0102_0304_0506_0708),
            [1, 1, 2, 3, 4, 5, 6, 7, 8]
        );
        assert_eq!(encode_int(0), [1, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(encode_int(-1), [0, 255, 255, 255, 255, 255, 255, 255, 255]);
        // Ordering preserved across the sign boundary.
        assert!(encode_int(-2) < encode_int(-1));
        assert!(encode_int(-1) < encode_int(0));
        assert!(encode_int(0) < encode_int(1));
        assert!(encode_int(i64::MIN) < encode_int(i64::MAX));
    }

    #[test]
    fn identifiers_are_unique_and_pinned() {
        assert_eq!(
            [
                IDENT_NONE,
                IDENT_TERM,
                IDENT_EXACT,
                IDENT_EXACT_LANG,
                IDENT_YEAR,
                IDENT_MONTH,
                IDENT_DAY,
                IDENT_HOUR,
                IDENT_GEO,
                IDENT_INT,
                IDENT_FLOAT,
                IDENT_FULLTEXT,
                IDENT_BOOL,
                IDENT_TRIGRAM,
                IDENT_HASH,
                IDENT_SHA,
                IDENT_BIGFLOAT,
                IDENT_VFLOAT,
                IDENT_NGRAM,
            ],
            [
                0x0, 0x1, 0x2, 0x3, 0x4, 0x41, 0x42, 0x43, 0x5, 0x6, 0x7, 0x8, 0x9, 0xA, 0xB, 0xC,
                0xD, 0xE, 0xF
            ]
        );
    }

    #[test]
    fn scalar_tokens() {
        let int = IntTokenizer;
        assert_eq!(
            int.tokens(&val(TypeId::Int, Value::Int(42)))
                .expect("tokens")[0],
            encode_int(42)
        );
        assert!(
            int.tokens(&val(TypeId::String, Value::String("x".to_owned())))
                .is_err()
        );
        let float = FloatTokenizer;
        assert_eq!(
            float
                .tokens(&val(TypeId::Float, Value::Float(3.99)))
                .expect("tokens")[0],
            encode_int(3)
        );
        let boolean = BoolTokenizer;
        assert_eq!(
            boolean
                .tokens(&val(TypeId::Bool, Value::Bool(true)))
                .expect("tokens")[0],
            encode_int(1)
        );
    }

    #[test]
    fn registry_resolves() {
        assert!(get_tokenizer("int").is_some());
        assert!(get_tokenizer("nope").is_none());
        assert_eq!(get_tokenizer_by_id(IDENT_INT).expect("id").name(), "int");
        assert!(get_tokenizer_by_id(0x7F).is_none());
        assert!(get_tokenizers(&["int", "term"]).is_ok());
        assert!(
            matches!(get_tokenizers(&["int", "nope"]), Err(TokError::UnknownName(name)) if name == "nope")
        );
        assert_eq!(
            get_tokens(IDENT_INT, &[]).expect_err("arity"),
            TokError::Arity(0)
        );
        assert_eq!(
            get_tokens(0x7F, &[val(TypeId::Int, Value::Int(1))]).expect_err("id"),
            TokError::UnknownId(0x7F)
        );
    }
}
