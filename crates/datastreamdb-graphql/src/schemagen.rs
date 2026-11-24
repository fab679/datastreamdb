//! Schema handler: parse, expand, validate, complete. Port of
//! `NewHandler`, `FromString`, `genDgSchema`, and `getAllowedHeaders` from
//! `dgraph-main/graphql/schema/schemagen.go` plus `expandSchema` from
//! `gqlschema.go`.
//!
//! Deltas from Go: parsing/structural validation use `async-graphql-parser`
//! instead of gqlparser (syntax and structural messages differ); JWK fetching
//! at build time is skipped (no network; fetch happens lazily at request
//! time in the auth stage).

#![warn(missing_docs)]

use std::collections::{HashMap, HashSet};

use async_graphql_parser::parse_schema;

use crate::complete_ops::{clean_schema, complete_schema};
use crate::dgschema::gen_dg_schema;
use crate::directives::{
    APOLLO_KEY_ARG, APOLLO_KEY_DIRECTIVE, APOLLO_PROVIDES_DIRECTIVE, APOLLO_SCHEMA_EXTRAS,
    APOLLO_SCHEMA_QUERIES, CUSTOM_DIRECTIVE, DIRECTIVE_DEFS, FILTER_INPUTS, REMOTE_DIRECTIVE,
    SCHEMA_INPUTS,
};
use crate::errors::{ErrorList, GraphQlError};
use crate::meta::{MetaInfo, parse_meta_info};
use crate::model::{Directive, FieldDef, Schema, TypeDef, TypeKind, schema_from_document};
use crate::rules_post::{ValidationOptions, apply_field_validations, apply_type_validations};
use crate::rules_pre::{apply_defn_validations, apply_schema_doc_validations};
use crate::sdl::stringify;

/// A processed GraphQL schema: completed GQL plus derived DQL.
#[derive(Debug, Clone)]
pub struct Handler {
    /// Original input text.
    input: String,
    /// User definition names in source order.
    original_defs: Vec<String>,
    /// Completed schema.
    complete_schema: Schema,
    /// Derived `DataStreamDB` schema.
    datastreamdb_schema: String,
    /// Schema metadata.
    meta: MetaInfo,
}

impl Handler {
    /// Schema metadata (secrets, CORS, auth).
    #[must_use]
    pub fn meta_info(&self) -> &MetaInfo {
        &self.meta
    }

    /// Completed GraphQL schema as SDL.
    #[must_use]
    pub fn gql_schema(&self) -> String {
        stringify(&self.complete_schema, &self.original_defs, false)
    }

    /// Derived `DataStreamDB` schema.
    #[must_use]
    pub fn dg_schema(&self) -> &str {
        &self.datastreamdb_schema
    }

    /// Completed schema without Apollo extras.
    #[must_use]
    pub fn gql_schema_without_apollo_extras(&self) -> String {
        let mut pruned = self.complete_schema.clone();
        pruned.remove_type("_Entity");
        for def in pruned.types.values_mut() {
            def.directives
                .retain(|dir| dir.name != "generate" && dir.name != "auth");
            for field in &mut def.fields {
                field.directives.retain(|dir| dir.name != CUSTOM_DIRECTIVE);
            }
        }
        if let Some(query) = pruned.type_def_mut("Query") {
            query
                .fields
                .retain(|field| field.name != "_entities" && field.name != "_service");
        }
        stringify(&pruned, &self.original_defs, true)
    }

    /// Completed schema model (for resolvers).
    #[must_use]
    pub fn schema(&self) -> &Schema {
        &self.complete_schema
    }

    /// Original input text.
    #[must_use]
    pub fn input(&self) -> &str {
        &self.input
    }
}

/// Builds a [`Handler`] from input schema text.
///
/// # Errors
///
/// Returns [`ErrorList`] on parse or validation failures.
pub fn new_handler(input: &str, apollo_service_query: bool) -> Result<Handler, ErrorList> {
    new_handler_with_options(
        input,
        apollo_service_query,
        &ValidationOptions::default(),
        &HashMap::new(),
    )
}

