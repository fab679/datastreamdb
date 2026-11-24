//! GraphQL mutation to DQL upsert rewriting. Port of `AddRewriter`,
//! `UpdateRewriter`, and `deleteRewriter` from
//! `dgraph-main/graphql/resolve/mutation_rewriter.go`.
//!
//! Fragments lower to RDF through `fragment_nquads` for the embedded engine
//! (which has no JSON mutation path); upsert conditions evaluate client-side
//! from existence-query results in the resolve stage.

#![warn(missing_docs)]

use std::fmt::Write as _;

use std::collections::HashMap;

use datastreamdb_dql::GraphQuery;
use serde_json::{Map as JsonMap, Value as Json};

use crate::errors::GraphQlError;
use crate::op::{BoundField, BoundSchema, MutationType};
use crate::rewrite_auth::{AuthRewriter, AuthSelector};
use crate::rewrite_existence::existence_queries;
use crate::rewrite_filter::add_filter;
use crate::rewrite_object::{MutationFragment, MutationKind, XidMetadata, rewrite_object};
use crate::rewrite_vars::{VariableGenerator, add_type_filter, add_type_func, add_uid_func};

/// Upsert query variable name.
pub(crate) const MUTATION_QUERY_VAR: &str = "x";
/// Upsert query variable as UID function.
pub(crate) const MUTATION_QUERY_VAR_UID: &str = "uid(x)";
/// Update/delete mutation condition.
pub(crate) const UPDATE_MUTATION_CONDITION: &str = "gt(len(x), 0)";

/// One executable upsert: queries plus RDF mutations.
#[derive(Debug, Clone, Default)]
pub struct UpsertMutation {
    /// Queries (existence, auth, upsert roots).
    pub queries: Vec<GraphQuery>,
    /// Set blocks (RDF text).
    pub set_nquads: Vec<String>,
    /// Delete blocks (RDF text).
    pub del_nquads: Vec<String>,
    /// Upsert conditions (`@if`).
    pub conditions: Vec<String>,
    /// New blank-node variables to types.
    pub new_nodes: HashMap<String, String>,
    /// Top-level fragment UIDs (for `numUids` and post-selection).
    pub root_uids: Vec<String>,
}

/// Rewriter state shared across existence and mutation phases.
pub struct MutationRewriter {
    /// Variable generator.
    pub(crate) var_gen: VariableGenerator,
    /// Xid bookkeeping.
    pub(crate) xid_meta: XidMetadata,
    /// Add fragments.
    pub(crate) add_frags: Vec<MutationFragment>,
    /// Update set fragment.
    pub(crate) set_frag: Option<MutationFragment>,
    /// Update remove fragment.
    pub(crate) del_frag: Option<MutationFragment>,
}

impl MutationRewriter {
    /// Fresh rewriter.
    #[must_use]
    pub fn new() -> Self {
        Self {
            var_gen: VariableGenerator::new(),
            xid_meta: XidMetadata::default(),
            add_frags: Vec::new(),
            set_frag: None,
            del_frag: None,
        }
    }
}

impl Default for MutationRewriter {
    fn default() -> Self {
        Self::new()
    }
}

/// Type mutated by a root mutation field.
fn mutated_type(schema: &BoundSchema, field: &BoundField) -> String {
    for prefix in ["add", "update", "delete"] {
        if let Some(rest) = field.name.strip_prefix(prefix) {
            if schema.schema.type_def(rest).is_some() {
                return rest.to_owned();
            }
        }
    }
    field.type_name().to_owned()
}

/// Extracts the update/delete filter argument.
fn extract_mutation_filter(field: &BoundField) -> JsonMap<String, Json> {
    match field.mutation_type {
        MutationType::Update => field
            .arg("input")
            .and_then(|val| val.as_object())
            .and_then(|input| input.get("filter"))
            .and_then(|val| val.as_object())
            .cloned()
            .unwrap_or_default(),
        MutationType::Delete => field
            .arg("filter")
            .and_then(|val| val.as_object())
            .cloned()
            .unwrap_or_default(),
        _ => JsonMap::new(),
    }
}

