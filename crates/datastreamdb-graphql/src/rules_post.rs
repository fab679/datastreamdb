//! Post-GraphQL type and field validations. Port of the `typeValidations`
//! and `fieldValidations` rules from `dgraph-main/graphql/schema/rules.go`:
//! id counts, `@datastreamdb`/`@secret` types, conflicts, remote types,
//! `@generate`, `@lambdaOnMutate`, and field-level checks.

#![warn(missing_docs)]

use crate::directives::{
    AUTH_DIRECTIVE, CUSTOM_DIRECTIVE, DATASTREAMDB_DIRECTIVE, DATASTREAMDB_TYPE_ARG, DQL_ARG,
    GENERATE_ADD_FIELD, GENERATE_AGGREGATE_FIELD, GENERATE_DELETE_FIELD, GENERATE_DIRECTIVE,
    GENERATE_GET_FIELD, GENERATE_MUTATION_ARG, GENERATE_PASSWORD_FIELD, GENERATE_QUERY_ARG,
    GENERATE_QUERY_FIELD, GENERATE_SUBSCRIPTION_ARG, GENERATE_UPDATE_FIELD,
    LAMBDA_ON_MUTATE_DIRECTIVE, REMOTE_DIRECTIVE, SECRET_DIRECTIVE, SUBSCRIPTION_DIRECTIVE,
};
use crate::errors::{ErrorList, GraphQlError};
use crate::model::{FieldDef, Schema, TypeDef, TypeKind, Value};
use crate::rules_util::{
    has_custom_or_lambda, is_id_field, is_query_or_mutation_type, is_reserved_argument,
    is_reserved_keyword, is_scalar,
};

/// Lambda URL configuration for embedded validation (no alpha flags here).
#[derive(Debug, Clone, Default)]
pub struct ValidationOptions {
    /// Lambda endpoint URL; `None` means lambda is unavailable.
    pub lambda_url: Option<String>,
}

/// At most one ID field per type.
pub(crate) fn id_count_check(_schema: &Schema, typ: &TypeDef) -> ErrorList {
    let id_fields: Vec<&FieldDef> = typ.fields.iter().filter(|f| is_id_field(f)).collect();
    let mut errs = ErrorList::default();
    if id_fields.len() > 1 {
        let names: Vec<&str> = id_fields.iter().map(|f| f.name.as_str()).collect();
        let joined = format!(
            "{} and {}",
            names[..names.len() - 1].join(", "),
            names[names.len() - 1]
        );
        let mut err = GraphQlError::new(format!(
            "Fields {joined} are listed as IDs for type {}, but a type can have only one ID \
             field. Pick a single field as the ID for type {}.",
            typ.name, typ.name
        ));
        err.locations = id_fields.iter().map(|f| f.position).collect();
        errs.push(err);
    }
    errs
}

/// Validates the type argument of a type-level `@datastreamdb` directive.
pub(crate) fn datastreamdb_directive_type_validation(_schema: &Schema, typ: &TypeDef) -> ErrorList {
    let mut errs = ErrorList::default();
    let Some(dir) = typ.directive(DATASTREAMDB_DIRECTIVE) else {
        return errs;
    };
    let Some(type_arg) = dir.arg(DATASTREAMDB_TYPE_ARG) else {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; type argument for @datastreamdb directive should not be empty.",
                typ.name
            ),
            dir.position.0,
            dir.position.1,
        ));
        return errs;
    };
    if type_arg.raw().is_empty() {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; type argument for @datastreamdb directive should not be empty.",
                typ.name
            ),
            dir.position.0,
            dir.position.1,
        ));
        return errs;
    }
    if !matches!(type_arg, Value::String(_)) {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; type argument for @datastreamdb directive should of type String.",
                typ.name
            ),
            dir.position.0,
            dir.position.1,
        ));
        return errs;
    }
    if is_reserved_keyword(&type_arg.raw()) {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; type argument '{}' for @datastreamdb directive is a reserved keyword.",
                typ.name,
                type_arg.raw()
            ),
            dir.position.0,
            dir.position.1,
        ));
    }
    errs
}

