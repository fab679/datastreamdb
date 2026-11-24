//! Pre-GraphQL validations: schema-document and bare-definition rules. Port
//! of `typeNameValidation`, `customQueryNameValidation`,
//! `customMutationNameValidation`, `dataTypeCheck`, `nameCheck`, and
//! `directiveLocationCheck` from `dgraph-main/graphql/schema/rules.go`.

#![warn(missing_docs)]

use std::collections::HashSet;

use crate::directives::{
    APOLLO_EXTENDS_DIRECTIVE, APOLLO_EXTERNAL_DIRECTIVE, APOLLO_KEY_DIRECTIVE,
    APOLLO_PROVIDES_DIRECTIVE, APOLLO_REQUIRES_DIRECTIVE, AUTH_DIRECTIVE, CASCADE_DIRECTIVE,
    CUSTOM_DIRECTIVE, DATASTREAMDB_DIRECTIVE, DEFAULT_DIRECTIVE, EMBEDDING_DIRECTIVE,
    GENERATE_DIRECTIVE, ID_DIRECTIVE, INVERSE_DIRECTIVE, LAMBDA_DIRECTIVE,
    LAMBDA_ON_MUTATE_DIRECTIVE, REMOTE_DIRECTIVE, REMOTE_RESPONSE_DIRECTIVE, SEARCH_DIRECTIVE,
    SECRET_DIRECTIVE, SUBSCRIPTION_DIRECTIVE,
};
use crate::errors::{ErrorList, GraphQlError};
use crate::model::{Schema, TypeDef, TypeKind};
use crate::rules_util::{
    has_custom_or_lambda, is_query_or_mutation, is_query_or_mutation_type, is_reserved_keyword,
};

/// Type-level applicability of a directive. Port of Go `directiveLocationMap`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TypeApplicability {
    /// Unknown directive (checked structurally elsewhere).
    Unknown,
    /// Field-only directive.
    FieldOnly,
    /// Allowed on these type kinds.
    Kinds(&'static [TypeKind]),
}

/// Directives usable at type level, with allowed kinds.
fn directive_type_locations(name: &str) -> TypeApplicability {
    match name {
        INVERSE_DIRECTIVE
        | SEARCH_DIRECTIVE
        | ID_DIRECTIVE
        | CUSTOM_DIRECTIVE
        | LAMBDA_DIRECTIVE
        | APOLLO_EXTERNAL_DIRECTIVE
        | APOLLO_REQUIRES_DIRECTIVE
        | APOLLO_PROVIDES_DIRECTIVE
        | REMOTE_RESPONSE_DIRECTIVE
        | CASCADE_DIRECTIVE
        | EMBEDDING_DIRECTIVE
        | DEFAULT_DIRECTIVE => TypeApplicability::FieldOnly,
        DATASTREAMDB_DIRECTIVE
        | SUBSCRIPTION_DIRECTIVE
        | SECRET_DIRECTIVE
        | AUTH_DIRECTIVE
        | LAMBDA_ON_MUTATE_DIRECTIVE
        | GENERATE_DIRECTIVE
        | APOLLO_KEY_DIRECTIVE
        | APOLLO_EXTENDS_DIRECTIVE => {
            TypeApplicability::Kinds(&[TypeKind::Object, TypeKind::Interface])
        }
        REMOTE_DIRECTIVE => TypeApplicability::Kinds(&[
            TypeKind::Object,
            TypeKind::Interface,
            TypeKind::Union,
            TypeKind::InputObject,
            TypeKind::Enum,
        ]),
        _ => TypeApplicability::Unknown,
    }
}

