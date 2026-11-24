//! Mutation object rewriting: fragments, existence queries, references.
//! Port of `rewriteObject`, `existenceQueries`, `asIDReference`, and their
//! helpers from `dgraph-main/graphql/resolve/mutation_rewriter.go`.

#![warn(missing_docs)]

use std::collections::HashMap;

use datastreamdb_dql::GraphQuery;
use serde_json::{Map as JsonMap, Value as Json};

use crate::errors::GraphQlError;
use crate::op::BoundSchema;
use crate::rewrite_vars::VariableGenerator;

/// Mutation flavor for object rewriting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MutationKind {
    /// Plain add.
    Add,
    /// Add with `upsert: true`.
    AddWithUpsert,
    /// Update `set` patch.
    UpdateWithSet,
    /// Update `remove` patch.
    UpdateWithRemove,
}

/// A partially built mutation: JSON body plus queries, conditions, checks.
#[derive(Debug, Clone, Default)]
pub(crate) struct MutationFragment {
    /// Existence/auth queries accompanying the mutation.
    pub queries: Vec<GraphQuery>,
    /// Upsert conditions (`@if`).
    pub conditions: Vec<String>,
    /// Mutation body (JSON object).
    pub fragment: Json,
    /// Delete bodies (JSON objects).
    pub deletes: Vec<Json>,
    /// Post-mutation result check.
    pub check: FragmentCheck,
    /// New blank-node variables to types.
    pub new_nodes: HashMap<String, String>,
}

/// Post-mutation result check (data form of Go's `resultChecker` closures).
///
/// Note: Go's `checkQueryResult` only fires on JWT extraction failure, which
/// this port handles upfront when threading auth variables; it has no
/// construction site here.
#[derive(Debug, Clone, Default)]
pub(crate) enum FragmentCheck {
    /// Always passes.
    #[default]
    None,
    /// Both checks must pass.
    Both(Box<FragmentCheck>, Box<FragmentCheck>),
    /// Auth row-count check plus an inner check.
    Auth {
        /// Query whose rows must match `<query>.auth`.
        query: String,
        /// Inner check.
        inner: Box<FragmentCheck>,
    },
}

impl FragmentCheck {
    /// Evaluates the check against an upsert result map.
    pub(crate) fn check(&self, result: &JsonMap<String, Json>) -> Result<(), GraphQlError> {
        match self {
            FragmentCheck::None => Ok(()),
            FragmentCheck::Auth { query, inner } => {
                let rows = result
                    .get(query)
                    .and_then(|val| val.as_array())
                    .map_or(0, Vec::len);
                if rows == 0 {
                    return inner.check(result);
                }
                let auth_rows = result
                    .get(&format!("{query}.auth"))
                    .and_then(|val| val.as_array())
                    .map_or(0, Vec::len);
                if auth_rows != rows {
                    return Err(GraphQlError::new("authorization failed"));
                }
                inner.check(result)
            }
            FragmentCheck::Both(left, right) => {
                let mut errs = crate::errors::ErrorList::default();
                if let Err(err) = left.check(result) {
                    errs.push(err);
                }
                if let Err(err) = right.check(result) {
                    errs.push(err);
                }
                if errs.is_empty() {
                    Ok(())
                } else {
                    Err(GraphQlError::new(errs.to_string()))
                }
            }
        }
    }

    /// Combines two checks.
    pub(crate) fn combine(self, other: FragmentCheck) -> FragmentCheck {
        match (self, other) {
            (FragmentCheck::None, check) | (check, FragmentCheck::None) => check,
            (left, right) => FragmentCheck::Both(Box::new(left), Box::new(right)),
        }
    }
}

/// Cross-object xid bookkeeping for one mutation.
#[derive(Debug, Default)]
pub(crate) struct XidMetadata {
    /// xid variable to the input object holding it.
    pub variable_obj_map: HashMap<String, Json>,
    /// Whether the xid variable was seen at top level.
    pub seen_at_top_level: HashMap<String, bool>,
    /// UIDs already covered by existence queries.
    pub seen_uids: HashMap<String, bool>,
}

