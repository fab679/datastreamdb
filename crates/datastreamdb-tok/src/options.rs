//! Index options: typed maps + allowed-option parsing. Port of `tok/options`.

#![warn(missing_docs)]

use std::{collections::HashMap, sync::Arc};

/// A typed option value.
#[derive(Clone, Debug, PartialEq)]
pub enum OptionValue {
    /// Integer option.
    Int(i64),
    /// Float option.
    Float(f64),
    /// String option.
    Str(String),
    /// Boolean option.
    Bool(bool),
}

/// Options map (plus type-erased objects for interface options).
#[derive(Debug, Default)]
pub struct Options {
    values: HashMap<String, OptionValue>,
    objects: HashMap<String, Arc<dyn std::any::Any + Send + Sync>>,
}

/// Options error.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum OptionsError {
    /// Stored value has a different type than requested.
    #[error("option {key:?} has incompatible type")]
    TypeMismatch {
        /// Option key.
        key: String,
    },
    /// Option is not in the allowed set.
    #[error("option {0} is not allowed!")]
    NotAllowed(String),
    /// Value fails to parse.
    #[error("cannot parse option {key:?} value {value:?}: {reason}")]
    Parse {
        /// Option key.
        key: String,
        /// Raw value.
        value: String,
        /// Cause.
        reason: String,
    },
}

impl Options {
    /// Empty options.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets a typed option.
    pub fn set_opt(&mut self, key: &str, value: impl Into<OptionValue>) {
        self.values.insert(key.to_owned(), value.into());
    }

    /// Sets a type-erased object option.
    pub fn set_object<T: Send + Sync + 'static>(&mut self, key: &str, value: Arc<T>) {
        self.objects.insert(key.to_owned(), value);
    }

    fn get(&self, key: &str) -> Option<&OptionValue> {
        self.values.get(key)
    }
}

impl From<i64> for OptionValue {
    fn from(value: i64) -> Self {
        Self::Int(value)
    }
}

impl From<i32> for OptionValue {
    fn from(value: i32) -> Self {
        Self::Int(i64::from(value))
    }
}

impl From<f64> for OptionValue {
    fn from(value: f64) -> Self {
        Self::Float(value)
    }
}

impl From<String> for OptionValue {
    fn from(value: String) -> Self {
        Self::Str(value)
    }
}

impl From<&str> for OptionValue {
    fn from(value: &str) -> Self {
        Self::Str(value.to_owned())
    }
}

impl From<bool> for OptionValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

/// Types retrievable from [`Options`].
pub trait OptionValueFrom: Sized {
    /// Extracts from a stored value (type-checked).
    fn from_option(value: &OptionValue) -> Option<Self>;
}

impl OptionValueFrom for i64 {
    fn from_option(value: &OptionValue) -> Option<Self> {
        match value {
            OptionValue::Int(v) => Some(*v),
            _ => None,
        }
    }
}

impl OptionValueFrom for f64 {
    fn from_option(value: &OptionValue) -> Option<Self> {
        match value {
            OptionValue::Float(v) => Some(*v),
            _ => None,
        }
    }
}

impl OptionValueFrom for String {
    fn from_option(value: &OptionValue) -> Option<Self> {
        match value {
            OptionValue::Str(v) => Some(v.clone()),
            _ => None,
        }
    }
}

impl OptionValueFrom for bool {
    fn from_option(value: &OptionValue) -> Option<Self> {
        match value {
            OptionValue::Bool(v) => Some(*v),
            _ => None,
        }
    }
}

/// Reads a typed option: `(value, found, error)`. Missing keys yield
/// `(default, false, None)`; mistyped keys yield `(default, true, Some(_))`.
/// Port of `GetOpt`.
#[must_use]
pub fn get_opt<T: OptionValueFrom>(
    opts: &Options,
    key: &str,
    default: T,
) -> (T, bool, Option<OptionsError>) {
    match opts.get(key) {
        None => (default, false, None),
        Some(stored) => match T::from_option(stored) {
            Some(value) => (value, true, None),
            None => (
                default,
                true,
                Some(OptionsError::TypeMismatch {
                    key: key.to_owned(),
                }),
            ),
        },
    }
}

/// Reads a type-erased object option. Port of `GetInterfaceOpt`.
#[must_use]
pub fn get_object<T: Send + Sync + 'static>(opts: &Options, key: &str) -> Option<Arc<T>> {
    opts.objects
        .get(key)
        .and_then(|object| object.clone().downcast::<T>().ok())
}

/// Parses one option value from text.
pub type OptionParser = fn(&str) -> Result<OptionValue, OptionsError>;

/// Integer option parser.
///
/// # Errors
///
/// Returns [`OptionsError::Parse`] on malformed integers.
pub fn int_parser(text: &str) -> Result<OptionValue, OptionsError> {
    text.parse::<i64>()
        .map(OptionValue::Int)
        .map_err(|e| OptionsError::Parse {
            key: String::new(),
            value: text.to_owned(),
            reason: e.to_string(),
        })
}

/// Float option parser.
///
/// # Errors
///
/// Returns [`OptionsError::Parse`] on malformed floats.
pub fn float_parser(text: &str) -> Result<OptionValue, OptionsError> {
    text.parse::<f64>()
        .map(OptionValue::Float)
        .map_err(|e| OptionsError::Parse {
            key: String::new(),
            value: text.to_owned(),
            reason: e.to_string(),
        })
}