/// Builds a [`Handler`] with validation options and secrets.
///
/// # Errors
///
/// Returns [`ErrorList`] on parse or validation failures.
pub fn new_handler_with_options<H: std::hash::BuildHasher>(
    input: &str,
    apollo_service_query: bool,
    options: &ValidationOptions,
    secrets: &HashMap<String, String, H>,
) -> Result<Handler, ErrorList> {
    if input.is_empty() {
        let mut errs = ErrorList::default();
        errs.push(GraphQlError::new("No schema specified"));
        return Err(errs);
    }
    let mut meta = parse_meta_info(input).map_err(|err| {
        let mut errs = ErrorList::default();
        errs.push(err);
        errs
    })?;

    let doc = parse_schema(input).map_err(|err| {
        let mut errs = ErrorList::default();
        errs.push(GraphQlError::new(err.to_string()));
        errs
    })?;
    let mut schema = schema_from_document(&doc).map_err(|err| {
        let mut errs = ErrorList::default();
        errs.push(err);
        errs
    })?;

    let mut errs = ErrorList::default();
    for name in schema.order.clone() {
        errs.extend(apply_defn_validations(&schema.types[&name]));
    }
    errs.extend(apply_schema_doc_validations(&schema));
    if !errs.is_empty() {
        return Err(errs);
    }

    let mut defns: Vec<String> = Vec::new();
    let mut provides: HashMap<String, HashSet<String>> = HashMap::new();
    for name in schema.order.clone() {
        let defn = schema.types[&name].clone();
        defns.push(defn.name.clone());
        if matches!(
            defn.kind,
            TypeKind::Object | TypeKind::Interface | TypeKind::Union
        ) && defn.directive(REMOTE_DIRECTIVE).is_none()
        {
            for field in &defn.fields {
                if let Some(provides_dir) = field.directive(APOLLO_PROVIDES_DIRECTIVE) {
                    if let Some(arg) = provides_dir.arg(APOLLO_KEY_ARG) {
                        let entry = provides.entry(field.ty.name().to_owned()).or_default();
                        for name in arg.raw().split_whitespace() {
                            entry.insert(name.to_owned());
                        }
                    }
                }
            }
        }
    }
    let types_to_complete = defns.clone();

    expand_schema(&mut schema).map_err(|err| {
        let mut errs = ErrorList::default();
        errs.push(err);
        errs
    })?;
    errs.extend(validate_structure(&schema));
    if !errs.is_empty() {
        return Err(errs);
    }
    errs.extend(post_gql_validation(&mut schema, &defns, options, secrets));
    if !errs.is_empty() {
        return Err(errs);
    }

    let auth_header = meta.auth_meta.as_ref().map_or("", |auth| auth.get_header());
    meta.extra_cors_headers = get_allowed_headers(&schema, &defns, auth_header);
    let datastreamdb_schema = gen_dg_schema(&schema, &types_to_complete, &provides);
    complete_schema(
        &mut schema,
        &types_to_complete,
        &provides,
        apollo_service_query,
    );
    clean_schema(&mut schema);

    let no_queries = schema
        .type_def("Query")
        .is_none_or(|query| query.fields.is_empty());
    let no_mutations = schema
        .type_def("Mutation")
        .is_none_or(|mutation| mutation.fields.is_empty());
    if no_queries && no_mutations {
        let mut errs = ErrorList::default();
        errs.push(GraphQlError::new(
            "No query or mutation found in the generated schema",
        ));
        return Err(errs);
    }

    Ok(Handler {
        input: input.to_owned(),
        original_defs: defns,
        complete_schema: schema,
        datastreamdb_schema,
        meta,
    })
}