/// Source field context for nested rewrites.
#[derive(Debug, Clone)]
pub(crate) struct SrcField {
    /// Field name.
    pub name: String,
    /// Field type name.
    pub type_name: String,
    /// Parent type name.
    pub parent_type: String,
    /// Whether the field is a list.
    pub is_list: bool,
}

/// Inverse field info, if the field has `@hasInverse`.
pub(crate) fn inverse_of(schema: &BoundSchema, field: &SrcField) -> Option<(String, String, bool)> {
    let def = schema
        .schema
        .type_def(&field.parent_type)?
        .field(&field.name)?;
    let dir = def.directive(crate::directives::INVERSE_DIRECTIVE)?;
    let target = dir.arg(crate::directives::INVERSE_ARG)?.raw();
    let target_def = schema.schema.type_def(&field.type_name)?.field(&target)?;
    Some((target, field.type_name.clone(), target_def.ty.is_list()))
}

/// Deletes a conflicting inverse object from mutation input.
pub(crate) fn delete_inverse_object(
    schema: &BoundSchema,
    obj: &mut JsonMap<String, Json>,
    src: Option<&SrcField>,
) {
    let Some(src) = src else {
        return;
    };
    if let Some((name, _, is_list)) = inverse_of(schema, src) {
        if !is_list {
            obj.remove(&name);
        }
    }
}

/// Attaches the inverse link from a child back to its parent UID.
pub(crate) fn add_inverse_link(
    schema: &BoundSchema,
    obj: &mut JsonMap<String, Json>,
    src: Option<&SrcField>,
    src_uid: &str,
) {
    let Some(src) = src else {
        return;
    };
    let Some((name, _, is_list)) = inverse_of(schema, src) else {
        return;
    };
    let pred = schema
        .predicate(&src.type_name, &name)
        .unwrap_or(&name)
        .to_owned();
    let link = Json::Object(JsonMap::from_iter([(
        "uid".to_owned(),
        Json::String(src_uid.to_owned()),
    )]));
    if is_list {
        obj.insert(pred, Json::Array(vec![link]));
    } else {
        obj.insert(pred, link);
    }
}

/// Extracts an xid value as a string, checking its GraphQL type.
pub(crate) fn extract_val(
    xid_val: &Json,
    xid_name: &str,
    type_name: &str,
) -> Result<String, GraphQlError> {
    match type_name {
        "Int" => match xid_val {
            Json::Number(num) if num.is_i64() => Ok(num.to_string()),
            _ => Err(GraphQlError::new(format!(
                "encountered an XID {xid_name} with {type_name} that isn't a Int but data type \
                 in schema is Int"
            ))),
        },
        "Int64" => match xid_val {
            Json::Number(num) if num.is_i64() => Ok(num.to_string()),
            Json::String(text) => Ok(text.clone()),
            _ => Err(GraphQlError::new(format!(
                "encountered an XID {xid_name} with {type_name} that isn't a Int64 but data type \
                 in schema is Int64"
            ))),
        },
        "String" | "ID" => match xid_val {
            Json::String(text) => Ok(text.clone()),
            _ => Err(GraphQlError::new(format!(
                "encountered an XID {xid_name} with {type_name} that isn't a String"
            ))),
        },
        _ => Err(GraphQlError::new(format!(
            "encountered an XID {xid_name} with {type_name} that isn't allowed as Xid"
        ))),
    }
}

/// Interface variable for inherited `@id(interface: true)` fields.
pub(crate) fn interface_variable(
    schema: &BoundSchema,
    type_name: &str,
    var_gen: &mut VariableGenerator,
    xid_name: &str,
    xid_string: &str,
) -> (Option<String>, String) {
    let origin = schema.field_originated_from(type_name, xid_name);
    let inherited = origin != type_name;
    let has_interface_arg = schema
        .schema
        .type_def(type_name)
        .and_then(|def| def.field(xid_name))
        .and_then(|field| field.directive(crate::directives::ID_DIRECTIVE))
        .and_then(|dir| dir.arg(crate::directives::ID_DIRECTIVE_INTERFACE_ARG))
        .is_some_and(|val| matches!(val, crate::model::Value::Bool(true)));
    if inherited && has_interface_arg {
        let var = var_gen.next(
            schema,
            type_name,
            (&format!("Int.{xid_name}"), xid_string),
            false,
        );
        (Some(origin), var)
    } else {
        (None, String::new())
    }
}

