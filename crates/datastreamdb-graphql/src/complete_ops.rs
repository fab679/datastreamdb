//! Schema completion driver: payloads, queries, mutations, cleanup.
//! Split from `complete.rs`. Port of the query/mutation half of
//! `completeSchema` plus `cleanSchema` from `dgraph-main/graphql/schema/gqlschema.go`.

#![warn(missing_docs)]

use std::collections::{HashMap, HashSet};

use crate::complete::{
    GenerateParams, add_aggregation_result_type, add_filter_argument,
    add_filter_argument_for_field, add_filter_type, add_input_type, add_order_argument,
    add_pagination_arguments, add_patch_type, add_reference_type, add_type_has_filter,
    add_type_orderable, add_union_filter_type, add_union_member_type_enum,
    add_union_reference_type, add_update_type, camel_case, get_id_field, get_xid_field,
    has_embedding, has_extends, has_filterable, has_id, has_xid, is_enum_list, is_multi_lang_field,
    is_type_list, new_arg, new_field, num_uids_field, parse_generate_params,
};
use crate::directives::{
    CUSTOM_DIRECTIVE, DEPRECATED_DIRECTIVE, EMBEDDING_ENUM_SUFFIX, ID_DIRECTIVE, MSG,
    SIMILAR_BY_ARG_NAME, SIMILAR_BY_EMBEDDING_QUERY_SUFFIX, SIMILAR_BY_ID_QUERY_SUFFIX,
    SIMILAR_DISTANCE_THRESHOLD_ARG_NAME, SIMILAR_EF_ARG_NAME, SIMILAR_QUERY_DISTANCE_FIELD_NAME,
    SIMILAR_QUERY_PREFIX, SIMILAR_TOP_K_ARG_NAME, SIMILAR_VECTOR_ARG_NAME, SUBSCRIPTION_DIRECTIVE,
};
use crate::model::{
    Directive, EnumValueDef, FieldDef, GqlType, InputValueDef, Schema, TypeDef, TypeKind, Value,
};
use crate::rules_util::{
    get_password_field, has_custom_or_lambda, has_embedding_directive, has_id_directive,
    is_query_or_mutation, is_scalar,
};

/// Adds the `AddTPayload` object for a type.
fn add_add_payload_type(schema: &mut Schema, defn_name: &str, provides: &HashSet<String>) {
    let defn = schema
        .type_def(defn_name)
        .cloned()
        .unwrap_or_else(|| unreachable!());
    let mut qry = new_field(
        camel_case(&defn.name),
        GqlType::list(GqlType::named(&defn.name)),
    );
    add_filter_argument(schema, &mut qry);
    add_order_argument(schema, &mut qry, provides);
    add_pagination_arguments(&mut qry);
    if schema.type_def(&format!("Add{}Input", defn.name)).is_some() {
        schema.insert_type(TypeDef {
            name: format!("Add{}Payload", defn.name),
            kind: TypeKind::Object,
            description: None,
            directives: Vec::new(),
            fields: vec![qry, num_uids_field()],
            implements: Vec::new(),
            enum_values: Vec::new(),
            possible_types: Vec::new(),
            position: (0, 0),
            builtin: false,
        });
    }
}

/// Adds the `UpdateTPayload` object for a type.
fn add_update_payload_type(schema: &mut Schema, defn_name: &str, provides: &HashSet<String>) {
    let defn = schema
        .type_def(defn_name)
        .cloned()
        .unwrap_or_else(|| unreachable!());
    if !has_filterable(&defn) || schema.type_def(&format!("{}Patch", defn.name)).is_none() {
        return;
    }
    let mut qry = new_field(
        camel_case(&defn.name),
        GqlType::list(GqlType::named(&defn.name)),
    );
    add_filter_argument(schema, &mut qry);
    add_order_argument(schema, &mut qry, provides);
    add_pagination_arguments(&mut qry);
    schema.insert_type(TypeDef {
        name: format!("Update{}Payload", defn.name),
        kind: TypeKind::Object,
        description: None,
        directives: Vec::new(),
        fields: vec![qry, num_uids_field()],
        implements: Vec::new(),
        enum_values: Vec::new(),
        possible_types: Vec::new(),
        position: (0, 0),
        builtin: false,
    });
}

