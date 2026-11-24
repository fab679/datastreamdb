//! Mutation execution: existence queries, conditions, RDF application,
//! and payload selection. Client-side upsert evaluation for the embedded
//! engine (which has no server-side upsert path).

#![warn(missing_docs)]

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_graphql::Error as GqlExecError;
use async_graphql::dynamic::{FieldFuture, FieldValue, ResolverContext};
use datastreamdb_dql::GraphQuery;
use serde_json::{Map as JsonMap, Value as Json};

use crate::auth_rules::AuthVars;
use crate::dql_render::as_string;
use crate::op::{BoundField, BoundOperation, MutationType};
use crate::resolve::{JsonNode, Shared};
use crate::rewrite_mutation::{
    MutationRewriter, add_rewrite, add_rewrite_queries, delete_rewrite, evaluate_condition,
    from_mutation_result, mutated_root_uids, update_rewrite, update_rewrite_queries,
};

/// Root mutation resolver: full upsert flow plus payload selection.
pub(crate) fn resolve_root_mutation(ctx: ResolverContext<'_>) -> FieldFuture<'_> {
    let field_name = ctx.field().name().to_owned();
    let alias = ctx.field().alias().map(str::to_owned);
    FieldFuture::new(async move {
        let shared = ctx.data::<Arc<Shared>>()?.clone();
        let op = ctx.data::<BoundOperation>()?.clone();
        let auth_vars = ctx.data::<AuthVars>()?.clone();
        let key = alias.as_deref().unwrap_or(&field_name);
        let Some(root) = op.roots.iter().find(|root| root.alias == key) else {
            return Err(GqlExecError::new(format!(
                "unknown root field \"{field_name}\""
            )));
        };
        if root.mutation_type == MutationType::HttpMutation {
            return Err(GqlExecError::new(format!(
                "custom mutation \"{}\" is not supported in embedded mode",
                root.name
            )));
        }
        let payload = execute_mutation(&shared, root, &auth_vars)?;
        let type_name = root.type_name().to_owned();
        // Payload types are concrete: no `with_type` (it breaks downcasts).
        Ok(Some(FieldValue::owned_any(JsonNode {
            value: payload,
            type_name,
        })))
    })
}

/// Executes one root mutation and returns its payload JSON.
fn execute_mutation(
    shared: &Shared,
    root: &BoundField,
    auth_vars: &AuthVars,
) -> Result<Json, GqlExecError> {
    let mut rewriter = MutationRewriter::new();
    match root.mutation_type {
        MutationType::Add => execute_add(shared, root, &mut rewriter, auth_vars),
        MutationType::Update => execute_update(shared, root, &mut rewriter, auth_vars),
        MutationType::Delete => execute_delete(shared, root, auth_vars),
        MutationType::HttpMutation | MutationType::NotSupported => Err(GqlExecError::new(format!(
            "unsupported mutation \"{}\"",
            root.name
        ))),
    }
}

/// Executes an add mutation.
fn execute_add(
    shared: &Shared,
    root: &BoundField,
    rewriter: &mut MutationRewriter,
    auth_vars: &AuthVars,
) -> Result<Json, GqlExecError> {
    let (queries, _types, errs) = add_rewrite_queries(&shared.bound, root, rewriter);
    check_errors(errs)?;
    let (mut id_existence, existence_json) = {
        let (existence, _) = run_parsed(shared, &queries)?;
        let id_existence = existence_uids(&existence, &queries);
        let existence_json = existence_results(&existence, &queries);
        (id_existence, existence_json)
    };
    let (upserts, errs) = add_rewrite(&shared.bound, root, rewriter, &mut id_existence, auth_vars);
    check_errors(errs)?;
    let mut assigned: HashMap<String, u64> = HashMap::new();
    let mut frag_uids = Vec::new();
    let mut check_map = JsonMap::new();
    for upsert in &upserts {
        let vars = {
            let (result, _) = run_parsed(shared, &upsert.queries)?;
            result_vars(&result, &upsert.queries)
        };
        let counts = var_counts(&vars);
        if !upsert
            .conditions
            .iter()
            .all(|cond| evaluate_condition(cond, &counts))
        {
            continue;
        }
        check_map.extend(check_rows(&vars));
        let rendered_set = render_blocks(&upsert.set_nquads, &vars);
        let rendered_del = render_blocks(&upsert.del_nquads, &vars);
        let step = mutate_blocks(shared, &rendered_set, &rendered_del)?;
        assigned.extend(step);
        frag_uids.extend(upsert.root_uids.iter().cloned());
    }
    check_fragments(&rewriter.add_frags, &check_map)?;
    let mut new_nodes = HashMap::new();
    for upsert in &upserts {
        new_nodes.extend(
            upsert
                .new_nodes
                .iter()
                .map(|(key, val)| (key.clone(), val.clone())),
        );
    }
    authorize_new_nodes(shared, &new_nodes, &assigned, auth_vars)?;
    let assigned_str: HashMap<String, String> = assigned
        .iter()
        .map(|(key, uid)| (key.clone(), format!("{uid:#x}")))
        .collect();
    let uids = mutated_root_uids(&existence_json, "", &assigned_str, &frag_uids);
    finish_payload(shared, root, auth_vars, &uids)
}

/// Executes an update mutation.
fn execute_update(
    shared: &Shared,
    root: &BoundField,
    rewriter: &mut MutationRewriter,
    auth_vars: &AuthVars,
) -> Result<Json, GqlExecError> {
    let (queries, _types, errs) = update_rewrite_queries(&shared.bound, root, rewriter);
    check_errors(errs)?;
    let mut id_existence = {
        let (existence, _) = run_parsed(shared, &queries)?;
        existence_uids(&existence, &queries)
    };
    let (upserts, errs) =
        update_rewrite(&shared.bound, root, rewriter, &mut id_existence, auth_vars);
    check_errors(errs)?;
    let mut assigned: HashMap<String, u64> = HashMap::new();
    let mut target_uids = Vec::new();
    for upsert in &upserts {
        let vars = {
            let (result, _) = run_parsed(shared, &upsert.queries)?;
            result_vars(&result, &upsert.queries)
        };
        let counts = var_counts(&vars);
        if !upsert
            .conditions
            .iter()
            .all(|cond| evaluate_condition(cond, &counts))
        {
            continue;
        }
        let x_uids = vars.get("x").cloned().unwrap_or_default();
        let rendered_set = render_blocks(&upsert.set_nquads, &vars);
        let rendered_del = render_blocks(&upsert.del_nquads, &vars);
        let step = mutate_blocks(shared, &rendered_set, &rendered_del)?;
        assigned.extend(step);
        target_uids.extend(x_uids);
        for blank in upsert.new_nodes.keys() {
            if let Some(uid) = assigned.get(blank) {
                target_uids.push(format!("{uid:#x}"));
            }
        }
    }
    if let Some(frag) = rewriter.set_frag.as_ref() {
        let map = check_map_for(&upserts, shared)?;
        frag.check
            .check(&map)
            .map_err(|err| GqlExecError::new(err.message))?;
    }
    if let Some(frag) = rewriter.del_frag.as_ref() {
        let map = check_map_for(&upserts, shared)?;
        frag.check
            .check(&map)
            .map_err(|err| GqlExecError::new(err.message))?;
    }
    let mut new_nodes = HashMap::new();
    for upsert in &upserts {
        new_nodes.extend(
            upsert
                .new_nodes
                .iter()
                .map(|(key, val)| (key.clone(), val.clone())),
        );
    }
    authorize_new_nodes(shared, &new_nodes, &assigned, auth_vars)?;
    finish_payload(shared, root, auth_vars, &target_uids)
}

/// Executes a delete mutation with schema-driven node deletion.
fn execute_delete(
    shared: &Shared,
    root: &BoundField,
    auth_vars: &AuthVars,
) -> Result<Json, GqlExecError> {
    let mut rewriter = MutationRewriter::new();
    let upserts = delete_rewrite(&shared.bound, root, &mut rewriter, auth_vars)
        .map_err(|err| GqlExecError::new(err.message))?;
    let mut num_uids = 0usize;
    for upsert in &upserts {
        let vars = {
            let (result, _) = run_parsed(shared, &upsert.queries)?;
            result_vars(&result, &upsert.queries)
        };
        let x_uids = vars.get("x").cloned().unwrap_or_default();
        num_uids = x_uids.len();
        let rendered_del = render_blocks(&upsert.del_nquads, &vars);
        mutate_blocks(shared, "", &rendered_del)?;
    }
    Ok(finish_delete_payload(num_uids))
}

/// Applies set/delete RDF blocks through the engine.
fn mutate_blocks(
    shared: &Shared,
    set: &str,
    del: &str,
) -> Result<HashMap<String, u64>, GqlExecError> {
    if set.trim().is_empty() && del.trim().is_empty() {
        return Ok(HashMap::new());
    }
    let text = format!("{{\nset {{\n{set}\n}}\ndelete {{\n{del}\n}}\n}}");
    shared
        .engine
        .mutate(shared.namespace, &text)
        .map_err(|err| GqlExecError::new(format!("mutation failed: {err}")))
}

/// Builds the check map for update fragments.
fn check_map_for(
    upserts: &[crate::rewrite_mutation::UpsertMutation],
    shared: &Shared,
) -> Result<JsonMap<String, Json>, GqlExecError> {
    let mut map = JsonMap::new();
    for upsert in upserts {
        let (result, _) = run_parsed(shared, &upsert.queries)?;
        map.extend(check_rows(&result_vars(&result, &upsert.queries)));
    }
    Ok(map)
}

/// Verifies new nodes pass their types' add auth rules.
///
/// Port of Go `authorizeNewNodes`. Embedded delta: mutations commit inline,
/// so a failure here cannot roll back already-applied triples (it still
/// fails the request instead of returning success).
fn authorize_new_nodes(
    shared: &Shared,
    new_nodes: &HashMap<String, String>,
    assigned: &HashMap<String, u64>,
    auth_vars: &AuthVars,
) -> Result<(), GqlExecError> {
    let mut by_type: HashMap<String, Vec<u64>> = HashMap::new();
    for (var, type_name) in new_nodes {
        let uid = assigned
            .get(var)
            .or_else(|| assigned.get(&format!("_:{var}")))
            .copied();
        if let Some(uid) = uid {
            by_type.entry(type_name.clone()).or_default().push(uid);
        }
    }
    if by_type.is_empty() {
        return Ok(());
    }
    let mut var_gen = crate::rewrite_vars::VariableGenerator::new();
    let mut auth = crate::rewrite_auth::AuthRewriter {
        auth_variables: auth_vars.clone(),
        is_writing_auth: false,
        filter_by_uid: false,
        selector: crate::rewrite_auth::AuthSelector::Add,
        var_gen: &mut var_gen,
        var_name: String::new(),
        parent_var_name: String::new(),
        has_auth_rules: true,
        has_cascade: false,
        schema: &shared.bound,
    };
    let mut created: Vec<String> = by_type.keys().cloned().collect();
    created.sort();
    let mut blocks = Vec::new();
    let mut needs_auth = Vec::new();
    for type_name in &created {
        let var_name = auth.var_gen.next(&shared.bound, type_name, ("", ""), false);
        auth.var_name.clone_from(&var_name);
        auth.parent_var_name = format!("{type_name}Root");
        let (auth_queries, auth_filter) = auth.rewrite_auth_queries(type_name);
        let rbac = auth.evaluate_static_rules(type_name);
        if rbac == crate::auth_rules::RuleResult::Negative {
            return Err(GqlExecError::new("authorization failed"));
        }
        if rbac == crate::auth_rules::RuleResult::Positive || auth_queries.is_empty() {
            continue;
        }
        let mut nodes = by_type[type_name].clone();
        nodes.sort_unstable();
        let mut typ_query = GraphQuery::new();
        typ_query.attr.clone_from(type_name);
        typ_query.func = Some(datastreamdb_dql::Function {
            name: "uid".to_owned(),
            args: vec![datastreamdb_dql::Arg {
                value: var_name.clone(),
                ..datastreamdb_dql::Arg::default()
            }],
            ..datastreamdb_dql::Function::default()
        });
        typ_query.filter = auth_filter.map(Box::new);
        let mut uid_child = GraphQuery::new();
        "uid".clone_into(&mut uid_child.attr);
        typ_query.children = vec![uid_child];
        let mut var_query = GraphQuery::new();
        var_query.var = var_name;
        "var".clone_into(&mut var_query.attr);
        var_query.func = Some(datastreamdb_dql::Function {
            name: "uid".to_owned(),
            uid: nodes,
            ..datastreamdb_dql::Function::default()
        });
        needs_auth.push(type_name.clone());
        blocks.push(typ_query);
        blocks.push(var_query);
        blocks.extend(auth_queries);
    }
    if needs_auth.is_empty() {
        return Ok(());
    }
    let (result, _) = run_parsed(shared, &blocks)?;
    for type_name in &needs_auth {
        let expected = by_type[type_name].len();
        let got = attr_uids(&result, &blocks, type_name).len();
        if got != expected {
            return Err(GqlExecError::new("authorization failed"));
        }
    }
    Ok(())
}

/// Runs fragment checks against results.
fn check_fragments(
    frags: &[crate::rewrite_object::MutationFragment],
    results: &JsonMap<String, Json>,
) -> Result<(), GqlExecError> {
    for frag in frags {
        frag.check
            .check(results)
            .map_err(|err| GqlExecError::new(err.message))?;
    }
    Ok(())
}

/// Converts rewrite errors into an execution error.
fn check_errors(errs: Vec<crate::errors::GraphQlError>) -> Result<(), GqlExecError> {
    if errs.is_empty() {
        Ok(())
    } else {
        let messages: Vec<String> = errs.into_iter().map(|err| err.message).collect();
        Err(GqlExecError::new(messages.join("; ")))
    }
}

/// Parses (with outer var needs) and executes DQL blocks.
fn run_parsed(
    shared: &Shared,
    queries: &[GraphQuery],
) -> Result<
    (
        datastreamdb_query::request::ExecutionResult,
        datastreamdb_query::subgraph::Latency,
    ),
    GqlExecError,
> {
    let request = datastreamdb_dql::Request {
        text: as_string(queries),
        variables: HashMap::new(),
    };
    let needs = unreferenced_defines(queries);
    let parsed = datastreamdb_dql::parse_with_need_vars(request, &needs)
        .map_err(|err| GqlExecError::new(format!("DQL parse failed: {err}")))?;
    let read_ts = shared.engine.posting().read_ts();
    shared
        .engine
        .query_parsed(shared.namespace, parsed.query, read_ts)
        .map_err(|err| GqlExecError::new(format!("DQL execution failed: {err}")))
}

/// Var defines never referenced in the blocks (delete targets, upsert roots).
fn unreferenced_defines(queries: &[GraphQuery]) -> Vec<String> {
    let mut defines = Vec::new();
    let mut refs = HashSet::new();
    for query in queries {
        collect_defines(query, &mut defines);
        collect_uid_refs(query, &mut refs);
    }
    defines
        .into_iter()
        .filter(|var| !refs.contains(var))
        .collect()
}

/// Collects all defined var names recursively.
fn collect_defines(query: &GraphQuery, out: &mut Vec<String>) {
    if !query.var.is_empty() {
        out.push(query.var.clone());
    }
    for child in &query.children {
        collect_defines(child, out);
    }
}

/// Collects `uid(var)` references recursively.
fn collect_uid_refs(query: &GraphQuery, out: &mut HashSet<String>) {
    if let Some(func) = &query.func {
        collect_func_refs(func, out);
    }
    if let Some(filter) = &query.filter {
        collect_filter_refs(filter, out);
    }
    for child in &query.children {
        collect_uid_refs(child, out);
    }
}

/// Collects `uid(var)` references in a function.
fn collect_func_refs(func: &datastreamdb_dql::Function, out: &mut HashSet<String>) {
    for arg in &func.args {
        if let Some(inner) = arg
            .value
            .strip_prefix("uid(")
            .and_then(|inner| inner.strip_suffix(')'))
        {
            out.insert(inner.to_owned());
        }
    }
}

/// Collects `uid(var)` references in a filter tree.
fn collect_filter_refs(filter: &datastreamdb_dql::FilterTree, out: &mut HashSet<String>) {
    if let Some(func) = &filter.func {
        collect_func_refs(func, out);
    }
    for child in &filter.child {
        collect_filter_refs(child, out);
    }
}

/// Var/attr name to UID bindings (hex) for executed blocks.
fn result_vars(
    result: &datastreamdb_query::request::ExecutionResult,
    queries: &[GraphQuery],
) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    let mut defines = Vec::new();
    for query in queries {
        collect_defines(query, &mut defines);
    }
    for var in defines {
        out.insert(var.clone(), hex_uids(&var_uids(result, &var)));
    }
    for (index, query) in queries.iter().enumerate() {
        if let Some(root) = result.subgraphs.get(index) {
            if let Some(dest) = &root.dest_uids {
                out.insert(query.attr.clone(), hex_uids(&dest.uids));
            }
        }
    }
    out
}