/// Whether existence results disagree about one node's UID.
fn got_multiple_existing_nodes<H: std::hash::BuildHasher>(
    schema: &BoundSchema,
    type_name: &str,
    obj: &JsonMap<String, Json>,
    var_gen: &mut VariableGenerator,
    id_existence: &HashMap<String, String, H>,
) -> bool {
    let mut seen = String::new();
    for xid in schema.xid_fields(type_name) {
        if let Some(val) = obj.get(&xid.name) {
            if val.is_null() {
                continue;
            }
            let Ok(xid_string) = extract_val(val, &xid.name, xid.ty.name()) else {
                continue;
            };
            let variable = var_gen.next(schema, type_name, (&xid.name, &xid_string), false);
            if let Some(uid) = id_existence.get(&variable) {
                if seen.is_empty() {
                    seen.clone_from(uid);
                } else if seen != *uid {
                    return true;
                }
            }
        }
    }
    false
}

/// Error for an xid owned by another implementing type.
fn xid_error_for_interface_type(
    schema: &BoundSchema,
    type_name: &str,
    xid_string: &str,
    xid_name: &str,
    interface_name: &str,
) -> GraphQlError {
    let _ = schema;
    let _ = type_name;
    GraphQlError::new(format!(
        "id {xid_string} already exists for field {xid_name} in some other implementing type of \
         interface {interface_name}"
    ))
}

/// Requires values for non-null fields (excluding IDs, defaults, customs).
fn ensure_non_nulls(
    schema: &BoundSchema,
    type_name: &str,
    obj: &JsonMap<String, Json>,
    exclusion: &str,
) -> Result<(), GraphQlError> {
    let Some(def) = schema.schema.type_def(type_name) else {
        return Ok(());
    };
    for field in &def.fields {
        if field.ty.nullable
            || field.ty.name() == "ID"
            || field
                .directive(crate::directives::DEFAULT_DIRECTIVE)
                .is_some()
            || field.name == exclusion
            || field
                .directive(crate::directives::CUSTOM_DIRECTIVE)
                .is_some()
        {
            continue;
        }
        if obj.get(&field.name).is_none_or(Json::is_null) {
            return Err(GraphQlError::new(format!(
                "type {type_name} requires a value for field {}, but no value present",
                field.name
            )));
        }
    }
    Ok(())
}