/// Adds the `DeleteTPayload` object for a type.
fn add_delete_payload_type(schema: &mut Schema, defn_name: &str, provides: &HashSet<String>) {
    let defn = schema
        .type_def(defn_name)
        .cloned()
        .unwrap_or_else(|| unreachable!());
    if !has_filterable(&defn) {
        return;
    }
    let mut qry = new_field(
        camel_case(&defn.name),
        GqlType::list(GqlType::named(&defn.name)),
    );
    add_filter_argument(schema, &mut qry);
    add_order_argument(schema, &mut qry, provides);
    add_pagination_arguments(&mut qry);
    let msg = new_field(MSG.to_owned(), GqlType::named("String"));
    schema.insert_type(TypeDef {
        name: format!("Delete{}Payload", defn.name),
        kind: TypeKind::Object,
        description: None,
        directives: Vec::new(),
        fields: vec![qry, msg, num_uids_field()],
        implements: Vec::new(),
        enum_values: Vec::new(),
        possible_types: Vec::new(),
        position: (0, 0),
        builtin: false,
    });
}

/// Adds the `getT` query for a type.
fn add_get_query(
    schema: &mut Schema,
    defn_name: &str,
    provides: &HashSet<String>,
    generate_subscription: bool,
) {
    let defn = schema
        .type_def(defn_name)
        .cloned()
        .unwrap_or_else(|| unreachable!());
    let (id_present, xid_present) = (has_id(&defn), has_xid(&defn));
    if !id_present && !xid_present {
        return;
    }
    let mut qry = new_field(format!("get{}", defn.name), GqlType::named(&defn.name));
    if id_present {
        let fields = get_id_field(&defn, provides);
        qry.args.push(InputValueDef {
            name: fields[0].name.clone(),
            description: None,
            ty: GqlType::named("ID"),
            default: None,
            directives: Vec::new(),
            position: (0, 0),
        });
        qry.args
            .last_mut()
            .unwrap_or_else(|| unreachable!())
            .ty
            .nullable = xid_present;
    }
    if xid_present {
        let xid_count = defn
            .fields
            .iter()
            .filter(|field| has_id_directive(field))
            .count();
        let mut unscoped = false;
        for field in &defn.fields {
            if !has_id_directive(field) {
                continue;
            }
            if !field
                .directive(ID_DIRECTIVE)
                .and_then(|dir| dir.arg(crate::directives::ID_DIRECTIVE_INTERFACE_ARG))
                .is_some_and(|val| matches!(val, Value::Bool(true)))
            {
                unscoped = true;
            }
            qry.args.push(InputValueDef {
                name: field.name.clone(),
                description: None,
                ty: GqlType::named(field.ty.name()),
                default: None,
                directives: Vec::new(),
                position: (0, 0),
            });
            qry.args
                .last_mut()
                .unwrap_or_else(|| unreachable!())
                .ty
                .nullable = id_present || xid_count > 1;
        }
        if defn.kind == TypeKind::Interface && unscoped {
            qry.directives.push(Directive {
                name: DEPRECATED_DIRECTIVE.to_owned(),
                args: vec![(
                    "reason".to_owned(),
                    Value::String(
                        "@id argument for get query on interface is being deprecated. Only those \
                         @id fields which have interface argument set to true will be available \
                         in getQuery argument on interface post v21.11.0, please update your \
                         schema accordingly."
                            .to_owned(),
                    ),
                )],
                position: (0, 0),
            });
        }
    }
    push_query(schema, qry.clone());
    if defn.directive(SUBSCRIPTION_DIRECTIVE).is_some() || generate_subscription {
        push_subscription(schema, qry);
    }
}

