//! Type ids, names, and datetime parsing. Port of `types/scalar_types.go`.
//!
//! Discriminants match `pb.Posting.ValType` and are frozen: stored data
//! depends on them.

#![warn(missing_docs)]

use std::{fmt, str::FromStr};

use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, TimeZone};

/// Nanoseconds per second, for datetime/float conversions.
pub const NANOS_PER_SEC: f64 = 1_000_000_000.0;
/// Nominal big-float precision in bits (Go parity constant).
pub const BIG_FLOAT_PRECISION: u32 = 200;

/// Scalar type ids. Discriminants match `pb.Posting.ValType`:
///
/// ```text
/// default=0 binary=1 int=2 float=3 bool=4 datetime=5 geo=6 uid=7
/// password=8 string=9 object=10 bigfloat=11 vfloat=12
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum TypeId {
    /// Default / no hint.
    Default = 0,
    /// Raw bytes.
    Binary = 1,
    /// 64-bit integer.
    Int = 2,
    /// 64-bit float.
    Float = 3,
    /// Boolean.
    Bool = 4,
    /// Timestamp.
    DateTime = 5,
    /// `GeoJSON` geometry.
    Geo = 6,
    /// UID reference.
    Uid = 7,
    /// Hashed password.
    Password = 8,
    /// UTF-8 string.
    String = 9,
    /// Structured object.
    Object = 10,
    /// Arbitrary precision float.
    BigFloat = 11,
    /// Float vector (`float32vector`).
    VFloat = 12,
    /// Sentinel for unknown types (never stored).
    Undefined = 100,
}

impl TypeId {
    /// Canonical schema name, e.g. `"datetime"`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Binary => "binary",
            Self::Int => "int",
            Self::Float => "float",
            Self::Bool => "bool",
            Self::DateTime => "datetime",
            Self::Geo => "geo",
            Self::Uid => "uid",
            Self::Password => "password",
            Self::String => "string",
            Self::Object => "object",
            Self::BigFloat => "bigfloat",
            Self::VFloat => "float32vector",
            Self::Undefined => "",
        }
    }

    /// All named types (excludes [`TypeId::Undefined`]).
    #[must_use]
    pub fn all() -> &'static [Self] {
        use TypeId::{
            BigFloat, Binary, Bool, DateTime, Default, Float, Geo, Int, Object, Password, String,
            Uid, VFloat,
        };
        &[
            Default, Binary, Int, Float, Bool, DateTime, Geo, Uid, Password, String, Object,
            BigFloat, VFloat,
        ]
    }

    /// Whether the type is scalar (everything but [`TypeId::Uid`]).
    #[must_use]
    pub fn is_scalar(self) -> bool {
        self != Self::Uid
    }

    /// Whether the type is numeric (int or float).
    #[must_use]
    pub fn is_number(self) -> bool {
        matches!(self, Self::Int | Self::Float)
    }

    /// Looks up a type by canonical name.
    #[must_use]
    pub fn for_name(name: &str) -> Option<Self> {
        Self::all().iter().copied().find(|t| t.as_str() == name)
    }
}

impl fmt::Display for TypeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error for [`TypeId::from_str`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("unknown scalar type: {0:?}")]
pub struct UnknownTypeId(String);

impl FromStr for TypeId {
    type Err = UnknownTypeId;

    /// Parses a canonical schema type name.
    ///
    /// # Errors
    ///
    /// Returns [`UnknownTypeId`] for names outside the type table.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::for_name(s).ok_or_else(|| UnknownTypeId(s.to_owned()))
    }
}

impl TryFrom<i32> for TypeId {
    type Error = UnknownTypeId;

    /// Converts a wire discriminant (matches `pb.Posting.ValType`).
    ///
    /// # Errors
    ///
    /// Returns [`UnknownTypeId`] for discriminants outside the type table.
    fn try_from(value: i32) -> Result<Self, Self::Error> {
        Self::all()
            .iter()
            .copied()
            .find(|t| *t as i32 == value)
            .ok_or_else(|| UnknownTypeId(value.to_string()))
    }
}

/// Whether a UTC offset in seconds is valid per RFC 3339 (±23:59).
#[must_use]
pub fn good_time_zone(offset_secs: i32) -> bool {
    const BOUNDARY: i32 = 23 * 60 * 60 + 59 * 60;
    (-BOUNDARY..=BOUNDARY).contains(&offset_secs)
}