/// String option parser.
///
/// # Errors
///
/// Never errors; kept fallible for parser uniformity.
pub fn string_parser(text: &str) -> Result<OptionValue, OptionsError> {
    Ok(OptionValue::Str(text.to_owned()))
}

/// Boolean option parser.
///
/// # Errors
///
/// Returns [`OptionsError::Parse`] for non-bool text.
pub fn bool_parser(text: &str) -> Result<OptionValue, OptionsError> {
    match text {
        "true" => Ok(OptionValue::Bool(true)),
        "false" => Ok(OptionValue::Bool(false)),
        _ => Err(OptionsError::Parse {
            key: String::new(),
            value: text.to_owned(),
            reason: "expected true/false".to_owned(),
        }),
    }
}

/// Allowed options with parsers. Port of `AllowedOptions`.
#[derive(Clone, Default)]
pub struct AllowedOptions {
    parsers: HashMap<String, OptionParser>,
}

impl AllowedOptions {
    /// Empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an integer option.
    #[must_use]
    pub fn add_int_option(mut self, name: &str) -> Self {
        self.parsers
            .insert(name.to_owned(), int_parser as OptionParser);
        self
    }

    /// Adds a string option.
    #[must_use]
    pub fn add_string_option(mut self, name: &str) -> Self {
        self.parsers
            .insert(name.to_owned(), string_parser as OptionParser);
        self
    }

    /// Adds a float option.
    #[must_use]
    pub fn add_float_option(mut self, name: &str) -> Self {
        self.parsers
            .insert(name.to_owned(), float_parser as OptionParser);
        self
    }

    /// Adds a boolean option.
    #[must_use]
    pub fn add_bool_option(mut self, name: &str) -> Self {
        self.parsers
            .insert(name.to_owned(), bool_parser as OptionParser);
        self
    }

    /// Adds a custom-parsed option.
    #[must_use]
    pub fn add_custom_option(mut self, name: &str, parser: OptionParser) -> Self {
        self.parsers.insert(name.to_owned(), parser);
        self
    }

    /// Names in the set.
    #[must_use]
    pub fn option_list(&self) -> Vec<String> {
        self.parsers.keys().cloned().collect()
    }

    /// Parses one value with the named parser.
    ///
    /// # Errors
    ///
    /// Returns [`OptionsError`] for unknown options or bad values.
    pub fn parsed_option(&self, name: &str, value: &str) -> Result<OptionValue, OptionsError> {
        let parser = self
            .parsers
            .get(name)
            .ok_or_else(|| OptionsError::NotAllowed(name.to_owned()))?;
        parser(value).map_err(|mut err| {
            if let OptionsError::Parse { key, .. } = &mut err {
                if key.is_empty() {
                    name.clone_into(key);
                }
            }
            err
        })
    }

    /// Parses pairs into `opts`, failing on the first bad pair.
    ///
    /// # Errors
    ///
    /// Returns [`OptionsError`] for unknown options or bad values.
    pub fn populate(&self, pairs: &[(&str, &str)], opts: &mut Options) -> Result<(), OptionsError> {
        for &(key, value) in pairs {
            let parsed = self.parsed_option(key, value)?;
            opts.values.insert(key.to_owned(), parsed);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_options() {
        // Port of TestSimpleOptions.
        let mut opts = Options::new();
        opts.set_opt("fish", 17i64);
        let (floatval, found, err) = get_opt(&opts, "fish", 32.2f64);
        assert!((floatval - 32.2).abs() < f64::EPSILON);
        assert!(found);
        assert!(err.is_some());
        let (intval, found, err) = get_opt(&opts, "fish", 0i64);
        assert_eq!(intval, 17);
        assert!(found);
        assert!(err.is_none());
        let (stringval, found, err) = get_opt(&opts, "cat", "fishcat".to_owned());
        assert_eq!(stringval, "fishcat");
        assert!(!found);
        assert!(err.is_none());
    }

    #[test]
    fn object_options() {
        // Port of TestInterfaceOptions (adapted to typed handles).
        struct FooBar;
        let mut opts = Options::new();
        opts.set_object("foob", Arc::new(FooBar));
        assert!(get_object::<FooBar>(&opts, "foob").is_some());
        assert!(get_object::<FooBar>(&opts, "barb").is_none());
        assert!(get_object::<String>(&opts, "foob").is_none());
    }

    #[test]
    fn allowed_options_parse() {
        let allowed = AllowedOptions::new()
            .add_int_option("layers")
            .add_float_option("threshold")
            .add_string_option("metric")
            .add_bool_option("exact");
        assert_eq!(
            allowed.parsed_option("layers", "4").expect("parse"),
            OptionValue::Int(4)
        );
        assert!(allowed.parsed_option("nope", "1").is_err());
        assert!(allowed.parsed_option("layers", "x").is_err());
        let mut opts = Options::new();
        allowed
            .populate(&[("layers", "4"), ("metric", "cosine")], &mut opts)
            .expect("populate");
        assert_eq!(get_opt(&opts, "layers", 0i64).0, 4);
    }
}