/// Rewrites one input object into a mutation fragment.
///
/// Returns the fragment, an upsert variable (add-with-upsert only), and any
/// errors. Port of Go `rewriteObject`.
#[allow(clippy::too_many_lines, reason = "exact port of Go rewriteObject")]
#[allow(clippy::too_many_arguments, reason = "exact port of Go rewriteObject")]
pub(crate) fn rewrite_object<H: std::hash::BuildHasher>(
    schema: &BoundSchema,
    type_name: &str,
    src_field: Option<&SrcField>,
    src_uid: &str,
    var_gen: &mut VariableGenerator,
    mut obj: JsonMap<String, Json>,
    xid_meta: &mut XidMetadata,
    id_existence: &mut HashMap<String, String, H>,
    mutation_kind: MutationKind,
    auth_vars: &crate::auth_rules::AuthVars,
) -> (Option<MutationFragment>, String, Vec<GraphQlError>) {
    // Owned: upsert re-points it at the existing node.
    let mut src_uid = src_uid.to_owned();
    let mut upsert_var = String::new();
    let mut errs = Vec::new();
    let mut variable = String::new();
    let at_top_level = src_field.is_none();

    if let Some(id) = schema.id_field(type_name) {
        if let Some(id_val) = obj.get(&id.name).cloned() {
            if !id_val.is_null() {
                let id_string = id_val.as_str().unwrap_or_default().to_owned();
                variable = var_gen.next(schema, type_name, (&id.name, &id_string), false);
                if id_existence.contains_key(&variable) {
                    if at_top_level {
                        errs.push(GraphQlError::new(format!(
                            "id {id_string} already exists for type {type_name}"
                        )));
                        return (None, upsert_var, errs);
                    }
                    let frag = crate::rewrite_refs::as_id_reference(
                        schema,
                        &id_string,
                        src_field,
                        &src_uid,
                        var_gen,
                        auth_vars,
                        mutation_kind == MutationKind::UpdateWithRemove,
                    );
                    return (Some(frag), upsert_var, Vec::new());
                }
                errs.push(GraphQlError::new(format!(
                    "ID \"{id_string}\" isn't a {}",
                    src_field.map_or(type_name, |src| src.type_name.as_str())
                )));
                return (None, upsert_var, errs);
            }
            obj.remove(&id.name);
        }
    }

    let xids = schema.xid_fields(type_name);
    if !xids.is_empty() {
        let multiple = got_multiple_existing_nodes(schema, type_name, &obj, var_gen, id_existence);
        let mut xid_variables = Vec::new();
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
            variable = var_gen.next(schema, type_name, (&xid.name, &xid_string), false);
            let existence_error = GraphQlError::new(
                "multiple nodes found for given xid values, updation not possible".to_owned(),
            );
            let (interface_typ, interface_var) =
                interface_variable(schema, type_name, var_gen, &xid.name, &xid_string);
            let interface_exists =
                !interface_var.is_empty() && id_existence.contains_key(&interface_var);
            let typ_uid = id_existence.get(&variable).cloned();
            if interface_exists || typ_uid.is_some() {
                if at_top_level {
                    if mutation_kind == MutationKind::AddWithUpsert {
                        if multiple {
                            errs.push(existence_error);
                            return (None, String::new(), errs);
                        }
                        if typ_uid.is_some() {
                            upsert_var.clone_from(&variable);
                            src_uid = format!("uid({variable})");
                        } else {
                            errs.push(xid_error_for_interface_type(
                                schema,
                                type_name,
                                &xid_string,
                                &xid.name,
                                &interface_typ.unwrap_or_default(),
                            ));
                            return (None, String::new(), errs);
                        }
                    } else if let Some(_typ_uid) = typ_uid {
                        errs.push(GraphQlError::new(format!(
                            "id {xid_string} already exists for field {} inside type {type_name}",
                            xid.name
                        )));
                        return (None, upsert_var, errs);
                    } else {
                        errs.push(xid_error_for_interface_type(
                            schema,
                            type_name,
                            &xid_string,
                            &xid.name,
                            &interface_typ.unwrap_or_default(),
                        ));
                        return (None, upsert_var, errs);
                    }
                } else {
                    if multiple {
                        errs.push(existence_error);
                        return (None, String::new(), errs);
                    }
                    if let Some(typ_uid) = typ_uid {
                        let frag = crate::rewrite_refs::as_id_reference(
                            schema,
                            &typ_uid,
                            src_field,
                            &src_uid,
                            var_gen,
                            auth_vars,
                            mutation_kind == MutationKind::UpdateWithRemove,
                        );
                        return (Some(frag), upsert_var, Vec::new());
                    }
                    errs.push(xid_error_for_interface_type(
                        schema,
                        type_name,
                        &xid_string,
                        &xid.name,
                        &interface_typ.unwrap_or_default(),
                    ));
                    return (None, upsert_var, errs);
                }
            } else {
                xid_variables.push(variable.clone());
            }
        }
        if !xid_variables.is_empty() {
            let exclude = src_field
                .and_then(|src| inverse_of(schema, src))
                .map(|(name, _, _)| name)
                .unwrap_or_default();
            if let Some(first) = xid_meta.variable_obj_map.get(&xid_variables[0]).cloned() {
                if let Some(map) = first.as_object() {
                    obj.clone_from(map);
                }
            }
            if let Err(err) = ensure_non_nulls(schema, type_name, &obj, &exclude) {
                if !(mutation_kind == MutationKind::UpdateWithSet && at_top_level) {
                    errs.push(err);
                    return (None, upsert_var, errs);
                }
            }
            for xid_variable in &xid_variables {
                id_existence.insert(xid_variable.clone(), format!("_:{variable}"));
            }
        }
        if upsert_var.is_empty() {
            for xid in &xids {
                if obj.get(&xid.name).is_some_and(|val| !val.is_null()) {
                    continue;
                }
                let required = matches!(xid.ty.to_string().as_str(), "String!" | "Int!" | "Int64!");
                let applies = matches!(
                    mutation_kind,
                    MutationKind::Add | MutationKind::AddWithUpsert
                ) || !at_top_level;
                if applies && required {
                    errs.push(GraphQlError::new(format!(
                        "field {} cannot be empty",
                        xid.name
                    )));
                    return (None, upsert_var, errs);
                }
            }
        }
    }

    if variable.is_empty() {
        variable = var_gen.next(schema, type_name, ("", ""), false);
    }
    let mut my_uid = format!("_:{variable}");
    let mut datastreamdb_types = vec![schema.datastreamdb_name(type_name)];
    if let Some(def) = schema.schema.type_def(type_name) {
        datastreamdb_types.extend(def.implements.iter().cloned());
    }
    let mut new_obj = JsonMap::new();
    if (mutation_kind != MutationKind::Add
        && mutation_kind != MutationKind::AddWithUpsert
        && at_top_level)
        || !upsert_var.is_empty()
    {
        new_obj.insert("uid".to_owned(), Json::String(src_uid.clone()));
        my_uid.clone_from(&src_uid);
    } else if mutation_kind == MutationKind::UpdateWithRemove {
        errs.push(GraphQlError::new("id is not provided".to_owned()));
        return (None, upsert_var, errs);
    } else {
        new_obj.insert(
            "datastreamdb.type".to_owned(),
            Json::Array(datastreamdb_types.into_iter().map(Json::String).collect()),
        );
        new_obj.insert("uid".to_owned(), Json::String(my_uid.clone()));
    }

    let action =
        if mutation_kind == MutationKind::Add || mutation_kind == MutationKind::AddWithUpsert {
            "add"
        } else {
            "update"
        };
    if let Some(def) = schema.schema.type_def(type_name) {
        for field in &def.fields.clone() {
            let Some(pred) = schema.predicate(type_name, &field.name) else {
                continue;
            };
            if new_obj.contains_key(pred) {
                continue;
            }
            if let Some(default) = default_value(field, action) {
                new_obj.insert(pred.to_owned(), Json::String(default));
            }
        }
    }

    delete_inverse_object(schema, &mut obj, src_field);
    add_inverse_link(schema, &mut new_obj, src_field, &my_uid);
    let mut frag = MutationFragment {
        fragment: Json::Object(new_obj),
        new_nodes: HashMap::from([(variable.clone(), type_name.to_owned())]),
        ..MutationFragment::default()
    };

    let mut fields: Vec<String> = obj.keys().cloned().collect();
    fields.sort();
    for field_name in fields {
        let val = obj[&field_name].clone();
        let field_def = schema
            .schema
            .type_def(type_name)
            .and_then(|def| def.field(&field_name))
            .cloned();
        let mut pred = schema
            .predicate(type_name, &field_name)
            .unwrap_or(&field_name)
            .to_owned();
        if pred.starts_with('<') && pred.ends_with('>') {
            pred = pred[1..pred.len() - 1].to_owned();
        }
        let Some(field_def) = field_def else {
            continue;
        };
        let mut val = val;
        if field_def
            .directive(crate::directives::EMBEDDING_DIRECTIVE)
            .is_some()
        {
            val = Json::String(val.to_string());
        }
        match val {
            Json::Object(map) => {
                if schema.is_union(field_def.ty.name()) {
                    let (child, _, mut child_errs) = rewrite_union_field(
                        schema,
                        &field_def,
                        type_name,
                        &my_uid,
                        var_gen,
                        map,
                        xid_meta,
                        id_existence,
                        mutation_kind,
                        auth_vars,
                    );
                    if let Some(child) = child {
                        set_fragment_field(&mut frag, &pred, child.fragment.clone());
                        merge_child(&mut frag, &child);
                    }
                    errs.append(&mut child_errs);
                } else if schema.is_geo(field_def.ty.name()) {
                    let geo = Json::Object(JsonMap::from_iter([
                        (
                            "type".to_owned(),
                            Json::String(field_def.ty.name().to_owned()),
                        ),
                        (
                            "coordinates".to_owned(),
                            rewrite_geo_object(&map, field_def.ty.name()),
                        ),
                    ]));
                    set_fragment_field(&mut frag, &pred, geo);
                } else {
                    let child_src = SrcField {
                        name: field_def.name.clone(),
                        type_name: field_def.ty.name().to_owned(),
                        parent_type: type_name.to_owned(),
                        is_list: field_def.ty.is_list(),
                    };
                    let (child, _, mut child_errs) = rewrite_object(
                        schema,
                        field_def.ty.name(),
                        Some(&child_src),
                        &my_uid,
                        var_gen,
                        map,
                        xid_meta,
                        id_existence,
                        mutation_kind,
                        auth_vars,
                    );
                    if let Some(child) = child {
                        set_fragment_field(&mut frag, &pred, child.fragment.clone());
                        merge_child(&mut frag, &child);
                    }
                    errs.append(&mut child_errs);
                }
            }
            Json::Array(items) => {
                let mut parts = Vec::new();
                for item in items {
                    match item {
                        Json::Object(map) => {
                            if schema.is_union(field_def.ty.name()) {
                                let (child, _, mut child_errs) = rewrite_union_field(
                                    schema,
                                    &field_def,
                                    type_name,
                                    &my_uid,
                                    var_gen,
                                    map,
                                    xid_meta,
                                    id_existence,
                                    mutation_kind,
                                    auth_vars,
                                );
                                if let Some(child) = child {
                                    parts.push(child.fragment.clone());
                                    merge_child(&mut frag, &child);
                                }
                                errs.append(&mut child_errs);
                            } else if schema.is_geo(field_def.ty.name()) {
                                parts.push(Json::Object(JsonMap::from_iter([
                                    (
                                        "type".to_owned(),
                                        Json::String(field_def.ty.name().to_owned()),
                                    ),
                                    (
                                        "coordinates".to_owned(),
                                        rewrite_geo_object(&map, field_def.ty.name()),
                                    ),
                                ])));
                            } else {
                                let child_src = SrcField {
                                    name: field_def.name.clone(),
                                    type_name: field_def.ty.name().to_owned(),
                                    parent_type: type_name.to_owned(),
                                    is_list: field_def.ty.is_list(),
                                };
                                let (child, _, mut child_errs) = rewrite_object(
                                    schema,
                                    field_def.ty.name(),
                                    Some(&child_src),
                                    &my_uid,
                                    var_gen,
                                    map,
                                    xid_meta,
                                    id_existence,
                                    mutation_kind,
                                    auth_vars,
                                );
                                if let Some(child) = child {
                                    parts.push(child.fragment.clone());
                                    merge_child(&mut frag, &child);
                                }
                                errs.append(&mut child_errs);
                            }
                        }
                        scalar => parts.push(scalar),
                    }
                }
                append_fragment_field(&mut frag, &pred, parts);
            }
            scalar => {
                set_fragment_field(&mut frag, &pred, scalar);
            }
        }
    }
    (Some(frag), upsert_var, errs)
}

