//! Field-directive validators. Port of `embeddingValidation`,
//! `hasInverseValidation`, `searchValidation`, `datastreamdbDirectiveValidation`,
//! `passwordValidation`, `defaultDirectiveValidation`, and `idValidation`
//! from `dgraph-main/graphql/schema/rules.go`.
//!
//! Delta from Go: block strings are indistinguishable from quoted strings in
//! the `async-graphql-parser` AST, so both are accepted anywhere Go requires
//! `StringValue`.

#![warn(missing_docs)]

use std::collections::HashMap;

use crate::directives::{
    DATASTREAMDB_DIRECTIVE, DATASTREAMDB_PRED_ARG, EMBEDDING_DIRECTIVE, ID_DIRECTIVE,
    ID_DIRECTIVE_INTERFACE_ARG, INVERSE_ARG, INVERSE_DIRECTIVE, LAMBDA_DIRECTIVE, REMOTE_DIRECTIVE,
    SEARCH_ARGS, SEARCH_DIRECTIVE, built_in_filter, filters_collisions, is_enum_directive,
    supported_search,
};
use crate::errors::{ErrorList, GraphQlError};
use crate::model::{BaseType, Directive, FieldDef, Schema, TypeDef, TypeKind, Value};
use crate::rules_post::ValidationOptions;
use crate::rules_util::{
    get_search_args, get_search_options, has_embedding_directive, has_id_directive, is_geo_type,
    is_id_field, is_reserved_keyword, parent_interface, parse_search_type, search_message,
};

/// Validates `@embedding` (only on `[Float!]` fields).
pub(crate) fn embedding_validation(
    _schema: &Schema,
    typ: &TypeDef,
    field: &FieldDef,
    _dir: &Directive,
) -> ErrorList {
    let mut errs = ErrorList::default();
    let BaseType::List(inner) = &field.ty.base else {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; Field {}: The field with @embedding directive is of type {}, but \
                 @embedding directive only applies to fields of type [Float!].",
                typ.name,
                field.name,
                field.ty.name()
            ),
            field.position.0,
            field.position.1,
        ));
        return errs;
    };
    if !inner.name().eq_ignore_ascii_case("Float") || inner.nullable {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; Field {}: The field with @embedding directive is of type [{}], but \
                 @embedding directive only applies to fields of type [Float!].",
                typ.name,
                field.name,
                field.ty.name()
            ),
            field.position.0,
            field.position.1,
        ));
    }
    errs
}

/// Checks an inverse link for type/consistency errors. Port of Go `isInverse`.
fn is_inverse(
    schema: &Schema,
    expected_inv_type: &str,
    expected_inv_field: &str,
    type_name: &str,
    field: &FieldDef,
) -> Option<String> {
    let mut expected_inv_type = expected_inv_type.to_owned();
    if let Some(expected_def) = schema.type_def(&expected_inv_type) {
        if let Some(parent) = parent_interface(schema, expected_def, expected_inv_field) {
            if let Some(fld) = parent.field(expected_inv_field) {
                if fld.directive(INVERSE_DIRECTIVE).is_some() {
                    expected_inv_type = parent.name.clone();
                }
            }
        }
    }
    if field.ty.name() != expected_inv_type {
        return Some(format!(
            "Type {expected_inv_type}; Field {expected_inv_field}: @hasInverse is required to \
             link the fields of same type, but the field {} is of the type {} instead of \
             {expected_inv_type}. To link these make sure the fields are of the same type.",
            field.name, field.ty
        ));
    }
    let inv_directive = field.directive(INVERSE_DIRECTIVE)?;
    let back = inv_directive
        .arg(INVERSE_ARG)
        .map(Value::raw)
        .unwrap_or_default();
    if inv_directive.arg(INVERSE_ARG).is_none() || back != expected_inv_field {
        return Some(format!(
            "Type {expected_inv_type}; Field {expected_inv_field}: @hasInverse should be \
             consistant. {expected_inv_type}.{expected_inv_field} is the inverse of \
             {type_name}.{}, but {type_name}.{} is the inverse of \
             {expected_inv_type}.{back}.",
            field.name, field.name
        ));
    }
    None
}