/// Pushes a field onto the `Query` root.
fn push_query(schema: &mut Schema, field: FieldDef) {
    if let Some(query) = schema.type_def_mut("Query") {
        query.fields.push(field);
    }
}

/// Pushes a field onto the `Subscription` root.
fn push_subscription(schema: &mut Schema, field: FieldDef) {
    if let Some(sub) = schema.type_def_mut("Subscription") {
        sub.fields.push(field);
    }
}

/// Pushes a field onto the `Mutation` root.
fn push_mutation(schema: &mut Schema, field: FieldDef) {
    if let Some(mutation) = schema.type_def_mut("Mutation") {
        mutation.fields.push(field);
    }
}

/// Adds the embedding enum for similarity queries, returning its name.
fn add_embedding_enum(schema: &mut Schema, defn_name: &str) -> String {
    let defn = schema
        .type_def(defn_name)
        .cloned()
        .unwrap_or_else(|| unreachable!());
    let enum_name = format!("{defn_name}{EMBEDDING_ENUM_SUFFIX}");
    let mut en = TypeDef {
        name: enum_name.clone(),
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
        if has_embedding_directive(field) {
            en.enum_values.push(EnumValueDef {
                name: field.name.clone(),
                description: None,
                directives: Vec::new(),
            });
        }
    }
    schema.insert_type(en);
    enum_name
}

/// Ensures the `vector_distance: Float` field exists on a type.
fn ensure_distance_field(schema: &mut Schema, defn_name: &str) {
    let Some(defn) = schema.type_def_mut(defn_name) else {
        return;
    };
    if defn.field(SIMILAR_QUERY_DISTANCE_FIELD_NAME).is_none() {
        defn.fields.push(new_field(
            SIMILAR_QUERY_DISTANCE_FIELD_NAME.to_owned(),
            GqlType::named("Float"),
        ));
    }
}

/// Adds the `querySimilarTByEmbedding` query for a type.
fn add_similar_by_embedding_query(schema: &mut Schema, defn_name: &str) {
    let mut qry = new_field(
        format!("{SIMILAR_QUERY_PREFIX}{defn_name}{SIMILAR_BY_EMBEDDING_QUERY_SUFFIX}"),
        GqlType::list(GqlType::named(defn_name)),
    );
    ensure_distance_field(schema, defn_name);
    let enum_name = add_embedding_enum(schema, defn_name);
    qry.args.push(new_arg(
        SIMILAR_BY_ARG_NAME,
        GqlType::named(&enum_name).required(),
    ));
    qry.args.push(new_arg(
        SIMILAR_TOP_K_ARG_NAME,
        GqlType::named("Int").required(),
    ));
    qry.args.push(new_arg(
        SIMILAR_VECTOR_ARG_NAME,
        GqlType::list(GqlType::named("Float").required()).required(),
    ));
    qry.args
        .push(new_arg(SIMILAR_EF_ARG_NAME, GqlType::named("Int")));
    qry.args.push(new_arg(
        SIMILAR_DISTANCE_THRESHOLD_ARG_NAME,
        GqlType::named("Float"),
    ));
    add_filter_argument(schema, &mut qry);
    push_query(schema, qry);
}

