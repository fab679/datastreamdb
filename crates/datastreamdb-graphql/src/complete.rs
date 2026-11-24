//! Schema completion: generated inputs, filters, queries, and mutations.
//! Port of `completeSchema`, `cleanSchema`, and the `add*` family from
//! `dgraph-main/graphql/schema/gqlschema.go`.

#![warn(missing_docs)]

use std::collections::HashSet;

use crate::directives::{
    APOLLO_EXTENDS_DIRECTIVE, APOLLO_EXTERNAL_DIRECTIVE, APOLLO_KEY_ARG, APOLLO_KEY_DIRECTIVE,
    DATASTREAMDB_DIRECTIVE, DATASTREAMDB_PRED_ARG, GENERATE_ADD_FIELD, GENERATE_AGGREGATE_FIELD,
    GENERATE_DELETE_FIELD, GENERATE_DIRECTIVE, GENERATE_GET_FIELD, GENERATE_MUTATION_ARG,
    GENERATE_PASSWORD_FIELD, GENERATE_QUERY_ARG, GENERATE_QUERY_FIELD, GENERATE_SUBSCRIPTION_ARG,
    GENERATE_UPDATE_FIELD, NUM_UID, built_in_filter, inbuilt_type_to_datastreamdb,
    is_orderable as is_orderable_type, is_summable as is_summable_type,
};
use crate::model::{
    EnumValueDef, FieldDef, GqlType, InputValueDef, Schema, TypeDef, TypeKind, Value,
};
use crate::rules_util::{
    field_name, get_password_field, get_search_args, has_custom_or_lambda, has_embedding_directive,
    has_id_directive, parse_search_type,
};

/// Lowercase-first camel case. Port of Go `CamelCase`.
pub(crate) fn camel_case(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Parsed `@generate` parameters (all true except subscriptions).
#[derive(Debug, Clone)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "mirrors Go GenerateDirectiveParams"
)]
pub(crate) struct GenerateParams {
    /// Generate `getT`.
    pub get_query: bool,
    /// Generate `queryT`.
    pub filter_query: bool,
    /// Generate `checkTPassword`.
    pub password_query: bool,
    /// Generate `aggregateT`.
    pub aggregate_query: bool,
    /// Generate `addT`.
    pub add_mutation: bool,
    /// Generate `updateT`.
    pub update_mutation: bool,
    /// Generate `deleteT`.
    pub delete_mutation: bool,
    /// Generate subscriptions.
    pub subscription: bool,
}

impl Default for GenerateParams {
    fn default() -> Self {
        Self {
            get_query: true,
            filter_query: true,
            password_query: true,
            aggregate_query: true,
            add_mutation: true,
            update_mutation: true,
            delete_mutation: true,
            subscription: false,
        }
    }
}

/// Parses `@generate` parameters from a definition.
pub(crate) fn parse_generate_params(defn: &TypeDef) -> GenerateParams {
    let mut params = GenerateParams::default();
    let Some(dir) = defn.directive(GENERATE_DIRECTIVE) else {
        return params;
    };
    if let Some(query) = dir.arg(GENERATE_QUERY_ARG) {
        if let Some(Value::Bool(val)) = query.field(GENERATE_GET_FIELD) {
            params.get_query = *val;
        }
        if let Some(Value::Bool(val)) = query.field(GENERATE_QUERY_FIELD) {
            params.filter_query = *val;
        }
        if let Some(Value::Bool(val)) = query.field(GENERATE_PASSWORD_FIELD) {
            params.password_query = *val;
        }
        if let Some(Value::Bool(val)) = query.field(GENERATE_AGGREGATE_FIELD) {
            params.aggregate_query = *val;
        }
    }
    if let Some(mutation) = dir.arg(GENERATE_MUTATION_ARG) {
        if let Some(Value::Bool(val)) = mutation.field(GENERATE_ADD_FIELD) {
            params.add_mutation = *val;
        }
        if let Some(Value::Bool(val)) = mutation.field(GENERATE_UPDATE_FIELD) {
            params.update_mutation = *val;
        }
        if let Some(Value::Bool(val)) = mutation.field(GENERATE_DELETE_FIELD) {
            params.delete_mutation = *val;
        }
    }
    if let Some(Value::Bool(val)) = dir.arg(GENERATE_SUBSCRIPTION_ARG) {
        params.subscription = *val;
    }
    params
}