/// Validates `@secret` directives (at most one, naming no existing field).
pub(crate) fn password_directive_validation(_schema: &Schema, typ: &TypeDef) -> ErrorList {
    let mut errs = ErrorList::default();
    let mut fields: Vec<String> = Vec::new();
    for dir in &typ.directives {
        if dir.name != SECRET_DIRECTIVE {
            continue;
        }
        let val = dir.arg("field").map_or(String::new(), Value::raw);
        if val.is_empty() {
            errs.push(GraphQlError::at(
                format!(
                    "Type {}; Argument \"field\" of secret directive is empty",
                    typ.name
                ),
                typ.position.0,
                typ.position.1,
            ));
            return errs;
        }
        fields.push(val);
    }
    if fields.is_empty() {
        return errs;
    }
    if fields.len() > 1 {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; has more than one secret fields {}",
                typ.name,
                fields.join(",")
            ),
            typ.position.0,
            typ.position.1,
        ));
        return errs;
    }
    let val = &fields[0];
    if typ.fields.iter().any(|f| &f.name == val) {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; has a secret directive and field of the same name {val}",
                typ.name
            ),
            typ.position.0,
            typ.position.1,
        ));
    }
    errs
}

/// Rejects `@auth`+`@remote` and `@withSubscription`+`@remote` mixes.
pub(crate) fn conflicting_directive_validation(_schema: &Schema, typ: &TypeDef) -> ErrorList {
    let mut errs = ErrorList::default();
    let (mut has_auth, mut has_remote, mut has_subscription) = (false, false, false);
    for dir in &typ.directives {
        match dir.name.as_str() {
            AUTH_DIRECTIVE => has_auth = true,
            REMOTE_DIRECTIVE => has_remote = true,
            SUBSCRIPTION_DIRECTIVE => has_subscription = true,
            _ => {}
        }
    }
    if has_auth && has_remote {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; cannot have both @{AUTH_DIRECTIVE} and @{REMOTE_DIRECTIVE} directive",
                typ.name
            ),
            typ.position.0,
            typ.position.1,
        ));
    }
    if has_subscription && has_remote {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; cannot have both @{SUBSCRIPTION_DIRECTIVE} and @{REMOTE_DIRECTIVE} directive",
                typ.name
            ),
            typ.position.0,
            typ.position.1,
        ));
    }
    errs
}

/// Requires a non-ID, non-custom field on concrete object types.
pub(crate) fn non_id_fields_check(_schema: &Schema, typ: &TypeDef) -> ErrorList {
    let mut errs = ErrorList::default();
    if typ.kind != TypeKind::Object || is_query_or_mutation_type(typ) {
        return errs;
    }
    if typ.directive(REMOTE_DIRECTIVE).is_some() {
        return errs;
    }
    let has_non_id = typ
        .fields
        .iter()
        .any(|field| !is_id_field(field) && !has_custom_or_lambda(field));
    if !has_non_id {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; is invalid, a type must have atleast one field that is not of ID! \
                 type and doesn't have @custom/@lambda directive.",
                typ.name
            ),
            typ.position.0,
            typ.position.1,
        ));
    }
    errs
}