/// Parses a complete schema document into a model (admin/describe path).
///
/// # Errors
///
/// Returns [`ErrorList`] on parse or structural failures.
pub fn from_string(schema: &str, _namespace: u64) -> Result<Schema, ErrorList> {
    let doc = parse_schema(schema).map_err(|err| {
        let mut errs = ErrorList::default();
        errs.push(GraphQlError::new(err.to_string()));
        errs
    })?;
    let model = schema_from_document(&doc).map_err(|err| {
        let mut errs = ErrorList::default();
        errs.push(err);
        errs
    })?;
    let errs = validate_structure(&model);
    if errs.is_empty() {
        Ok(model)
    } else {
        Err(errs)
    }
}

/// Copies interface fields into implementors and merges extras.
fn expand_schema(schema: &mut Schema) -> Result<(), GraphQlError> {
    expand_interfaces(schema)?;
    for scalar in ["Int", "Float", "String", "Boolean", "ID"] {
        if schema.type_def(scalar).is_none() {
            schema.insert_type(TypeDef {
                name: scalar.to_owned(),
                kind: TypeKind::Scalar,
                description: None,
                directives: Vec::new(),
                fields: Vec::new(),
                implements: Vec::new(),
                enum_values: Vec::new(),
                possible_types: Vec::new(),
                position: (0, 0),
                builtin: true,
            });
        }
    }
    let extras = format!("{SCHEMA_INPUTS}{DIRECTIVE_DEFS}{FILTER_INPUTS}");
    merge_extras(schema, &extras)?;
    expand_apollo_extras(schema)?;
    Ok(())
}

/// Copies inherited interface fields into object types.
fn expand_interfaces(schema: &mut Schema) -> Result<(), GraphQlError> {
    let interfaces: HashMap<String, TypeDef> = schema
        .types
        .iter()
        .filter(|(_, def)| def.kind == TypeKind::Interface)
        .map(|(name, def)| (name.clone(), def.clone()))
        .collect();
    let names: Vec<String> = schema.order.clone();
    for name in names {
        let implements = schema.types[&name].implements.clone();
        if schema.types[&name].kind != TypeKind::Object || implements.is_empty() {
            continue;
        }
        let mut seen: HashMap<String, String> = HashMap::new();
        let mut counts: HashMap<String, i64> = HashMap::new();
        for field in &schema.types[&name].fields {
            *counts.entry(field.name.clone()).or_default() += 1;
        }
        let initial: Vec<FieldDef> = schema.types[&name].fields.clone();
        for iface_name in &implements {
            let Some(iface) = interfaces.get(iface_name) else {
                continue;
            };
            let mut inherited = Vec::new();
            for field in &iface.fields {
                if counts.get(&field.name).copied().unwrap_or(0) == 1 {
                    let own = initial.iter().find(|own| own.name == field.name);
                    if let Some(own) = own {
                        if field.ty.to_string() != own.ty.to_string() {
                            let defn = &schema.types[&name];
                            return Err(GraphQlError::at(
                                format!(
                                    "For type {} to implement interface {} the field {} must have \
                                     type {}",
                                    defn.name, iface.name, field.name, field.ty
                                ),
                                defn.position.0,
                                defn.position.1,
                            ));
                        }
                    }
                    if !seen.contains_key(&field.name) {
                        if let Some(target) = schema
                            .types
                            .get_mut(&name)
                            .and_then(|def| def.field_mut(&field.name))
                        {
                            *target = field.clone();
                        }
                    } else if field.ty.name() != "ID" {
                        inherited.push(field.clone());
                    }
                } else if field.ty.name() == "ID" && seen.contains_key(&field.name) {
                    let current = schema.types[&name]
                        .field(&field.name)
                        .map(|found| found.ty.to_string())
                        .unwrap_or_default();
                    if field.ty.to_string() != current {
                        let defn = &schema.types[&name];
                        return Err(GraphQlError::at(
                            format!(
                                "field {} is of type {} in interface {} and is of type {} in \
                                 interface {}",
                                field.name, field.ty, iface.name, current, seen[&field.name]
                            ),
                            defn.position.0,
                            defn.position.1,
                        ));
                    }
                } else {
                    inherited.push(field.clone());
                }
                seen.insert(field.name.clone(), iface.name.clone());
            }
            if let Some(password) = iface.directive("secret").cloned() {
                if let Some(defn) = schema.types.get_mut(&name) {
                    defn.directives.push(password);
                }
            }
            if let Some(defn) = schema.types.get_mut(&name) {
                let mut fields = inherited;
                fields.append(&mut defn.fields);
                defn.fields = fields;
            }
        }
    }
    schema.reindex();
    Ok(())
}

