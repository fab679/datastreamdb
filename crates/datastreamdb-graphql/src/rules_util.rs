//! Shared validation helpers. Port of the small helpers used by
//! `dgraph-main/graphql/schema/rules.go` (reserved words, search parsing,
//! interface parents, search messages).

#![warn(missing_docs)]

use std::collections::HashMap;

use crate::directives::{
    DATASTREAMDB_DIRECTIVE, DATASTREAMDB_PRED_ARG, DATASTREAMDB_TYPE_ARG, ID_DIRECTIVE,
    MULTI_POLYGON, POINT, POLYGON, SEARCH_ARGS, SEARCH_DIRECTIVE, SECRET_DIRECTIVE, default_search,
    inbuilt_type_to_datastreamdb,
};
use crate::model::{Directive, FieldDef, GqlType, Schema, TypeDef, TypeKind, Value};

/// Whether the definition is the `Query` or `Mutation` root object.
pub(crate) fn is_query_or_mutation_type(def: &TypeDef) -> bool {
    def.kind == TypeKind::Object && is_query_or_mutation(&def.name)
}

/// Whether the name is `Query` or `Mutation`.
pub(crate) fn is_query_or_mutation(name: &str) -> bool {
    name == "Query" || name == "Mutation"
}

/// Whether the name maps to a `DataStreamDB` scalar.
pub(crate) fn is_scalar(name: &str) -> bool {
    inbuilt_type_to_datastreamdb(name).is_some()
}

/// Whether the name is a reserved field argument.
pub(crate) fn is_reserved_argument(name: &str) -> bool {
    matches!(name, "first" | "offset" | "filter" | "order")
}

/// Whether the name is a reserved keyword.
pub(crate) fn is_reserved_keyword(name: &str) -> bool {
    if is_scalar(name) || is_query_or_mutation(name) {
        return true;
    }
    if matches!(name, "uid" | "Subscription" | "Point") {
        return true;
    }
    name.eq_ignore_ascii_case("as")
}

/// Whether the field is an ID field (`ID` type).
pub(crate) fn is_id_field(field: &FieldDef) -> bool {
    field.ty.name() == "ID"
}

/// Whether the field carries `@id`.
pub(crate) fn has_id_directive(field: &FieldDef) -> bool {
    field.directive(ID_DIRECTIVE).is_some()
}

/// Whether the field carries `@embedding`.
pub(crate) fn has_embedding_directive(field: &FieldDef) -> bool {
    field
        .directive(crate::directives::EMBEDDING_DIRECTIVE)
        .is_some()
}

/// Whether the field carries `@custom` or `@lambda`.
pub(crate) fn has_custom_or_lambda(field: &FieldDef) -> bool {
    field
        .directive(crate::directives::CUSTOM_DIRECTIVE)
        .is_some()
        || field
            .directive(crate::directives::LAMBDA_DIRECTIVE)
            .is_some()
}

/// Finds the first interface of `def` that declares `field_name`.
pub(crate) fn parent_interface<'a>(
    schema: &'a Schema,
    def: &TypeDef,
    field_name: &str,
) -> Option<&'a TypeDef> {
    for iface in &def.implements {
        let interface_def = schema.type_def(iface)?;
        if interface_def.field(field_name).is_some() {
            return Some(interface_def);
        }
    }
    None
}

/// Whether the type is a geo type.
pub(crate) fn is_geo_type(name: &str) -> bool {
    matches!(name, POINT | POLYGON | MULTI_POLYGON)
}

/// Parses the search type from a `@search(by:)` argument.
///
/// Syntax: `<searchType> [ "(" <searchOptions> ")" ]`.
pub(crate) fn parse_search_type(search_arg: &str) -> &str {
    match search_arg.find('(') {
        Some(idx) => search_arg[..idx].trim(),
        None => search_arg,
    }
}

/// Parses `key: value` search options from a `@search(by:)` argument.
///
/// Returns the options and whether they parsed. Port of Go
/// `parseSearchOptions` (single-level `key: value` pairs; Go uses YAML for
/// the same single-level shape).
pub(crate) fn parse_search_options(search_arg: &str) -> (HashMap<String, String>, bool) {
    let search_arg = search_arg.trim();
    let open_paren = search_arg.find('(');
    if open_paren.is_none() && !search_arg.ends_with(')') {
        return (HashMap::new(), true);
    }
    let Some(open) = open_paren else {
        return (HashMap::new(), false);
    };
    if open + 1 == search_arg.len() - 1 || !search_arg.ends_with(')') {
        return (HashMap::new(), false);
    }
    let inner = &search_arg[open + 1..search_arg.len() - 1];
    let mut map = HashMap::new();
    for pair in inner.split(',') {
        let mut kv = pair.splitn(2, ':');
        let (Some(key), Some(val)) = (kv.next(), kv.next()) else {
            return (HashMap::new(), false);
        };
        let (key, val) = (key.trim(), val.trim());
        if key.is_empty() || val.is_empty() {
            return (HashMap::new(), false);
        }
        map.insert(key.to_owned(), val.to_owned());
    }
    (map, true)
}

/// Stringifies search options in DQL syntax. Port of Go `getSearchOptions`.
pub(crate) fn get_search_options(search_arg: &str) -> (String, bool) {
    let (kv_map, ok) = parse_search_options(search_arg);
    if kv_map.is_empty() {
        return (String::new(), ok);
    }
    let mut keys: Vec<&String> = kv_map.keys().collect();
    keys.sort();
    let mut out = String::from("(");
    for (i, key) in keys.iter().enumerate() {
        let val = kv_map[*key].trim();
        if val.is_empty() {
            return (String::new(), false);
        }
        out.push_str(key.trim());
        out.push_str(": \"");
        out.push_str(val);
        out.push('"');
        if i + 1 < keys.len() {
            out.push_str(", ");
        }
    }
    out.push(')');
    (out, true)
}

