//! Shared commons: errors, config, metrics, logging helpers.
//!
//! Rust port of the upstream Go `x` package.
//! See `README.md` / `TODO.md` for the port plan.

#![warn(missing_docs)]

pub mod keycodec;
pub mod keys;
pub mod values;

pub use keycodec::{
    BYTE_COUNT, BYTE_COUNT_REV, BYTE_DATA, BYTE_INDEX, BYTE_REVERSE, BYTE_SCHEMA, BYTE_SPLIT,
    BYTE_TYPE, BYTE_UNUSED, DEFAULT_PREFIX, KeyError, ParsedKey, attr_from_2103, count_key,
    data_key, data_prefix, index_key, index_key_after_all_terms, is_drop_op_key, is_reverse_attr,
    namespace_attr_list, namespace_to_bytes, parse, predicate_prefix, reverse_key, schema_key,
    schema_prefix, split_key, type_key, type_prefix,
};
pub use keys::{
    FACET_DELIMITER, NS_SEPARATOR, ROOT_NAMESPACE, STAR, SUPER_ADMIN_ID, all_acl_predicates,
    attr_in_root_namespace, is_acl_predicate, is_pre_defined_predicate, is_pre_defined_type,
    is_super_admin, namespace_attr, parse_attr, parse_namespace, parse_namespace_attr,
    predicate_lang, remove_duplicates, strip_space_tab,
};
pub use values::{ValueTypeInfo, value_type};

/// Returns the crate name. Placeholder until the ported API lands.
#[must_use]
pub fn crate_name() -> &'static str {
    env!("CARGO_PKG_NAME")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_name_matches_package() {
        assert_eq!(crate_name(), "datastreamdb-x");
    }
}
