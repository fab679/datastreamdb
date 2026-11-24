//! GraphQL query to DQL rewriting. Port of `queryRewriter` and the
//! `rewriteAs*` family from `dgraph-main/graphql/resolve/query_rewriter.go`.
//!
//! The rewrite returns DQL blocks plus variable bindings (password queries
//! parameterize the secret). Vector `similar_to` rewrites are ported; their
//! execution depends on the vector milestone.

#![warn(missing_docs)]

use std::collections::HashMap;

use datastreamdb_dql::{FilterTree, Function, GraphQuery};

use crate::auth_rules::RuleResult;
use crate::errors::GraphQlError;
use crate::op::{BoundField, BoundSchema, QueryType};
use crate::rewrite_auth::{AuthRewriter, AuthSelector};
use crate::rewrite_filter::{add_filter, maybe_quote_arg};
use crate::rewrite_select::{add_selection_set_from, add_uid};
use crate::rewrite_vars::{
    VariableGenerator, add_eq_func, add_to_filter_tree, add_type_filter, add_type_func,
    add_uid_func, id_arg_value, order_by, plain_arg, var_arg, xid_args,
};

/// DQL variable holding the password guess.
const PWD_VAR: &str = "$pwd0";

/// Rewritten query: DQL blocks plus variable bindings.
pub struct RewrittenQuery {
    /// DQL blocks.
    pub queries: Vec<GraphQuery>,
    /// DQL variable bindings (`$var -> value`).
    pub vars: HashMap<String, String>,
}

/// Rewrites one root query field into DQL.
///
/// # Errors
///
/// Returns [`GraphQlError`] on unimplemented query kinds or bad ID arguments.
pub fn rewrite_query(
    schema: &BoundSchema,
    field: &BoundField,
    auth_vars: &crate::auth_rules::AuthVars,
) -> Result<RewrittenQuery, GraphQlError> {
    let selector = if field.query_type == QueryType::Password {
        AuthSelector::Password
    } else {
        AuthSelector::Query
    };
    let mut var_gen = VariableGenerator::new();
    let parent_var = format!("{}Root", field.type_name());
    let mut auth = AuthRewriter {
        auth_variables: auth_vars.clone(),
        is_writing_auth: false,
        filter_by_uid: false,
        selector,
        var_gen: &mut var_gen,
        var_name: String::new(),
        parent_var_name: parent_var,
        has_auth_rules: crate::rewrite_auth::has_auth_rules(schema, field, selector),
        has_cascade: crate::rewrite_auth::has_cascade_directive(field),
        schema,
    };
    match field.query_type {
        QueryType::Get => {
            let (xids, uid) = id_arg_value(schema, field)?;
            Ok(RewrittenQuery {
                queries: rewrite_as_get(schema, field, uid, &xids, &mut auth),
                vars: HashMap::new(),
            })
        }
        QueryType::SimilarById => {
            let (xids, uid) = id_arg_value(schema, field)?;
            Ok(RewrittenQuery {
                queries: rewrite_as_similar_by_id(schema, field, uid, &xids, &mut auth),
                vars: HashMap::new(),
            })
        }
        QueryType::SimilarByEmbedding => Ok(RewrittenQuery {
            queries: rewrite_as_similar_by_embedding(schema, field, &mut auth),
            vars: HashMap::new(),
        }),
        QueryType::Filter => Ok(RewrittenQuery {
            queries: rewrite_as_query(schema, field, &mut auth),
            vars: HashMap::new(),
        }),
        QueryType::Password => password_query(schema, field, &mut auth),
        QueryType::Aggregate => Ok(RewrittenQuery {
            queries: aggregate_query(schema, field, &mut auth),
            vars: HashMap::new(),
        }),
        QueryType::Entities => Ok(RewrittenQuery {
            queries: entities_query(schema, field, &mut auth)?,
            vars: HashMap::new(),
        }),
        other => Err(GraphQlError::new(format!(
            "unimplemented query type {other:?}"
        ))),
    }
}

