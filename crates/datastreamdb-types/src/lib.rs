//! Scalar type system: ids, values, conversion, ordering, geo.
//!
//! Rust port of the upstream Go `types` package: [`TypeId`] tags with frozen
//! discriminants, the [`Val`] value model, the full conversion matrix
//! ([`conversion`]), comparison/sorting ([`compare`], [`sort`]), bcrypt
//! passwords ([`password`]), sharded maps ([`sharded_map`]), and geospatial
//! support ([`geo`], [`earth`], [`s2loop`], [`geo_index`], [`geofilter`]).
//!
//! Documented deltas from Go: datetime binaries use our 12-byte layout,
//! bigfloats are decimal, float→datetime of out-of-range values errors, and
//! language collation uses ICU4X. See each module for details.

// Numeric `as` casts throughout this crate mirror Go's conversion semantics
// exactly (wrapping, truncation, and IEEE rounding are all specified, not
// accidental), with ported tests proving each behavior.
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_sign_loss)]
#![warn(missing_docs)]

pub mod compare;
pub mod conversion;
pub mod earth;
pub mod geo;
pub mod geo_index;
pub mod geofilter;
pub mod password;
pub mod s2loop;
pub mod scalar;
pub mod sharded_map;
pub mod sort;
pub mod strconv;
pub mod value;

pub use compare::{compare_between, compare_vals, equal, equal_checked, less_inner, less_than};
pub use conversion::{
    ApiValue, ConversionError, convert, datetime_to_binary, marshal, object_value, parse_v_float,
};
pub use scalar::{TypeId, format_datetime, good_time_zone, parse_time};
pub use sharded_map::ShardedMap;
pub use sort::{SortError, is_sortable, sort_top_n, sort_values, sort_with_facet};
pub use value::{
    Val, Value, bytes_as_float_array, float_array_as_bytes, float_array_as_string, type_for_value,
};
