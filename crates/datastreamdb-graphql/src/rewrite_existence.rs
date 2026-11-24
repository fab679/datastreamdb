//! Mutation existence queries: UID/XID lookups before rewriting.
//! Split from `rewrite_object.rs`. Port of `existenceQueries` and its helpers
//! from `dgraph-main/graphql/resolve/mutation_rewriter.go`.

#![warn(missing_docs)]

use datastreamdb_dql::{Function, GraphQuery};
use serde_json::{Map as JsonMap, Value as Json};

use crate::errors::GraphQlError;
use crate::op::BoundSchema;
use crate::rewrite_object::{
    SrcField, XidMetadata, delete_inverse_object, extract_val, interface_variable, inverse_of,
};
use crate::rewrite_vars::{VariableGenerator, add_uid_func, plain_arg};

/// Builds an existence query checking one xid value.
fn check_xid_exists_query(
    schema: &BoundSchema,
    variable: &str,
    xid_string: &str,
    xid_name: &str,
    type_name: &str,
) -> GraphQuery {
    let pred = schema.predicate(type_name, xid_name).unwrap_or(xid_name);
    let mut qry = GraphQuery::new();
    variable.clone_into(&mut qry.attr);
    qry.func = Some(Function {
        name: "eq".to_owned(),
        args: vec![
            plain_arg(pred),
            plain_arg(&crate::rewrite_filter::maybe_quote_arg(
                "eq",
                &Json::String(xid_string.to_owned()),
            )),
        ],
        ..Function::default()
    });
    let mut uid_child = GraphQuery::new();
    "uid".clone_into(&mut uid_child.attr);
    let mut type_child = GraphQuery::new();
    "datastreamdb.type".clone_into(&mut type_child.attr);
    qry.children = vec![uid_child, type_child];
    qry
}

/// Builds an existence query checking one UID.
fn check_uid_exists_query(val: &Json, variable: &str) -> Result<GraphQuery, GraphQlError> {
    let text = val.as_str().unwrap_or_default();
    if text.is_empty() {
        return Err(GraphQlError::new("ID value was null"));
    }
    let uid = parse_uid_arg(text)?;
    let mut qry = GraphQuery::new();
    variable.clone_into(&mut qry.attr);
    add_uid_func(&mut qry, vec![uid]);
    let mut uid_child = GraphQuery::new();
    "uid".clone_into(&mut uid_child.attr);
    let mut type_child = GraphQuery::new();
    "datastreamdb.type".clone_into(&mut type_child.attr);
    qry.children = vec![uid_child, type_child];
    Ok(qry)
}

/// Parses a UID argument or reports a GraphQL error.
fn parse_uid_arg(text: &str) -> Result<u64, GraphQlError> {
    let parsed = if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()
    } else {
        text.parse::<u64>().ok()
    };
    parsed
        .ok_or_else(|| GraphQlError::new(format!("ID argument ({text}) was not able to be parsed")))
}

/// Whether an xid repeat is a conflicting duplicate.
fn is_duplicate_xid(
    schema: &BoundSchema,
    xid_meta: &XidMetadata,
    at_top_level: bool,
    xid_var: &str,
    new_obj: &JsonMap<String, Json>,
    src_field: Option<&SrcField>,
) -> bool {
    if at_top_level
        && xid_meta
            .seen_at_top_level
            .get(xid_var)
            .copied()
            .unwrap_or(false)
    {
        return true;
    }
    if let Some(src) = src_field {
        if let Some((_, _, is_list)) = inverse_of(schema, src) {
            if !is_list {
                return true;
            }
        }
    }
    if let Some(existing) = xid_meta.variable_obj_map.get(xid_var) {
        if let Some(existing) = existing.as_object() {
            if new_obj.len() > 1 && existing.len() > 1 && existing != new_obj {
                return true;
            }
        }
    }
    false
}