/// Rewrites a `getT` query.
fn rewrite_as_get(
    schema: &BoundSchema,
    field: &BoundField,
    uid: u64,
    xids: &HashMap<String, String>,
    auth: &mut AuthRewriter<'_>,
) -> Vec<GraphQuery> {
    let rbac = auth.evaluate_static_rules(field.type_name());
    if rbac == RuleResult::Negative {
        let mut empty = GraphQuery::new();
        empty.attr = format!("{}()", field.name);
        return vec![empty];
    }
    if schema.is_interface(field.type_name()) {
        let implementing = schema.implementing_types(field.type_name());
        let any_pass = implementing
            .iter()
            .any(|typ| auth.evaluate_static_rules(typ) != RuleResult::Negative);
        if !any_pass {
            let mut empty = GraphQuery::new();
            empty.attr = format!("{}()", field.name);
            return vec![empty];
        }
    }
    if xids.is_empty() {
        let mut queries = rewrite_as_query_by_ids(schema, field, vec![uid], auth);
        if let Some(first) = queries.first_mut() {
            add_type_filter_top(schema, first, field);
        }
        return queries;
    }
    let mut names: Vec<&String> = xids.keys().collect();
    names.sort();
    let pred_map = xid_args(schema, field);
    let mut filters = Vec::new();
    for xid in names {
        filters.push(FilterTree {
            op: String::new(),
            child: Vec::new(),
            func: Some(Function {
                name: "eq".to_owned(),
                args: vec![
                    plain_arg(pred_map.get(xid).map_or(xid.as_str(), String::as_str)),
                    plain_arg(&maybe_quote_arg(
                        "eq",
                        &serde_json::Value::String(xids[xid].clone()),
                    )),
                ],
                ..Function::default()
            }),
        });
    }
    let mut root = GraphQuery::new();
    root.attr.clone_from(&field.name);
    if uid > 0 {
        root.func = Some(crate::rewrite_vars::uid_func(vec![uid], Vec::new()));
        root.filter = Some(Box::new(FilterTree {
            op: "and".to_owned(),
            child: filters,
            func: None,
        }));
    } else {
        let mut rest = filters;
        let first = rest.remove(0);
        root.func = first.func;
        if !rest.is_empty() {
            root.filter = Some(Box::new(FilterTree {
                op: "and".to_owned(),
                child: rest,
                func: None,
            }));
        }
    }
    let saved = auth.selector;
    auth.selector = AuthSelector::Query;
    let selection_auth = add_selection_set_from(schema, &mut root, field, auth);
    auth.selector = saved;
    add_uid(&mut root);
    add_type_filter(&mut root, schema, field.type_name());
    add_cascade(schema, &mut root, field);
    let mut queries = auth.add_auth_queries(field.type_name(), vec![root], rbac);
    queries.extend(selection_auth);
    queries
}

/// Rewrites a query addressed by UIDs.
pub(crate) fn rewrite_as_query_by_ids(
    schema: &BoundSchema,
    field: &BoundField,
    uids: Vec<u64>,
    auth: &mut AuthRewriter<'_>,
) -> Vec<GraphQuery> {
    let rbac = auth.evaluate_static_rules(field.type_name());
    let mut root = GraphQuery::new();
    root.attr.clone_from(&field.name);
    if rbac == RuleResult::Negative {
        root.attr = format!("{}()", field.name);
        return vec![root];
    }
    add_uid_func(&mut root, uids.clone());
    if let Some(ids) = crate::rewrite_filter::field_id_filter(schema, field, field.type_name()) {
        let wanted: std::collections::HashSet<u64> = uids.into_iter().collect();
        let kept: Vec<u64> = ids.into_iter().filter(|id| wanted.contains(id)).collect();
        add_uid_func(&mut root, kept);
    }
    add_arguments_to_field(schema, &mut root, field);
    let saved = auth.selector;
    auth.selector = AuthSelector::Query;
    let selection_auth = add_selection_set_from(schema, &mut root, field, auth);
    auth.selector = saved;
    add_uid(&mut root);
    add_cascade(schema, &mut root, field);
    let mut queries = auth.add_auth_queries(field.type_name(), vec![root], rbac);
    queries.extend(selection_auth);
    queries
}

/// Adds filter/order/pagination arguments to a root block.
fn add_arguments_to_field(schema: &BoundSchema, query: &mut GraphQuery, field: &BoundField) {
    let empty = serde_json::Map::new();
    let filter = field
        .arg("filter")
        .and_then(|val| val.as_object())
        .unwrap_or(&empty);
    let _ = add_filter(schema, query, field.type_name(), filter);
    add_order_arg(schema, query, field);
    if let Some(first) = field.arg("first") {
        query.args.insert("first".to_owned(), json_scalar(first));
    }
    if let Some(offset) = field.arg("offset") {
        query.args.insert("offset".to_owned(), json_scalar(offset));
    }
}

