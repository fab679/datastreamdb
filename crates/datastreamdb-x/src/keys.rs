//! Attribute/namespace keys and reserved names. Port of the `x/keys.go` key
//! helpers plus the small `x/x.go` bits the parsers need (`Star`,
//! `FacetDelimiter`, `PredicateLang`).
//!
//! Attributes on the wire look like `{ns-hex}-{attr}`, e.g. `0-name`.

#![warn(missing_docs)]

/// Equivalent to using `*` in a mutation.
pub const STAR: &str = "_STAR_ALL";

/// Separates a predicate from facet keys in JSON input (`friend|close`).
pub const FACET_DELIMITER: &str = "|";

/// Namespace id for the root (galaxy) namespace.
pub const ROOT_NAMESPACE: u64 = 0;

/// Separator between the hex namespace and the attribute.
pub const NS_SEPARATOR: &str = "-";

/// Builds a namespaced attribute (`{ns-hex}-{attr}`).
#[must_use]
pub fn namespace_attr(ns: u64, attr: &str) -> String {
    format!("{ns:x}{NS_SEPARATOR}{attr}")
}

/// Attribute in the root namespace.
#[must_use]
pub fn attr_in_root_namespace(attr: &str) -> String {
    namespace_attr(ROOT_NAMESPACE, attr)
}

/// Splits a namespaced attribute into namespace and attr.
///
/// # Panics
///
/// Panics on malformed input, like Go (which indexes the split and checks
/// the hex parse).
#[must_use]
pub fn parse_namespace_attr(attr: &str) -> (u64, &str) {
    let (ns, rest) = attr.split_once(NS_SEPARATOR).expect("namespaced attr");
    let ns = u64::from_str_radix(ns, 16).expect("hex namespace");
    (ns, rest)
}

/// Returns the attr part of a namespaced attribute.
///
/// # Panics
///
/// Panics on malformed input, like Go.
#[must_use]
pub fn parse_attr(attr: &str) -> &str {
    parse_namespace_attr(attr).1
}

/// Returns the namespace of a namespaced attribute.
///
/// # Panics
///
/// Panics on malformed input, like Go.
#[must_use]
pub fn parse_namespace(attr: &str) -> u64 {
    parse_namespace_attr(attr).0
}

/// Extracts the language tag from a predicate (or facet) name.
///
/// Returns the predicate and the tag (`name@en` -> `name`, `en`).
/// A leading `@` (index 0) does not split, matching Go's `LastIndex` check.
#[must_use]
pub fn predicate_lang(s: &str) -> (&str, &str) {
    match s.rfind('@') {
        Some(i) if i > 0 => (&s[..i], &s[i + 1..]),
        _ => (s, ""),
    }
}

/// Predicates expanded when `*` is the predicate.
const STAR_ALL_PREDICATES: &[&str] = &["datastreamdb.type"];

/// Reserved ACL predicates (compared case-insensitively, like Go).
const ACL_PREDICATES: &[&str] = &[
    "datastreamdb.xid",
    "datastreamdb.password",
    "datastreamdb.user.group",
    "datastreamdb.rule.predicate",
    "datastreamdb.rule.permission",
    "datastreamdb.acl.rule",
];

/// GraphQL-reserved predicates (exact match, like Go).
const OTHER_RESERVED_PREDICATES: &[&str] = &[
    "datastreamdb.graphql.xid",
    "datastreamdb.graphql.schema",
    "datastreamdb.drop.op",
    "datastreamdb.graphql.p_query",
    "datastreamdb.namespace.id",
    "datastreamdb.namespace.name",
];

/// Pre-defined type names (compared exactly, like Go).
const PRE_DEFINED_TYPES: &[&str] = &[
    "datastreamdb.graphql",
    "datastreamdb.type.User",
    "datastreamdb.type.Group",
    "datastreamdb.type.Rule",
    "datastreamdb.graphql.persisted_query",
    "datastreamdb.namespace",
];