/// Datetime parse error.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ParseTimeError {
    /// Input matched no supported format.
    #[error("cannot parse {input:?} as datetime")]
    Invalid {
        /// Offending input.
        input: String,
    },
    /// Numeric offset outside ±23:59.
    #[error("time zone offset hour out of range in {input:?}")]
    OffsetOutOfRange {
        /// Offending input.
        input: String,
    },
}

/// Parses a datetime, trying (like Go's `ParseTime`): `YYYY`, `YYYY-MM`,
/// `YYYY-MM-DD`, RFC 3339, `"2006-01-02 15:04:05 -0700 MST"`, then
/// `YYYY-MM-DDTHH:MM:SS` without a zone. Digit runs are length-checked like
/// Go's fixed layouts (chrono alone would accept short years).
///
/// # Errors
///
/// Returns [`ParseTimeError`] when no format matches or the zone offset is
/// out of range.
///
/// # Panics
///
/// Never panics on valid inputs; UTC offset construction is infallible.
pub fn parse_time(value: &str) -> Result<DateTime<FixedOffset>, ParseTimeError> {
    let invalid = || ParseTimeError::Invalid {
        input: value.to_owned(),
    };
    let utc = || FixedOffset::east_opt(0).expect("utc");
    // Year / year-month / date by length, like Go.
    if value.len() == 4 && value.as_bytes().iter().all(u8::is_ascii_digit) {
        if let Ok(date) = NaiveDate::parse_from_str(&format!("{value}-01-01"), "%Y-%m-%d") {
            return Ok(utc().from_utc_datetime(&date.and_hms_opt(0, 0, 0).expect("midnight")));
        }
        return Err(invalid());
    }
    if value.len() == 7 && is_date_prefix(&value[..7], true) {
        if let Ok(date) = NaiveDate::parse_from_str(&format!("{value}-01"), "%Y-%m-%d") {
            return Ok(utc().from_utc_datetime(&date.and_hms_opt(0, 0, 0).expect("midnight")));
        }
        return Err(invalid());
    }
    if value.len() == 10 && is_date_prefix(&value[..10], false) {
        if let Ok(date) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
            return Ok(utc().from_utc_datetime(&date.and_hms_opt(0, 0, 0).expect("midnight")));
        }
        return Err(invalid());
    }
    // RFC 3339 shape: longer than "2006-01-02T15:04:05" with 'T' at 10 and a
    // trailing 'Z' or a colon at len-3.
    if value.len() > 19
        && value.as_bytes().get(10) == Some(&b'T')
        && (value.ends_with('Z') || value.as_bytes().get(value.len() - 3) == Some(&b':'))
    {
        if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
            if !good_time_zone(parsed.offset().local_minus_utc()) {
                // chrono may accept offsets Go rejects; enforce Go's range.
                return Err(ParseTimeError::OffsetOutOfRange {
                    input: value.to_owned(),
                });
            }
            return Ok(parsed);
        }
        // Distinguish out-of-range offsets for Go's error message.
        if looks_like_bad_offset(value) {
            return Err(ParseTimeError::OffsetOutOfRange {
                input: value.to_owned(),
            });
        }
        return Err(invalid());
    }
    // "2006-01-02 15:04:05 -0700 MST".
    if value.len() > 20 && is_datetime_shape(value, b' ') {
        if let Ok(parsed) = DateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S %z %Z") {
            return Ok(parsed);
        }
    }
    // Bare "2006-01-02T15:04:05" (optional fractional seconds).
    if is_datetime_shape(value, b'T') {
        if let Ok(naive) = NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f") {
            return Ok(FixedOffset::east_opt(0)
                .expect("utc")
                .from_utc_datetime(&naive));
        }
    }
    Err(invalid())
}

/// Strict `YYYY[-MM[-DD]]` shape check (Go's fixed layouts reject short runs).
fn is_date_prefix(value: &str, month_only: bool) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() < 7 || !bytes[..4].iter().all(u8::is_ascii_digit) || bytes[4] != b'-' {
        return false;
    }
    if !bytes[5..7].iter().all(u8::is_ascii_digit) {
        return false;
    }
    if month_only {
        return bytes.len() == 7;
    }
    bytes.len() == 10 && bytes[7] == b'-' && bytes[8..10].iter().all(u8::is_ascii_digit)
}