/// Whether the definition carries `@extends`.
pub(crate) fn has_extends(def: &TypeDef) -> bool {
    def.directive(APOLLO_EXTENDS_DIRECTIVE).is_some()
}

/// Whether the field carries `@external`.
pub(crate) fn has_external(field: &FieldDef) -> bool {
    field.directive(APOLLO_EXTERNAL_DIRECTIVE).is_some()
}

/// Whether the field is a `@key` field (verbatim comparison like Go).
pub(crate) fn is_key_field(field: &FieldDef, typ: &TypeDef) -> bool {
    typ.directive(APOLLO_KEY_DIRECTIVE)
        .and_then(|key| key.arg(APOLLO_KEY_ARG))
        .is_some_and(|val| val.raw() == field.name)
}

/// Fields excluding `@external` non-key fields.
pub(crate) fn non_external_and_key_fields(defn: &TypeDef) -> Vec<&FieldDef> {
    defn.fields
        .iter()
        .filter(|field| !has_external(field) || is_key_field(field, defn))
        .collect()
}

/// Whether the field is external, non-key, and not `@provides`-listed.
pub(crate) fn external_and_non_key_field(
    field: &FieldDef,
    defn: &TypeDef,
    provides: &HashSet<String>,
) -> bool {
    has_external(field) && !is_key_field(field, defn) && !provides.contains(&field.name)
}

/// Whether the type has an ID field (excluding external non-key fields).
pub(crate) fn has_id(defn: &TypeDef) -> bool {
    non_external_and_key_fields(defn)
        .iter()
        .any(|field| field.ty.name() == "ID")
}

/// Whether the type has an `@id` field (excluding external non-key fields).
pub(crate) fn has_xid(defn: &TypeDef) -> bool {
    non_external_and_key_fields(defn)
        .iter()
        .any(|field| has_id_directive(field))
}

/// Whether the type has an `@embedding` field.
pub(crate) fn has_embedding(defn: &TypeDef) -> bool {
    non_external_and_key_fields(defn)
        .iter()
        .any(|field| has_embedding_directive(field))
}

/// Whether the field supports ordering.
pub(crate) fn is_orderable(field: &FieldDef, defn: &TypeDef, provides: &HashSet<String>) -> bool {
    if field.ty.is_list() || !is_orderable_type(field.ty.name()) {
        if has_external(field) {
            return is_key_field(field, defn) || provides.contains(&field.name);
        }
        return false;
    }
    if !has_external(field) {
        return !has_custom_or_lambda(field) && !is_multi_lang_field(field, false);
    }
    is_key_field(field, defn) || provides.contains(&field.name)
}

/// Whether any field of the type supports ordering.
pub(crate) fn has_orderables(defn: &TypeDef, provides: &HashSet<String>) -> bool {
    defn.fields
        .iter()
        .any(|field| is_orderable(field, defn, provides))
}

/// Whether the field supports `sum`/`avg`.
pub(crate) fn is_summable(field: &FieldDef, defn: &TypeDef, provides: &HashSet<String>) -> bool {
    if external_and_non_key_field(field, defn, provides) {
        return false;
    }
    !field.ty.is_list() && is_summable_type(field.ty.name()) && !has_custom_or_lambda(field)
}

/// Whether the field is a list of a non-scalar type.
pub(crate) fn is_type_list(field: &FieldDef) -> bool {
    inbuilt_type_to_datastreamdb(field.ty.name()).is_none() && field.ty.is_list()
}

/// Whether the field is a list of an enum.
pub(crate) fn is_enum_list(schema: &Schema, field: &FieldDef) -> bool {
    schema
        .type_def(field.ty.name())
        .is_some_and(|def| def.kind == TypeKind::Enum)
        && field.ty.is_list()
}

/// Whether a `TFilter` will be generated for the definition.
pub(crate) fn has_filterable(defn: &TypeDef) -> bool {
    defn.fields.iter().any(|field| {
        get_search_args(field).is_some_and(|args| !args.is_empty())
            || field.ty.name() == "ID"
            || !has_custom_or_lambda(field)
            || !is_multi_lang_field(field, false)
    })
}

/// Whether the field maps to multiple language tags.
pub(crate) fn is_multi_lang_field(field: &FieldDef, is_mutation_input: bool) -> bool {
    let Some(dir) = field.directive(DATASTREAMDB_DIRECTIVE) else {
        return false;
    };
    let Some(pred) = dir.arg(DATASTREAMDB_PRED_ARG) else {
        return false;
    };
    if !pred.raw().contains('@') {
        return false;
    }
    let raw = pred.raw();
    let langs = raw.split('@').nth(1).unwrap_or_default();
    if is_mutation_input {
        return langs.contains(':') || langs == ".";
    }
    langs.contains(':')
}