/// Adds ordering from an `order` argument.
fn add_order_arg(schema: &BoundSchema, query: &mut GraphQuery, field: &BoundField) {
    let mut order = field.arg("order").and_then(|val| val.as_object());
    while let Some(map) = order {
        if let Some(asc) = map.get("asc").and_then(|val| val.as_str()) {
            let pred = schema.predicate(field.type_name(), asc).unwrap_or(asc);
            query.order.push(order_by(pred, false));
        } else if let Some(desc) = map.get("desc").and_then(|val| val.as_str()) {
            let pred = schema.predicate(field.type_name(), desc).unwrap_or(desc);
            query.order.push(order_by(pred, true));
        }
        order = map.get("then").and_then(|val| val.as_object());
    }
}

/// Renders a scalar JSON value like Go's `%v`.
fn json_scalar(val: &serde_json::Value) -> String {
    match val {
        serde_json::Value::String(text) => text.clone(),
        _ => val.to_string(),
    }
}

/// Adds `@cascade` from a field.
fn add_cascade(_schema: &BoundSchema, query: &mut GraphQuery, field: &BoundField) {
    if let Some(cascade) = &field.cascade {
        query.cascade.clone_from(cascade);
    }
}

/// Adds a top-level type filter.
fn add_type_filter_top(schema: &BoundSchema, query: &mut GraphQuery, field: &BoundField) {
    add_type_filter(query, schema, field.type_name());
}

/// Rewrites a `queryT` filter query.
pub(crate) fn rewrite_as_query(
    schema: &BoundSchema,
    field: &BoundField,
    auth: &mut AuthRewriter<'_>,
) -> Vec<GraphQuery> {
    let (mut queries, rbac) = add_common_rules(schema, field, field.type_name(), auth);
    if rbac == RuleResult::Negative {
        return queries;
    }
    add_arguments_to_field(schema, &mut queries[0], field);
    let selection_auth = add_selection_set_from(schema, &mut queries[0], field, auth);
    if !auth.writing_auth() {
        add_uid(&mut queries[0]);
    }
    add_cascade(schema, &mut queries[0], field);
    let mut queries = auth.add_auth_queries(field.type_name(), queries, rbac);
    if !selection_auth.is_empty() {
        queries.extend(selection_auth);
        return queries;
    }
    root_query_optimization(&mut queries);
    queries
}

/// Adds common root rules (uid/type function) plus static auth verdict.
fn add_common_rules(
    schema: &BoundSchema,
    field: &BoundField,
    type_name: &str,
    auth: &mut AuthRewriter<'_>,
) -> (Vec<GraphQuery>, RuleResult) {
    let rbac = auth.evaluate_static_rules(type_name);
    let mut root = GraphQuery::new();
    root.attr = field.datastreamdb_alias(true);
    if rbac == RuleResult::Negative {
        root.attr = format!("{}()", field.datastreamdb_alias(true));
        return (vec![root], rbac);
    }
    let ids = crate::rewrite_filter::field_id_filter(schema, field, type_name);
    if auth.is_writing_auth || auth.filter_by_uid {
        if (!auth.var_name.is_empty() || !auth.parent_var_name.is_empty()) && ids.is_none() {
            auth.add_variable_uid_func(&mut root);
            if auth.filter_by_uid {
                auth.var_name.clear();
                auth.filter_by_uid = false;
            }
        } else if let Some(ids) = ids {
            add_uid_func(&mut root, ids);
        } else {
            add_type_func(&mut root, &schema.datastreamdb_name(type_name));
        }
    } else if let Some(ids) = ids {
        add_uid_func(&mut root, ids);
    } else {
        add_type_func(&mut root, &schema.datastreamdb_name(type_name));
    }
    (vec![root], rbac)
}

/// Moves an `eq` filter into the root function when rooted at `type(...)`.
fn root_query_optimization(queries: &mut [GraphQuery]) {
    let Some(root) = queries.first_mut() else {
        return;
    };
    let swap = root
        .filter
        .as_ref()
        .is_some_and(|filter| filter.func.as_ref().is_some_and(|func| func.name == "eq"))
        && root.func.as_ref().is_some_and(|func| func.name == "type");
    if !swap {
        return;
    }
    let filter_func = root.filter.as_mut().and_then(|filter| filter.func.take());
    let root_func = root.func.take();
    if let Some(filter) = root.filter.as_mut() {
        filter.func = root_func;
    }
    root.func = filter_func;
}