/// Strict `YYYY-MM-DD<sep>HH:MM:SS[.frac]` shape check.
fn is_datetime_shape(value: &str, sep: u8) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() < 19 || !is_date_prefix(&value[..10], false) {
        return false;
    }
    if bytes[10] != sep {
        return false;
    }
    let digits = |range: std::ops::Range<usize>| bytes[range].iter().all(u8::is_ascii_digit);
    if !(digits(11..13)
        && bytes[13] == b':'
        && digits(14..16)
        && bytes[16] == b':'
        && digits(17..19))
    {
        return false;
    }
    if bytes.len() == 19 {
        return true;
    }
    // Optional fractional seconds.
    let rest = &bytes[19..];
    rest.len() > 1 && rest[0] == b'.' && rest[1..].iter().all(u8::is_ascii_digit)
}

/// Heuristic for Go's "time zone offset hour out of range" message.
fn looks_like_bad_offset(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() < 6 {
        return false;
    }
    let (sign_pos, hour) = if bytes[bytes.len() - 3] == b':' {
        let h = &value[value.len() - 5..value.len() - 3];
        (value.len() - 6, h.parse::<i32>().unwrap_or(0))
    } else {
        return false;
    };
    let Some(sign) = bytes.get(sign_pos) else {
        return false;
    };
    if *sign != b'+' && *sign != b'-' {
        return false;
    }
    hour > 23
}