/// Adds the `querySimilarTById` query for a type.
fn add_similar_by_id_query(schema: &mut Schema, defn_name: &str, provides: &HashSet<String>) {
    let defn = schema
        .type_def(defn_name)
        .cloned()
        .unwrap_or_else(|| unreachable!());
    let (id_present, xid_present) = (has_id(&defn), has_xid(&defn));
    if !id_present && !xid_present {
        return;
    }
    let mut qry = new_field(
        format!("{SIMILAR_QUERY_PREFIX}{defn_name}{SIMILAR_BY_ID_QUERY_SUFFIX}"),
        GqlType::list(GqlType::named(defn_name)),
    );
    ensure_distance_field(schema, defn_name);
    if id_present {
        let fields = get_id_field(&defn, provides);
        let mut ty = GqlType::named("ID");
        ty.nullable = xid_present;
        qry.args.push(InputValueDef {
            name: fields[0].name.clone(),
            description: None,
            ty,
            default: None,
            directives: Vec::new(),
            position: (0, 0),
        });
    }
    if xid_present {
        let xid_count = defn
            .fields
            .iter()
            .filter(|field| has_id_directive(field))
            .count();
        for field in &defn.fields {
            if !has_id_directive(field) {
                continue;
            }
            let mut ty = GqlType::named(field.ty.name());
            ty.nullable = id_present || xid_count > 1;
            qry.args.push(InputValueDef {
                name: field.name.clone(),
                description: None,
                ty,
                default: None,
                directives: Vec::new(),
                position: (0, 0),
            });
        }
    }
    let enum_name = add_embedding_enum(schema, defn_name);
    qry.args.push(new_arg(
        SIMILAR_BY_ARG_NAME,
        GqlType::named(&enum_name).required(),
    ));
    qry.args.push(new_arg(
        SIMILAR_TOP_K_ARG_NAME,
        GqlType::named("Int").required(),
    ));
    qry.args
        .push(new_arg(SIMILAR_EF_ARG_NAME, GqlType::named("Int")));
    qry.args.push(new_arg(
        SIMILAR_DISTANCE_THRESHOLD_ARG_NAME,
        GqlType::named("Float"),
    ));
    add_filter_argument(schema, &mut qry);
    push_query(schema, qry);
}

/// Adds the `queryT` filter query for a type.
fn add_filter_query(
    schema: &mut Schema,
    defn_name: &str,
    provides: &HashSet<String>,
    generate_subscription: bool,
) {
    let defn = schema
        .type_def(defn_name)
        .cloned()
        .unwrap_or_else(|| unreachable!());
    let mut qry = new_field(
        format!("query{defn_name}"),
        GqlType::list(GqlType::named(defn_name)),
    );
    add_filter_argument(schema, &mut qry);
    add_order_argument(schema, &mut qry, provides);
    add_pagination_arguments(&mut qry);
    push_query(schema, qry.clone());
    if defn.directive(SUBSCRIPTION_DIRECTIVE).is_some() || generate_subscription {
        push_subscription(schema, qry);
    }
}

/// Adds the `aggregateT` query for a type.
fn add_aggregation_query(schema: &mut Schema, defn_name: &str, generate_subscription: bool) {
    let defn = schema
        .type_def(defn_name)
        .cloned()
        .unwrap_or_else(|| unreachable!());
    let mut qry = new_field(
        format!("aggregate{defn_name}"),
        GqlType::named(format!("{defn_name}AggregateResult")),
    );
    add_filter_argument_for_field(schema, &mut qry, defn_name);
    push_query(schema, qry.clone());
    if defn.directive(SUBSCRIPTION_DIRECTIVE).is_some() || generate_subscription {
        push_subscription(schema, qry);
    }
}

/// Adds the `checkTPassword` query for a type.
fn add_password_query(schema: &mut Schema, defn_name: &str, provides: &HashSet<String>) {
    let defn = schema
        .type_def(defn_name)
        .cloned()
        .unwrap_or_else(|| unreachable!());
    if !has_id(&defn) && !has_xid(&defn) {
        return;
    }
    let id_fields = if has_id(&defn) {
        get_id_field(&defn, provides)
    } else {
        get_xid_field(&defn, provides)
    };
    let Some(pwd) = get_password_field(&defn) else {
        return;
    };
    let qry = FieldDef {
        name: format!("check{defn_name}Password"),
        description: None,
        args: vec![
            InputValueDef {
                name: id_fields[0].name.clone(),
                description: None,
                ty: id_fields[0].ty.clone(),
                default: None,
                directives: Vec::new(),
                position: (0, 0),
            },
            new_arg(&pwd.name, GqlType::named("String").required()),
        ],
        ty: GqlType::named(defn_name),
        directives: Vec::new(),
        position: (0, 0),
    };
    push_query(schema, qry);
}