/// Rewrites existence queries for an add mutation's inputs.
pub fn add_rewrite_queries(
    schema: &BoundSchema,
    field: &BoundField,
    rewriter: &mut MutationRewriter,
) -> (Vec<GraphQuery>, Vec<String>, Vec<GraphQlError>) {
    let mut queries = Vec::new();
    let mut types = Vec::new();
    let mut errs = Vec::new();
    let mutated = mutated_type(schema, field);
    let inputs = field
        .arg("input")
        .and_then(|val| val.as_array())
        .cloned()
        .unwrap_or_default();
    for input in inputs {
        if let Some(mut obj) = input.as_object().cloned() {
            let (mut sub_queries, mut sub_types, mut sub_errs) = existence_queries(
                schema,
                &mutated,
                None,
                &mut rewriter.var_gen,
                &mut obj,
                &mut rewriter.xid_meta,
            );
            errs.append(&mut sub_errs);
            queries.append(&mut sub_queries);
            types.append(&mut sub_types);
        }
    }
    (queries, types, errs)
}

/// Rewrites existence queries for an update mutation's set/remove patches.
pub fn update_rewrite_queries(
    schema: &BoundSchema,
    field: &BoundField,
    rewriter: &mut MutationRewriter,
) -> (Vec<GraphQuery>, Vec<String>, Vec<GraphQlError>) {
    let mut queries = Vec::new();
    let mut types = Vec::new();
    let mut errs = Vec::new();
    let mutated = mutated_type(schema, field);
    let input = field
        .arg("input")
        .and_then(|val| val.as_object())
        .cloned()
        .unwrap_or_default();
    for patch in ["set", "remove"] {
        if let Some(Json::Object(mut obj)) = input.get(patch).cloned() {
            if obj.is_empty() {
                continue;
            }
            let (mut sub_queries, mut sub_types, mut sub_errs) = existence_queries(
                schema,
                &mutated,
                None,
                &mut rewriter.var_gen,
                &mut obj,
                &mut rewriter.xid_meta,
            );
            errs.append(&mut sub_errs);
            queries.append(&mut sub_queries);
            types.append(&mut sub_types);
        }
    }
    (queries, types, errs)
}