/// Sets a field on a fragment's JSON body.
fn set_fragment_field(frag: &mut MutationFragment, pred: &str, val: Json) {
    if let Some(obj) = frag.fragment.as_object_mut() {
        obj.insert(pred.to_owned(), val);
    }
}

/// Appends list items to a fragment's JSON body field.
fn append_fragment_field(frag: &mut MutationFragment, pred: &str, parts: Vec<Json>) {
    if let Some(obj) = frag.fragment.as_object_mut() {
        match obj.get_mut(pred) {
            Some(Json::Array(existing)) => existing.extend(parts),
            _ => {
                obj.insert(pred.to_owned(), Json::Array(parts));
            }
        }
    }
}

/// Merges a child fragment's queries, deletes, and checks into its parent.
fn merge_child(parent: &mut MutationFragment, child: &MutationFragment) {
    parent.new_nodes.extend(
        child
            .new_nodes
            .iter()
            .map(|(key, val)| (key.clone(), val.clone())),
    );
    parent.queries.extend(child.queries.iter().cloned());
    parent.deletes.extend(child.deletes.iter().cloned());
    let check = std::mem::take(&mut parent.check);
    parent.check = check.combine(child.check.clone());
}

/// `@default` value for an action, if configured.
fn default_value(field: &crate::model::FieldDef, action: &str) -> Option<String> {
    let dir = field.directive(crate::directives::DEFAULT_DIRECTIVE)?;
    let arg = dir.arg(action)?;
    let value = arg.field("value")?;
    if value.raw() == "$now" {
        return Some("2000-01-01T00:00:00.00Z".to_owned());
    }
    Some(value.raw())
}