/// Rewrites existence queries for one input object (DFS).
#[allow(clippy::too_many_lines, reason = "exact port of Go existenceQueries")]
pub(crate) fn existence_queries(
    schema: &BoundSchema,
    type_name: &str,
    src_field: Option<&SrcField>,
    var_gen: &mut VariableGenerator,
    obj: &mut JsonMap<String, Json>,
    xid_meta: &mut XidMetadata,
) -> (Vec<GraphQuery>, Vec<String>, Vec<GraphQlError>) {
    let at_top_level = src_field.is_none();
    let mut queries = Vec::new();
    let mut types = Vec::new();
    let mut errs = Vec::new();
    delete_inverse_object(schema, obj, src_field);

    if let Some(id) = schema.id_field(type_name) {
        if let Some(id_val) = obj.get(&id.name).cloned() {
            if !id_val.is_null() {
                let id_string = id_val.as_str().unwrap_or_default().to_owned();
                if xid_meta.seen_uids.contains_key(&id_string) {
                    return (queries, types, errs);
                }
                xid_meta.seen_uids.insert(id_string.clone(), true);
                let variable = var_gen.next(schema, type_name, (&id.name, &id_string), false);
                match check_uid_exists_query(&id_val, &variable) {
                    Ok(query) => {
                        queries.push(query);
                        types.push(src_field.map_or_else(
                            || schema.datastreamdb_name(type_name),
                            |src| schema.datastreamdb_name(&src.type_name),
                        ));
                    }
                    Err(err) => errs.push(err),
                }
                return (queries, types, errs);
            }
            obj.remove(&id.name);
        }
    }

    let xids = schema.xid_fields(type_name);
    if !xids.is_empty() {
        for xid in &xids {
            let Some(val) = obj.get(&xid.name) else {
                continue;
            };
            if val.is_null() {
                continue;
            }
            let Ok(xid_string) = extract_val(val, &xid.name, xid.ty.name()) else {
                continue;
            };
            let variable = var_gen.next(schema, type_name, (&xid.name, &xid_string), false);
            if xid_meta.variable_obj_map.contains_key(&variable) {
                if is_duplicate_xid(schema, xid_meta, at_top_level, &variable, obj, src_field) {
                    errs.push(GraphQlError::new(format!(
                        "duplicate XID found: {xid_string}"
                    )));
                    return (Vec::new(), Vec::new(), errs);
                }
                let old_len = xid_meta
                    .variable_obj_map
                    .get(&variable)
                    .and_then(|val| val.as_object())
                    .map_or(0, JsonMap::len);
                if old_len == 1 && obj.len() > 1 {
                    xid_meta
                        .variable_obj_map
                        .insert(variable.clone(), Json::Object(obj.clone()));
                } else {
                    return (queries, types, errs);
                }
            } else {
                xid_meta
                    .variable_obj_map
                    .insert(variable.clone(), Json::Object(obj.clone()));
                xid_meta
                    .seen_at_top_level
                    .insert(variable.clone(), at_top_level);
                queries.push(check_xid_exists_query(
                    schema,
                    &variable,
                    &xid_string,
                    &xid.name,
                    type_name,
                ));
                types.push(schema.datastreamdb_name(type_name));
                let (interface_typ, interface_var) =
                    interface_variable(schema, type_name, var_gen, &xid.name, &xid_string);
                if let Some(interface_typ) = interface_typ {
                    queries.push(check_xid_exists_query(
                        schema,
                        &interface_var,
                        &xid_string,
                        &xid.name,
                        type_name,
                    ));
                    types.push(schema.datastreamdb_name(&interface_typ));
                }
            }
        }
    }

    let mut fields: Vec<String> = obj.keys().cloned().collect();
    fields.sort();
    for field_name in fields {
        let val = obj[&field_name].clone();
        let field_def = schema
            .schema
            .type_def(type_name)
            .and_then(|def| def.field(&field_name))
            .cloned();
        let Some(field_def) = field_def else {
            continue;
        };
        match val {
            Json::Object(map) => {
                let child_src = SrcField {
                    name: field_def.name.clone(),
                    type_name: field_def.ty.name().to_owned(),
                    parent_type: type_name.to_owned(),
                    is_list: field_def.ty.is_list(),
                };
                if schema.is_union(field_def.ty.name()) {
                    let (mut sub_queries, mut sub_types, mut sub_errs) = existence_queries_union(
                        schema, type_name, &child_src, var_gen, map, xid_meta, None,
                    );
                    errs.append(&mut sub_errs);
                    queries.append(&mut sub_queries);
                    types.append(&mut sub_types);
                } else {
                    let mut map = map;
                    let (mut sub_queries, mut sub_types, mut sub_errs) = existence_queries(
                        schema,
                        field_def.ty.name(),
                        Some(&child_src),
                        var_gen,
                        &mut map,
                        xid_meta,
                    );
                    errs.append(&mut sub_errs);
                    queries.append(&mut sub_queries);
                    types.append(&mut sub_types);
                }
            }
            Json::Array(items) => {
                for (index, item) in items.into_iter().enumerate() {
                    if let Json::Object(map) = item {
                        let child_src = SrcField {
                            name: field_def.name.clone(),
                            type_name: field_def.ty.name().to_owned(),
                            parent_type: type_name.to_owned(),
                            is_list: field_def.ty.is_list(),
                        };
                        if schema.is_union(field_def.ty.name()) {
                            let (mut sub_queries, mut sub_types, mut sub_errs) =
                                existence_queries_union(
                                    schema,
                                    type_name,
                                    &child_src,
                                    var_gen,
                                    map,
                                    xid_meta,
                                    Some(index),
                                );
                            errs.append(&mut sub_errs);
                            queries.append(&mut sub_queries);
                            types.append(&mut sub_types);
                        } else {
                            let mut map = map;
                            let (mut sub_queries, mut sub_types, mut sub_errs) = existence_queries(
                                schema,
                                field_def.ty.name(),
                                Some(&child_src),
                                var_gen,
                                &mut map,
                                xid_meta,
                            );
                            errs.append(&mut sub_errs);
                            queries.append(&mut sub_queries);
                            types.append(&mut sub_types);
                        }
                    }
                }
            }
            _ => {
                if field_def
                    .directive(crate::directives::ID_DIRECTIVE)
                    .is_some()
                    && val.as_str().is_some_and(str::is_empty)
                {
                    let pred = schema
                        .predicate(type_name, &field_name)
                        .unwrap_or(&field_name);
                    errs.push(GraphQlError::new(format!(
                        "encountered an empty value for @id field `{pred}`"
                    )));
                    return (Vec::new(), Vec::new(), errs);
                }
            }
        }
    }
    (queries, types, errs)
}