/// Rewrites an add mutation into upsert mutations.
#[allow(clippy::too_many_lines, reason = "exact port of Go AddRewriter")]
pub fn add_rewrite<H: std::hash::BuildHasher>(
    schema: &BoundSchema,
    field: &BoundField,
    rewriter: &mut MutationRewriter,
    id_existence: &mut HashMap<String, String, H>,
    auth_vars: &crate::auth_rules::AuthVars,
) -> (Vec<UpsertMutation>, Vec<GraphQlError>) {
    let mut kind = MutationKind::Add;
    if field.arg("upsert").and_then(Json::as_bool).unwrap_or(false) {
        kind = MutationKind::AddWithUpsert;
    }
    let mutated = mutated_type(schema, field);
    let mut queries = Vec::new();
    let mut new_nodes = HashMap::new();
    let mut mutations: Vec<UpsertMutation> = Vec::new();
    let mut errs = Vec::new();
    let inputs = field
        .arg("input")
        .and_then(|val| val.as_array())
        .cloned()
        .unwrap_or_default();
    for input in inputs {
        let Some(obj) = input.as_object().cloned() else {
            continue;
        };
        let (fragment, upsert_var, mut frag_errs) = rewrite_object(
            schema,
            &mutated,
            None,
            "",
            &mut rewriter.var_gen,
            obj,
            &mut rewriter.xid_meta,
            id_existence,
            kind,
            auth_vars,
        );
        errs.append(&mut frag_errs);
        let Some(mut fragment) = fragment else {
            continue;
        };
        if !upsert_var.is_empty() {
            let mut auth = AuthRewriter {
                auth_variables: auth_vars.clone(),
                is_writing_auth: false,
                filter_by_uid: false,
                selector: AuthSelector::Update,
                var_gen: &mut rewriter.var_gen,
                var_name: String::new(),
                parent_var_name: format!("{mutated}Root"),
                has_auth_rules: false,
                has_cascade: false,
                schema,
            };
            auth.has_auth_rules =
                crate::rewrite_auth::has_auth_rules(schema, field, AuthSelector::Update);
            let node_id = id_existence.get(&upsert_var).cloned().unwrap_or_default();
            queries.extend(rewrite_upsert_query_from_mutation(
                schema,
                field,
                &mut auth,
                &upsert_var,
                &upsert_var,
                &node_id,
            ));
            fragment
                .conditions
                .push(format!("gt(len({upsert_var}), 0)"));
        }
        rewriter.add_frags.push(fragment);
    }
    let mut set_blocks = Vec::new();
    let mut del_blocks = Vec::new();
    let mut conditions = Vec::new();
    let mut root_uids = Vec::new();
    for frag in &rewriter.add_frags {
        let (set, del) = fragment_nquads(frag);
        if !set.trim().is_empty() {
            set_blocks.push(set);
        }
        if !del.trim().is_empty() {
            del_blocks.push(del);
        }
        conditions.extend(frag.conditions.iter().cloned());
        queries.extend(frag.queries.iter().cloned());
        new_nodes.extend(
            frag.new_nodes
                .iter()
                .map(|(key, val)| (key.clone(), val.clone())),
        );
        if let Some(uid) = frag
            .fragment
            .as_object()
            .and_then(|obj| obj.get("uid"))
            .and_then(|val| val.as_str())
        {
            root_uids.push(uid.to_owned());
        }
    }
    if !set_blocks.is_empty() || !del_blocks.is_empty() {
        mutations.push(UpsertMutation {
            queries,
            set_nquads: set_blocks,
            del_nquads: del_blocks,
            conditions,
            new_nodes,
            root_uids,
        });
    }
    (mutations, errs)
}