/// Default search index for a GraphQL type (enums always hash).
pub(crate) fn get_default_search_index(type_name: &str) -> &str {
    default_search(type_name).unwrap_or("hash")
}

/// Appends `hash` when `@id` is present without `hash`/`exact`.
pub(crate) fn add_hash_if_required(field: &FieldDef, mut indexes: Vec<String>) -> Vec<String> {
    if field.directive(ID_DIRECTIVE).is_some()
        && !indexes.iter().any(|index| index == "hash")
        && !indexes.iter().any(|index| index == "exact")
    {
        indexes.push("hash".to_owned());
    }
    indexes
}

/// Search arguments applied to a field (`None` when neither `@search` nor
/// `@id` applies). Port of Go `getSearchArgs`.
pub(crate) fn get_search_args(field: &FieldDef) -> Option<Vec<String>> {
    let search = field.directive(SEARCH_DIRECTIVE);
    let id = field.directive(ID_DIRECTIVE);
    let field_type = field.ty.name();
    let Some(search) = search else {
        id?;
        if field_type == "String" {
            return Some(vec!["hash".to_owned()]);
        }
        return Some(vec![get_default_search_index(field_type).to_owned()]);
    };
    let empty = search
        .arg(SEARCH_ARGS)
        .is_none_or(|val| !matches!(val, Value::List(items) if !items.is_empty()));
    if empty {
        return Some(vec![get_default_search_index(field_type).to_owned()]);
    }
    let mut out = Vec::new();
    if let Some(Value::List(items)) = search.arg(SEARCH_ARGS) {
        for item in items {
            out.push(item.raw());
        }
    }
    out = add_hash_if_required(field, out);
    out.sort();
    Some(out)
}

/// Human-readable search capability message. Port of Go `searchMessage`.
pub(crate) fn search_message(schema: &Schema, field: &FieldDef) -> String {
    let mut possible: Vec<&str> = Vec::new();
    for (name, gql_type, _) in crate::directives::SUPPORTED_SEARCHES {
        if *gql_type == field.ty.name() {
            possible.push(name);
        }
    }
    let is_enum = schema
        .type_def(field.ty.name())
        .is_some_and(|def| def.kind == TypeKind::Enum);
    if possible.len() == 1 || is_enum {
        return "are searchable by just @search".to_owned();
    }
    if possible.is_empty() {
        return "can't have the @search directive".to_owned();
    }
    possible.sort_unstable();
    format!(
        "can have @search by {} and {}",
        possible[..possible.len() - 1].join(", "),
        possible[possible.len() - 1]
    )
}

/// `DataStreamDB` type name for a definition (`@datastreamdb(type:)` wins).
pub(crate) fn type_name(def: &TypeDef) -> String {
    def.directive(DATASTREAMDB_DIRECTIVE)
        .and_then(|dir| dir.arg(DATASTREAMDB_TYPE_ARG))
        .map_or_else(|| def.name.clone(), Value::raw)
}

/// `@datastreamdb(pred:)` argument of a field, if present.
pub(crate) fn get_datastreamdb_pred_arg(field: &FieldDef) -> Option<&Value> {
    field
        .directive(DATASTREAMDB_DIRECTIVE)
        .and_then(|dir| dir.arg(DATASTREAMDB_PRED_ARG))
}

/// `DataStreamDB` predicate for a field (`@datastreamdb(pred:)` wins, else `Type.field`).
pub(crate) fn field_name(field: &FieldDef, type_name: &str) -> String {
    get_datastreamdb_pred_arg(field).map_or_else(
        || format!("{type_name}.{}", field.name),
        super::model::Value::raw,
    )
}

/// Synthesizes the password field from a `@secret` directive.
pub(crate) fn convert_password_directive(dir: &Directive) -> Option<FieldDef> {
    if dir.name != SECRET_DIRECTIVE {
        return None;
    }
    let name = dir.arg("field")?.raw();
    let mut directives = Vec::new();
    if let Some(pred) = dir.arg("pred") {
        directives.push(Directive {
            name: DATASTREAMDB_DIRECTIVE.to_owned(),
            args: vec![(DATASTREAMDB_PRED_ARG.to_owned(), pred.clone())],
            position: dir.position,
        });
    }
    Some(FieldDef {
        name,
        description: None,
        args: Vec::new(),
        ty: GqlType::named("String").required(),
        directives,
        position: dir.position,
    })
}

/// Password field of a type, if it carries `@secret`.
pub(crate) fn get_password_field(def: &TypeDef) -> Option<FieldDef> {
    def.directives
        .iter()
        .filter_map(convert_password_directive)
        .next_back()
}

/// Finds the first interface of `def` whose password field is `field_name`.
pub(crate) fn parent_interface_for_pwd_field<'a>(
    schema: &'a Schema,
    def: &TypeDef,
    field_name: &str,
) -> Option<&'a TypeDef> {
    for iface in &def.implements {
        let interface_def = schema.type_def(iface)?;
        if get_password_field(interface_def).is_some_and(|pwd| pwd.name == field_name) {
            return Some(interface_def);
        }
    }
    None
}