/// Whether the predicate was defined internally by `DataStreamDB`.
///
/// Malformed (non-namespaced) names return false instead of panicking:
/// only well-formed internal names can match.
#[must_use]
pub fn is_pre_defined_predicate(pred: &str) -> bool {
    let Some((_, name)) = try_parse_namespace_attr(pred) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    STAR_ALL_PREDICATES.contains(&lower.as_str())
        || ACL_PREDICATES.contains(&lower.as_str())
        || OTHER_RESERVED_PREDICATES.contains(&name)
}

/// Whether the type name is a pre-defined `DataStreamDB` type.
///
/// Malformed (non-namespaced) names return false instead of panicking.
#[must_use]
pub fn is_pre_defined_type(typ: &str) -> bool {
    try_parse_namespace_attr(typ).is_some_and(|(_, name)| PRE_DEFINED_TYPES.contains(&name))
}

/// Guardians group id: members bypass all ACL checks.
pub const SUPER_ADMIN_ID: &str = "guardians";

/// Whether any group grants super-admin (Go `IsSuperAdmin`).
#[must_use]
pub fn is_super_admin(groups: &[String]) -> bool {
    groups.iter().any(|g| g == SUPER_ADMIN_ID)
}

/// Whether `pred` is reserved for the ACL feature (case-insensitive).
#[must_use]
pub fn is_acl_predicate(pred: &str) -> bool {
    ACL_PREDICATES.contains(&pred.to_ascii_lowercase().as_str())
}

/// Every ACL-reserved predicate (un-namespaced, like Go).
#[must_use]
pub fn all_acl_predicates() -> Vec<String> {
    ACL_PREDICATES.iter().map(ToString::to_string).collect()
}

/// Splits a namespaced attribute, or `None` when malformed.
fn try_parse_namespace_attr(attr: &str) -> Option<(u64, &str)> {
    let (ns, rest) = attr.split_once(NS_SEPARATOR)?;
    let ns = u64::from_str_radix(ns, 16).ok()?;
    Some((ns, rest))
}

/// Sorts and deduplicates strings in place (port of `RemoveDuplicates`).
#[must_use]
pub fn remove_duplicates(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values.dedup();
    values
}

/// Removes spaces and tabs (port of the `WhiteSpace` replacer).
#[must_use]
pub fn strip_space_tab(s: &str) -> String {
    s.chars().filter(|c| *c != ' ' && *c != '\t').collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespace_roundtrip() {
        assert_eq!(namespace_attr(0, "name"), "0-name");
        assert_eq!(namespace_attr(0xf2, "graduation"), "f2-graduation");
        assert_eq!(attr_in_root_namespace("age"), "0-age");
        assert_eq!(parse_namespace_attr("f2-graduation"), (0xf2, "graduation"));
        assert_eq!(parse_attr("0-name"), "name");
        assert_eq!(parse_namespace("10-jobs"), 0x10);
    }

    #[test]
    fn language_split() {
        assert_eq!(predicate_lang("name@en"), ("name", "en"));
        assert_eq!(predicate_lang("name"), ("name", ""));
        assert_eq!(predicate_lang("@en"), ("@en", ""));
        assert_eq!(predicate_lang("a@b@c"), ("a@b", "c"));
    }

    #[test]
    fn acl_helpers() {
        assert!(is_super_admin(&["dev".to_owned(), "guardians".to_owned()]));
        assert!(!is_super_admin(&["dev".to_owned()]));
        assert!(is_acl_predicate("datastreamdb.password"));
        assert!(is_acl_predicate("DATASTREAMDB.XID"));
        assert!(!is_acl_predicate("name"));
        assert_eq!(all_acl_predicates().len(), 6);
    }

    #[test]
    fn reserved_names() {
        assert!(is_pre_defined_predicate("0-datastreamdb.type"));
        assert!(is_pre_defined_predicate("0-DATASTREAMDB.XID"));
        assert!(is_pre_defined_predicate("0-datastreamdb.graphql.schema"));
        assert!(!is_pre_defined_predicate("0-datastreamdb.blah"));
        assert!(!is_pre_defined_predicate("0-person.name"));
        assert!(is_pre_defined_type("0-datastreamdb.type.User"));
        assert!(!is_pre_defined_type("0-Person"));
    }
}