/// Formats a datetime like Go's `time.MarshalText` (RFC 3339, `Z` for UTC,
/// fractional seconds only when nonzero).
#[must_use]
pub fn format_datetime(value: &DateTime<FixedOffset>) -> String {
    let offset = value.offset().local_minus_utc();
    let naive = value.naive_local();
    let mut out = naive.format("%Y-%m-%dT%H:%M:%S").to_string();
    let nanos = value.timestamp_subsec_nanos();
    if nanos != 0 {
        let mut frac = format!("{nanos:09}");
        while frac.ends_with('0') {
            frac.pop();
        }
        out.push('.');
        out.push_str(&frac);
    }
    if offset == 0 {
        out.push('Z');
    } else {
        use std::fmt::Write as _;
        let sign = if offset < 0 { '-' } else { '+' };
        let abs = offset.abs();
        let _ = write!(out, "{sign}{:02}:{:02}", abs / 3600, abs % 3600 / 60);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike as _;

    #[test]
    fn discriminants_match_pb_proto() {
        assert_eq!(TypeId::Default as i32, 0);
        assert_eq!(TypeId::Binary as i32, 1);
        assert_eq!(TypeId::Int as i32, 2);
        assert_eq!(TypeId::Float as i32, 3);
        assert_eq!(TypeId::Bool as i32, 4);
        assert_eq!(TypeId::DateTime as i32, 5);
        assert_eq!(TypeId::Geo as i32, 6);
        assert_eq!(TypeId::Uid as i32, 7);
        assert_eq!(TypeId::Password as i32, 8);
        assert_eq!(TypeId::String as i32, 9);
        assert_eq!(TypeId::Object as i32, 10);
        assert_eq!(TypeId::BigFloat as i32, 11);
        assert_eq!(TypeId::VFloat as i32, 12);
    }

    #[test]
    fn names_roundtrip() {
        for t in TypeId::all() {
            assert_eq!(&TypeId::for_name(t.as_str()), &Some(*t));
            assert_eq!(t.as_str().parse::<TypeId>().as_ref(), Ok(t));
        }
        assert_eq!(TypeId::for_name("--invalid--"), None);
        assert!("--invalid--".parse::<TypeId>().is_err());
        assert!(!TypeId::Uid.is_scalar());
        assert!(TypeId::Int.is_number() && TypeId::Float.is_number());
        assert!(!TypeId::String.is_number());
    }

    #[test]
    fn parses_dates_without_zone() {
        // Port of datesWithoutTz.
        let utc = FixedOffset::east_opt(0).expect("utc");
        let at = |y, mo, d, h, mi, s, nanos| {
            utc.with_ymd_and_hms(y, mo, d, h, mi, s)
                .single()
                .and_then(|dt| dt.with_nanosecond(nanos))
                .expect("valid test date")
        };
        for (input, expected) in [
            ("2018-10-28T04:00:10", at(2018, 10, 28, 4, 0, 10, 0)),
            (
                "2018-05-30T09:30:10.5",
                at(2018, 5, 30, 9, 30, 10, 500_000_000),
            ),
            ("2018", at(2018, 1, 1, 0, 0, 0, 0)),
            ("2018-01", at(2018, 1, 1, 0, 0, 0, 0)),
            ("2018-01-01", at(2018, 1, 1, 0, 0, 0, 0)),
        ] {
            assert_eq!(parse_time(input).as_ref(), Ok(&expected), "{input}");
        }
    }

    #[test]
    fn parses_dates_with_zone() {
        // Port of datesWithTz.
        let utc = FixedOffset::east_opt(0).expect("utc");
        let m6 = FixedOffset::east_opt(-6 * 3600).expect("zone");
        let p23 = FixedOffset::east_opt(23 * 3600).expect("zone");
        let m23_40 = FixedOffset::east_opt(-23 * 3600 - 40 * 60).expect("zone");
        let p23_59 = FixedOffset::east_opt(23 * 3600 + 59 * 60).expect("zone");
        for (input, zone, y, mo, d, h, mi, s, nanos) in [
            ("2018-10-28T04:00:10Z", utc, 2018, 10, 28, 4, 0, 10, 0),
            ("2018-10-28T04:00:10-00:00", utc, 2018, 10, 28, 4, 0, 10, 0),
            (
                "2018-05-30T09:30:10.5Z",
                utc,
                2018,
                5,
                30,
                9,
                30,
                10,
                500_000_000,
            ),
            (
                "2018-05-30T09:30:10.5-00:00",
                utc,
                2018,
                5,
                30,
                9,
                30,
                10,
                500_000_000,
            ),
            ("2018-05-30T09:30:10-06:00", m6, 2018, 5, 30, 9, 30, 10, 0),
            ("2018-05-28T14:41:57+23:00", p23, 2018, 5, 28, 14, 41, 57, 0),
            (
                "2018-05-28T14:41:57-23:40",
                m23_40,
                2018,
                5,
                28,
                14,
                41,
                57,
                0,
            ),
            (
                "2018-05-28T14:41:57+23:59",
                p23_59,
                2018,
                5,
                28,
                14,
                41,
                57,
                0,
            ),
        ] {
            let expected = zone
                .with_ymd_and_hms(y, mo, d, h, mi, s)
                .single()
                .and_then(|dt| dt.with_nanosecond(nanos))
                .expect("valid test date");
            assert_eq!(parse_time(input).as_ref(), Ok(&expected), "{input}");
        }
    }

    #[test]
    fn rejects_invalid_dates() {
        // Port of invalidDates + invalid timezones.
        for bad in [
            "abcd",
            "12345",
            "123456",
            "1234567",
            "12345678",
            "123456789",
            "1234567891",
            "11111111111111111Z",
            "111111111111111:11",
            "1111-11-11T11:11111111:1",
            "1111-11-11T11:11:1111:11",
            "18-10-28T04:00:10Z",
            "318-10-28T04:00:10",
            "2018-110-28T04:00:10",
            "20181-4-28T25:00:10",
            "2018-10-218T04:00:10",
            "2018-14-28T25:00:10",
            "2018-142-8T25:00:10",
            "2018-05-33T09:65:10.5",
            "201",
            "2018-011",
            "2018-01-011",
        ] {
            assert!(parse_time(bad).is_err(), "{bad}");
        }
        for bad in [
            "2018-05-28T14:41:57+25:00",
            "2018-05-28T14:41:57+30:00",
            "2018-05-28T14:41:57-25:01",
            "2018-05-28T14:41:57-30:00",
        ] {
            let err = parse_time(bad).expect_err(bad);
            assert!(
                matches!(err, ParseTimeError::OffsetOutOfRange { .. }),
                "{bad}: {err}"
            );
        }
    }

    #[test]
    fn formats_like_go_marshal_text() {
        let utc = FixedOffset::east_opt(0).expect("utc");
        let dt = utc
            .with_ymd_and_hms(2006, 1, 2, 15, 4, 5)
            .single()
            .expect("date");
        assert_eq!(format_datetime(&dt), "2006-01-02T15:04:05Z");
        let dt = dt.with_nanosecond(500_000_000).expect("nanos");
        assert_eq!(format_datetime(&dt), "2006-01-02T15:04:05.5Z");
        let m6 = FixedOffset::east_opt(-6 * 3600).expect("zone");
        let dt = m6
            .with_ymd_and_hms(2018, 5, 30, 9, 30, 10)
            .single()
            .expect("date");
        assert_eq!(format_datetime(&dt), "2018-05-30T09:30:10-06:00");
    }
}