#[allow(clippy::too_many_arguments, reason = "mirrors rewriteObject")]
fn rewrite_union_field<H: std::hash::BuildHasher>(
    schema: &BoundSchema,
    field_def: &crate::model::FieldDef,
    parent_type: &str,
    my_uid: &str,
    var_gen: &mut VariableGenerator,
    obj: JsonMap<String, Json>,
    xid_meta: &mut XidMetadata,
    id_existence: &mut HashMap<String, String, H>,
    mutation_kind: MutationKind,
    auth_vars: &crate::auth_rules::AuthVars,
) -> (Option<MutationFragment>, String, Vec<GraphQlError>) {
    let mut obj = obj;
    let Some(member_ref) = obj.keys().next().cloned() else {
        return (None, String::new(), Vec::new());
    };
    let member_val = obj.remove(&member_ref).unwrap_or(Json::Null);
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
    let Json::Object(map) = member_val else {
        return (None, String::new(), Vec::new());
    };
    let child_src = SrcField {
        name: field_def.name.clone(),
        type_name: member_type.clone(),
        parent_type: parent_type.to_owned(),
        is_list: field_def.ty.is_list(),
    };
    rewrite_object(
        schema,
        &member_type,
        Some(&child_src),
        my_uid,
        var_gen,
        map,
        xid_meta,
        id_existence,
        mutation_kind,
        auth_vars,
    )
}