/// Validates `@hasInverse` and backfills the reverse directive.
pub(crate) fn has_inverse_validation(
    schema: &mut Schema,
    typ_name: &str,
    field_name: &str,
    dir: &Directive,
) -> ErrorList {
    let mut errs = ErrorList::default();
    let typ = schema
        .type_def(typ_name)
        .cloned()
        .unwrap_or_else(|| unreachable!());
    let field = typ
        .field(field_name)
        .cloned()
        .unwrap_or_else(|| unreachable!());
    let inv_type_name = field.ty.name().to_owned();
    let inv_kind = schema.type_def(&inv_type_name).map(|def| def.kind);
    if !matches!(inv_kind, Some(TypeKind::Object | TypeKind::Interface)) {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; Field {}: Field {} is of type {inv_type_name}, but @hasInverse \
                 directive only applies to fields with object types.",
                typ.name, field.name, field.name
            ),
            field.position.0,
            field.position.1,
        ));
        return errs;
    }
    let Some(inv_field_arg) = dir.arg(INVERSE_ARG) else {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; Field {}: @hasInverse directive doesn't have field argument.",
                typ.name, field.name
            ),
            dir.position.0,
            dir.position.1,
        ));
        return errs;
    };
    let inv_field_name = inv_field_arg.raw();
    let inv_type = schema
        .type_def(&inv_type_name)
        .cloned()
        .unwrap_or_else(|| unreachable!());
    let Some(inv_field) = inv_type.field(&inv_field_name).cloned() else {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; Field {}: inverse field {inv_field_name} doesn't exist for type \
                 {inv_type_name}.",
                typ.name, field.name
            ),
            dir.position.0,
            dir.position.1,
        ));
        return errs;
    };
    if let Some(msg) = is_inverse(schema, &typ.name, &field.name, &inv_type_name, &inv_field) {
        errs.push(GraphQlError::at(msg, dir.position.0, dir.position.1));
        return errs;
    }
    if inv_field.directive(INVERSE_DIRECTIVE).is_none() {
        let backfill = Directive {
            name: INVERSE_DIRECTIVE.to_owned(),
            args: vec![(INVERSE_ARG.to_owned(), Value::Enum(field.name.clone()))],
            position: dir.position,
        };
        if let Some(target) = schema
            .type_def_mut(&inv_type_name)
            .and_then(|def| def.field_mut(&inv_field_name))
        {
            target.directives.push(backfill.clone());
        }
        if inv_type.kind == TypeKind::Interface {
            let implementors = schema
                .implementors
                .get(&inv_type_name)
                .cloned()
                .unwrap_or_default();
            for implementor in implementors {
                if let Some(target) = schema
                    .type_def_mut(&implementor)
                    .and_then(|def| def.field_mut(&inv_field_name))
                {
                    target.directives.push(backfill.clone());
                }
            }
        }
    }
    errs
}