/// Var/attr name to row counts.
fn var_counts(vars: &HashMap<String, Vec<String>>) -> HashMap<String, usize> {
    vars.iter()
        .map(|(key, uids)| (key.clone(), uids.len()))
        .collect()
}

/// UIDs bound to a var anywhere in an execution result.
fn var_uids(result: &datastreamdb_query::request::ExecutionResult, var: &str) -> Vec<u64> {
    let mut out = Vec::new();
    for root in &result.subgraphs {
        collect_var_uids(root, var, &mut out);
    }
    out
}

/// Recursively collects UIDs for a var.
fn collect_var_uids(sg: &datastreamdb_query::SubGraph, var: &str, out: &mut Vec<u64>) {
    if sg.params.var == var {
        if let Some(dest) = &sg.dest_uids {
            out.extend(dest.uids.iter().copied());
        }
    }
    for child in &sg.children {
        collect_var_uids(child, var, out);
    }
}

/// Formats UIDs as hex strings.
fn hex_uids(uids: &[u64]) -> Vec<String> {
    uids.iter().map(|uid| format!("{uid:#x}")).collect()
}

/// Existence var to first UID (hex) for rewrite decisions.
fn existence_uids(
    result: &datastreamdb_query::request::ExecutionResult,
    queries: &[GraphQuery],
) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for query in queries {
        let name = if query.var.is_empty() {
            query.attr.clone()
        } else {
            query.var.clone()
        };
        if let Some(first) = var_uids(result, &name).first().copied() {
            out.insert(name, format!("{first:#x}"));
        } else if let Some(first) = attr_uids(result, queries, &query.attr).first().copied() {
            out.insert(name, format!("{first:#x}"));
        }
    }
    out
}