/// Adds generated queries for a type.
fn add_queries(
    schema: &mut Schema,
    defn_name: &str,
    provides: &HashSet<String>,
    params: &GenerateParams,
) {
    if params.get_query {
        add_get_query(schema, defn_name, provides, params.subscription);
        let defn = schema
            .type_def(defn_name)
            .cloned()
            .unwrap_or_else(|| unreachable!());
        if has_embedding(&defn) {
            add_similar_by_id_query(schema, defn_name, provides);
            add_similar_by_embedding_query(schema, defn_name);
        }
    }
    if params.password_query {
        add_password_query(schema, defn_name, provides);
    }
    if params.filter_query {
        add_filter_query(schema, defn_name, provides, params.subscription);
    }
    if params.aggregate_query {
        add_aggregation_query(schema, defn_name, params.subscription);
    }
}

/// Adds the `addT` mutation for a type.
fn add_add_mutation(schema: &mut Schema, defn_name: &str) {
    if schema.type_def(&format!("Add{defn_name}Input")).is_none() {
        return;
    }
    let mut add = new_field(
        format!("add{defn_name}"),
        GqlType::named(format!("Add{defn_name}Payload")),
    );
    add.args.push(new_arg(
        "input",
        GqlType::list(GqlType::named(format!("Add{defn_name}Input")).required()).required(),
    ));
    let defn = schema
        .type_def(defn_name)
        .cloned()
        .unwrap_or_else(|| unreachable!());
    if has_xid(&defn) {
        add.args.push(new_arg("upsert", GqlType::named("Boolean")));
    }
    push_mutation(schema, add);
}

/// Adds the `updateT` mutation for a type.
fn add_update_mutation(schema: &mut Schema, defn_name: &str) {
    let defn = schema
        .type_def(defn_name)
        .cloned()
        .unwrap_or_else(|| unreachable!());
    if !has_filterable(&defn) || schema.type_def(&format!("{defn_name}Patch")).is_none() {
        return;
    }
    let mut upd = new_field(
        format!("update{defn_name}"),
        GqlType::named(format!("Update{defn_name}Payload")),
    );
    upd.args.push(new_arg(
        "input",
        GqlType::named(format!("Update{defn_name}Input")).required(),
    ));
    push_mutation(schema, upd);
}

/// Adds the `deleteT` mutation for a type.
fn add_delete_mutation(schema: &mut Schema, defn_name: &str) {
    let defn = schema
        .type_def(defn_name)
        .cloned()
        .unwrap_or_else(|| unreachable!());
    if !has_filterable(&defn) {
        return;
    }
    let mut del = new_field(
        format!("delete{defn_name}"),
        GqlType::named(format!("Delete{defn_name}Payload")),
    );
    del.args.push(new_arg(
        "filter",
        GqlType::named(format!("{defn_name}Filter")).required(),
    ));
    push_mutation(schema, del);
}

/// Adds generated mutations for an object type.
fn add_mutations(schema: &mut Schema, defn_name: &str, params: &GenerateParams) {
    if params.add_mutation {
        add_add_mutation(schema, defn_name);
    }
    if params.update_mutation {
        add_update_mutation(schema, defn_name);
    }
    if params.delete_mutation {
        add_delete_mutation(schema, defn_name);
    }
}