/// Validates `@remote` placement on types, fields, and interfaces.
pub(crate) fn remote_type_validation(schema: &Schema, typ: &TypeDef) -> ErrorList {
    let mut errs = ErrorList::default();
    if is_query_or_mutation_type(typ) {
        return errs;
    }
    if typ.directive(REMOTE_DIRECTIVE).is_none() {
        for field in &typ.fields {
            if has_custom_or_lambda(field) {
                continue;
            }
            if let Some(target) = schema.type_def(field.ty.name()) {
                if target.directive(REMOTE_DIRECTIVE).is_some() {
                    errs.push(GraphQlError::at(
                        format!(
                            "Type {}; field {}; is of a type that has @remote directive. Those \
                             would need to be resolved by a @custom/@lambda directive.",
                            typ.name, field.name
                        ),
                        field.position.0,
                        field.position.1,
                    ));
                    return errs;
                }
            }
        }
        for implements in &typ.implements {
            if let Some(target) = schema.type_def(implements) {
                if target.directive(REMOTE_DIRECTIVE).is_some() {
                    errs.push(GraphQlError::at(
                        format!(
                            "Type {}; without @remote directive can't implement an interface \
                             {}; with have @remote directive.",
                            typ.name, implements
                        ),
                        typ.position.0,
                        typ.position.1,
                    ));
                    return errs;
                }
            }
        }
        return errs;
    }
    for field in &typ.fields {
        if has_custom_or_lambda(field) {
            errs.push(GraphQlError::at(
                format!(
                    "Type {}; field {}; can't have @custom/@lambda directive as a @remote type \
                     can't have fields with @custom/@lambda directive.",
                    typ.name, field.name
                ),
                field.position.0,
                field.position.1,
            ));
            return errs;
        }
    }
    for implements in &typ.implements {
        if let Some(target) = schema.type_def(implements) {
            if target.directive(REMOTE_DIRECTIVE).is_none() {
                errs.push(GraphQlError::at(
                    format!(
                        "Type {}; with @remote directive implements interface {}; which doesn't \
                         have @remote directive.",
                        typ.name, implements
                    ),
                    typ.position.0,
                    typ.position.1,
                ));
                return errs;
            }
        }
    }
    errs
}

/// Validates `@lambdaOnMutate` placement and boolean arguments.
pub(crate) fn lambda_on_mutate_validation(
    _schema: &Schema,
    typ: &TypeDef,
    options: &ValidationOptions,
) -> ErrorList {
    let mut errs = ErrorList::default();
    let Some(dir) = typ.directive(LAMBDA_ON_MUTATE_DIRECTIVE) else {
        return errs;
    };
    if options.lambda_url.as_deref().unwrap_or_default().is_empty() {
        errs.push(GraphQlError::at(
            format!(
                "Type {}: has the @lambdaOnMutate directive, but the `--graphql lambda-url` \
                 flag wasn't specified during alpha startup.",
                typ.name
            ),
            dir.position.0,
            dir.position.1,
        ));
    }
    if typ.directive(REMOTE_DIRECTIVE).is_some() {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; @lambdaOnMutate directive not allowed along with @remote directive.",
                typ.name
            ),
            dir.position.0,
            dir.position.1,
        ));
    }
    for (name, val) in &dir.args {
        if !matches!(val, Value::Bool(_)) {
            errs.push(GraphQlError::at(
                format!(
                    "Type {}; {name} argument in @lambdaOnMutate directive can only be \
                     true/false, found: `{val}`.",
                    typ.name
                ),
                dir.position.0,
                dir.position.1,
            ));
        }
    }
    errs
}