/// Rewrites a `checkTPassword` query.
fn password_query(
    schema: &BoundSchema,
    field: &BoundField,
    auth: &mut AuthRewriter<'_>,
) -> Result<RewrittenQuery, GraphQlError> {
    let (xids, uid) = id_arg_value(schema, field)?;
    let mut queries = rewrite_as_get(schema, field, uid, &xids, auth);
    if queries[0].attr.ends_with("()") {
        return Ok(RewrittenQuery {
            queries,
            vars: HashMap::new(),
        });
    }
    let pwd_name = schema
        .password_field(field.type_name())
        .map(|pwd| pwd.name)
        .unwrap_or_default();
    let pred = schema
        .predicate(field.type_name(), &pwd_name)
        .unwrap_or(&pwd_name)
        .to_owned();
    let password = field
        .arg(&pwd_name)
        .and_then(|val| val.as_str())
        .unwrap_or_default()
        .to_owned();
    queries[0]
        .args
        .insert(PWD_VAR.to_owned(), "string".to_owned());
    let mut check = GraphQuery::new();
    "checkPwd".clone_into(&mut check.attr);
    check.func.clone_from(&queries[0].func);
    check.filter.clone_from(&queries[0].filter);
    let mut pwd_child = GraphQuery::new();
    "pwd".clone_into(&mut pwd_child.var);
    pwd_child.attr = format!("checkpwd({pred}, {PWD_VAR})");
    check.children = vec![pwd_child];
    let mut combined = FilterTree {
        op: "and".to_owned(),
        child: vec![crate::rewrite_auth::eq_val_filter()],
        func: None,
    };
    if let Some(existing) = queries[0].filter.take() {
        combined.child.push(*existing);
    }
    queries[0].filter = Some(Box::new(combined));
    queries.push(check);
    Ok(RewrittenQuery {
        queries,
        vars: HashMap::from([(PWD_VAR.to_owned(), password)]),
    })
}

/// Rewrites an `aggregateT` query.
fn aggregate_query(
    schema: &BoundSchema,
    field: &BoundField,
    auth: &mut AuthRewriter<'_>,
) -> Vec<GraphQuery> {
    let main_type = field
        .type_name()
        .strip_suffix("AggregateResult")
        .unwrap_or_else(|| field.type_name())
        .to_owned();
    let (mut queries, rbac) = add_common_rules(schema, field, &main_type, auth);
    if rbac == RuleResult::Negative {
        return queries;
    }
    let empty = serde_json::Map::new();
    let filter = field
        .arg("filter")
        .and_then(|val| val.as_object())
        .unwrap_or(&empty);
    let _ = add_filter(schema, &mut queries[0], &main_type, filter);
    let mut queries = auth.add_auth_queries(&main_type, queries, rbac);
    let main_query = &mut queries[0];
    "var".clone_into(&mut main_query.attr);
    let mut final_query = GraphQuery::new();
    final_query.attr = format!("{}()", field.datastreamdb_alias(true));
    let mut var_added: HashMap<String, bool> = HashMap::new();
    let mut count_added = false;
    for child_field in &field.children {
        if child_field.name == "count" {
            if !count_added {
                let mut child = GraphQuery::new();
                "countVar".clone_into(&mut child.var);
                "count(uid)".clone_into(&mut child.attr);
                main_query.children.push(child);
                count_added = true;
            }
            let mut final_child = GraphQuery::new();
            final_child.alias = child_field.datastreamdb_alias(false);
            "max(val(countVar))".clone_into(&mut final_child.attr);
            final_query.children.push(final_child);
            continue;
        }
        for function in ["Max", "Min", "Sum", "Avg"] {
            if !child_field.name.ends_with(function) {
                continue;
            }
            let base = child_field.name[..child_field.name.len() - 3].to_owned();
            let pred = schema
                .predicate(&main_type, &base)
                .unwrap_or(&base)
                .to_owned();
            if !var_added.contains_key(&base) {
                let mut child = GraphQuery::new();
                child.var = format!("{base}Var");
                child.attr = pred;
                main_query.children.push(child);
                var_added.insert(base.clone(), true);
            }
            let mut final_child = GraphQuery::new();
            final_child.alias = child_field.datastreamdb_alias(false);
            final_child.attr = format!("{}(val({base}Var))", function.to_lowercase());
            final_query.children.push(final_child);
            break;
        }
    }
    let mut out = vec![final_query];
    out.append(&mut queries);
    out
}