/// Adds filter/order/pagination arguments to a type's fields.
fn add_field_filters(
    schema: &mut Schema,
    defn_name: &str,
    provides: &HashSet<String>,
    apollo_service_query: bool,
) {
    let defn = schema
        .type_def(defn_name)
        .cloned()
        .unwrap_or_else(|| unreachable!());
    let mut updated = defn.fields.clone();
    for field in &mut updated {
        if has_custom_or_lambda(field) || is_multi_lang_field(field, false) {
            continue;
        }
        if apollo_service_query && schema.type_def(field.ty.name()).is_some_and(has_extends) {
            continue;
        }
        let snapshot = schema.clone();
        add_filter_argument(&snapshot, field);
        if is_type_list(field) && !is_enum_list(&snapshot, field) {
            add_order_argument(&snapshot, field, provides);
            add_pagination_arguments(field);
        }
    }
    if let Some(target) = schema.type_def_mut(defn_name) {
        target.fields = updated;
    }
}

/// Adds `TAggregate` fields for object-list fields.
fn add_aggregate_fields(schema: &mut Schema, defn_name: &str, apollo_service_query: bool) {
    let defn = schema
        .type_def(defn_name)
        .cloned()
        .unwrap_or_else(|| unreachable!());
    let mut extra = Vec::new();
    for field in &defn.fields {
        if apollo_service_query && schema.type_def(field.ty.name()).is_some_and(has_extends) {
            continue;
        }
        let kind = schema.type_def(field.ty.name()).map(|def| def.kind);
        if is_type_list(field)
            && !has_custom_or_lambda(field)
            && matches!(kind, Some(TypeKind::Object | TypeKind::Interface))
        {
            let mut aggregate = new_field(
                format!("{}Aggregate", field.name),
                GqlType::named(format!("{}AggregateResult", field.ty.name())),
            );
            let snapshot = schema.clone();
            add_filter_argument_for_field(&snapshot, &mut aggregate, field.ty.name());
            extra.push(aggregate);
        }
    }
    if let Some(target) = schema.type_def_mut(defn_name) {
        target.fields.extend(extra);
    }
}

/// Generates all derived types and root fields for the schema.
pub(crate) fn complete_schema(
    schema: &mut Schema,
    definitions: &[String],
    provides: &HashMap<String, HashSet<String>>,
    apollo_service_query: bool,
) {
    for root in ["Query", "Mutation", "Subscription"] {
        if schema.type_def(root).is_none() {
            schema.insert_type(TypeDef {
                name: root.to_owned(),
                kind: TypeKind::Object,
                description: None,
                directives: Vec::new(),
                fields: Vec::new(),
                implements: Vec::new(),
                enum_values: Vec::new(),
                possible_types: Vec::new(),
                position: (0, 0),
                builtin: false,
            });
        } else if let Some(def) = schema.type_def_mut(root) {
            def.kind = TypeKind::Object;
        }
    }
    for key in definitions {
        if key == "Query" {
            let defn = schema
                .type_def(key)
                .cloned()
                .unwrap_or_else(|| unreachable!());
            for field in defn.fields.clone() {
                if field.directive(SUBSCRIPTION_DIRECTIVE).is_some()
                    && field.directive(CUSTOM_DIRECTIVE).is_some()
                {
                    push_subscription(schema, field);
                }
            }
            continue;
        }
        if is_query_or_mutation(key) {
            continue;
        }
        let defn = schema
            .type_def(key)
            .cloned()
            .unwrap_or_else(|| unreachable!());
        if defn.kind == TypeKind::Union {
            add_union_reference_type(schema, &defn);
            add_union_filter_type(schema, &defn);
            add_union_member_type_enum(schema, &defn);
            continue;
        }
        if defn.kind != TypeKind::Interface && defn.kind != TypeKind::Object {
            continue;
        }
        let params = parse_generate_params(&defn);
        let provides_map = provides.get(key).cloned().unwrap_or_default();
        add_reference_type(schema, &defn, &provides_map);
        if params.update_mutation {
            add_patch_type(schema, key, &provides_map);
            add_update_type(schema, &defn);
            add_update_payload_type(schema, key, &provides_map);
        }
        if params.delete_mutation {
            add_delete_payload_type(schema, key, &provides_map);
        }
        match defn.kind {
            TypeKind::Interface => {
                if params.update_mutation {
                    add_update_mutation(schema, key);
                }
                if params.delete_mutation {
                    add_delete_mutation(schema, key);
                }
            }
            TypeKind::Object => {
                if params.add_mutation {
                    add_input_type(schema, &defn, &provides_map);
                    add_add_payload_type(schema, key, &provides_map);
                }
                add_mutations(schema, key, &params);
            }
            _ => {}
        }
        add_filter_type(schema, key, &provides_map);
        add_type_orderable(schema, key, &provides_map);
        add_field_filters(schema, key, &provides_map, apollo_service_query);
        add_aggregation_result_type(schema, key, &provides_map);
        if !(apollo_service_query && has_extends(&defn)) {
            add_queries(schema, key, &provides_map, &params);
        }
        add_type_has_filter(schema, key, &provides_map);
        add_aggregate_fields(schema, key, apollo_service_query);
    }
    schema.reindex();
}