/// Rewrites existence queries for a union-typed input object.
fn existence_queries_union(
    schema: &BoundSchema,
    parent_type: &str,
    src_field: &SrcField,
    var_gen: &mut VariableGenerator,
    obj: JsonMap<String, Json>,
    xid_meta: &mut XidMetadata,
    list_index: Option<usize>,
) -> (Vec<GraphQuery>, Vec<String>, Vec<GraphQlError>) {
    if obj.len() != 1 {
        let msg = match list_index {
            Some(index) => format!(
                "value for field `{}` in type `{parent_type}` index `{index}` must have exactly \
                 one child, found {} children",
                src_field.name,
                obj.len()
            ),
            None => format!(
                "value for field `{}` in type `{parent_type}` must have exactly one child, found \
                 {} children",
                src_field.name,
                obj.len()
            ),
        };
        return (Vec::new(), Vec::new(), vec![GraphQlError::new(msg)]);
    }
    let (member_ref, member_val) = obj.into_iter().next().unwrap_or_default();
    let member_type = format!(
        "{}{}",
        member_ref
            .chars()
            .next()
            .map_or_else(String::new, |ch| ch.to_uppercase().collect::<String>()),
        member_ref
            .get(1..member_ref.len().saturating_sub(3))
            .unwrap_or_default()
    );
    let Json::Object(mut map) = member_val else {
        return (Vec::new(), Vec::new(), Vec::new());
    };
    let child_src = SrcField {
        name: src_field.name.clone(),
        type_name: member_type.clone(),
        parent_type: parent_type.to_owned(),
        is_list: src_field.is_list,
    };
    existence_queries(
        schema,
        &member_type,
        Some(&child_src),
        var_gen,
        &mut map,
        xid_meta,
    )
}