/// Parsed `_entities` representations.
struct EntityRepresentations {
    type_name: String,
    key_field: String,
    key_is_id: bool,
    key_is_external: bool,
    key_vals: Vec<serde_json::Value>,
}

/// Parses the `representations` argument of `_entities`.
fn representations_arg(
    schema: &BoundSchema,
    field: &BoundField,
) -> Result<EntityRepresentations, GraphQlError> {
    let Some(representations) = field.arg("representations").and_then(|val| val.as_array()) else {
        return Err(GraphQlError::new(
            "error parsing `representations` argument",
        ));
    };
    if representations.is_empty() {
        return Err(GraphQlError::new(
            "expecting at least one item in `representations` argument",
        ));
    }
    let first = representations[0].as_object().ok_or_else(|| {
        GraphQlError::new("error parsing 0th item in the `_representations` argument")
    })?;
    let typename = first
        .get("__typename")
        .and_then(|val| val.as_str())
        .ok_or_else(|| {
            GraphQlError::new(
                "unable to extract __typename from 0th item in the `_representations` argument",
            )
        })?;
    let def = schema
        .schema
        .type_def(typename)
        .ok_or_else(|| GraphQlError::new(format!("type {typename} not found in the schema")))?;
    let key_dir = def
        .directive(crate::directives::APOLLO_KEY_DIRECTIVE)
        .ok_or_else(|| {
            GraphQlError::new(format!("type {typename} doesn't have a key Directive"))
        })?;
    let key_field = key_dir
        .arg(crate::directives::APOLLO_KEY_ARG)
        .map(super::model::Value::raw)
        .unwrap_or_default();
    let mut key_vals = Vec::with_capacity(representations.len());
    for (i, rep) in representations.iter().enumerate() {
        let rep = rep.as_object().ok_or_else(|| {
            GraphQlError::new(format!(
                "error parsing {i}th item in the `_representations` argument"
            ))
        })?;
        let name = rep.get("__typename").and_then(|val| val.as_str()).ok_or_else(|| {
            GraphQlError::new(format!(
                "unable to extract __typename from {i}th item in the `_representations` argument"
            ))
        })?;
        if name != typename {
            return Err(GraphQlError::new(format!(
                "expected only one unique typename in `_representations` argument, got: \
                 [{typename}, {name}]"
            )));
        }
        let val = rep.get(&key_field).ok_or_else(|| {
            GraphQlError::new(format!(
                "unable to extract value for key field `{key_field}` from {i}th item in the \
                 `_representations` argument"
            ))
        })?;
        key_vals.push(val.clone());
    }
    let key_def = def.field(&key_field);
    Ok(EntityRepresentations {
        type_name: typename.to_owned(),
        key_field,
        key_is_id: key_def.is_some_and(|field| field.ty.name() == "ID"),
        key_is_external: key_def.is_some_and(|field| {
            field
                .directive(crate::directives::APOLLO_EXTERNAL_DIRECTIVE)
                .is_some()
        }),
        key_vals,
    })
}

/// Rewrites the Apollo `_entities` query.
fn entities_query(
    schema: &BoundSchema,
    field: &BoundField,
    auth: &mut AuthRewriter<'_>,
) -> Result<Vec<GraphQuery>, GraphQlError> {
    let parsed = representations_arg(schema, field)?;
    let rbac = auth.evaluate_static_rules(&parsed.type_name);
    let mut root = GraphQuery::new();
    root.attr.clone_from(&field.name);
    if rbac == RuleResult::Negative {
        root.attr = format!("{}()", field.name);
        return Ok(vec![root]);
    }
    if parsed.key_is_id && !parsed.key_is_external {
        let ids: Vec<u64> = parsed
            .key_vals
            .iter()
            .filter_map(|val| val.as_str())
            .filter_map(|text| {
                if let Some(hex) = text.strip_prefix("0x") {
                    u64::from_str_radix(hex, 16).ok()
                } else {
                    text.parse::<u64>().ok()
                }
            })
            .collect();
        crate::rewrite_vars::add_uid_func(&mut root, ids);
    } else {
        let pred = schema
            .predicate(&parsed.type_name, &parsed.key_field)
            .unwrap_or(&parsed.key_field)
            .to_owned();
        let values: Vec<String> = parsed
            .key_vals
            .iter()
            .map(|val| maybe_quote_arg("eq", val))
            .collect();
        add_eq_func(&mut root, &pred, &values);
        root.order.push(order_by(&pred, false));
    }
    add_type_filter(&mut root, schema, &parsed.type_name);
    let selection_auth = add_selection_set_from(schema, &mut root, field, auth);
    add_uid(&mut root);
    let mut queries = auth.add_auth_queries(&parsed.type_name, vec![root], rbac);
    queries.extend(selection_auth);
    Ok(queries)
}