/// `@default` value for an action (`add`/`update`), if any.
pub(crate) fn get_default_value(field: &FieldDef, action: &str) -> Option<String> {
    let dir = field.directive(crate::directives::DEFAULT_DIRECTIVE)?;
    let arg = dir.arg(action)?;
    let value = arg.field("value")?;
    if value.raw() == "$now" {
        return Some("2000-01-01T00:00:00.00Z".to_owned());
    }
    Some(value.raw())
}

/// Builds a fresh field skeleton.
pub(crate) fn new_field(name: String, ty: GqlType) -> FieldDef {
    FieldDef {
        name,
        description: None,
        args: Vec::new(),
        ty,
        directives: Vec::new(),
        position: (0, 0),
    }
}

/// Builds a fresh input argument.
pub(crate) fn new_arg(name: &str, ty: GqlType) -> InputValueDef {
    InputValueDef {
        name: name.to_owned(),
        description: None,
        ty,
        default: None,
        directives: Vec::new(),
        position: (0, 0),
    }
}

/// The `numUids: Int` payload field.
pub(crate) fn num_uids_field() -> FieldDef {
    new_field(NUM_UID.to_owned(), GqlType::named("Int"))
}

/// First ID field copy (directives/args stripped).
pub(crate) fn get_id_field(defn: &TypeDef, provides: &HashSet<String>) -> Vec<FieldDef> {
    for field in &defn.fields {
        if field.ty.name() == "ID" {
            if external_and_non_key_field(field, defn, provides) {
                continue;
            }
            let mut copy = field.clone();
            copy.directives.clear();
            copy.args.clear();
            return vec![copy];
        }
    }
    Vec::new()
}

/// First `@id` field copy (directives/args stripped).
pub(crate) fn get_xid_field(defn: &TypeDef, provides: &HashSet<String>) -> Vec<FieldDef> {
    for field in &defn.fields {
        if has_id_directive(field) {
            if external_and_non_key_field(field, defn, provides) {
                continue;
            }
            let mut copy = field.clone();
            copy.directives.clear();
            copy.args.clear();
            return vec![copy];
        }
    }
    Vec::new()
}

/// Converts an output field into an input/patch field (`TRef` for objects).
pub(crate) fn create_field(schema: &Schema, field: &FieldDef) -> FieldDef {
    let kind = schema.type_def(field.ty.name()).map(|def| def.kind);
    if matches!(
        kind,
        Some(TypeKind::Object | TypeKind::Interface | TypeKind::Union)
    ) {
        let reffed = format!("{}Ref", field.ty.name());
        let ty = if field.ty.is_list() {
            let inner_nullable = match &field.ty.base {
                crate::model::BaseType::List(inner) => inner.nullable,
                crate::model::BaseType::Named(_) => true,
            };
            let mut inner = GqlType::named(reffed);
            inner.nullable = inner_nullable;
            let mut list = GqlType::list(inner);
            list.nullable = field.ty.nullable;
            list
        } else {
            let mut named = GqlType::named(reffed);
            named.nullable = field.ty.nullable;
            named
        };
        return new_field(field.name.clone(), ty);
    }
    let mut copy = field.clone();
    copy.directives.clear();
    copy.args.clear();
    copy
}

/// Fields eligible for `TPatch` (no IDs, customs, multi-lang, reverses).
pub(crate) fn get_patch_fields(
    schema: &Schema,
    defn: &TypeDef,
    provides: &HashSet<String>,
) -> Vec<FieldDef> {
    let mut out = Vec::new();
    for field in &defn.fields {
        if field.ty.name() == "ID" {
            continue;
        }
        if external_and_non_key_field(field, defn, provides) {
            continue;
        }
        if has_custom_or_lambda(field) {
            continue;
        }
        if is_multi_lang_field(field, true) {
            continue;
        }
        let fname = field_name(field, &defn.name);
        if fname.starts_with('~') || fname.starts_with("<~") {
            continue;
        }
        if let Some(target) = schema.type_def(field.ty.name()) {
            if target.kind == TypeKind::Interface && !has_id(target) && !has_xid(target) {
                continue;
            }
        }
        out.push(create_field(schema, field));
    }
    if let Some(pwd) = get_password_field(defn) {
        out.push(pwd);
    }
    out
}

