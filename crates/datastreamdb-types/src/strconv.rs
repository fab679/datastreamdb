//! Go `strconv` parity: integer/float/bool parsing with identical errors,
//! and Go `%G` / `%f` shortest float formatting.
//!
//! Documented deltas: hexadecimal float literals and underscores (accepted by
//! Go's `ParseFloat`) are rejected; everything asserted by Go's tests matches.

#![warn(missing_docs)]

/// Integer parse error with Go's message shape.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ParseIntError {
    /// Not an integer literal.
    #[error("strconv.ParseInt: parsing {input:?}: invalid syntax")]
    Syntax {
        /// Offending input.
        input: String,
    },
    /// Outside `i64` range.
    #[error("strconv.ParseInt: parsing {input:?}: value out of range")]
    Range {
        /// Offending input.
        input: String,
    },
}

/// Parses like Go's `strconv.ParseInt(s, 10, 64)`.
///
/// # Errors
///
/// Returns [`ParseIntError`] with Go's message shape on bad syntax or overflow.
pub fn parse_go_int(s: &str) -> Result<i64, ParseIntError> {
    if s.is_empty() {
        return Err(ParseIntError::Syntax {
            input: s.to_owned(),
        });
    }
    let (negative, digits) = match s.as_bytes()[0] {
        b'+' => (false, &s[1..]),
        b'-' => (true, &s[1..]),
        _ => (false, s),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ParseIntError::Syntax {
            input: s.to_owned(),
        });
    }
    // Accumulate as u64 to detect overflow exactly like Go.
    let mut value: u64 = 0;
    for b in digits.bytes() {
        let d = u64::from(b - b'0');
        value = value
            .checked_mul(10)
            .and_then(|v| v.checked_add(d))
            .ok_or_else(|| ParseIntError::Range {
                input: s.to_owned(),
            })?;
    }
    if negative {
        if value > i64::MAX as u64 + 1 {
            return Err(ParseIntError::Range {
                input: s.to_owned(),
            });
        }
        Ok(value.wrapping_neg() as i64)
    } else {
        if value > i64::MAX as u64 {
            return Err(ParseIntError::Range {
                input: s.to_owned(),
            });
        }
        Ok(value as i64)
    }
}

/// Float parse error with Go's message shape.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ParseFloatError {
    /// Not a float literal.
    #[error("strconv.ParseFloat: parsing {input:?}: invalid syntax")]
    Syntax {
        /// Offending input.
        input: String,
    },
    /// Finite literal overflowing to infinity.
    #[error("strconv.ParseFloat: parsing {input:?}: value out of range")]
    Range {
        /// Offending input.
        input: String,
    },
}

/// Parses like Go's `strconv.ParseFloat(s, 64)` (decimal forms).
///
/// # Errors
///
/// Returns [`ParseFloatError`] with Go's message shape on bad syntax or
/// overflow (`1e400`).
pub fn parse_go_float(s: &str) -> Result<f64, ParseFloatError> {
    let syntax = || ParseFloatError::Syntax {
        input: s.to_owned(),
    };
    if s.is_empty() {
        return Err(syntax());
    }
    // Go accepts case-insensitive inf/infinity/nan (with sign).
    let lower = s.to_ascii_lowercase();
    let unsigned = lower.strip_prefix(['+', '-']).unwrap_or(&lower);
    if unsigned == "inf" || unsigned == "infinity" {
        return Ok(if s.starts_with('-') {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        });
    }
    if unsigned == "nan" {
        return Ok(f64::NAN);
    }
    // Rust's parser covers Go's decimal forms; reject the rest explicitly.
    if s.contains(['_', 'x', 'X', 'p', 'P']) {
        return Err(syntax());
    }
    // Go accepts a trailing-dot mantissa ("1.", "1.e5"); normalize for Rust.
    let mut owned;
    let text = if trailing_dot_mantissa(s) {
        owned = s.to_owned();
        let at = owned.find(['e', 'E']).unwrap_or(owned.len());
        owned.insert(at, '0');
        owned.as_str()
    } else {
        s
    };
    match text.parse::<f64>() {
        Ok(v) if v.is_infinite() => Err(ParseFloatError::Range {
            input: s.to_owned(),
        }),
        Ok(v) => Ok(v),
        Err(_) => Err(syntax()),
    }
}