/// Rewrites an update mutation into upsert mutations.
#[allow(clippy::too_many_lines, reason = "exact port of Go UpdateRewriter")]
pub fn update_rewrite<H: std::hash::BuildHasher>(
    schema: &BoundSchema,
    field: &BoundField,
    rewriter: &mut MutationRewriter,
    id_existence: &mut HashMap<String, String, H>,
    auth_vars: &crate::auth_rules::AuthVars,
) -> (Vec<UpsertMutation>, Vec<GraphQlError>) {
    let mutated = mutated_type(schema, field);
    let mut queries = Vec::new();
    let mut new_nodes = HashMap::new();
    let mut mutations = Vec::new();
    let mut errs = Vec::new();
    let mut auth = AuthRewriter {
        auth_variables: auth_vars.clone(),
        is_writing_auth: false,
        filter_by_uid: false,
        selector: AuthSelector::Update,
        var_gen: &mut rewriter.var_gen,
        var_name: String::new(),
        parent_var_name: format!("{mutated}Root"),
        has_auth_rules: false,
        has_cascade: false,
        schema,
    };
    auth.has_auth_rules = crate::rewrite_auth::has_auth_rules(schema, field, AuthSelector::Update);
    queries.extend(rewrite_upsert_query_from_mutation(
        schema,
        field,
        &mut auth,
        MUTATION_QUERY_VAR,
        &field.name,
        "",
    ));
    let input = field
        .arg("input")
        .and_then(|val| val.as_object())
        .cloned()
        .unwrap_or_default();
    let set = input.get("set").and_then(|val| val.as_object()).cloned();
    let remove = input.get("remove").and_then(|val| val.as_object()).cloned();
    if set.as_ref().is_none_or(JsonMap::is_empty) && remove.as_ref().is_none_or(JsonMap::is_empty) {
        return (Vec::new(), errs);
    }
    drop(auth);
    if let Some(set) = set {
        if !set.is_empty() {
            let (fragment, _, mut frag_errs) = rewrite_object(
                schema,
                &mutated,
                None,
                MUTATION_QUERY_VAR_UID,
                &mut rewriter.var_gen,
                set,
                &mut rewriter.xid_meta,
                id_existence,
                MutationKind::UpdateWithSet,
                auth_vars,
            );
            errs.append(&mut frag_errs);
            rewriter.set_frag = fragment;
        }
    }
    if let Some(remove) = remove {
        if !remove.is_empty() {
            let (fragment, _, mut frag_errs) = rewrite_object(
                schema,
                &mutated,
                None,
                MUTATION_QUERY_VAR_UID,
                &mut rewriter.var_gen,
                remove,
                &mut rewriter.xid_meta,
                id_existence,
                MutationKind::UpdateWithRemove,
                auth_vars,
            );
            errs.append(&mut frag_errs);
            rewriter.del_frag = fragment;
        }
    }
    let mut set_blocks = Vec::new();
    let mut del_blocks = Vec::new();
    if let Some(frag) = rewriter.set_frag.as_mut() {
        frag.conditions.push(UPDATE_MUTATION_CONDITION.to_owned());
        let (set, del) = fragment_nquads(frag);
        if !set.trim().is_empty() {
            set_blocks.push(set);
        }
        if !del.trim().is_empty() {
            del_blocks.push(del);
        }
        queries.extend(frag.queries.iter().cloned());
        new_nodes.extend(
            frag.new_nodes
                .iter()
                .map(|(key, val)| (key.clone(), val.clone())),
        );
    }
    if let Some(frag) = rewriter.del_frag.as_mut() {
        frag.conditions.push(UPDATE_MUTATION_CONDITION.to_owned());
        let (_, del) = fragment_nquads_for_delete(frag);
        if !del.trim().is_empty() {
            del_blocks.push(del);
        }
        queries.extend(frag.queries.iter().cloned());
        new_nodes.extend(
            frag.new_nodes
                .iter()
                .map(|(key, val)| (key.clone(), val.clone())),
        );
    }
    if !set_blocks.is_empty() || !del_blocks.is_empty() {
        let mut conditions = vec![UPDATE_MUTATION_CONDITION.to_owned()];
        if let Some(frag) = rewriter.set_frag.as_ref() {
            conditions.extend(
                frag.conditions
                    .iter()
                    .filter(|cond| *cond != UPDATE_MUTATION_CONDITION)
                    .cloned(),
            );
        }
        mutations.push(UpsertMutation {
            queries,
            set_nquads: set_blocks,
            del_nquads: del_blocks,
            conditions,
            new_nodes,
            root_uids: Vec::new(),
        });
    }
    (mutations, errs)
}

/// Rewrites a delete mutation into upsert mutations.
///
/// # Errors
///
/// Returns [`GraphQlError`] when called for a non-delete mutation.
pub fn delete_rewrite(
    schema: &BoundSchema,
    field: &BoundField,
    rewriter: &mut MutationRewriter,
    auth_vars: &crate::auth_rules::AuthVars,
) -> Result<Vec<UpsertMutation>, GraphQlError> {
    if field.mutation_type != MutationType::Delete {
        return Err(GraphQlError::new(format!(
            "(internal error) call to build delete mutation for {:?} mutation type",
            field.mutation_type
        )));
    }
    let mutated = mutated_type(schema, field);
    let mut auth = AuthRewriter {
        auth_variables: auth_vars.clone(),
        is_writing_auth: false,
        filter_by_uid: false,
        selector: AuthSelector::Delete,
        var_gen: &mut rewriter.var_gen,
        var_name: String::new(),
        parent_var_name: format!("{mutated}Root"),
        has_auth_rules: false,
        has_cascade: false,
        schema,
    };
    auth.has_auth_rules = crate::rewrite_auth::has_auth_rules(schema, field, AuthSelector::Delete);
    let mut queries = rewrite_upsert_query_from_mutation(
        schema,
        field,
        &mut auth,
        MUTATION_QUERY_VAR,
        &field.name,
        "",
    );
    let mut deletes = vec![Json::Object(JsonMap::from_iter([(
        "uid".to_owned(),
        Json::String(MUTATION_QUERY_VAR_UID.to_owned()),
    )]))];
    if queries[0].attr != format!("{}()", field.alias) {
        deletes.extend(remove_node_reference(
            schema,
            field,
            &mut auth,
            &mut queries[0],
        ));
    }
    expand_node_deletes(schema, &mutated, &mut deletes);
    let del_nquads = deletes_to_nquads(&deletes);
    let upserts = vec![UpsertMutation {
        queries,
        set_nquads: Vec::new(),
        del_nquads: vec![del_nquads],
        conditions: Vec::new(),
        new_nodes: HashMap::new(),
        root_uids: Vec::new(),
    }];
    Ok(upserts)
}