/// Rejects user types named like generated/static types.
pub(crate) fn type_name_validation(schema: &Schema) -> ErrorList {
    let mut forbidden: HashSet<String> = [
        "Int64",
        "DateTime",
        "DataStreamDBIndex",
        "AuthRule",
        "HTTPMethod",
        "Mode",
        "CustomHTTP",
        "IntFilter",
        "Int64Filter",
        "FloatFilter",
        "DateTimeFilter",
        "StringTermFilter",
        "StringRegExpFilter",
        "StringFullTextFilter",
        "StringExactFilter",
        "StringHashFilter",
        "PointGeoFilter",
        "PointRef",
        "NearFilter",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();

    for name in &schema.order {
        let defn = &schema.types[name];
        if is_query_or_mutation(&defn.name) || defn.directive(REMOTE_DIRECTIVE).is_some() {
            continue;
        }
        match defn.kind {
            TypeKind::Union => {
                forbidden.insert(format!("{}Ref", defn.name));
                forbidden.insert(format!("{}Filter", defn.name));
                forbidden.insert(format!("{}Type", defn.name));
            }
            TypeKind::Object | TypeKind::Interface => {
                forbidden.insert(format!("{}Ref", defn.name));
                forbidden.insert(format!("{}Patch", defn.name));
                forbidden.insert(format!("Update{}Input", defn.name));
                forbidden.insert(format!("Update{}Payload", defn.name));
                forbidden.insert(format!("Delete{}Input", defn.name));
                forbidden.insert(format!("{}AggregateResult", defn.name));
                if defn.kind == TypeKind::Object {
                    forbidden.insert(format!("Add{}Input", defn.name));
                    forbidden.insert(format!("Add{}Payload", defn.name));
                }
                forbidden.insert(format!("{}Filter", defn.name));
                forbidden.insert(format!("{}Order", defn.name));
                forbidden.insert(format!("{}Orderable", defn.name));
            }
            _ => {}
        }
    }

    let mut errs = ErrorList::default();
    for name in &schema.order {
        let defn = &schema.types[name];
        if forbidden.contains(&defn.name) {
            errs.push(GraphQlError::at(
                format!(
                    "{} is a reserved word, so you can't declare a {} with this name. \
                     Pick a different name for the {}.",
                    defn.name,
                    defn.kind.go_name(),
                    defn.kind.go_name()
                ),
                defn.position.0,
                defn.position.1,
            ));
        }
    }
    errs
}

/// Rejects custom queries named like generated queries.
pub(crate) fn custom_query_name_validation(schema: &Schema) -> ErrorList {
    let mut forbidden: HashSet<String> = HashSet::new();
    for name in &schema.order {
        let defn = &schema.types[name];
        if defn.name == "Query"
            || !matches!(defn.kind, TypeKind::Object | TypeKind::Interface)
            || defn.directive(REMOTE_DIRECTIVE).is_some()
        {
            continue;
        }
        forbidden.insert(format!("get{}", defn.name));
        forbidden.insert(format!("check{}Password", defn.name));
        forbidden.insert(format!("query{}", defn.name));
    }
    let mut errs = ErrorList::default();
    if let Some(query) = schema.type_def("Query") {
        for field in &query.fields {
            if forbidden.contains(&field.name) {
                errs.push(GraphQlError::at(
                    format!(
                        "{} is a reserved word, so you can't declare a query with this name. \
                         Pick a different name for the query.",
                        field.name
                    ),
                    field.position.0,
                    field.position.1,
                ));
            }
        }
    }
    errs
}

/// Rejects custom mutations named like generated mutations.
pub(crate) fn custom_mutation_name_validation(schema: &Schema) -> ErrorList {
    let mut forbidden: HashSet<String> = HashSet::new();
    for name in &schema.order {
        let defn = &schema.types[name];
        if defn.name == "Mutation"
            || !matches!(defn.kind, TypeKind::Object | TypeKind::Interface)
            || defn.directive(REMOTE_DIRECTIVE).is_some()
        {
            continue;
        }
        match defn.kind {
            TypeKind::Interface => {
                forbidden.insert(format!("update{}", defn.name));
                forbidden.insert(format!("delete{}", defn.name));
            }
            TypeKind::Object => {
                forbidden.insert(format!("add{}", defn.name));
                forbidden.insert(format!("update{}", defn.name));
                forbidden.insert(format!("delete{}", defn.name));
            }
            _ => {}
        }
    }
    let mut errs = ErrorList::default();
    if let Some(mutation) = schema.type_def("Mutation") {
        for field in &mutation.fields {
            if forbidden.contains(&field.name) {
                errs.push(GraphQlError::at(
                    format!(
                        "{} is a reserved word, so you can't declare a mutation with this name. \
                         Pick a different name for the mutation.",
                        field.name
                    ),
                    field.position.0,
                    field.position.1,
                ));
            }
        }
    }
    errs
}

/// Rejects user scalar definitions.
pub(crate) fn data_type_check(defn: &TypeDef) -> ErrorList {
    let mut errs = ErrorList::default();
    if defn.kind == TypeKind::Scalar {
        errs.push(GraphQlError::at(
            "You can't add scalar definitions. Only type, interface, union, input and enums \
             are allowed in initial schema.",
            defn.position.0,
            defn.position.1,
        ));
    }
    errs
}

/// Rejects reserved type names; root types may only carry custom/lambda fields.
pub(crate) fn name_check(defn: &TypeDef) -> ErrorList {
    let mut errs = ErrorList::default();
    if defn.kind != TypeKind::Scalar && is_reserved_keyword(&defn.name) {
        if is_query_or_mutation_type(defn) {
            for field in &defn.fields {
                if !has_custom_or_lambda(field) {
                    errs.push(GraphQlError::at(
                        format!(
                            "GraphQL Query and Mutation types are only allowed to have fields \
                             with @custom/@lambda directive. Other fields are built automatically \
                             for you. Found {} {} without @custom/@lambda.",
                            defn.name, field.name
                        ),
                        defn.position.0,
                        defn.position.1,
                    ));
                    return errs;
                }
            }
        } else {
            errs.push(GraphQlError::at(
                format!(
                    "{} is a reserved word, so you can't declare a type with this name. \
                     Pick a different name for the type.",
                    defn.name
                ),
                defn.position.0,
                defn.position.1,
            ));
        }
    }
    errs
}

/// Rejects type-level directives that don't apply there.
pub(crate) fn directive_location_check(defn: &TypeDef) -> ErrorList {
    let mut errs = ErrorList::default();
    for dir in &defn.directives {
        match directive_type_locations(&dir.name) {
            TypeApplicability::Unknown => {}
            TypeApplicability::FieldOnly => errs.push(GraphQlError::at(
                format!(
                    "Type {}; has the @{} directive, but it is not applicable at type level.",
                    defn.name, dir.name
                ),
                dir.position.0,
                dir.position.1,
            )),
            TypeApplicability::Kinds(kinds) => {
                if !kinds.contains(&defn.kind) {
                    errs.push(GraphQlError::at(
                        format!(
                            "Type {}; has the @{} directive, but it is not applicable on types \
                             of {} kind.",
                            defn.name,
                            dir.name,
                            defn.kind.go_name()
                        ),
                        dir.position.0,
                        dir.position.1,
                    ));
                }
            }
        }
    }
    errs
}

/// Runs all schema-document validations.
pub(crate) fn apply_schema_doc_validations(schema: &Schema) -> ErrorList {
    let mut errs = ErrorList::default();
    errs.extend(type_name_validation(schema));
    errs.extend(custom_query_name_validation(schema));
    errs.extend(custom_mutation_name_validation(schema));
    errs
}

/// Runs all bare-definition validations.
pub(crate) fn apply_defn_validations(defn: &TypeDef) -> ErrorList {
    let mut errs = ErrorList::default();
    errs.extend(data_type_check(defn));
    errs.extend(name_check(defn));
    errs.extend(directive_location_check(defn));
    errs
}