/// Merges parsed extras into the schema, rejecting duplicates.
fn merge_extras(schema: &mut Schema, extras: &str) -> Result<(), GraphQlError> {
    let doc = parse_schema(extras)
        .map_err(|err| GraphQlError::new(format!("invalid schema extras: {err}")))?;
    let mut extra = schema_from_document(&doc)
        .map_err(|err| GraphQlError::new(format!("invalid schema extras: {err}")))?;
    for (name, def) in extra.types.drain() {
        if schema.types.contains_key(&name) {
            return Err(GraphQlError::new(format!(
                "type \"{name}\" is defined more than once"
            )));
        }
        schema.insert_type(def);
    }
    schema.directives.extend(extra.directives);
    Ok(())
}

/// Adds Apollo federation extras when `@key` types exist.
fn expand_apollo_extras(schema: &mut Schema) -> Result<(), GraphQlError> {
    let keyed: Vec<String> = schema
        .order
        .iter()
        .filter(|name| {
            schema.types[*name]
                .directive(APOLLO_KEY_DIRECTIVE)
                .is_some()
        })
        .cloned()
        .collect();
    if keyed.is_empty() {
        return Ok(());
    }
    schema.insert_type(TypeDef {
        name: "_Entity".to_owned(),
        kind: TypeKind::Union,
        description: None,
        directives: Vec::new(),
        fields: Vec::new(),
        implements: Vec::new(),
        enum_values: Vec::new(),
        possible_types: keyed,
        position: (0, 0),
        builtin: false,
    });
    let doc = parse_schema(APOLLO_SCHEMA_QUERIES)
        .map_err(|err| GraphQlError::new(format!("invalid apollo extras: {err}")))?;
    let queries = schema_from_document(&doc)
        .map_err(|err| GraphQlError::new(format!("invalid apollo extras: {err}")))?;
    if let Some(apollo_query) = queries.type_def("Query") {
        if let Some(query) = schema.type_def_mut("Query") {
            query.fields.extend(apollo_query.fields.iter().cloned());
        } else {
            schema.insert_type(apollo_query.clone());
        }
    }
    merge_extras(schema, APOLLO_SCHEMA_EXTRAS)?;
    Ok(())
}