/// UIDs of a root block addressed by attribute name.
fn attr_uids(
    result: &datastreamdb_query::request::ExecutionResult,
    queries: &[GraphQuery],
    attr: &str,
) -> Vec<u64> {
    for (index, query) in queries.iter().enumerate() {
        if query.attr == attr {
            if let Some(root) = result.subgraphs.get(index) {
                if let Some(dest) = &root.dest_uids {
                    return dest.uids.clone();
                }
            }
        }
    }
    Vec::new()
}

/// Existence results as a JSON map (for `MutatedRootUIDs`).
fn existence_results(
    result: &datastreamdb_query::request::ExecutionResult,
    queries: &[GraphQuery],
) -> JsonMap<String, Json> {
    let mut out = JsonMap::new();
    for query in queries {
        let name = if query.var.is_empty() {
            query.attr.clone()
        } else {
            query.var.clone()
        };
        let rows: Vec<Json> = var_uids(result, &name)
            .into_iter()
            .map(|uid| {
                Json::Object(JsonMap::from_iter([(
                    "uid".to_owned(),
                    Json::String(format!("{uid:#x}")),
                )]))
            })
            .collect();
        if !rows.is_empty() {
            out.insert(name, Json::Array(rows));
        }
    }
    out
}

/// Check rows (`[{uid}]`) per var for fragment checks.
fn check_rows(vars: &HashMap<String, Vec<String>>) -> JsonMap<String, Json> {
    vars.iter()
        .map(|(var, uids)| {
            let rows: Vec<Json> = uids
                .iter()
                .map(|uid| {
                    Json::Object(JsonMap::from_iter([(
                        "uid".to_owned(),
                        Json::String(uid.clone()),
                    )]))
                })
                .collect();
            (var.clone(), Json::Array(rows))
        })
        .collect()
}