/// Recursively drops empty input types.
fn cleanup_input(schema: &mut Schema, name: &str, seen: &mut HashSet<String>) {
    let Some(def) = schema.type_def(name).cloned() else {
        return;
    };
    if seen.contains(name) {
        return;
    }
    let mut kept = Vec::new();
    for field in &def.fields {
        let named = field.ty.name().to_owned();
        let is_enum = schema
            .type_def(&named)
            .is_some_and(|target| target.kind == TypeKind::Enum);
        if is_scalar(&named) || is_enum {
            kept.push(field.clone());
            continue;
        }
        seen.insert(name.to_owned());
        cleanup_input(schema, &named, seen);
        if schema.type_def(&named).is_some() {
            kept.push(field.clone());
        }
    }
    if let Some(target) = schema.type_def_mut(name) {
        target.fields = kept;
    }
    let empty = schema
        .type_def(name)
        .is_some_and(|target| target.fields.is_empty());
    if empty {
        schema.remove_type(name);
        return;
    }
    if name.starts_with("Update") && name.ends_with("Input") {
        let single = schema
            .type_def(name)
            .is_some_and(|target| target.fields.len() == 1);
        if single && name.len() > 11 {
            let inner = &name[6..name.len() - 5];
            if let Some(target) = schema.type_def(inner) {
                if target
                    .directive(crate::directives::REMOTE_DIRECTIVE)
                    .is_none()
                    && matches!(target.kind, TypeKind::Object | TypeKind::Interface)
                {
                    schema.remove_type(name);
                }
            }
        }
    }
}

/// Drops empty generated inputs and orphaned add/update mutations.
pub(crate) fn clean_schema(schema: &mut Schema) {
    let names: Vec<String> = schema.types.keys().cloned().collect();
    for name in names {
        if name.ends_with("Ref")
            || name.ends_with("Patch")
            || ((name.starts_with("Add") || name.starts_with("Update")) && name.ends_with("Input"))
        {
            cleanup_input(schema, &name, &mut HashSet::new());
        }
    }
    let Some(mutation) = schema.type_def_mut("Mutation") else {
        return;
    };
    let mut kept = Vec::new();
    for field in mutation.fields.clone() {
        if field.directive(CUSTOM_DIRECTIVE).is_some()
            || !(field.name.starts_with("add") || field.name.starts_with("update"))
        {
            kept.push(field);
            continue;
        }
        let input = if field.name.starts_with("add") {
            format!("Add{}Input", &field.name[3..])
        } else {
            format!("Update{}Input", &field.name[6..])
        };
        if schema.type_def(&input).is_none() {
            schema.remove_type(&input);
            continue;
        }
        kept.push(field);
    }
    if let Some(mutation) = schema.type_def_mut("Mutation") {
        mutation.fields = kept;
    }
}
