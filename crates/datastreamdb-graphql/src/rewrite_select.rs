//! Selection-set rewriting: child blocks, ordering, pagination, cascade,
//! and nested aggregate fields. Port of `addSelectionSetFrom`,
//! `buildAggregateFields`, `addOrder`, `addPagination`, and
//! `addCascadeDirective` from `dgraph-main/graphql/resolve/query_rewriter.go`.

#![warn(missing_docs)]

use std::collections::HashSet;

use datastreamdb_dql::{Function, GraphQuery};

use crate::auth_rules::RuleResult;
use crate::op::{BoundField, BoundSchema};
use crate::rewrite_auth::{
    AuthRewriter, build_common_auth_queries, constructed_for, is_aggregate_field, uid_var_filter,
};
use crate::rewrite_filter::filter_child;
use crate::rewrite_vars::{add_type_filter, order_by};

/// Rewrites a field's selection set into child blocks, returning extra auth
/// queries.
#[allow(
    clippy::too_many_lines,
    reason = "exact port of Go addSelectionSetFrom"
)]
pub(crate) fn add_selection_set_from(
    schema: &BoundSchema,
    query: &mut GraphQuery,
    field: &BoundField,
    auth: &mut AuthRewriter<'_>,
) -> Vec<GraphQuery> {
    let mut auth_queries = Vec::new();
    if !field.children.is_empty() {
        if is_abstract_type(schema, field) {
            let mut child = GraphQuery::new();
            "datastreamdb.type".clone_into(&mut child.attr);
            query.children.push(child);
        } else if !auth.writing_auth()
            && field.children.len() == 1
            && field.children[0].name == "__typename"
        {
            let mut child = GraphQuery::new();
            "uid".clone_into(&mut child.attr);
            "datastreamdb.uid".clone_into(&mut child.alias);
            query.children.push(child);
        }
    }
    let mut field_added: HashSet<String> = HashSet::new();
    for child_field in &field.children {
        if child_field.is_custom_http {
            continue;
        }
        if child_field.skip || !child_field.include || child_field.name == "__typename" {
            continue;
        }
        if is_aggregate_field(child_field) {
            let (mut children, mut child_auth) = build_aggregate_fields(schema, child_field, auth);
            auth_queries.append(&mut child_auth);
            query.children.append(&mut children);
            field_added.insert(child_field.datastreamdb_alias(false));
            continue;
        }
        let mut child = GraphQuery::new();
        child.alias = child_field.datastreamdb_alias(false);
        if child_field.type_name() == "ID" && !child_field.is_external {
            "uid".clone_into(&mut child.attr);
        } else {
            schema
                .predicate(&child_field.object, &child_field.name)
                .unwrap_or(&child_field.name)
                .clone_into(&mut child.attr);
        }
        if !filter_child(schema, &mut child, child_field, child_field.type_name()) {
            continue;
        }
        if child.attr.starts_with('~') {
            add_type_filter(&mut child, schema, child_field.type_name());
        }
        add_order(schema, &mut child, child_field);
        add_pagination(&mut child, child_field);
        add_cascade_directive(&mut child, child_field);
        let mut rbac = auth.evaluate_static_rules(child_field.type_name());

        let (saved_parent, saved_var) =
            if !child_field.children.is_empty() && !auth.is_writing_auth && auth.has_auth_rules {
                let saved = (
                    std::mem::take(&mut auth.parent_var_name),
                    std::mem::take(&mut auth.var_name),
                );
                let parent = auth.var_gen.next(
                    schema,
                    child_field.type_name(),
                    ("", ""),
                    auth.is_writing_auth,
                );
                let var = auth.var_gen.next(
                    schema,
                    child_field.type_name(),
                    ("", ""),
                    auth.is_writing_auth,
                );
                auth.parent_var_name = parent;
                auth.var_name = var;
                (Some(saved.0), Some(saved.1))
            } else {
                (None, None)
            };

        let mut selection_auth = Vec::new();
        if !schema.is_geo(child_field.type_name()) {
            selection_auth = add_selection_set_from(schema, &mut child, child_field, auth);
        }
        let restore = |auth: &mut AuthRewriter<'_>| {
            if let (Some(parent), Some(var)) = (saved_parent.clone(), saved_var.clone()) {
                auth.parent_var_name = parent;
                auth.var_name = var;
            }
        };

        field_added.insert(child_field.datastreamdb_alias(false));
        if rbac == RuleResult::Positive || rbac == RuleResult::Uncertain {
            query.children.push(child.clone());
        }
        let mut field_auth = Vec::new();
        let mut auth_filter = None;
        if rbac == RuleResult::Negative
            && auth.has_auth_rules
            && auth.has_cascade
            && !auth.is_writing_auth
        {
            query.children.push(child.clone());
            let var_name = auth.var_gen.next(
                schema,
                child_field.type_name(),
                ("", ""),
                auth.is_writing_auth,
            );
            let mut dummy = GraphQuery::new();
            dummy.var.clone_from(&var_name);
            "var".clone_into(&mut dummy.attr);
            dummy.func = Some(Function {
                name: "uid".to_owned(),
                ..Function::default()
            });
            field_auth.push(dummy);
            auth_filter = Some(uid_var_filter(&var_name));
            rbac = RuleResult::Positive;
        } else if rbac == RuleResult::Negative {
            restore(auth);
            continue;
        }
        if rbac == RuleResult::Uncertain {
            let (queries, filter) = auth.rewrite_auth_queries(child_field.type_name());
            field_auth = queries;
            auth_filter = filter;
        }
        if !child_field.children.is_empty() && !auth.is_writing_auth && auth.has_auth_rules {
            let parent_name = saved_parent.clone().unwrap_or_default();
            let vars = build_common_auth_queries(
                schema,
                child_field,
                &auth.var_name.clone(),
                &auth.parent_var_name.clone(),
                &parent_name,
            );
            let mut parent_qry = vars.parent_qry;
            let mut selection_qry = vars.selection_qry;
            if let Some(first) = parent_qry.children.first_mut() {
                first.filter = child.filter.clone().map(|boxed| *boxed).map(Box::new);
            }
            selection_qry.filter = auth_filter.map(Box::new);
            if let Some(last) = query.children.last_mut() {
                last.filter = Some(Box::new(uid_var_filter(&selection_qry.var)));
            }
            auth_queries.push(parent_qry);
            auth_queries.push(selection_qry);
        }
        auth_queries.extend(selection_auth);
        auth_queries.extend(field_auth);
        restore(auth);
    }
    auth_queries
}