/// Whether the mantissa ends with a bare `.` before the end/exponent.
fn trailing_dot_mantissa(s: &str) -> bool {
    let mantissa = match s.find(['e', 'E']) {
        Some(e) => &s[..e],
        None => s,
    };
    mantissa.ends_with('.') && mantissa.len() > 1
}

/// Bool parse error with Go's message shape.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("strconv.ParseBool: parsing {input:?}: invalid syntax")]
pub struct ParseBoolError {
    /// Offending input.
    pub input: String,
}

/// Parses like Go's `strconv.ParseBool`: `1 t T TRUE true True` are true;
/// `0 f F FALSE false False` are false.
///
/// # Errors
///
/// Returns [`ParseBoolError`] for anything else.
pub fn parse_go_bool(s: &str) -> Result<bool, ParseBoolError> {
    match s {
        "1" | "t" | "T" | "TRUE" | "true" | "True" => Ok(true),
        "0" | "f" | "F" | "FALSE" | "false" | "False" => Ok(false),
        _ => Err(ParseBoolError {
            input: s.to_owned(),
        }),
    }
}

/// Shortest decimal digits + decimal exponent for a finite nonzero float:
/// `value = 0.digits × 10^exp`, digits without leading/trailing zeros.
fn shortest_parts(v: f64) -> (Vec<u8>, i32) {
    // ryu gives the shortest round-trip digits; normalize to (digits, exp).
    let text = ryu::Buffer::new().format(v).to_owned();
    let mut digits: Vec<u8> = Vec::new();
    let mut point: i32 = 0; // digits before the decimal point
    let mut exp10: i32 = 0;
    let mut seen_point = false;
    let mut mantissa = text.as_str();
    if let Some(e) = text.find(['e', 'E']) {
        exp10 = text[e + 1..].parse::<i32>().unwrap_or(0);
        mantissa = &text[..e];
    }
    for b in mantissa.bytes() {
        if b == b'.' {
            seen_point = true;
        } else if b.is_ascii_digit() {
            if !seen_point {
                point += 1;
            }
            digits.push(b);
        }
    }
    while digits.len() > 1 && digits.last() == Some(&b'0') {
        digits.pop();
    }
    while digits.len() > 1 && digits.first() == Some(&b'0') {
        digits.remove(0);
        point -= 1;
    }
    (digits, point + exp10)
}

/// Formats like Go's `FormatFloat(v, 'G', -1, 64)`.
#[must_use]
pub fn format_float_g(v: f64) -> String {
    if v.is_nan() {
        return "NaN".to_owned();
    }
    if v.is_infinite() {
        return if v > 0.0 {
            "+Inf".to_owned()
        } else {
            "-Inf".to_owned()
        };
    }
    if v == 0.0 {
        return if v.is_sign_negative() {
            "-0".to_owned()
        } else {
            "0".to_owned()
        };
    }
    let sign = if v.is_sign_negative() { "-" } else { "" };
    let (digits, exp) = shortest_parts(v.abs());
    let text: String = digits.iter().map(|&b| b as char).collect();
    // Go %G (shortest): exponential when exp-1 < -4 or exp-1 >= 21, i.e. the
    // decimal exponent of d.ddd×10^(exp-1) outside [-4, 21).
    let dec_exp = exp - 1;
    if !(-4..21).contains(&dec_exp) {
        let mut mantissa = text;
        if mantissa.len() > 1 {
            mantissa.insert(1, '.');
        }
        return format!("{sign}{mantissa}E{dec_exp:+03}");
    }
    // Plain form: place the point at exp.
    if exp <= 0 {
        format!("{sign}0.{zeros}{text}", zeros = "0".repeat((-exp) as usize))
    } else if (exp as usize) >= text.len() {
        format!(
            "{sign}{text}{zeros}",
            zeros = "0".repeat(exp as usize - text.len())
        )
    } else {
        format!("{sign}{}.{}", &text[..exp as usize], &text[exp as usize..])
    }
}