/// Fields eligible for `AddTInput`/`TRef` (no IDs, customs, reverses).
pub(crate) fn get_fields_without_id_type(
    schema: &Schema,
    defn: &TypeDef,
    provides: &HashSet<String>,
    is_adding_input: bool,
) -> Vec<FieldDef> {
    let mut out = Vec::new();
    for field in &defn.fields {
        if field.ty.name() == "ID" {
            continue;
        }
        if external_and_non_key_field(field, defn, provides) {
            continue;
        }
        if has_custom_or_lambda(field) {
            continue;
        }
        if is_multi_lang_field(field, true) && is_adding_input {
            continue;
        }
        let fname = field_name(field, &defn.name);
        if fname.starts_with('~') || fname.starts_with("<~") {
            continue;
        }
        if let Some(target) = schema.type_def(field.ty.name()) {
            if target.kind == TypeKind::Interface && !has_id(target) && !has_xid(target) {
                continue;
            }
        }
        let mut created = create_field(schema, field);
        if get_default_value(field, "add").is_some() {
            created.ty.nullable = true;
        }
        out.push(created);
    }
    if let Some(pwd) = get_password_field(defn) {
        out.push(pwd);
    }
    out
}

/// Adds the `URef` input for a union.
pub(crate) fn add_union_reference_type(schema: &mut Schema, defn: &TypeDef) {
    let name = format!("{}Ref", defn.name);
    let mut input = TypeDef {
        name: name.clone(),
        kind: TypeKind::InputObject,
        description: None,
        directives: Vec::new(),
        fields: Vec::new(),
        implements: Vec::new(),
        enum_values: Vec::new(),
        possible_types: Vec::new(),
        position: (0, 0),
        builtin: false,
    };
    for member in &defn.possible_types {
        input.fields.push(new_field(
            format!("{}Ref", camel_case(member)),
            GqlType::named(format!("{member}Ref")),
        ));
    }
    schema.insert_type(input);
}

/// Adds the `UFilter` input for a union.
pub(crate) fn add_union_filter_type(schema: &mut Schema, defn: &TypeDef) {
    let name = format!("{}Filter", defn.name);
    let mut input = TypeDef {
        name: name.clone(),
        kind: TypeKind::InputObject,
        description: None,
        directives: Vec::new(),
        fields: vec![new_field(
            "memberTypes".to_owned(),
            GqlType::list(GqlType::named(format!("{}Type", defn.name)).required()),
        )],
        implements: Vec::new(),
        enum_values: Vec::new(),
        possible_types: Vec::new(),
        position: (0, 0),
        builtin: false,
    };
    for member in &defn.possible_types {
        input.fields.push(new_field(
            format!("{}Filter", camel_case(member)),
            GqlType::named(format!("{member}Filter")),
        ));
    }
    schema.insert_type(input);
}

/// Adds the `UType` member enum for a union.
pub(crate) fn add_union_member_type_enum(schema: &mut Schema, defn: &TypeDef) {
    let name = format!("{}Type", defn.name);
    let mut en = TypeDef {
        name: name.clone(),
        kind: TypeKind::Enum,
        description: None,
        directives: Vec::new(),
        fields: Vec::new(),
        implements: Vec::new(),
        enum_values: Vec::new(),
        possible_types: Vec::new(),
        position: (0, 0),
        builtin: false,
    };
    for member in &defn.possible_types {
        en.enum_values.push(EnumValueDef {
            name: member.clone(),
            description: None,
            directives: Vec::new(),
        });
    }
    schema.insert_type(en);
}

/// Adds the `AddTInput` input for an object type.
pub(crate) fn add_input_type(schema: &mut Schema, defn: &TypeDef, provides: &HashSet<String>) {
    let mut fields = get_fields_without_id_type(schema, defn, provides, true);
    if has_extends(defn) {
        let mut with_id = get_id_field(defn, provides);
        with_id.append(&mut fields);
        fields = with_id;
    }
    if !fields.is_empty() {
        schema.insert_type(TypeDef {
            name: format!("Add{}Input", defn.name),
            kind: TypeKind::InputObject,
            description: None,
            directives: Vec::new(),
            fields,
            implements: Vec::new(),
            enum_values: Vec::new(),
            possible_types: Vec::new(),
            position: (0, 0),
            builtin: false,
        });
    }
}