/// Validates one `@search(by:)` argument. Port of Go `validateSearchArg`.
fn validate_search_arg(
    search_arg: &str,
    schema: &Schema,
    typ: &TypeDef,
    field: &FieldDef,
    dir: &Directive,
) -> Option<GraphQlError> {
    let at = |message: String| GraphQlError::at(message, dir.position.0, dir.position.1);
    let is_enum = schema
        .type_def(field.ty.name())
        .is_some_and(|def| def.kind == TypeKind::Enum);
    let search_type = parse_search_type(search_arg).to_owned();
    let Some((gql_type, dg_index)) = supported_search(&search_type) else {
        return Some(at(format!(
            "Type {}; Field {}: the argument to @search {search_arg} isn't valid. Fields of \
             type {} {}.",
            typ.name,
            field.name,
            field.ty.name(),
            search_message(schema, field)
        )));
    };
    if gql_type != field.ty.name() && !is_enum {
        let expected = supported_search(search_arg).map_or(gql_type, |(gql, _)| gql);
        return Some(at(format!(
            "Type {}; Field {}: has the @search directive but the argument {search_arg} \
             doesn't apply to field type {}.  Search by {search_arg} applies to fields of type \
             {expected}. Fields of type {} {}.",
            typ.name,
            field.name,
            field.ty.name(),
            field.ty.name(),
            search_message(schema, field)
        )));
    }
    if is_enum && !is_enum_directive(search_arg) {
        return Some(at(format!(
            "Type {}; Field {}: has the @search directive but the argument {search_arg} \
             doesn't apply to field type {} which is an Enum. Enum only supports hash, exact, \
             regexp and trigram",
            typ.name,
            field.name,
            field.ty.name()
        )));
    }
    if dg_index == "hnsw" {
        if !has_embedding_directive(field) {
            return Some(at(format!(
                "Type {}; Field {}: has the @search directive but the argument {search_arg} \
                 requires the field also has @{EMBEDDING_DIRECTIVE} directive.",
                typ.name, field.name
            )));
        }
        let (_, valid) = get_search_options(search_arg);
        if !valid {
            return Some(at(format!(
                "Type {}; Field {}: has the @search directive but the argument '{search_arg}' \
                 with search options is malformed. Search options are comma-separated key-value \
                 pairs in YAML format => <optionName><COLON><SPACE><VALUE>",
                typ.name, field.name
            )));
        }
    }
    None
}

/// Validates `@search` arguments, indexes, and collisions.
#[allow(clippy::too_many_lines, reason = "exact port of Go searchValidation")]
pub(crate) fn search_validation(
    schema: &Schema,
    typ: &TypeDef,
    field: &FieldDef,
    dir: &Directive,
) -> ErrorList {
    let mut errs = ErrorList::default();
    let Some(arg) = dir.arg(SEARCH_ARGS) else {
        let kind = schema.type_def(field.ty.name()).map(|def| def.kind);
        if kind == Some(TypeKind::Enum)
            || is_geo_type(field.ty.name())
            || (kind == Some(TypeKind::Scalar) && !is_id_field(field))
        {
            return errs;
        }
        errs.push(GraphQlError::at(
            format!(
                "Type {}; Field {}: has the @search directive but fields of type {} can't have \
                 the @search directive.",
                typ.name,
                field.name,
                field.ty.name()
            ),
            dir.position.0,
            dir.position.1,
        ));
        return errs;
    };
    if !matches!(arg, Value::List(_)) {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; Field {}: the @search directive requires a list argument, like \
                 @search(by: [\"hash\"])",
                typ.name, field.name
            ),
            dir.position.0,
            dir.position.1,
        ));
        return errs;
    }
    let Some(search_args) = get_search_args(field) else {
        return errs;
    };
    let mut search_indexes: HashMap<String, String> = HashMap::new();
    for search_arg in &search_args {
        if let Some(err) = validate_search_arg(search_arg, schema, typ, field, dir) {
            errs.push(err);
            return errs;
        }
        let search_type = parse_search_type(search_arg);
        let Some(search_index) = built_in_filter(search_type) else {
            errs.push(GraphQlError::at(
                format!(
                    "Type {}; Field {}: the argument to @search '{search_arg}' is not among \
                     supported search types.",
                    typ.name, field.name
                ),
                dir.position.0,
                dir.position.1,
            ));
            return errs;
        };
        if let Some(prev) = search_indexes.get(search_index) {
            if field.ty.name() == "String"
                || schema
                    .type_def(field.ty.name())
                    .is_some_and(|def| def.kind == TypeKind::Enum)
            {
                errs.push(GraphQlError::at(
                    format!(
                        "Type {}; Field {}: the argument to @search '{search_arg}' is the same \
                         as the index '{prev}' provided before and shouldn't be used together",
                        typ.name, field.name
                    ),
                    dir.position.0,
                    dir.position.1,
                ));
                return errs;
            }
            errs.push(GraphQlError::at(
                format!(
                    "Type {}; Field {}: has the search directive on {}. {} allows only one \
                     argument for @search.",
                    typ.name,
                    field.name,
                    field.ty.name(),
                    field.ty.name()
                ),
                dir.position.0,
                dir.position.1,
            ));
            return errs;
        }
        for index in filters_collisions(search_index) {
            if let Some(prev) = search_indexes.get(*index) {
                errs.push(GraphQlError::at(
                    format!(
                        "Type {}; Field {}: the arguments '{search_arg}' and '{prev}' can't be \
                         used together as arguments to @search.",
                        typ.name, field.name
                    ),
                    dir.position.0,
                    dir.position.1,
                ));
                return errs;
            }
        }
        search_indexes.insert(search_index.to_owned(), search_type.to_owned());
    }
    errs
}