/// Renders RDF blocks with `uid(var)` resolved and multi-UID expansion.
fn render_blocks(blocks: &[String], vars: &HashMap<String, Vec<String>>) -> String {
    let mut out = String::new();
    for block in blocks {
        for line in block.lines() {
            for rendered in expand_triple(line, vars) {
                out.push_str(&rendered);
                out.push('\n');
            }
        }
    }
    out
}

/// Expands one triple over multi-UID vars (single-UID vars substitute).
fn expand_triple(triple: &str, vars: &HashMap<String, Vec<String>>) -> Vec<String> {
    let mut referenced: Vec<&String> = vars
        .keys()
        .filter(|var| triple.contains(&format!("uid({var})")))
        .collect();
    referenced.sort();
    if referenced.is_empty() {
        return vec![triple.to_owned()];
    }
    // Empty var: this triple is a no-op (matches Go upsert semantics).
    if referenced.iter().any(|var| vars[*var].is_empty()) {
        return Vec::new();
    }
    if let Some(multi) = referenced.iter().find(|var| vars[**var].len() > 1) {
        return vars[*multi]
            .iter()
            .map(|uid| substitute_all(triple, vars, Some((multi, uid))))
            .collect();
    }
    vec![substitute_all(triple, vars, None)]
}

/// Wraps a UID string as an RDF subject when needed.
fn rdf_uid(uid: &str) -> String {
    if uid.starts_with('<') || uid.starts_with("_:") || uid.starts_with("uid(") {
        uid.to_owned()
    } else {
        format!("<{uid}>")
    }
}

