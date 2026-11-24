//! Apollo federation and `@remoteResponse` validators. Port of
//! `apolloKeyValidation`, `apolloExtendsValidation`,
//! `apolloRequiresValidation`, `apolloProvidesValidation`,
//! `apolloExternalValidation`, and `remoteResponseValidation` from
//! `dgraph-main/graphql/schema/rules.go`.

#![warn(missing_docs)]

use crate::directives::{
    APOLLO_EXTENDS_DIRECTIVE, APOLLO_EXTERNAL_DIRECTIVE, APOLLO_KEY_ARG, APOLLO_KEY_DIRECTIVE,
    DATASTREAMDB_DIRECTIVE, ID_DIRECTIVE, INVERSE_DIRECTIVE, REMOTE_DIRECTIVE, SEARCH_DIRECTIVE,
};
use crate::errors::{ErrorList, GraphQlError};
use crate::model::{Directive, FieldDef, Schema, TypeDef};

/// Whether the field carries `@external`.
fn has_external(field: &FieldDef) -> bool {
    field.directive(APOLLO_EXTERNAL_DIRECTIVE).is_some()
}

/// Whether the field is a `@key` field of the type.
///
/// Note: Go compares the whole `fields` argument verbatim (multi-field keys
/// never match); mirrored exactly.
fn is_key_field(field: &FieldDef, typ: &TypeDef) -> bool {
    let Some(key) = typ.directive(APOLLO_KEY_DIRECTIVE) else {
        return false;
    };
    key.arg(APOLLO_KEY_ARG)
        .is_some_and(|val| val.raw() == field.name)
}

/// Validates `@key` placement, shape, and target field.
pub(crate) fn apollo_key_validation(_schema: &Schema, typ: &TypeDef) -> ErrorList {
    let mut errs = ErrorList::default();
    let keys: Vec<&Directive> = typ
        .directives
        .iter()
        .filter(|dir| dir.name == APOLLO_KEY_DIRECTIVE)
        .collect();
    if keys.is_empty() {
        return errs;
    }
    if keys.len() > 1 {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; @key directive should not be defined more than once.",
                typ.name
            ),
            keys[1].position.0,
            keys[1].position.1,
        ));
        return errs;
    }
    let dir = keys[0];
    let Some(arg) = dir.arg(APOLLO_KEY_ARG) else {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; Argument {APOLLO_KEY_ARG} inside @key directive must be defined.",
                typ.name
            ),
            dir.position.0,
            dir.position.1,
        ));
        return errs;
    };
    if arg.raw().is_empty() {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; Argument {APOLLO_KEY_ARG} inside @key directive must be defined.",
                typ.name
            ),
            dir.position.0,
            dir.position.1,
        ));
        return errs;
    }
    let Some(field) = typ.field(&arg.raw()) else {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; @key directive uses a field {} which is not defined inside the type.",
                typ.name,
                arg.raw()
            ),
            dir.position.0,
            dir.position.1,
        ));
        return errs;
    };
    if field.ty.name() != "ID" && field.directive(ID_DIRECTIVE).is_none() {
        errs.push(GraphQlError::at(
            format!(
                "Type {}: Field {}: used inside @key directive should be of type ID or have @id \
                 directive.",
                typ.name, field.name
            ),
            dir.position.0,
            dir.position.1,
        ));
        return errs;
    }
    if typ.directive(REMOTE_DIRECTIVE).is_some() {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; @remote directive cannot be defined with @key directive",
                typ.name
            ),
            typ.position.0,
            typ.position.1,
        ));
    }
    errs
}

/// Validates `@extends` (requires `@key`, forbids `@remote`).
pub(crate) fn apollo_extends_validation(_schema: &Schema, typ: &TypeDef) -> ErrorList {
    let mut errs = ErrorList::default();
    let Some(extends) = typ.directive(APOLLO_EXTENDS_DIRECTIVE) else {
        return errs;
    };
    if typ.directive(APOLLO_KEY_DIRECTIVE).is_none() {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; Type Extension cannot be defined without @key directive",
                typ.name
            ),
            extends.position.0,
            extends.position.1,
        ));
    }
    if typ.directive(REMOTE_DIRECTIVE).is_some() {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; @remote directive cannot be defined with @extends directive",
                typ.name
            ),
            typ.position.0,
            typ.position.1,
        ));
    }
    errs
}