/// Adds the `TRef` input for an object or interface type.
pub(crate) fn add_reference_type(schema: &mut Schema, defn: &TypeDef, provides: &HashSet<String>) {
    let mut fields = if defn.kind == TypeKind::Interface {
        if !has_id(defn) && !has_xid(defn) {
            return;
        }
        let mut out = get_id_field(defn, provides);
        out.extend(get_xid_field(defn, provides));
        out
    } else {
        let mut out = get_id_field(defn, provides);
        out.extend(get_fields_without_id_type(schema, defn, provides, true));
        out
    };
    if fields.len() == 1 && (has_id(defn) || has_xid(defn)) {
        fields[0].ty.nullable = false;
    } else {
        for field in &mut fields {
            field.ty.nullable = true;
        }
    }
    if !fields.is_empty() {
        schema.insert_type(TypeDef {
            name: format!("{}Ref", defn.name),
            kind: TypeKind::InputObject,
            description: None,
            directives: Vec::new(),
            fields,
            implements: Vec::new(),
            enum_values: Vec::new(),
            possible_types: Vec::new(),
            position: (0, 0),
            builtin: false,
        });
    }
}

/// Adds the `UpdateTInput` input for a type.
pub(crate) fn add_update_type(schema: &mut Schema, defn: &TypeDef) {
    if !has_filterable(defn) || schema.type_def(&format!("{}Patch", defn.name)).is_none() {
        return;
    }
    schema.insert_type(TypeDef {
        name: format!("Update{}Input", defn.name),
        kind: TypeKind::InputObject,
        description: None,
        directives: Vec::new(),
        fields: vec![
            new_field(
                "filter".to_owned(),
                GqlType::named(format!("{}Filter", defn.name)).required(),
            ),
            new_field(
                "set".to_owned(),
                GqlType::named(format!("{}Patch", defn.name)),
            ),
            new_field(
                "remove".to_owned(),
                GqlType::named(format!("{}Patch", defn.name)),
            ),
        ],
        implements: Vec::new(),
        enum_values: Vec::new(),
        possible_types: Vec::new(),
        position: (0, 0),
        builtin: false,
    });
}

/// Adds the `TPatch` input for a type.
pub(crate) fn add_patch_type(schema: &mut Schema, defn_name: &str, provides: &HashSet<String>) {
    let defn = schema
        .type_def(defn_name)
        .cloned()
        .unwrap_or_else(|| unreachable!());
    if !has_filterable(&defn) {
        return;
    }
    let mut fields = get_patch_fields(schema, &defn, provides);
    if fields.is_empty() {
        return;
    }
    for field in &mut fields {
        field.ty.nullable = true;
    }
    schema.insert_type(TypeDef {
        name: format!("{}Patch", defn.name),
        kind: TypeKind::InputObject,
        description: None,
        directives: Vec::new(),
        fields,
        implements: Vec::new(),
        enum_values: Vec::new(),
        possible_types: Vec::new(),
        position: (0, 0),
        builtin: false,
    });
}

/// Adds a `filter: TFilter` argument to a field.
pub(crate) fn add_filter_argument(schema: &Schema, field: &mut FieldDef) {
    let name = field.ty.name().to_owned();
    add_filter_argument_for_field(schema, field, &name);
}

/// Adds a `filter: TFilter` argument for an explicit type name.
pub(crate) fn add_filter_argument_for_field(
    schema: &Schema,
    field: &mut FieldDef,
    type_name: &str,
) {
    if inbuilt_type_to_datastreamdb(type_name).is_some() {
        return;
    }
    let Some(target) = schema.type_def(type_name) else {
        return;
    };
    if target.kind == TypeKind::Union || has_filterable(target) {
        field.args.push(new_arg(
            "filter",
            GqlType::named(format!("{type_name}Filter")),
        ));
    }
}

/// Adds an `order: TOrder` argument when the target has orderables.
pub(crate) fn add_order_argument(
    schema: &Schema,
    field: &mut FieldDef,
    provides: &HashSet<String>,
) {
    let name = field.ty.name().to_owned();
    if schema
        .type_def(&name)
        .is_some_and(|def| has_orderables(def, provides))
    {
        field
            .args
            .push(new_arg("order", GqlType::named(format!("{name}Order"))));
    }
}