/// Formats an `f32` like Go's `FormatFloat(f64(v), 'f', -1, 32)`: shortest
/// digits, never exponential.
#[must_use]
pub fn format_float_f32(v: f32) -> String {
    if v.is_nan() {
        return "NaN".to_owned();
    }
    if v.is_infinite() {
        return if v > 0.0 {
            "+Inf".to_owned()
        } else {
            "-Inf".to_owned()
        };
    }
    if v == 0.0 {
        return if v.is_sign_negative() {
            "-0".to_owned()
        } else {
            "0".to_owned()
        };
    }
    // Shortest f32 digits: round-trip through ryu at f32 precision.
    let text = ryu::Buffer::new().format(v).to_owned();
    let mut digits = String::new();
    let mut point: i32 = 0;
    let mut exp10: i32 = 0;
    let mut mantissa = text.as_str();
    if let Some(e) = text.find(['e', 'E']) {
        exp10 = text[e + 1..].parse::<i32>().unwrap_or(0);
        mantissa = &text[..e];
    }
    let mut seen_point = false;
    for b in mantissa.bytes() {
        if b == b'.' {
            seen_point = true;
        } else if b.is_ascii_digit() {
            if !seen_point {
                point += 1;
            }
            digits.push(b as char);
        }
    }
    while digits.len() > 1 && digits.ends_with('0') {
        digits.pop();
    }
    while digits.len() > 1 && digits.starts_with('0') {
        digits.remove(0);
        point -= 1;
    }
    let sign = if v.is_sign_negative() { "-" } else { "" };
    let exp = point + exp10;
    if exp <= 0 {
        format!(
            "{sign}0.{zeros}{digits}",
            zeros = "0".repeat((-exp) as usize)
        )
    } else if (exp as usize) >= digits.len() {
        format!(
            "{sign}{digits}{zeros}",
            zeros = "0".repeat(exp as usize - digits.len())
        )
    } else {
        format!(
            "{sign}{}.{}",
            &digits[..exp as usize],
            &digits[exp as usize..]
        )
    }
}

/// Unsigned parse error with Go's message shape.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ParseUintError {
    /// Not an unsigned integer literal.
    #[error("strconv.ParseUint: parsing {input:?}: invalid syntax")]
    Syntax {
        /// Offending input.
        input: String,
    },
    /// Outside `u64` range.
    #[error("strconv.ParseUint: parsing {input:?}: value out of range")]
    Range {
        /// Offending input.
        input: String,
    },
}

/// Detects the base and strips the prefix of a base-0 literal, returning
/// `(base, digits)` over the unsigned text (sign already removed).
fn split_base0(s: &str) -> (u32, &str) {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'0' {
        match bytes[1] {
            b'x' | b'X' => return (16, &s[2..]),
            b'o' | b'O' => return (8, &s[2..]),
            b'b' | b'B' => return (2, &s[2..]),
            _ => {}
        }
    }
    if bytes.len() > 1 && bytes[0] == b'0' {
        return (8, s);
    }
    (10, s)
}