/// Builds the upsert root query selecting mutation targets.
fn rewrite_upsert_query_from_mutation(
    schema: &BoundSchema,
    field: &BoundField,
    auth: &mut AuthRewriter<'_>,
    var: &str,
    attr: &str,
    node_id: &str,
) -> Vec<GraphQuery> {
    let mutated = mutated_type(schema, field);
    let mut root = GraphQuery::new();
    var.clone_into(&mut root.var);
    attr.clone_into(&mut root.attr);
    let rbac = auth.evaluate_static_rules(&mutated);
    if rbac == crate::auth_rules::RuleResult::Negative {
        root.attr = format!("{}()", field.alias);
        return vec![root];
    }
    if schema.is_interface(&mutated) {
        let any_pass = schema
            .implementing_types(&mutated)
            .iter()
            .any(|typ| auth.evaluate_static_rules(typ) != crate::auth_rules::RuleResult::Negative);
        if !any_pass {
            root.attr = format!("{}()", field.alias);
            return vec![root];
        }
    }
    let mut uid_child = GraphQuery::new();
    "uid".clone_into(&mut uid_child.attr);
    root.children = vec![uid_child];
    if node_id.is_empty() {
        let filter = extract_mutation_filter(field);
        let ids = filter_ids(schema, &filter, &mutated);
        if let Some(ids) = ids {
            add_uid_func(&mut root, ids);
        } else {
            add_type_func(&mut root, &schema.datastreamdb_name(&mutated));
        }
        let _ = add_filter(schema, &mut root, &mutated, &filter);
    } else {
        let Some(uid) = parse_uid(node_id) else {
            root.attr = format!("{}()", field.alias);
            return vec![root];
        };
        add_uid_func(&mut root, vec![uid]);
        add_type_filter(&mut root, schema, &mutated);
    }
    auth.add_auth_queries(&mutated, vec![root], rbac)
}

/// Extracts ID values from a mutation filter.
fn filter_ids(
    schema: &BoundSchema,
    filter: &JsonMap<String, Json>,
    mutated: &str,
) -> Option<Vec<u64>> {
    let id_field = schema.id_field(mutated)?;
    let ids = filter.get(&id_field.name)?;
    let slice = match ids {
        Json::String(_) => vec![ids.clone()],
        Json::Array(items) => items.clone(),
        _ => return None,
    };
    Some(crate::rewrite_vars::convert_ids(&slice))
}

/// Parses a UID in decimal or hex form.
fn parse_uid(text: &str) -> Option<u64> {
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()
    } else {
        text.parse::<u64>().ok()
    }
}