/// Adds `first`/`offset` pagination arguments.
pub(crate) fn add_pagination_arguments(field: &mut FieldDef) {
    field.args.push(new_arg("first", GqlType::named("Int")));
    field.args.push(new_arg("offset", GqlType::named("Int")));
}

/// Converts a field's search arguments to filter input names.
fn get_filter_types(schema: &mut Schema, field: &FieldDef) -> Vec<String> {
    let Some(args) = get_search_args(field) else {
        return Vec::new();
    };
    let mut names = Vec::with_capacity(args.len());
    for arg in &args {
        let search = parse_search_type(arg).to_owned();
        let mut filter = built_in_filter(&search).unwrap_or_default().to_owned();
        let is_enum = schema
            .type_def(field.ty.name())
            .is_some_and(|def| def.kind == TypeKind::Enum);
        if (search == "hash" || search == "exact") && is_enum {
            let string_filter = format!("String{}Filter", capitalize(&search));
            let mut fields = Vec::new();
            if let Some(def) = schema.type_def(&string_filter) {
                for item in def.fields.clone() {
                    let ty = if item.ty.is_list() {
                        GqlType::list(GqlType::named(field.ty.name()))
                    } else {
                        GqlType::named(field.ty.name())
                    };
                    let mut rebuilt = new_field(item.name.clone(), ty);
                    rebuilt.description.clone_from(&item.description);
                    fields.push(rebuilt);
                }
            }
            filter = format!("{}_{search}", field.ty.name());
            schema.insert_type(TypeDef {
                name: filter.clone(),
                kind: TypeKind::InputObject,
                description: None,
                directives: Vec::new(),
                fields,
                implements: Vec::new(),
                enum_values: Vec::new(),
                possible_types: Vec::new(),
                position: (0, 0),
                builtin: false,
            });
        }
        names.push(filter);
    }
    names
}

/// Capitalizes the first character.
fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Merges several filter inputs into one combined input.
fn merge_and_add_filters(filter_types: &[String], schema: &mut Schema, filter_name: &str) {
    if filter_types.len() <= 1 {
        return;
    }
    let mut fields = Vec::new();
    for name in filter_types {
        if let Some(def) = schema.type_def(name) {
            fields.extend(def.fields.iter().cloned());
        }
    }
    schema.insert_type(TypeDef {
        name: filter_name.to_owned(),
        kind: TypeKind::InputObject,
        description: None,
        directives: Vec::new(),
        fields,
        implements: Vec::new(),
        enum_values: Vec::new(),
        possible_types: Vec::new(),
        position: (0, 0),
        builtin: false,
    });
}

/// Adds the `TFilter` input for a type.
pub(crate) fn add_filter_type(schema: &mut Schema, defn_name: &str, provides: &HashSet<String>) {
    let defn = schema
        .type_def(defn_name)
        .cloned()
        .unwrap_or_else(|| unreachable!());
    let filter_name = format!("{}Filter", defn.name);
    let mut filter = TypeDef {
        name: filter_name.clone(),
        kind: TypeKind::InputObject,
        description: None,
        directives: Vec::new(),
        fields: Vec::new(),
        implements: Vec::new(),
        enum_values: Vec::new(),
        possible_types: Vec::new(),
        position: (0, 0),
        builtin: false,
    };
    for field in &defn.fields {
        if external_and_non_key_field(field, &defn, provides) {
            continue;
        }
        if field.ty.name() == "ID" {
            filter.fields.push(new_field(
                field.name.clone(),
                GqlType::list(GqlType::named("ID").required()),
            ));
            continue;
        }
        let filter_types = get_filter_types(schema, field);
        if !filter_types.is_empty() {
            let joined = filter_types.join("_");
            filter
                .fields
                .push(new_field(field.name.clone(), GqlType::named(&joined)));
            merge_and_add_filters(&filter_types, schema, &joined);
        }
    }
    if !get_fields_without_id_type(schema, &defn, provides, false).is_empty() {
        filter.fields.push(new_field(
            "has".to_owned(),
            GqlType::list(GqlType::named(format!("{}HasFilter", defn.name))),
        ));
    }
    let single_non_id = filter.fields.len() == 1
        && filter
            .fields
            .first()
            .is_some_and(|field| field.ty.name() != "ID");
    if single_non_id || filter.fields.len() > 1 {
        filter.fields.push(new_field(
            "and".to_owned(),
            GqlType::list(GqlType::named(&filter_name)),
        ));
        filter.fields.push(new_field(
            "or".to_owned(),
            GqlType::list(GqlType::named(&filter_name)),
        ));
    }
    filter
        .fields
        .push(new_field("not".to_owned(), GqlType::named(&filter_name)));
    schema.insert_type(filter);
}