/// Whether the underscores in an unsigned base-0 literal are placed legally.
/// Exact port of Go's `underscoreOK`: underscores must follow the base prefix
/// or a digit and precede a digit; only `0-9` (plus `a-f` for hex) count.
fn underscore_ok(unsigned: &str) -> bool {
    let bytes = unsigned.as_bytes();
    let mut i = 0;
    let mut saw_digit_or_prefix = false;
    let mut saw_underscore = false;
    let mut hex = false;
    // Optional base prefix (counts as digit context for a following underscore).
    if bytes.len() >= 2 && bytes[0] == b'0' {
        match bytes[1] {
            b'x' | b'X' => {
                i = 2;
                saw_digit_or_prefix = true;
                saw_underscore = false;
                hex = true;
            }
            b'o' | b'O' | b'b' | b'B' => {
                i = 2;
                saw_digit_or_prefix = true;
                saw_underscore = false;
            }
            _ => {}
        }
    }
    while i < bytes.len() {
        let c = bytes[i];
        let is_digit = c.is_ascii_digit() || (hex && matches!(c, b'a'..=b'f' | b'A'..=b'F'));
        if is_digit {
            saw_digit_or_prefix = true;
            saw_underscore = false;
        } else if c == b'_' {
            if !saw_digit_or_prefix {
                return false;
            }
            saw_digit_or_prefix = false;
            saw_underscore = true;
        } else {
            // Non-digit, non-underscore: the digit parser rejects it later.
            if saw_underscore {
                return false;
            }
            saw_digit_or_prefix = false;
            saw_underscore = false;
        }
        i += 1;
    }
    !saw_underscore
}

/// Parses like Go's `strconv.ParseInt(s, 0, 64)`.
///
/// Accepts `0x`/`0o`/`0b` prefixes, a leading `0` for octal, signs, and Go
/// integer-literal underscores.
///
/// # Errors
///
/// Returns [`ParseIntError`] with Go's message shape on bad syntax or overflow.
pub fn parse_go_int_base0(s: &str) -> Result<i64, ParseIntError> {
    let syntax = || ParseIntError::Syntax {
        input: s.to_owned(),
    };
    let range = || ParseIntError::Range {
        input: s.to_owned(),
    };
    let (negative, unsigned) = match s.as_bytes().first() {
        Some(b'+') => (false, &s[1..]),
        Some(b'-') => (true, &s[1..]),
        _ => (false, s),
    };
    if unsigned.is_empty() {
        return Err(syntax());
    }
    if unsigned.contains('_') && !underscore_ok(unsigned) {
        return Err(syntax());
    }
    let (base, digits) = split_base0(unsigned);
    let digits = digits.replace('_', "");
    if digits.is_empty() {
        return Err(syntax());
    }
    let value = u64::from_str_radix(&digits, base).map_err(|e| {
        use std::num::IntErrorKind::{NegOverflow, PosOverflow};
        match e.kind() {
            PosOverflow | NegOverflow => range(),
            _ => syntax(),
        }
    })?;
    if negative {
        if value > i64::MAX as u64 + 1 {
            return Err(range());
        }
        Ok(value.wrapping_neg() as i64)
    } else {
        if value > i64::MAX as u64 {
            return Err(range());
        }
        Ok(value as i64)
    }
}

/// Parses like Go's `strconv.ParseUint(s, 0, 64)`.
///
/// Accepts `0x`/`0o`/`0b` prefixes, a leading `0` for octal, and Go
/// integer-literal underscores. A sign prefix is rejected, like Go.
///
/// # Errors
///
/// Returns [`ParseUintError`] with Go's message shape on bad syntax or overflow.
pub fn parse_go_uint_base0(s: &str) -> Result<u64, ParseUintError> {
    let syntax = || ParseUintError::Syntax {
        input: s.to_owned(),
    };
    let range = || ParseUintError::Range {
        input: s.to_owned(),
    };
    if s.is_empty() || s.starts_with(['+', '-']) {
        return Err(syntax());
    }
    if s.contains('_') && !underscore_ok(s) {
        return Err(syntax());
    }
    let (base, digits) = split_base0(s);
    let digits = digits.replace('_', "");
    if digits.is_empty() {
        return Err(syntax());
    }
    u64::from_str_radix(&digits, base).map_err(|e| {
        use std::num::IntErrorKind::{NegOverflow, PosOverflow};
        match e.kind() {
            PosOverflow | NegOverflow => range(),
            _ => syntax(),
        }
    })
}