/// Embedding search metric for a field (`euclidean` default).
fn embedding_metric(schema: &BoundSchema, type_name: &str, field_name: &str) -> String {
    let Some(def) = schema
        .schema
        .type_def(type_name)
        .and_then(|def| def.field(field_name))
    else {
        return "euclidean".to_owned();
    };
    let Some(search) = def.directive(crate::directives::SEARCH_DIRECTIVE) else {
        return "euclidean".to_owned();
    };
    let Some(args) = crate::rules_util::get_search_args(def) else {
        return "euclidean".to_owned();
    };
    let Some(first) = args.first() else {
        return "euclidean".to_owned();
    };
    let _ = search;
    let (options, _) = crate::rules_util::parse_search_options(first);
    options
        .get("metric")
        .cloned()
        .unwrap_or_else(|| "euclidean".to_owned())
}

/// Rewrites a `querySimilarTById` query.
fn rewrite_as_similar_by_id(
    schema: &BoundSchema,
    field: &BoundField,
    uid: u64,
    xids: &HashMap<String, String>,
    auth: &mut AuthRewriter<'_>,
) -> Vec<GraphQuery> {
    let type_name = field.type_name().to_owned();
    let similar_by = field
        .arg("by")
        .and_then(|val| val.as_str())
        .unwrap_or_default()
        .to_owned();
    let pred = schema
        .predicate(&type_name, &similar_by)
        .unwrap_or(&similar_by)
        .to_owned();
    let top_k = field.arg("topK").map(json_scalar).unwrap_or_default();
    let metric = embedding_metric(schema, &type_name, &similar_by);
    let distance = if metric == "dotproduct" {
        "math(1.0 - (v1 dot v2))".to_owned()
    } else if metric == "cosine" {
        "math(1.0 - ((v1 dot v2) / sqrt( (v1 dot v1) * (v2 dot v2) )))".to_owned()
    } else {
        "math(sqrt((v2 - v1) dot (v2 - v1)))".to_owned()
    };
    let mut queries = rewrite_as_get(schema, field, uid, xids, auth);
    let Some(last) = queries.last_mut() else {
        return queries;
    };
    "var".clone_into(&mut last.attr);
    let mut result = std::mem::take(&mut last.children);
    let mut vec_child = GraphQuery::new();
    vec_child.attr.clone_from(&pred);
    "vec".clone_into(&mut vec_child.var);
    last.children = vec![vec_child];
    let mut agg_child = GraphQuery::new();
    "v1".clone_into(&mut agg_child.var);
    "max(val(vec))".clone_into(&mut agg_child.attr);
    let mut agg_query = GraphQuery::new();
    "var()".clone_into(&mut agg_query.attr);
    agg_query.children = vec![agg_child];
    let mut similar_args = vec![plain_arg(&pred), plain_arg(&top_k), plain_arg("val(v1)")];
    if let Some(ef) = field.arg("ef") {
        similar_args.push(plain_arg(&format!("ef: {}", json_scalar(ef))));
    }
    if let Some(dt) = field.arg("distance_threshold") {
        similar_args.push(plain_arg(&format!(
            "distance_threshold: {}",
            json_scalar(dt)
        )));
    }
    let mut v2_child = GraphQuery::new();
    "v2".clone_into(&mut v2_child.var);
    v2_child.attr = pred;
    let mut distance_child = GraphQuery::new();
    "distance".clone_into(&mut distance_child.var);
    distance_child.attr = distance;
    let mut similar_query = GraphQuery::new();
    "var".clone_into(&mut similar_query.attr);
    similar_query.children = vec![v2_child, distance_child];
    similar_query.func = Some(Function {
        name: "similar_to".to_owned(),
        args: similar_args,
        ..Function::default()
    });
    let distance_alias = format!("{type_name}.vector_distance");
    if let Some(existing) = result
        .iter_mut()
        .find(|child| child.alias == distance_alias)
    {
        "val(distance)".clone_into(&mut existing.attr);
    } else {
        let mut extra = GraphQuery::new();
        extra.alias = distance_alias;
        "val(distance)".clone_into(&mut extra.attr);
        result.push(extra);
    }
    let mut sort_query = GraphQuery::new();
    sort_query.attr = field.datastreamdb_alias(true);
    sort_query.children = result;
    sort_query.func = Some(Function {
        name: "uid".to_owned(),
        args: vec![var_arg("distance")],
        ..Function::default()
    });
    sort_query.order = vec![order_by("val(distance)", false)];
    add_arguments_to_field(schema, &mut sort_query, field);
    queries.push(agg_query);
    queries.push(similar_query);
    queries.push(sort_query);
    queries
}