/// Whether the field is a multi-language field (`pred@lang1:lang2`).
fn is_multi_lang_field(field: &FieldDef) -> bool {
    let Some(dir) = field.directive(DATASTREAMDB_DIRECTIVE) else {
        return false;
    };
    let Some(pred) = dir.arg(DATASTREAMDB_PRED_ARG) else {
        return false;
    };
    if !pred.raw().contains('@') {
        return false;
    }
    pred.raw()
        .split('@')
        .nth(1)
        .is_some_and(|langs| langs.contains(':'))
}

/// Validates a field-level `@datastreamdb` directive (forward, reverse, lang).
#[allow(
    clippy::too_many_lines,
    reason = "exact port of Go datastreamdbDirectiveValidation"
)]
pub(crate) fn datastreamdb_directive_validation(
    schema: &Schema,
    typ: &TypeDef,
    field: &FieldDef,
    dir: &Directive,
) -> ErrorList {
    let mut errs = ErrorList::default();
    if field.ty.name() == "ID" {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; Field {}: has the @datastreamdb directive but fields of type ID can't have \
                 the @datastreamdb directive.",
                typ.name, field.name
            ),
            dir.position.0,
            dir.position.1,
        ));
        return errs;
    }
    let Some(pred_arg) = dir.arg(DATASTREAMDB_PRED_ARG) else {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; Field {}: pred argument for @datastreamdb directive should not be empty.",
                typ.name, field.name
            ),
            dir.position.0,
            dir.position.1,
        ));
        return errs;
    };
    if pred_arg.raw().is_empty() {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; Field {}: pred argument for @datastreamdb directive should not be empty.",
                typ.name, field.name
            ),
            dir.position.0,
            dir.position.1,
        ));
        return errs;
    }
    if !matches!(pred_arg, Value::String(_)) {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; Field {}: pred argument for @datastreamdb directive should be of type \
                 String.",
                typ.name, field.name
            ),
            dir.position.0,
            dir.position.1,
        ));
        return errs;
    }
    if is_reserved_keyword(&pred_arg.raw()) {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; Field {}: pred argument '{}' for @datastreamdb directive is a reserved \
                 keyword.",
                typ.name,
                field.name,
                pred_arg.raw()
            ),
            dir.position.0,
            dir.position.1,
        ));
        return errs;
    }
    if pred_arg.raw().starts_with('~') || pred_arg.raw().starts_with("<~") {
        if typ.kind == TypeKind::Interface {
            return errs;
        }
        if field.directive(INVERSE_DIRECTIVE).is_some() {
            errs.push(GraphQlError::at(
                format!(
                    "Type {}; Field {}: @hasInverse directive is not allowed when pred argument \
                     in @datastreamdb directive starts with a ~.",
                    typ.name, field.name
                ),
                dir.position.0,
                dir.position.1,
            ));
            return errs;
        }
        let forward_edge_pred = pred_arg
            .raw()
            .trim_matches(|ch| ch == '<' || ch == '~' || ch == '>')
            .to_owned();
        let inv_type_name = field.ty.name();
        let inv_kind = schema.type_def(inv_type_name).map(|def| def.kind);
        if !matches!(inv_kind, Some(TypeKind::Object | TypeKind::Interface)) {
            errs.push(GraphQlError::at(
                format!(
                    "Type {}; Field {} is of type {inv_type_name}, but reverse predicate in \
                     @datastreamdb directive only applies to fields with object types.",
                    typ.name, field.name
                ),
                field.position.0,
                field.position.1,
            ));
            return errs;
        }
        if !field.ty.is_list() {
            errs.push(GraphQlError::at(
                format!(
                    "Type {}; Field {}: with a datastreamdb directive that starts with ~ should be of \
                     type list.",
                    typ.name, field.name
                ),
                dir.position.0,
                dir.position.1,
            ));
            return errs;
        }
        let inv_type = schema
            .type_def(inv_type_name)
            .cloned()
            .unwrap_or_else(|| unreachable!());
        let mut forward_found = false;
        for fld in &inv_type.fields {
            let Some(fwd) = fld.directive(DATASTREAMDB_DIRECTIVE) else {
                continue;
            };
            let Some(fwd_pred) = fwd.arg(DATASTREAMDB_PRED_ARG) else {
                continue;
            };
            if fwd_pred.raw().is_empty() || fwd_pred.raw() != forward_edge_pred {
                continue;
            }
            let mut possible = vec![typ.name.clone()];
            possible.extend(typ.implements.iter().cloned());
            if !possible.iter().any(|pt| pt == fld.ty.name()) {
                let type_msg = if possible.len() == 1 {
                    format!("of type {}", possible[0])
                } else {
                    format!(
                        "any of types {} or {}",
                        possible[..possible.len() - 1].join(", "),
                        possible[possible.len() - 1]
                    )
                };
                errs.push(GraphQlError::at(
                    format!(
                        "Type {inv_type_name}; Field {}: should be {type_msg} to be compatible \
                         with @datastreamdb reverse directive but is of type {}.",
                        fld.name,
                        fld.ty.name()
                    ),
                    dir.position.0,
                    dir.position.1,
                ));
                return errs;
            }
            if fld.directive(INVERSE_DIRECTIVE).is_some() {
                errs.push(GraphQlError::at(
                    format!(
                        "Type {}; Field {}: @hasInverse directive is not allowed because field \
                         is forward edge of another field with reverse directive.",
                        inv_type.name, fld.name
                    ),
                    dir.position.0,
                    dir.position.1,
                ));
                return errs;
            }
            forward_found = true;
            break;
        }
        if !forward_found {
            errs.push(GraphQlError::at(
                format!(
                    "Type {}; Field {}: pred argument: {} is not supported as forward edge \
                     doesn't exist for type {inv_type_name}.",
                    typ.name,
                    field.name,
                    pred_arg.raw()
                ),
                dir.position.0,
                dir.position.1,
            ));
            return errs;
        }
    }
    if pred_arg.raw().contains('@') {
        if field.ty.name() != "String" {
            errs.push(GraphQlError::at(
                format!(
                    "Type {}; Field {}: Expected type `String` for language tag field but got \
                     `{}`",
                    typ.name,
                    field.name,
                    field.ty.name()
                ),
                field.position.0,
                field.position.1,
            ));
            return errs;
        }
        if let Some(id) = field.directive(ID_DIRECTIVE) {
            errs.push(GraphQlError::at(
                format!(
                    "Type {}; Field {}: @id directive not supported on language tag fields",
                    typ.name, field.name
                ),
                id.position.0,
                id.position.1,
            ));
            return errs;
        }
        if field.directive(SEARCH_DIRECTIVE).is_some() && is_multi_lang_field(field) {
            let search = field
                .directive(SEARCH_DIRECTIVE)
                .unwrap_or_else(|| unreachable!());
            errs.push(GraphQlError::at(
                format!(
                    "Type {}; Field {}: @search directive not applicable on language tag field \
                     with multiple languages",
                    typ.name, field.name
                ),
                search.position.0,
                search.position.1,
            ));
            return errs;
        }
        let raw = pred_arg.raw();
        let all_tags: Vec<&str> = raw.split('@').collect();
        let tags = all_tags.get(1).copied().unwrap_or_default();
        if tags == "*" {
            errs.push(GraphQlError::at(
                format!(
                    "Type {}; Field {}: `*` language tag not supported in GraphQL",
                    typ.name, field.name
                ),
                dir.position.0,
                dir.position.1,
            ));
            return errs;
        }
        if tags.is_empty() {
            errs.push(GraphQlError::at(
                format!(
                    "Type {}; Field {}: empty language tag not supported",
                    typ.name, field.name
                ),
                dir.position.0,
                dir.position.1,
            ));
            return errs;
        }
        if all_tags.len() > 2 {
            errs.push(GraphQlError::at(
                format!(
                    "Type {}; Field {}: multiple language tag not supported",
                    typ.name, field.name
                ),
                dir.position.0,
                dir.position.1,
            ));
            return errs;
        }
    }
    errs
}