/// Quotes a string like Go's `strconv.Quote`.
///
/// Documented delta: non-ASCII format/control characters outside Rust's
/// `char::is_control` (e.g. zero-width spaces) are emitted raw where Go
/// would `\u`-escape them; both forms unquote to the same value.
#[must_use]
pub fn quote_go_string(s: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\u{07}' => out.push_str("\\a"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{0B}' => out.push_str("\\v"),
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ if ('\u{20}'..'\u{7F}').contains(&c) => out.push(c),
            _ if (c as u32) < 0x80 => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            _ if !c.is_control() => out.push(c),
            _ => {
                let v = c as u32;
                if v <= 0xFFFF {
                    let _ = write!(out, "\\u{v:04x}");
                } else {
                    let _ = write!(out, "\\U{v:08x}");
                }
            }
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int_vectors() {
        for (input, expected) in [
            ("1", 1i64),
            ("13816", 13816),
            ("-1221", -1221),
            ("0", 0),
            ("203716381366627", 203_716_381_366_627),
            ("+42", 42),
            ("9223372036854775807", i64::MAX),
            ("-9223372036854775808", i64::MIN),
        ] {
            assert_eq!(parse_go_int(input).as_ref(), Ok(&expected), "{input}");
        }
        for bad in ["", "srfrog", "3.0", "-3a.5", "1_0", "+", "--1"] {
            let err = parse_go_int(bad).expect_err(bad);
            assert_eq!(
                err.to_string(),
                format!("strconv.ParseInt: parsing {bad:?}: invalid syntax")
            );
        }
        for bad in [
            "9223372036854775808",
            "-9223372036854775809",
            "99999999999999999999999",
        ] {
            let err = parse_go_int(bad).expect_err(bad);
            assert_eq!(
                err.to_string(),
                format!("strconv.ParseInt: parsing {bad:?}: value out of range")
            );
        }
    }

    #[test]
    #[allow(clippy::float_cmp, reason = "parsing is deterministic; assert exact")]
    fn float_vectors() {
        for (input, expected) in [
            ("1", 1.0),
            ("13816.251", 13816.251),
            ("-1221.12", -1221.12),
            ("-0.0", 0.0),
            ("1e10", 1e10),
            ("1e-2", 0.01),
        ] {
            assert_eq!(parse_go_float(input).unwrap(), expected, "{input}");
        }
        assert!(parse_go_float("NaN").unwrap().is_nan());
        assert_eq!(
            parse_go_float("1e400").unwrap_err().to_string(),
            "strconv.ParseFloat: parsing \"1e400\": value out of range"
        );
        for bad in ["", "srfrog", "-3a.5"] {
            let err = parse_go_float(bad).expect_err(bad);
            assert_eq!(
                err.to_string(),
                format!("strconv.ParseFloat: parsing {bad:?}: invalid syntax")
            );
        }
    }

    #[test]
    fn bool_vectors() {
        // Port of TestConvertStringToBool vocabulary.
        for t in ["1", "true", "True", "T"] {
            assert_eq!(parse_go_bool(t), Ok(true));
        }
        for f in ["F", "0", "false", "False"] {
            assert_eq!(parse_go_bool(f), Ok(false));
        }
        for bad in ["", "srfrog", "3", "-3"] {
            let err = parse_go_bool(bad).expect_err(bad);
            assert_eq!(
                err.to_string(),
                format!("strconv.ParseBool: parsing {bad:?}: invalid syntax")
            );
        }
    }

    #[test]
    fn g_format_vectors() {
        // Go %G shortest semantics (spec-derived).
        for (value, expected) in [
            (13816.251, "13816.251"),
            (2.12, "2.12"),
            (1.0, "1"),
            (-3.5, "-3.5"),
            (0.0001, "0.0001"),
            (0.00001, "1E-05"),
            (1e20, "100000000000000000000"),
            (1e21, "1E+21"),
            (1.234e30, "1.234E+30"),
            (100.0, "100"),
        ] {
            assert_eq!(format_float_g(value), expected, "{value}");
        }
        assert_eq!(format_float_g(f64::NAN), "NaN");
        assert_eq!(format_float_g(f64::INFINITY), "+Inf");
        assert_eq!(format_float_g(f64::NEG_INFINITY), "-Inf");
    }

    #[test]
    fn f32_format_vectors() {
        for (value, expected) in [
            (1.5f32, "1.5"),
            (0.1, "0.1"),
            (100.0, "100"),
            (1e10, "10000000000"),
        ] {
            assert_eq!(format_float_f32(value), expected, "{value}");
        }
    }

    #[test]
    fn base0_int_vectors() {
        for (input, expected) in [
            ("0", 0i64),
            ("13", 13),
            ("013", 11), // leading zero means octal, like Go
            ("0x0D", 13),
            ("0XFF", 255),
            ("0o17", 15),
            ("0b101", 5),
            ("+42", 42),
            ("-0x10", -16),
            ("1_000", 1000),
            ("0x_1", 1),
            ("0b1_0", 2),
            ("9223372036854775807", i64::MAX),
            ("-9223372036854775808", i64::MIN),
        ] {
            assert_eq!(parse_go_int_base0(input).as_ref(), Ok(&expected), "{input}");
        }
        for bad in [
            "", "+", "-", "0x", "0b", "0o", "08", "0xG", "1_", "_1", "1__2", "0_x1", "1_2x", "3.0",
            "srfrog",
        ] {
            let err = parse_go_int_base0(bad).expect_err(bad);
            assert_eq!(
                err.to_string(),
                format!("strconv.ParseInt: parsing {bad:?}: invalid syntax")
            );
        }
        for bad in [
            "9223372036854775808",
            "-9223372036854775809",
            "0xFFFFFFFFFFFFFFFFFF",
        ] {
            let err = parse_go_int_base0(bad).expect_err(bad);
            assert_eq!(
                err.to_string(),
                format!("strconv.ParseInt: parsing {bad:?}: value out of range")
            );
        }
    }

    #[test]
    fn base0_uint_vectors() {
        for (input, expected) in [
            ("0", 0u64),
            ("10", 10),
            ("010", 8),
            ("0xf2", 0xf2),
            ("0X123", 0x123),
            ("0o17", 15),
            ("0b101", 5),
            ("1_000", 1000),
            ("18446744073709551615", u64::MAX),
        ] {
            assert_eq!(
                parse_go_uint_base0(input).as_ref(),
                Ok(&expected),
                "{input}"
            );
        }
        for bad in ["", "+", "-100", "+1", "0x", "08", "1_", "_1", "3.0", "0x1x"] {
            let err = parse_go_uint_base0(bad).expect_err(bad);
            assert_eq!(
                err.to_string(),
                format!("strconv.ParseUint: parsing {bad:?}: invalid syntax")
            );
        }
        // Overflow trips before the trailing bad digit, like Go.
        for bad in [
            "18446744073709551616",
            "0xa14222b693e4ba34123",
            "0xa14222b693e4ba3412x",
        ] {
            let err = parse_go_uint_base0(bad).expect_err(bad);
            assert_eq!(
                err.to_string(),
                format!("strconv.ParseUint: parsing {bad:?}: value out of range")
            );
        }
    }

    #[test]
    fn quote_vectors() {
        for (input, expected) in [
            ("", "\"\""),
            ("abc", "\"abc\""),
            ("a\nb", "\"a\\nb\""),
            ("a\tb\rc\u{0b}d", "\"a\\tb\\rc\\vd\""),
            ("say \"hi\"", "\"say \\\"hi\\\"\""),
            ("back\\slash", "\"back\\\\slash\""),
            ("a'b", "\"a'b\""),
            ("\u{01}", "\"\\x01\""),
            ("\u{7f}", "\"\\x7f\""),
            ("héllo 世界", "\"héllo 世界\""),
            ("bell\u{07}", "\"bell\\a\""),
        ] {
            assert_eq!(quote_go_string(input), expected, "{input:?}");
        }
    }
}