/// Structural GraphQL validation (unknown types, duplicates, conformance).
fn validate_structure(schema: &Schema) -> ErrorList {
    let mut errs = ErrorList::default();
    for name in &schema.order {
        let defn = &schema.types[name];
        let mut seen_fields = HashSet::new();
        for field in &defn.fields {
            if !seen_fields.insert(field.name.clone()) {
                errs.push(GraphQlError::at(
                    format!(
                        "Field \"{}\" is defined more than once in type \"{}\".",
                        field.name, defn.name
                    ),
                    field.position.0,
                    field.position.1,
                ));
            }
            check_type_name(schema, defn, field.ty.name(), field.position, &mut errs);
            for arg in &field.args {
                check_type_name(schema, defn, arg.ty.name(), arg.position, &mut errs);
                if arg.ty.is_list() && is_output_only(schema, arg.ty.name()) {
                    errs.push(GraphQlError::at(
                        format!(
                            "Argument \"{}\" of field \"{}\" has non-input type \"{}\".",
                            arg.name,
                            field.name,
                            arg.ty.name()
                        ),
                        arg.position.0,
                        arg.position.1,
                    ));
                }
            }
            if defn.kind == TypeKind::InputObject && is_output_only(schema, field.ty.name()) {
                errs.push(GraphQlError::at(
                    format!(
                        "Field \"{}\" of input \"{}\" has non-input type \"{}\".",
                        field.name,
                        defn.name,
                        field.ty.name()
                    ),
                    field.position.0,
                    field.position.1,
                ));
            }
            for dir in &field.directives {
                check_directive(schema, &dir.name, dir.position, &mut errs);
            }
        }
        for iface in &defn.implements {
            match schema.type_def(iface) {
                Some(target) if target.kind == TypeKind::Interface => {}
                _ => errs.push(GraphQlError::at(
                    format!(
                        "Type \"{}\" cannot implement non-interface type \"{iface}\".",
                        defn.name
                    ),
                    defn.position.0,
                    defn.position.1,
                )),
            }
        }
        if defn.kind == TypeKind::Union {
            for member in &defn.possible_types {
                match schema.type_def(member) {
                    Some(target) if target.kind == TypeKind::Object => {}
                    _ => errs.push(GraphQlError::at(
                        format!(
                            "Union \"{}\" member \"{member}\" must be an object type.",
                            defn.name
                        ),
                        defn.position.0,
                        defn.position.1,
                    )),
                }
            }
        }
        for dir in &defn.directives {
            check_directive(schema, &dir.name, dir.position, &mut errs);
        }
    }
    errs
}

/// Reports unknown type names.
fn check_type_name(
    schema: &Schema,
    _defn: &TypeDef,
    name: &str,
    position: (usize, usize),
    errs: &mut ErrorList,
) {
    if schema.type_def(name).is_none() && !is_builtin_scalar(name) {
        errs.push(GraphQlError::at(
            format!("Unknown type \"{name}\"."),
            position.0,
            position.1,
        ));
    }
}

/// Reports unknown directives (built-ins always resolve).
fn check_directive(schema: &Schema, name: &str, position: (usize, usize), errs: &mut ErrorList) {
    if schema.directives.contains_key(name) {
        return;
    }
    if matches!(name, "skip" | "include" | "deprecated" | "specifiedBy") {
        return;
    }
    errs.push(GraphQlError::at(
        format!("Unknown directive \"{name}\"."),
        position.0,
        position.1,
    ));
}

/// Whether the name is a spec scalar.
fn is_builtin_scalar(name: &str) -> bool {
    matches!(name, "Int" | "Float" | "String" | "Boolean" | "ID")
}

/// Whether the type is output-only (object/union, not input/enum/scalar).
fn is_output_only(schema: &Schema, name: &str) -> bool {
    schema.type_def(name).is_some_and(|def| {
        matches!(def.kind, TypeKind::Object | TypeKind::Union)
            || (def.kind == TypeKind::Interface && !is_builtin_scalar(name))
    }) || schema.type_def(name).is_none() && !is_builtin_scalar(name)
}

/// Runs post-GraphQL type, field, and directive validations.
fn post_gql_validation<H: std::hash::BuildHasher>(
    schema: &mut Schema,
    definitions: &[String],
    options: &ValidationOptions,
    secrets: &HashMap<String, String, H>,
) -> ErrorList {
    let mut errs = ErrorList::default();
    for name in definitions {
        let Some(typ) = schema.type_def(name).cloned() else {
            continue;
        };
        errs.extend(apply_type_validations(schema, &typ, options));
        for field in typ.fields.clone() {
            errs.extend(apply_field_validations(&typ, &field));
            for dir in field.directives.clone() {
                errs.extend(apply_directive_validation(
                    schema,
                    &typ.name,
                    &field.name,
                    &dir,
                    options,
                    secrets,
                ));
            }
        }
    }
    errs.extend(
        crate::rules_schema::datastreamdb_directive_predicate_validation(schema, definitions),
    );
    errs
}