/// Adds the `THasFilter` enum for a type.
pub(crate) fn add_type_has_filter(
    schema: &mut Schema,
    defn_name: &str,
    provides: &HashSet<String>,
) {
    let defn = schema
        .type_def(defn_name)
        .cloned()
        .unwrap_or_else(|| unreachable!());
    let mut filter = TypeDef {
        name: format!("{}HasFilter", defn.name),
        kind: TypeKind::Enum,
        description: None,
        directives: Vec::new(),
        fields: Vec::new(),
        implements: Vec::new(),
        enum_values: Vec::new(),
        possible_types: Vec::new(),
        position: (0, 0),
        builtin: false,
    };
    for field in &defn.fields {
        if field.ty.name() == "ID"
            || has_custom_or_lambda(field)
            || is_multi_lang_field(field, false)
        {
            continue;
        }
        if external_and_non_key_field(field, &defn, provides) {
            continue;
        }
        filter.enum_values.push(EnumValueDef {
            name: field.name.clone(),
            description: None,
            directives: Vec::new(),
        });
    }
    if !filter.enum_values.is_empty() {
        schema.insert_type(filter);
    }
}

/// Adds the `TOrder` input and `TOrderable` enum for a type.
pub(crate) fn add_type_orderable(schema: &mut Schema, defn_name: &str, provides: &HashSet<String>) {
    let defn = schema
        .type_def(defn_name)
        .cloned()
        .unwrap_or_else(|| unreachable!());
    if !has_orderables(&defn, provides) {
        return;
    }
    let order_name = format!("{}Order", defn.name);
    let orderable_name = format!("{}Orderable", defn.name);
    schema.insert_type(TypeDef {
        name: order_name.clone(),
        kind: TypeKind::InputObject,
        description: None,
        directives: Vec::new(),
        fields: vec![
            new_field("asc".to_owned(), GqlType::named(&orderable_name)),
            new_field("desc".to_owned(), GqlType::named(&orderable_name)),
            new_field("then".to_owned(), GqlType::named(&order_name)),
        ],
        implements: Vec::new(),
        enum_values: Vec::new(),
        possible_types: Vec::new(),
        position: (0, 0),
        builtin: false,
    });
    let mut orderable = TypeDef {
        name: orderable_name,
        kind: TypeKind::Enum,
        description: None,
        directives: Vec::new(),
        fields: Vec::new(),
        implements: Vec::new(),
        enum_values: Vec::new(),
        possible_types: Vec::new(),
        position: (0, 0),
        builtin: false,
    };
    for field in &defn.fields {
        if is_orderable(field, &defn, provides) {
            orderable.enum_values.push(EnumValueDef {
                name: field.name.clone(),
                description: None,
                directives: Vec::new(),
            });
        }
    }
    schema.insert_type(orderable);
}

/// Adds the `TAggregateResult` object for a type.
pub(crate) fn add_aggregation_result_type(
    schema: &mut Schema,
    defn_name: &str,
    provides: &HashSet<String>,
) {
    let defn = schema
        .type_def(defn_name)
        .cloned()
        .unwrap_or_else(|| unreachable!());
    let mut fields = vec![new_field("count".to_owned(), GqlType::named("Int"))];
    for field in &defn.fields {
        let aggregate_ty = GqlType::named(field.ty.name());
        if is_orderable(field, &defn, provides) || is_multi_lang_field(field, false) {
            fields.push(new_field(
                format!("{}Min", field.name),
                aggregate_ty.clone(),
            ));
            fields.push(new_field(
                format!("{}Max", field.name),
                aggregate_ty.clone(),
            ));
        }
        if is_summable(field, &defn, provides) {
            fields.push(new_field(format!("{}Sum", field.name), aggregate_ty));
            fields.push(new_field(
                format!("{}Avg", field.name),
                GqlType::named("Float"),
            ));
        }
    }
    schema.insert_type(TypeDef {
        name: format!("{}AggregateResult", defn.name),
        kind: TypeKind::Object,
        description: None,
        directives: Vec::new(),
        fields,
        implements: Vec::new(),
        enum_values: Vec::new(),
        possible_types: Vec::new(),
        position: (0, 0),
        builtin: false,
    });
}