/// Validates `@default` placement and value shapes.
#[allow(
    clippy::too_many_lines,
    reason = "exact port of Go defaultDirectiveValidation"
)]
pub(crate) fn default_directive_validation(
    schema: &Schema,
    typ: &TypeDef,
    field: &FieldDef,
    dir: &Directive,
) -> ErrorList {
    let mut errs = ErrorList::default();
    if typ.directive(REMOTE_DIRECTIVE).is_some() {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; Field {}: cannot use @default directive on a @remote type",
                typ.name, field.name
            ),
            dir.position.0,
            dir.position.1,
        ));
        return errs;
    }
    if !crate::rules_post::is_scalar_or_enum(schema, field) {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; Field {}: cannot use @default directive on field with non-scalar \
                 type {}",
                typ.name,
                field.name,
                field.ty.name()
            ),
            dir.position.0,
            dir.position.1,
        ));
        return errs;
    }
    if field.ty.is_list() {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; Field {}: cannot use @default directive on field with list type [{}]",
                typ.name,
                field.name,
                field.ty.name()
            ),
            dir.position.0,
            dir.position.1,
        ));
        return errs;
    }
    if has_id_directive(field) {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; Field {}: cannot use @default directive on field with @id directive",
                typ.name, field.name
            ),
            dir.position.0,
            dir.position.1,
        ));
        return errs;
    }
    if field.ty.name() == "ID" {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; Field {}: cannot use @default directive on field with type ID",
                typ.name, field.name
            ),
            dir.position.0,
            dir.position.1,
        ));
        return errs;
    }
    if field
        .directive(crate::directives::CUSTOM_DIRECTIVE)
        .is_some()
    {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; Field {}: cannot use @default directive on field with @custom \
                 directive",
                typ.name, field.name
            ),
            dir.position.0,
            dir.position.1,
        ));
        return errs;
    }
    if field.directive(LAMBDA_DIRECTIVE).is_some() {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; Field {}: cannot use @default directive on field with @lambda \
                 directive",
                typ.name, field.name
            ),
            dir.position.0,
            dir.position.1,
        ));
        return errs;
    }
    for (_, arg_val) in &dir.args {
        let value = arg_val.field("value").map(Value::raw).unwrap_or_default();
        let field_type = field.ty.name();
        if value == "$now" && field_type != "DateTime" {
            errs.push(GraphQlError::at(
                format!(
                    "Type {}; Field {}: @default directive provides value \"{value}\" which \
                     cannot be used with {field_type}",
                    typ.name, field.name
                ),
                dir.position.0,
                dir.position.1,
            ));
            return errs;
        }
        if field_type == "Int" && value.parse::<i64>().is_err() {
            errs.push(GraphQlError::at(
                format!(
                    "Type {}; Field {}: @default directive provides value \"{value}\" which \
                     cannot be used with {field_type}",
                    typ.name, field.name
                ),
                dir.position.0,
                dir.position.1,
            ));
            return errs;
        }
        if field_type == "Float" && value.parse::<f64>().is_err() {
            errs.push(GraphQlError::at(
                format!(
                    "Type {}; Field {}: @default directive provides value \"{value}\" which \
                     cannot be used with {field_type}",
                    typ.name, field.name
                ),
                dir.position.0,
                dir.position.1,
            ));
            return errs;
        }
        if field_type == "Boolean" && value != "true" && value != "false" {
            errs.push(GraphQlError::at(
                format!(
                    "Type {}; Field {}: @default directive provides value \"{value}\" which \
                     cannot be used with {field_type}",
                    typ.name, field.name
                ),
                dir.position.0,
                dir.position.1,
            ));
            return errs;
        }
    }
    errs
}