/// Whether the field's type is abstract (interface or union).
fn is_abstract_type(schema: &BoundSchema, field: &BoundField) -> bool {
    schema.is_interface(field.type_name()) || schema.is_union(field.type_name())
}

/// Adds `orderasc`/`orderdesc` from an `order` argument.
fn add_order(schema: &BoundSchema, query: &mut GraphQuery, field: &BoundField) {
    let mut order = field.arg("order").and_then(|val| val.as_object());
    while let Some(map) = order {
        if let Some(asc) = map.get("asc").and_then(|val| val.as_str()) {
            let pred = schema
                .predicate(field.type_name(), asc)
                .unwrap_or(asc)
                .to_owned();
            query.order.push(order_by(&pred, false));
        } else if let Some(desc) = map.get("desc").and_then(|val| val.as_str()) {
            let pred = schema
                .predicate(field.type_name(), desc)
                .unwrap_or(desc)
                .to_owned();
            query.order.push(order_by(&pred, true));
        }
        order = map.get("then").and_then(|val| val.as_object());
    }
}

/// Adds `first`/`offset` from pagination arguments.
fn add_pagination(query: &mut GraphQuery, field: &BoundField) {
    if let Some(first) = field.arg("first") {
        query.args.insert("first".to_owned(), json_scalar(first));
    }
    if let Some(offset) = field.arg("offset") {
        query.args.insert("offset".to_owned(), json_scalar(offset));
    }
}

/// Renders a scalar JSON value like Go's `%v`.
fn json_scalar(val: &serde_json::Value) -> String {
    match val {
        serde_json::Value::String(text) => text.clone(),
        _ => val.to_string(),
    }
}

/// Adds `@cascade` from the field's cascade directive.
fn add_cascade_directive(query: &mut GraphQuery, field: &BoundField) {
    if let Some(cascade) = &field.cascade {
        query.cascade.clone_from(cascade);
    }
}

/// `DataStreamDB` predicate for an aggregate function field (`nameMin` -> `T.name`).
fn aggregate_predicate(schema: &BoundSchema, field: &BoundField, constructed: &str) -> String {
    let mut base = field.name.as_str();
    for suffix in ["Max", "Min", "Sum", "Avg"] {
        if let Some(stripped) = base.strip_suffix(suffix) {
            base = stripped;
            break;
        }
    }
    if base == field.name {
        return schema
            .predicate(&field.object, &field.name)
            .unwrap_or(&field.name)
            .to_owned();
    }
    schema
        .predicate(constructed, base)
        .unwrap_or(base)
        .to_owned()
}