/// Substitutes single-UID vars (plus one forced multi-UID choice).
fn substitute_all(
    block: &str,
    vars: &HashMap<String, Vec<String>>,
    forced: Option<(&&String, &String)>,
) -> String {
    let mut out = block.to_owned();
    for (var, uids) in vars {
        if let Some((forced_var, forced_uid)) = forced {
            if var == *forced_var {
                out = out.replace(&format!("uid({var})"), &rdf_uid(forced_uid));
                continue;
            }
        }
        if uids.len() == 1 {
            out = out.replace(&format!("uid({var})"), &rdf_uid(&uids[0]));
        }
    }
    out
}

/// Runs the post-mutation selection and builds the payload JSON.
fn finish_payload(
    shared: &Shared,
    root: &BoundField,
    auth_vars: &AuthVars,
    uids: &[String],
) -> Result<Json, GqlExecError> {
    let parsed: Vec<u64> = uids
        .iter()
        .filter_map(|uid| {
            uid.strip_prefix("0x")
                .and_then(|hex| u64::from_str_radix(hex, 16).ok())
                .or_else(|| uid.parse::<u64>().ok())
        })
        .collect();
    let queries = from_mutation_result(&shared.bound, root, &parsed, auth_vars);
    let selected = {
        let (mut result, _) = run_parsed(shared, &queries)?;
        let bytes = datastreamdb_query::output_render::to_json(&mut result.subgraphs)
            .map_err(|err| GqlExecError::new(format!("DQL encoding failed: {err}")))?;
        serde_json::from_slice(&bytes).unwrap_or(Json::Null)
    };
    let payload_type = root.type_name().to_owned();
    let data_field = shared
        .bound
        .schema
        .type_def(&payload_type)
        .and_then(|def| def.fields.first().cloned())
        .map(|field| field.name)
        .unwrap_or_default();
    let mut payload = JsonMap::new();
    let values = selected.get(&data_field).cloned().unwrap_or(Json::Null);
    let values = match crate::rewrite_mutation::mutation_query_field(&shared.bound, root) {
        Some(query_field) => crate::resolve::normalize_root(&shared.bound, &query_field, &values),
        None => values,
    };
    payload.insert(data_field, values);
    payload.insert(
        "numUids".to_owned(),
        Json::Number((parsed.len() as u64).into()),
    );
    Ok(Json::Object(payload))
}

/// Builds a delete payload (`msg` plus count).
fn finish_delete_payload(num_uids: usize) -> Json {
    let mut payload = JsonMap::new();
    payload.insert(
        "msg".to_owned(),
        Json::String(format!("Deleted {num_uids} nodes")),
    );
    payload.insert("numUids".to_owned(), Json::Number((num_uids as u64).into()));
    Json::Object(payload)
}