/// Rewrites a `querySimilarTByEmbedding` query.
fn rewrite_as_similar_by_embedding(
    schema: &BoundSchema,
    field: &BoundField,
    auth: &mut AuthRewriter<'_>,
) -> Vec<GraphQuery> {
    let mut queries = rewrite_as_query(schema, field, auth);
    let Some(first) = queries.first_mut() else {
        return queries;
    };
    let mut result = std::mem::take(&mut first.children);
    let type_name = field.type_name().to_owned();
    let similar_by = field
        .arg("by")
        .and_then(|val| val.as_str())
        .unwrap_or_default()
        .to_owned();
    let pred = schema
        .predicate(&type_name, &similar_by)
        .unwrap_or(&similar_by)
        .to_owned();
    let top_k = field.arg("topK").map(json_scalar).unwrap_or_default();
    let vec_json = field
        .arg("vector")
        .map(ToString::to_string)
        .unwrap_or_default();
    let metric = embedding_metric(schema, &type_name, &similar_by);
    let distance = if metric == "dotproduct" {
        "math(1.0 - (($search_vector) dot v2))".to_owned()
    } else if metric == "cosine" {
        "math(1.0 - ((($search_vector) dot v2) / sqrt( (($search_vector) dot ($search_vector)) * (v2 dot v2) )))".to_owned()
    } else {
        "math(sqrt((v2 - $search_vector) dot (v2 - $search_vector)))".to_owned()
    };
    let first = &mut queries[0];
    first.args.insert(
        "$search_vector".to_owned(),
        format!(" float32vector = \"{vec_json}\""),
    );
    let this_filter = FilterTree {
        op: String::new(),
        child: Vec::new(),
        func: first.func.clone(),
    };
    add_to_filter_tree(first, this_filter);
    "var".clone_into(&mut first.attr);
    let mut similar_args = vec![
        plain_arg(&pred),
        plain_arg(&top_k),
        plain_arg("$search_vector"),
    ];
    if let Some(ef) = field.arg("ef") {
        similar_args.push(plain_arg(&format!("ef: {}", json_scalar(ef))));
    }
    if let Some(dt) = field.arg("distance_threshold") {
        similar_args.push(plain_arg(&format!(
            "distance_threshold: {}",
            json_scalar(dt)
        )));
    }
    first.func = Some(Function {
        name: "similar_to".to_owned(),
        args: similar_args,
        ..Function::default()
    });
    let mut v2_child = GraphQuery::new();
    "v2".clone_into(&mut v2_child.var);
    v2_child.attr = pred;
    let mut distance_child = GraphQuery::new();
    "distance".clone_into(&mut distance_child.var);
    distance_child.attr = distance;
    first.children = vec![v2_child, distance_child];
    let distance_alias = format!("{type_name}.vector_distance");
    if let Some(existing) = result
        .iter_mut()
        .find(|child| child.alias == distance_alias)
    {
        "val(distance)".clone_into(&mut existing.attr);
    } else {
        let mut extra = GraphQuery::new();
        extra.alias = distance_alias;
        "val(distance)".clone_into(&mut extra.attr);
        result.push(extra);
    }
    let mut sort_query = GraphQuery::new();
    sort_query.attr = field.datastreamdb_alias(true);
    sort_query.children = result;
    sort_query.func = Some(Function {
        name: "uid".to_owned(),
        args: vec![var_arg("distance")],
        ..Function::default()
    });
    sort_query.order = vec![order_by("val(distance)", false)];
    queries.push(sort_query);
    queries
}