/// Dispatches one field directive to its validator.
fn apply_directive_validation<H: std::hash::BuildHasher>(
    schema: &mut Schema,
    typ_name: &str,
    field_name: &str,
    dir: &Directive,
    options: &ValidationOptions,
    secrets: &HashMap<String, String, H>,
) -> ErrorList {
    use crate::directives::{
        APOLLO_EXTERNAL_DIRECTIVE, APOLLO_PROVIDES_DIRECTIVE, APOLLO_REQUIRES_DIRECTIVE,
        CUSTOM_DIRECTIVE, DATASTREAMDB_DIRECTIVE, DEFAULT_DIRECTIVE, EMBEDDING_DIRECTIVE,
        ID_DIRECTIVE, INVERSE_DIRECTIVE, LAMBDA_DIRECTIVE, REMOTE_RESPONSE_DIRECTIVE,
        SEARCH_DIRECTIVE,
    };
    let typ = schema
        .type_def(typ_name)
        .cloned()
        .unwrap_or_else(|| unreachable!());
    let field = typ
        .field(field_name)
        .cloned()
        .unwrap_or_else(|| unreachable!());
    match dir.name.as_str() {
        INVERSE_DIRECTIVE => {
            crate::rules_directive::has_inverse_validation(schema, typ_name, field_name, dir)
        }
        SEARCH_DIRECTIVE => crate::rules_directive::search_validation(schema, &typ, &field, dir),
        EMBEDDING_DIRECTIVE => {
            crate::rules_directive::embedding_validation(schema, &typ, &field, dir)
        }
        DATASTREAMDB_DIRECTIVE => {
            crate::rules_directive::datastreamdb_directive_validation(schema, &typ, &field, dir)
        }
        ID_DIRECTIVE => crate::rules_directive::id_validation(schema, &typ, &field, dir),
        CUSTOM_DIRECTIVE => {
            crate::rules_custom::custom_directive_validation(schema, &typ, &field, dir, secrets)
        }
        LAMBDA_DIRECTIVE => {
            crate::rules_directive::lambda_directive_validation(schema, &typ, &field, dir, options)
        }
        DEFAULT_DIRECTIVE => {
            crate::rules_directive::default_directive_validation(schema, &typ, &field, dir)
        }
        REMOTE_RESPONSE_DIRECTIVE => {
            crate::rules_apollo::remote_response_validation(schema, &typ, &field, dir)
        }
        APOLLO_REQUIRES_DIRECTIVE => {
            crate::rules_apollo::apollo_requires_validation(schema, &typ, &field, dir)
        }
        APOLLO_PROVIDES_DIRECTIVE => {
            crate::rules_apollo::apollo_provides_validation(schema, &typ, &field, dir)
        }
        APOLLO_EXTERNAL_DIRECTIVE => {
            crate::rules_apollo::apollo_external_validation(schema, &typ, &field, dir)
        }
        // Type-level directives (@auth, @remote, @generate, ...) and unknown
        // names have no field-level checks.
        _ => ErrorList::default(),
    }
}

/// Collects CORS headers from `@custom` forward headers plus the auth header.
fn get_allowed_headers(schema: &Schema, definitions: &[String], auth_header: &str) -> Vec<String> {
    let mut headers: HashSet<String> = HashSet::new();
    for name in definitions {
        let Some(defn) = schema.type_def(name) else {
            continue;
        };
        for field in &defn.fields {
            let Some(custom) = field.directive(CUSTOM_DIRECTIVE) else {
                continue;
            };
            let Some(http) = custom.arg("http") else {
                continue;
            };
            let Some(forward) = http.field("forwardHeaders") else {
                continue;
            };
            if let crate::model::Value::List(items) = forward {
                for header in items {
                    let raw = header.raw();
                    let parts: Vec<&str> = raw.split(':').collect();
                    headers.insert(parts.last().copied().unwrap_or_default().to_owned());
                }
            }
        }
    }
    let mut out: Vec<String> = headers.into_iter().collect();
    out.sort();
    if !auth_header.is_empty() {
        out.push(auth_header.to_owned());
    }
    out
}