/// Expands uid-only delete objects to per-predicate star deletes.
///
/// The embedded engine has no `S * *` path, so node deletion is
/// schema-driven: every out-predicate of the mutated type plus `datastreamdb.type`.
fn expand_node_deletes(schema: &BoundSchema, mutated: &str, deletes: &mut [Json]) {
    let mut preds: Vec<String> = schema
        .predicates
        .get(mutated)
        .map(|fields| {
            fields
                .values()
                .filter(|pred| !pred.starts_with('~') && !pred.starts_with("<~"))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    preds.sort();
    preds.dedup();
    for delete in deletes.iter_mut() {
        let Some(obj) = delete.as_object_mut() else {
            continue;
        };
        if obj.len() != 1 || !obj.contains_key("uid") {
            continue;
        }
        for pred in &preds {
            obj.insert(pred.clone(), Json::Null);
        }
        obj.insert("datastreamdb.type".to_owned(), Json::Null);
    }
}

/// Builds deletes for inverse references into deleted nodes.
fn remove_node_reference(
    schema: &BoundSchema,
    field: &BoundField,
    auth: &mut AuthRewriter<'_>,
    query: &mut GraphQuery,
) -> Vec<Json> {
    let mutated = mutated_type(schema, field);
    let Some(def) = schema.schema.type_def(&mutated).cloned() else {
        return Vec::new();
    };
    let mut deletes = Vec::new();
    for fld in &def.fields {
        let inv = inverse_field(schema, &mutated, &fld.name);
        let fwd = forward_field(schema, &mutated, &fld.name);
        let Some((inv_name, inv_is_list)) = inv.or(fwd) else {
            continue;
        };
        let target_var = auth.var_gen.next(schema, fld.ty.name(), ("", ""), false);
        let mut child = GraphQuery::new();
        child.var.clone_from(&target_var);
        schema
            .predicate(fld.ty.name(), &fld.name)
            .unwrap_or(&fld.name)
            .clone_into(&mut child.attr);
        query.children.push(child);
        let del_pred = schema
            .predicate(fld.ty.name(), &inv_name)
            .unwrap_or(&inv_name)
            .to_owned();
        let mut del = JsonMap::new();
        del.insert("uid".to_owned(), Json::String(format!("uid({target_var})")));
        let link = Json::Object(JsonMap::from_iter([(
            "uid".to_owned(),
            Json::String(MUTATION_QUERY_VAR_UID.to_owned()),
        )]));
        if inv_is_list {
            del.insert(del_pred, Json::Array(vec![link]));
        } else {
            del.insert(del_pred, link);
        }
        deletes.push(Json::Object(del));
    }
    deletes
}

/// Inverse field (name, is-list) for a field, if any.
fn inverse_field(schema: &BoundSchema, parent: &str, field: &str) -> Option<(String, bool)> {
    let def = schema.schema.type_def(parent)?.field(field)?;
    let dir = def.directive(crate::directives::INVERSE_DIRECTIVE)?;
    let target = dir.arg(crate::directives::INVERSE_ARG)?.raw();
    let target_def = schema.schema.type_def(def.ty.name())?.field(&target)?;
    Some((target, target_def.ty.is_list()))
}

/// Forward edge (name, is-list) for a reverse-predicate field, if any.
fn forward_field(schema: &BoundSchema, parent: &str, field: &str) -> Option<(String, bool)> {
    let def = schema.schema.type_def(parent)?.field(field)?;
    let dir = def.directive(crate::directives::DATASTREAMDB_DIRECTIVE)?;
    let pred = dir.arg(crate::directives::DATASTREAMDB_PRED_ARG)?.raw();
    if !pred.starts_with('~') && !pred.starts_with("<~") {
        return None;
    }
    let forward = pred.trim_matches(|ch| ch == '<' || ch == '~' || ch == '>');
    let target = schema.schema.type_def(def.ty.name())?;
    for candidate in &target.fields {
        if let Some(dir) = candidate.directive(crate::directives::DATASTREAMDB_DIRECTIVE) {
            if let Some(arg) = dir.arg(crate::directives::DATASTREAMDB_PRED_ARG) {
                if !arg.raw().is_empty() && arg.raw() == forward {
                    return Some((candidate.name.clone(), candidate.ty.is_list()));
                }
            }
        }
    }
    None
}

/// Renders a fragment body plus deletes to RDF blocks.
fn fragment_nquads(frag: &MutationFragment) -> (String, String) {
    let set = value_nquads(&frag.fragment, false);
    let mut del = String::new();
    for delete in &frag.deletes {
        del.push_str(&value_nquads(delete, true));
    }
    (set, del)
}

/// Renders a remove fragment body as deletes.
fn fragment_nquads_for_delete(frag: &MutationFragment) -> (String, String) {
    (String::new(), value_nquads(&frag.fragment, true))
}

/// Renders delete bodies to RDF.
fn deletes_to_nquads(deletes: &[Json]) -> String {
    let mut out = String::new();
    for delete in deletes {
        out.push_str(&value_nquads(delete, true));
    }
    out
}

/// Renders one JSON mutation value to RDF N-Quads.
fn value_nquads(val: &Json, delete: bool) -> String {
    let mut out = String::new();
    render_value(val, &mut out, delete);
    out
}

/// Recursive JSON to RDF rendering.
fn render_value(val: &Json, out: &mut String, delete: bool) {
    let Some(obj) = val.as_object() else {
        return;
    };
    let Some(subject) = obj.get("uid").and_then(|val| val.as_str()) else {
        return;
    };
    let subject = expand_uid(subject);
    let mut keys: Vec<&String> = obj.keys().collect();
    keys.sort();
    for key in keys {
        if key == "uid" {
            continue;
        }
        let value = &obj[key];
        if key == "datastreamdb.type" {
            if delete {
                if value.is_null() {
                    let _ = writeln!(out, "{subject} <datastreamdb.type> * .");
                }
                continue;
            }
            if let Some(types) = value.as_array() {
                for typ in types {
                    if let Some(typ) = typ.as_str() {
                        let _ = writeln!(out, "{subject} <datastreamdb.type> \"{typ}\" .");
                    }
                }
            }
            continue;
        }
        render_predicate(&subject, key, value, out, delete);
    }
}

/// Renders one predicate's values.
fn render_predicate(subject: &str, pred: &str, value: &Json, out: &mut String, delete: bool) {
    match value {
        Json::Null => {
            if delete {
                let _ = writeln!(out, "{subject} <{pred}> * .");
            }
        }
        Json::Object(obj) => {
            if let Some(target) = obj.get("uid").and_then(|val| val.as_str()) {
                let _ = writeln!(out, "{subject} <{pred}> {} .", expand_uid(target));
            }
            render_value(value, out, delete);
        }
        Json::Array(items) => {
            for item in items {
                render_predicate(subject, pred, item, out, delete);
            }
        }
        scalar => {
            let _ = writeln!(out, "{subject} <{pred}> {} .", rdf_scalar(scalar));
        }
    }
}

/// Expands `uid(...)` / blank / raw UID spellings to RDF subjects.
fn expand_uid(uid: &str) -> String {
    if uid.starts_with("_:") {
        uid.to_owned()
    } else if uid.starts_with("uid(") && uid.ends_with(')') {
        format!("uid({})", &uid[4..uid.len() - 1])
    } else if uid.starts_with("0x") || uid.parse::<u64>().is_ok() {
        format!("<{uid}>")
    } else {
        format!("uid({uid})")
    }
}

/// Renders a scalar JSON value as RDF (typed literals for non-strings).
fn rdf_scalar(val: &Json) -> String {
    match val {
        Json::String(text) => format!("{text:?}"),
        Json::Number(num) => {
            if num.is_i64() || num.is_u64() {
                format!("\"{num}\"^^<xs:int>")
            } else {
                format!("\"{num}\"^^<xs:float>")
            }
        }
        Json::Bool(val) => format!("\"{val}\"^^<xs:boolean>"),
        _ => "null".to_owned(),
    }
}

/// Rewrites the post-mutation selection query for add/update results.
#[must_use]
pub(crate) fn from_mutation_result(
    schema: &BoundSchema,
    field: &BoundField,
    uids: &[u64],
    auth_vars: &crate::auth_rules::AuthVars,
) -> Vec<GraphQuery> {
    let Some(query_field) = mutation_query_field(schema, field) else {
        return Vec::new();
    };
    let mut var_gen = VariableGenerator::new();
    let mut auth = AuthRewriter {
        auth_variables: auth_vars.clone(),
        is_writing_auth: false,
        filter_by_uid: false,
        selector: AuthSelector::Query,
        var_gen: &mut var_gen,
        var_name: String::new(),
        parent_var_name: format!("{}Root", query_field.type_name()),
        has_auth_rules: false,
        has_cascade: false,
        schema,
    };
    auth.has_auth_rules =
        crate::rewrite_auth::has_auth_rules(schema, &query_field, AuthSelector::Query);
    crate::rewrite_query::rewrite_as_query_by_ids(schema, &query_field, uids.to_vec(), &mut auth)
}

/// Builds the synthetic query field selecting mutation results.
///
/// Uses the first non-meta selection child (skipping `numUids`, `msg`,
/// `__typename`), like Go's `QueryField()`.
pub(crate) fn mutation_query_field(
    _schema: &BoundSchema,
    field: &BoundField,
) -> Option<BoundField> {
    let selected = field
        .children
        .iter()
        .find(|child| !matches!(child.name.as_str(), "numUids" | "msg" | "__typename"))?;
    let mut query_field = selected.clone();
    if field.cascade.is_some() && query_field.cascade.is_none() {
        query_field.cascade.clone_from(&field.cascade);
    }
    Some(query_field)
}

/// Collects mutated root UIDs from execution results.
#[must_use]
pub(crate) fn mutated_root_uids(
    result: &JsonMap<String, Json>,
    field_name: &str,
    assigned: &HashMap<String, String>,
    frag_uids: &[String],
) -> Vec<String> {
    let mut out = Vec::new();
    for frag_uid in frag_uids {
        if let Some(blank) = frag_uid.strip_prefix("_:") {
            // The embedded engine keys assignments with the `_:` prefix;
            // dgo (Go) strips it. Accept both.
            if let Some(uid) = assigned.get(blank).or_else(|| assigned.get(frag_uid)) {
                out.push(uid.clone());
                continue;
            }
        }
        if let Some(inner) = frag_uid
            .strip_prefix("uid(")
            .and_then(|inner| inner.strip_suffix(')'))
        {
            out.extend(extract_mutated(result, inner));
            continue;
        }
        out.extend(extract_mutated(result, field_name));
    }
    out
}

/// Extracts UIDs from a result field's objects.
fn extract_mutated(result: &JsonMap<String, Json>, field: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(Json::Array(items)) = result.get(field) {
        for item in items {
            if let Some(uid) = item
                .as_object()
                .and_then(|obj| obj.get("uid"))
                .and_then(|val| val.as_str())
            {
                out.push(uid.to_owned());
            }
        }
    }
    out
}

/// Evaluates `@if`/`cond` conditions against query-result var counts.
///
/// Shared by GraphQL upserts and DQL `@upsert` conditions: conjunctions of
/// `gt/lt/eq/ge/le(len(var), n)` over per-variable match counts.
#[must_use]
pub fn evaluate_condition(
    condition: &str,
    counts: &HashMap<String, usize, impl std::hash::BuildHasher>,
) -> bool {
    condition
        .split(" AND ")
        .all(|part| evaluate_single_condition(part.trim(), counts))
}

/// Evaluates one `gt/lt/eq(len(var), n)` condition.
fn evaluate_single_condition(
    condition: &str,
    counts: &HashMap<String, usize, impl std::hash::BuildHasher>,
) -> bool {
    let Some(inner) = condition.strip_suffix(')') else {
        return false;
    };
    let Some((func, args)) = inner.split_once('(') else {
        return false;
    };
    let args: Vec<&str> = args.split(',').map(str::trim).collect();
    if args.len() != 2 {
        return false;
    }
    let len = if let Some(var) = args[0]
        .strip_prefix("len(")
        .and_then(|inner| inner.strip_suffix(')'))
    {
        counts.get(var).copied().unwrap_or(0)
    } else {
        return false;
    };
    let Ok(expected) = args[1].parse::<usize>() else {
        return false;
    };
    match func {
        "gt" => len > expected,
        "lt" => len < expected,
        "eq" => len == expected,
        "ge" => len >= expected,
        "le" => len <= expected,
        _ => false,
    }
}