/// Validates `@id` field types and the `interface` argument.
pub(crate) fn id_validation(
    schema: &Schema,
    typ: &TypeDef,
    field: &FieldDef,
    dir: &Directive,
) -> ErrorList {
    let mut errs = ErrorList::default();
    if !matches!(field.ty.name(), "String" | "Int" | "Int64") || field.ty.is_list() {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; Field {}: with @id directive must be of type String, Int or Int64, \
                 not {}",
                typ.name, field.name, field.ty
            ),
            dir.position.0,
            dir.position.1,
        ));
        return errs;
    }
    let inherited = schema
        .implementors
        .get(&typ.name)
        .is_some_and(|implementors| {
            implementors.iter().any(|name| {
                schema
                    .type_def(name)
                    .is_some_and(|def| def.field(&field.name).is_some())
            })
        });
    if typ.kind != TypeKind::Interface && has_interface_arg(field) && !inherited {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; Field {}: @id field with interface argument can only be defined in \
                 interface,not in Type",
                typ.name, field.name
            ),
            dir.position.0,
            dir.position.1,
        ));
    }
    errs
}

/// Whether the field's `@id` carries `interface: true`.
fn has_interface_arg(field: &FieldDef) -> bool {
    if !has_id_directive(field) {
        return false;
    }
    field
        .directive(ID_DIRECTIVE)
        .and_then(|dir| dir.arg(ID_DIRECTIVE_INTERFACE_ARG))
        .is_some_and(|val| matches!(val, Value::Bool(true)))
}

/// Validates `@lambda` (requires a configured lambda URL, then custom checks).
pub(crate) fn lambda_directive_validation(
    schema: &Schema,
    typ: &TypeDef,
    field: &FieldDef,
    dir: &Directive,
    options: &ValidationOptions,
) -> ErrorList {
    if options.lambda_url.as_deref().unwrap_or_default().is_empty() {
        let mut errs = ErrorList::default();
        errs.push(GraphQlError::at(
            format!(
                "Type {}; Field {}: has the @lambda directive, but the `--graphql \
                 \"lambda-url=...;\" flag wasn't specified during alpha startup.",
                typ.name, field.name
            ),
            dir.position.0,
            dir.position.1,
        ));
        return errs;
    }
    let mut errs = crate::rules_custom::custom_directive_validation(
        schema,
        typ,
        field,
        &crate::rules_custom::custom_directive_for_lambda(field),
        &HashMap::new(),
    );
    for err in &mut errs.0 {
        err.message = format!("While building @custom for @lambda: {}", err.message);
    }
    errs
}