/// Builds DQL for nested aggregate fields (`postsAggregate { ... }`).
#[allow(
    clippy::too_many_lines,
    reason = "exact port of Go buildAggregateFields"
)]
fn build_aggregate_fields(
    schema: &BoundSchema,
    field: &BoundField,
    auth: &mut AuthRewriter<'_>,
) -> (Vec<GraphQuery>, Vec<GraphQuery>) {
    let constructed = constructed_for(field);
    let constructed_pred = if is_aggregate_field(field) {
        let base = field.name.strip_suffix("Aggregate").unwrap_or(&field.name);
        schema
            .predicate(&field.object, base)
            .unwrap_or(base)
            .to_owned()
    } else {
        schema
            .predicate(&field.object, &field.name)
            .unwrap_or(&field.name)
            .to_owned()
    };
    let mut aggregate_children = Vec::new();
    let mut other_children = Vec::new();
    let mut main_field = GraphQuery::new();
    main_field.alias = field.datastreamdb_alias(false);
    main_field.attr.clone_from(&constructed_pred);
    let empty = serde_json::Map::new();
    let field_filter = field
        .arg("filter")
        .and_then(|val| val.as_object())
        .unwrap_or(&empty);
    let _ = crate::rewrite_filter::add_filter(schema, &mut main_field, &constructed, field_filter);
    if constructed_pred.starts_with('~') {
        add_type_filter(&mut main_field, schema, &constructed);
    }
    let mut var_added: HashSet<String> = HashSet::new();
    for aggregate_field in &field.children {
        if aggregate_field.name == "count" {
            let mut child = GraphQuery::new();
            child.alias = format!(
                "{}_{}",
                aggregate_field.datastreamdb_alias(false),
                field.datastreamdb_alias(false)
            );
            child.attr = format!("count({constructed_pred})");
            let _ =
                crate::rewrite_filter::add_filter(schema, &mut child, &constructed, field_filter);
            if constructed_pred.starts_with('~') {
                add_type_filter(&mut child, schema, &constructed);
            }
            aggregate_children.push(child);
            continue;
        }
        for function in ["Max", "Min", "Sum", "Avg"] {
            if !aggregate_field.name.ends_with(function) {
                continue;
            }
            let base = aggregate_field.name[..aggregate_field.name.len() - 3].to_owned();
            if !var_added.contains(&base) {
                let mut child = GraphQuery::new();
                child.var = format!("{}_{base}Var", field.datastreamdb_alias(false));
                child.attr = aggregate_predicate(schema, aggregate_field, &constructed);
                main_field.children.push(child);
                var_added.insert(base.clone());
            }
            let mut child = GraphQuery::new();
            child.alias = format!(
                "{}_{}",
                aggregate_field.datastreamdb_alias(false),
                field.datastreamdb_alias(false)
            );
            child.attr = format!(
                "{}(val({}_{base}Var))",
                function.to_lowercase(),
                field.datastreamdb_alias(false),
            );
            other_children.push(child);
            break;
        }
    }
    if !main_field.children.is_empty() {
        aggregate_children.insert(0, main_field);
    }
    let rbac = auth.evaluate_static_rules(&constructed);
    if rbac == RuleResult::Negative {
        return (Vec::new(), Vec::new());
    }
    let (saved_parent, saved_var) =
        if !field.children.is_empty() && !auth.is_writing_auth && auth.has_auth_rules {
            let saved = (
                std::mem::take(&mut auth.parent_var_name),
                std::mem::take(&mut auth.var_name),
            );
            let parent =
                auth.var_gen
                    .next(schema, field.type_name(), ("", ""), auth.is_writing_auth);
            let var = auth
                .var_gen
                .next(schema, field.type_name(), ("", ""), auth.is_writing_auth);
            auth.parent_var_name = parent;
            auth.var_name = var;
            (Some(saved.0), Some(saved.1))
        } else {
            (None, None)
        };
    let mut ret_auth = Vec::new();
    let mut auth_filter = None;
    if rbac == RuleResult::Uncertain {
        let (queries, filter) = auth.rewrite_auth_queries(&constructed);
        ret_auth = queries;
        auth_filter = filter;
    }
    if !field.children.is_empty() && !auth.is_writing_auth && auth.has_auth_rules {
        let vars = build_common_auth_queries(
            schema,
            field,
            &auth.var_name.clone(),
            &auth.parent_var_name.clone(),
            &saved_parent.clone().unwrap_or_default(),
        );
        let mut parent_qry = vars.parent_qry;
        let selection_qry = vars.selection_qry;
        let mut selection_qry = selection_qry;
        selection_qry.filter = auth_filter.map(Box::new);
        let mut appended = false;
        for child in &mut aggregate_children {
            if !appended {
                if let Some(first) = parent_qry.children.first_mut() {
                    first.filter.clone_from(&child.filter);
                }
                ret_auth.push(parent_qry.clone());
                ret_auth.push(selection_qry.clone());
                appended = true;
            }
            child.filter = Some(Box::new(uid_var_filter(&selection_qry.var)));
        }
        if let (Some(parent), Some(var)) = (saved_parent, saved_var) {
            auth.parent_var_name = parent;
            auth.var_name = var;
        }
    }
    aggregate_children.extend(other_children);
    (aggregate_children, ret_auth)
}

/// Adds a `uid` child to every block with children (recursive).
pub(crate) fn add_uid(query: &mut GraphQuery) {
    if query.children.is_empty() {
        return;
    }
    if !query.children.iter().any(|child| child.attr == "uid") {
        let mut uid = GraphQuery::new();
        "uid".clone_into(&mut uid.attr);
        "datastreamdb.uid".clone_into(&mut uid.alias);
        query.children.push(uid);
    }
    for child in &mut query.children {
        add_uid(child);
    }
}