/// Validates `@requires` (extended types only, external fields only).
pub(crate) fn apollo_requires_validation(
    _schema: &Schema,
    typ: &TypeDef,
    field: &FieldDef,
    dir: &Directive,
) -> ErrorList {
    let mut errs = ErrorList::default();
    if typ.directive(APOLLO_EXTENDS_DIRECTIVE).is_none() {
        errs.push(GraphQlError::at(
            format!(
                "Type {}: Field {}: @requires directive can only be defined on fields in type \
                 extensions. i.e., the type must have `@extends` or use `extend` keyword.",
                typ.name, field.name
            ),
            dir.position.0,
            dir.position.1,
        ));
        return errs;
    }
    let Some(arg) = dir.arg(APOLLO_KEY_ARG) else {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; Argument {APOLLO_KEY_ARG} inside @requires directive must be defined.",
                typ.name
            ),
            dir.position.0,
            dir.position.1,
        ));
        return errs;
    };
    if arg.raw().is_empty() {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; Argument {APOLLO_KEY_ARG} inside @requires directive must be defined.",
                typ.name
            ),
            dir.position.0,
            dir.position.1,
        ));
        return errs;
    }
    for name in arg.raw().split_whitespace() {
        let Some(target) = typ.field(name) else {
            errs.push(GraphQlError::at(
                format!(
                    "Type {}; @requires directive uses a field {name} which is not defined \
                     inside the type.",
                    typ.name
                ),
                dir.position.0,
                dir.position.1,
            ));
            return errs;
        };
        if !has_external(target) {
            errs.push(GraphQlError::at(
                format!("Type {}; Field {name} must be @external.", typ.name),
                dir.position.0,
                dir.position.1,
            ));
            return errs;
        }
    }
    errs
}

/// Validates `@provides` (target type needs `@key`, fields must exist).
pub(crate) fn apollo_provides_validation(
    schema: &Schema,
    typ: &TypeDef,
    field: &FieldDef,
    dir: &Directive,
) -> ErrorList {
    let mut errs = ErrorList::default();
    let Some(target) = schema.type_def(field.ty.name()) else {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; Field {} does not return a type that has a @key.",
                typ.name, field.name
            ),
            dir.position.0,
            dir.position.1,
        ));
        return errs;
    };
    if target.directive(APOLLO_KEY_DIRECTIVE).is_none() {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; Field {} does not return a type that has a @key.",
                typ.name, field.name
            ),
            dir.position.0,
            dir.position.1,
        ));
        return errs;
    }
    let Some(arg) = dir.arg(APOLLO_KEY_ARG) else {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; Argument {APOLLO_KEY_ARG} inside @provides directive must be defined.",
                typ.name
            ),
            dir.position.0,
            dir.position.1,
        ));
        return errs;
    };
    if arg.raw().is_empty() {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; Argument {APOLLO_KEY_ARG} inside @provides directive must be defined.",
                typ.name
            ),
            dir.position.0,
            dir.position.1,
        ));
        return errs;
    }
    for name in arg.raw().split_whitespace() {
        if target.field(name).is_none() {
            errs.push(GraphQlError::at(
                format!(
                    "Type {}; Field {}: @provides field {name} doesn't exist for type {}.",
                    typ.name, field.name, target.name
                ),
                dir.position.0,
                dir.position.1,
            ));
            return errs;
        }
    }
    errs
}

/// Validates `@external` (extended types only, no custom/lambda, restricted
/// directives unless a key field).
pub(crate) fn apollo_external_validation(
    _schema: &Schema,
    typ: &TypeDef,
    field: &FieldDef,
    dir: &Directive,
) -> ErrorList {
    let mut errs = ErrorList::default();
    if typ.directive(APOLLO_EXTENDS_DIRECTIVE).is_none() {
        errs.push(GraphQlError::at(
            format!(
                "Type {}: Field {}: @external directive can only be defined on fields in type \
                 extensions. i.e., the type must have `@extends` or use `extend` keyword.",
                typ.name, field.name
            ),
            dir.position.0,
            dir.position.1,
        ));
        return errs;
    }
    if crate::rules_util::has_custom_or_lambda(field) {
        errs.push(GraphQlError::at(
            format!(
                "Type {}: Field {}: @external directive can not be defined on  fields with \
                 @custom or @lambda directive.",
                typ.name, field.name
            ),
            dir.position.0,
            dir.position.1,
        ));
        return errs;
    }
    if !is_key_field(field, typ) {
        for name in [
            INVERSE_DIRECTIVE,
            SEARCH_DIRECTIVE,
            DATASTREAMDB_DIRECTIVE,
            ID_DIRECTIVE,
        ] {
            if let Some(other) = field.directive(name) {
                errs.push(GraphQlError::at(
                    format!(
                        "Type {}: Field {}: @{name} directive can not be defined on @external \
                         fields that are not @key.",
                        typ.name, field.name
                    ),
                    other.position.0,
                    other.position.1,
                ));
                return errs;
            }
        }
    }
    errs
}

/// Validates `@remoteResponse` (remote types only, `name` required).
pub(crate) fn remote_response_validation(
    _schema: &Schema,
    typ: &TypeDef,
    field: &FieldDef,
    dir: &Directive,
) -> ErrorList {
    let mut errs = ErrorList::default();
    if typ.directive(REMOTE_DIRECTIVE).is_none() {
        errs.push(GraphQlError::at(
            format!(
                "Type {}: Field {}: @remoteResponse directive can only be defined on fields of \
                 @remote type.",
                typ.name, field.name
            ),
            dir.position.0,
            dir.position.1,
        ));
        return errs;
    }
    let missing = dir.arg("name").is_none_or(|val| val.raw().is_empty());
    if missing {
        errs.push(GraphQlError::at(
            format!(
                "Type {}: Field {}: Argument name inside @remoteResponse directive must be \
                 defined.",
                typ.name, field.name
            ),
            dir.position.0,
            dir.position.1,
        ));
    }
    errs
}