/// Rewrites a `GeoJSON` input object into mutation coordinates.
fn rewrite_geo_object(map: &JsonMap<String, Json>, type_name: &str) -> Json {
    match type_name {
        "Point" => {
            let lng = map.get("longitude").cloned().unwrap_or(Json::Null);
            let lat = map.get("latitude").cloned().unwrap_or(Json::Null);
            Json::Array(vec![lng, lat])
        }
        "Polygon" => rewrite_polygon(map),
        "MultiPolygon" => {
            let empty = Vec::new();
            let polygons = map
                .get("polygons")
                .and_then(|val| val.as_array())
                .unwrap_or(&empty);
            Json::Array(
                polygons
                    .iter()
                    .filter_map(|polygon| polygon.as_object())
                    .map(rewrite_polygon)
                    .collect(),
            )
        }
        _ => Json::Null,
    }
}

/// Rewrites a polygon's rings into coordinate arrays.
fn rewrite_polygon(polygon: &JsonMap<String, Json>) -> Json {
    let empty = Vec::new();
    let coordinates = polygon
        .get("coordinates")
        .and_then(|val| val.as_array())
        .unwrap_or(&empty);
    Json::Array(
        coordinates
            .iter()
            .filter_map(|ring| ring.as_object())
            .map(|ring| {
                let empty = Vec::new();
                let points = ring
                    .get("points")
                    .and_then(|val| val.as_array())
                    .unwrap_or(&empty);
                Json::Array(
                    points
                        .iter()
                        .filter_map(|point| point.as_object())
                        .map(|point| {
                            let lng = point.get("longitude").cloned().unwrap_or(Json::Null);
                            let lat = point.get("latitude").cloned().unwrap_or(Json::Null);
                            Json::Array(vec![lng, lat])
                        })
                        .collect(),
                )
            })
            .collect(),
    )
}