/// Validates `@generate` arguments (Go-exact messages, including the
/// unterminated backtick quirk).
pub(crate) fn generate_directive_validation(_schema: &Schema, typ: &TypeDef) -> ErrorList {
    let mut errs = ErrorList::default();
    let Some(dir) = typ.directive(GENERATE_DIRECTIVE) else {
        return errs;
    };
    if let Some(query_arg) = dir.arg(GENERATE_QUERY_ARG) {
        if !matches!(query_arg, Value::Object(_)) {
            errs.push(GraphQlError::at(
                format!(
                    "Type {}; query argument for @generate directive should be of type Object.",
                    typ.name
                ),
                dir.position.0,
                dir.position.1,
            ));
        }
        for (field, label) in [
            (GENERATE_GET_FIELD, "get"),
            (GENERATE_QUERY_FIELD, "query"),
            (GENERATE_PASSWORD_FIELD, "password"),
            (GENERATE_AGGREGATE_FIELD, "aggregate"),
        ] {
            if let Some(val) = query_arg.field(field) {
                if !matches!(val, Value::Bool(_)) {
                    errs.push(GraphQlError::at(
                        format!(
                            "Type {}; {label} field inside query argument of @generate directive \
                             can only be true/false, found: `{}",
                            typ.name,
                            val.raw()
                        ),
                        dir.position.0,
                        dir.position.1,
                    ));
                }
            }
        }
    }
    if let Some(mutation_arg) = dir.arg(GENERATE_MUTATION_ARG) {
        if !matches!(mutation_arg, Value::Object(_)) {
            errs.push(GraphQlError::at(
                format!(
                    "Type {}; mutation argument for @generate directive should be of type \
                     Object.",
                    typ.name
                ),
                dir.position.0,
                dir.position.1,
            ));
        }
        for (field, label) in [
            (GENERATE_ADD_FIELD, "add"),
            (GENERATE_UPDATE_FIELD, "update"),
            (GENERATE_DELETE_FIELD, "delete"),
        ] {
            if let Some(val) = mutation_arg.field(field) {
                if !matches!(val, Value::Bool(_)) {
                    errs.push(GraphQlError::at(
                        format!(
                            "Type {}; {label} field inside mutation argument of @generate \
                             directive can only be true/false, found: `{}",
                            typ.name,
                            val.raw()
                        ),
                        dir.position.0,
                        dir.position.1,
                    ));
                }
            }
        }
    }
    if let Some(subscription_arg) = dir.arg(GENERATE_SUBSCRIPTION_ARG) {
        if !matches!(subscription_arg, Value::Bool(_)) {
            errs.push(GraphQlError::at(
                format!(
                    "Type {}; subscription argument in @generate directive can only be \
                     true/false, found: `{}`.",
                    typ.name,
                    subscription_arg.raw()
                ),
                dir.position.0,
                dir.position.1,
            ));
        }
    }
    errs
}

/// Rejects nested lists.
pub(crate) fn list_validity_check(typ: &TypeDef, field: &FieldDef) -> ErrorList {
    let mut errs = ErrorList::default();
    if field.ty.is_list() {
        if let crate::model::BaseType::List(inner) = &field.ty.base {
            if inner.is_list() {
                errs.push(GraphQlError::at(
                    format!(
                        "Type {}; Field {}: Nested lists are invalid.",
                        typ.name, field.name
                    ),
                    field.position.0,
                    field.position.1,
                ));
            }
        }
    }
    errs
}

/// Rejects reserved argument names on non-root fields.
pub(crate) fn field_argument_check(typ: &TypeDef, field: &FieldDef) -> ErrorList {
    let mut errs = ErrorList::default();
    if is_query_or_mutation_type(typ) || typ.directive(REMOTE_DIRECTIVE).is_some() {
        return errs;
    }
    for arg in &field.args {
        if is_reserved_argument(&arg.name) {
            errs.push(GraphQlError::at(
                format!(
                    "Type {}; Field {}: can't have {} as an argument because it is a reserved \
                     argument.",
                    typ.name, field.name, arg.name
                ),
                field.position.0,
                field.position.1,
            ));
            return errs;
        }
    }
    errs
}

/// Rejects reserved field names and `Aggregate` suffixes.
pub(crate) fn field_name_check(typ: &TypeDef, field: &FieldDef) -> ErrorList {
    let mut errs = ErrorList::default();
    if is_reserved_keyword(&field.name) {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; Field {}: {} is a reserved keyword and you cannot declare a field \
                 with this name.",
                typ.name, field.name, field.name
            ),
            field.position.0,
            field.position.1,
        ));
    }
    if field.name.ends_with("Aggregate") {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; Field {}: Aggregate is a reserved keyword and you cannot declare a \
                 field with Aggregate as suffix.",
                typ.name, field.name
            ),
            field.position.0,
            field.position.1,
        ));
    }
    errs
}

/// Rejects `ID` and `Boolean` lists.
pub(crate) fn is_valid_field_for_list(typ: &TypeDef, field: &FieldDef) -> ErrorList {
    let mut errs = ErrorList::default();
    if !field.ty.is_list() {
        return errs;
    }
    if let crate::model::BaseType::List(inner) = &field.ty.base {
        match inner.name() {
            "ID" | "Boolean" => errs.push(GraphQlError::at(
                format!(
                    "Type {}; Field {}: {} lists are invalid.",
                    typ.name,
                    field.name,
                    inner.name()
                ),
                field.position.0,
                field.position.1,
            )),
            _ => {}
        }
    }
    errs
}

/// Rejects `@auth` on fields.
pub(crate) fn has_auth_directive(typ: &TypeDef, field: &FieldDef) -> ErrorList {
    let mut errs = ErrorList::default();
    if field.directive(AUTH_DIRECTIVE).is_some() {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; Field {}: @{AUTH_DIRECTIVE} directive is not allowed on fields",
                typ.name, field.name
            ),
            field.position.0,
            field.position.1,
        ));
    }
    errs
}

/// Validates `@withSubscription` placement (types and custom DQL queries).
pub(crate) fn field_directive_check(typ: &TypeDef, field: &FieldDef) -> ErrorList {
    let mut errs = ErrorList::default();
    let subs = field.directive(SUBSCRIPTION_DIRECTIVE);
    let custom = field.directive(CUSTOM_DIRECTIVE);
    if subs.is_some() && typ.name != "Query" {
        errs.push(GraphQlError::at(
            format!(
                "Type {}; Field {}: @withSubscription directive is applicable only on types \
                 and custom dql queries",
                typ.name, field.name
            ),
            field.position.0,
            field.position.1,
        ));
        return errs;
    }
    if subs.is_some() && typ.name == "Query" && custom.is_some() {
        let has_dql = custom.is_some_and(|dir| dir.arg(DQL_ARG).is_some());
        if !has_dql {
            errs.push(GraphQlError::at(
                format!(
                    "Type {}; Field {}: custom query should have dql argument if \
                     @withSubscription directive is set",
                    typ.name, field.name
                ),
                field.position.0,
                field.position.1,
            ));
        }
    }
    errs
}

/// Runs all type validations (Apollo key/extends rules included).
pub(crate) fn apply_type_validations(
    schema: &Schema,
    typ: &TypeDef,
    options: &ValidationOptions,
) -> ErrorList {
    let mut errs = ErrorList::default();
    errs.extend(id_count_check(schema, typ));
    errs.extend(datastreamdb_directive_type_validation(schema, typ));
    errs.extend(password_directive_validation(schema, typ));
    errs.extend(conflicting_directive_validation(schema, typ));
    errs.extend(non_id_fields_check(schema, typ));
    errs.extend(remote_type_validation(schema, typ));
    errs.extend(generate_directive_validation(schema, typ));
    errs.extend(crate::rules_apollo::apollo_key_validation(schema, typ));
    errs.extend(crate::rules_apollo::apollo_extends_validation(schema, typ));
    errs.extend(lambda_on_mutate_validation(schema, typ, options));
    errs
}

/// Runs all field validations.
pub(crate) fn apply_field_validations(typ: &TypeDef, field: &FieldDef) -> ErrorList {
    let mut errs = ErrorList::default();
    errs.extend(list_validity_check(typ, field));
    errs.extend(field_argument_check(typ, field));
    errs.extend(field_name_check(typ, field));
    errs.extend(is_valid_field_for_list(typ, field));
    errs.extend(has_auth_directive(typ, field));
    errs.extend(field_directive_check(typ, field));
    errs
}

/// Whether the field type is a scalar or enum (for `@default` checks).
pub(crate) fn is_scalar_or_enum(schema: &Schema, field: &FieldDef) -> bool {
    is_scalar(field.ty.name())
        || schema
            .type_def(field.ty.name())
            .is_some_and(|def| def.kind == TypeKind::Enum)
}
