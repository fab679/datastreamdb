//! Top-level entry points: `Parse`, query/fragment/schema blocks, validation.

#![warn(missing_docs)]

use std::collections::HashMap;

use datastreamdb_lex::{ItemIterator, Lexer, StateFn};
use datastreamdb_proto::SchemaRequest;

use super::ast::{FragmentNode, GraphQuery, ParseResult, Request, Vars};
use super::directives::{parse_cascade, parse_groupby};
use super::filter::parse_filter;
use super::root::{get_root, godeep};
use super::state::{
    ITEM_AT, ITEM_COLON, ITEM_COMMA, ITEM_LEFT_CURL, ITEM_LEFT_ROUND, ITEM_LEFT_SQUARE, ITEM_NAME,
    ITEM_OP_TYPE, ITEM_RIGHT_CURL, ITEM_RIGHT_ROUND, ITEM_RIGHT_SQUARE, lex_top_level,
};
use super::vars::{
    VarMap, check_value_type, convert_to_var_map, parse_dql_variables, substitute_variables,
};
use crate::DqlError;

/// Parses a query request.
///
/// # Errors
///
/// Returns [`DqlError`] on malformed queries.
pub fn parse(request: Request) -> Result<ParseResult, DqlError> {
    parse_with_need_vars(request, &[])
}

/// Parses a query, marking outer variables (e.g. upsert mutations) as needed.
///
/// # Errors
///
/// Returns [`DqlError`] on malformed queries.
pub fn parse_with_need_vars(
    request: Request,
    need_vars: &[String],
) -> Result<ParseResult, DqlError> {
    let Request { text, variables } = request;
    let mut vmap = convert_to_var_map(&variables);

    let mut lexer = Lexer::default();
    lexer.reset(text);
    lexer.run(StateFn(lex_top_level));
    lexer.validate_result()?;

    let mut result = ParseResult::default();
    let mut it = lexer.new_iterator();
    let mut fmap: HashMap<String, FragmentNode> = HashMap::new();
    while it.advance() {
        let item = it.item();
        match item.typ {
            ITEM_OP_TYPE => match item.val.as_str() {
                "mutation" => {
                    return Err(item.errorf("Mutation block no longer allowed.").into());
                }
                "schema" => {
                    if result.schema.is_some() {
                        return Err(item.errorf("Only one schema block allowed ").into());
                    }
                    if !result.query.is_empty() {
                        return Err(item
                            .errorf("Schema block is not allowed with query block")
                            .into());
                    }
                    result.schema = Some(get_schema(&mut it)?);
                }
                // TODO(jchiu0): This is to be done in ParseSchema once it is ready.
                "fragment" => {
                    let node = get_fragment(&mut it)?;
                    fmap.insert(node.name.clone(), node);
                }
                "query" => {
                    if result.schema.is_some() {
                        return Err(item
                            .errorf("Schema block is not allowed with query block")
                            .into());
                    }
                    result
                        .query
                        .push(get_variables_and_query(&mut it, &mut vmap)?);
                }
                _ => {}
            },
            ITEM_LEFT_CURL => {
                result.query.push(get_query(&mut it)?);
            }
            ITEM_NAME => {
                it.prev();
                result.query.push(get_query(&mut it)?);
            }
            _ => {}
        }
    }

    if !result.query.is_empty() {
        for query in &mut result.query {
            // Expand fragments using the fragment map.
            query.expand_fragments(&fmap)?;
            // Substitute all DQL variables with corresponding values.
            substitute_variables(query, &vmap)?;
        }

        let mut all_vars: Vec<Vars> = Vec::with_capacity(result.query.len() + 1);
        for query in &result.query {
            // Collect vars used and defined.
            let mut vars = Vars::default();
            collect_vars(query, &mut vars);
            all_vars.push(vars);
        }
        all_vars.clone_into(&mut result.query_vars);

        // Add variables needed outside the query block (e.g. the mutation
        // block of an upsert uses query-block variables parsed here).
        if !need_vars.is_empty() {
            all_vars.push(Vars {
                needs: need_vars.to_vec(),
                defines: Vec::new(),
            });
        }
        check_dependency(&all_vars)?;
    }

    validate_result(&result)?;
    Ok(result)
}

fn validate_result(result: &ParseResult) -> Result<(), DqlError> {
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for query in &result.query {
        if query.alias == "var" || query.alias == "shortest" {
            continue;
        }
        if !seen.insert(query.alias.as_str()) {
            return Err(DqlError::message(format!(
                "Duplicate aliases not allowed: {}",
                query.alias
            )));
        }
    }
    Ok(())
}

fn flatten(vars: &[Vars]) -> (Vec<String>, Vec<String>) {
    let mut needs = Vec::new();
    let mut defines = Vec::new();
    for vars in vars {
        needs.extend(vars.needs.iter().cloned());
        defines.extend(vars.defines.iter().cloned());
    }
    (needs, defines)
}

fn check_dependency(vars: &[Vars]) -> Result<(), DqlError> {
    let (needs, defines) = flatten(vars);

    let needs = datastreamdb_x::remove_duplicates(needs);
    let before = defines.len();
    let defines = datastreamdb_x::remove_duplicates(defines);

    if defines.len() != before {
        return Err(DqlError::message(
            "Some variables are declared multiple times.",
        ));
    }
    let show = |items: &[String]| format!("[{}]", items.join(" "));
    if defines.len() > needs.len() {
        return Err(DqlError::message(format!(
            "Some variables are defined but not used\nDefined:{}\nUsed:{}\n",
            show(&defines),
            show(&needs)
        )));
    }
    if defines.len() < needs.len() {
        return Err(DqlError::message(format!(
            "Some variables are used but not defined\nDefined:{}\nUsed:{}\n",
            show(&defines),
            show(&needs)
        )));
    }
    for (define, need) in defines.iter().zip(needs.iter()) {
        if define != need {
            return Err(DqlError::message(format!(
                "Variables are not used properly. \nDefined:{}\nUsed:{}\n",
                show(&defines),
                show(&needs)
            )));
        }
    }
    Ok(())
}

/// Collects defined/needed variables in a query tree.
fn collect_vars(gq: &GraphQuery, vars: &mut Vars) {
    if !gq.var.is_empty() {
        vars.defines.push(gq.var.clone());
    }
    if !gq.facet_var.is_empty() {
        vars.defines.extend(gq.facet_var.values().cloned());
    }
    vars.needs
        .extend(gq.needs_var.iter().map(|v| v.name.clone()));
    for child in &gq.children {
        collect_vars(child, vars);
    }
    if let Some(filter) = gq.filter.as_ref() {
        collect_vars_filter(filter, vars);
    }
    if let Some(math) = gq.math_exp.as_ref() {
        collect_vars_math(math, vars);
    }
    if let Some(from) = gq.shortest_path_args.from.as_ref() {
        if let Some(first) = from.needs_var.first() {
            vars.needs.push(first.name.clone());
        }
    }
    if let Some(to) = gq.shortest_path_args.to.as_ref() {
        if let Some(first) = to.needs_var.first() {
            vars.needs.push(first.name.clone());
        }
    }
}

fn collect_vars_math(tree: &super::math::MathTree, vars: &mut Vars) {
    if !tree.var.is_empty() {
        vars.needs.push(tree.var.clone());
        return;
    }
    for child in &tree.child {
        collect_vars_math(child, vars);
    }
}

fn collect_vars_filter(filter: &super::ast::FilterTree, vars: &mut Vars) {
    if let Some(func) = filter.func.as_ref() {
        vars.needs
            .extend(func.needs_var.iter().map(|v| v.name.clone()));
    }
    for child in &filter.child {
        collect_vars_filter(child, vars);
    }
}

/// Parses `query Name($var: type, ...)` headers plus the query block.
fn get_variables_and_query(
    it: &mut ItemIterator,
    vmap: &mut VarMap,
) -> Result<GraphQuery, DqlError> {
    let mut name = String::new();
    loop {
        if !it.advance() {
            break;
        }
        let item = it.item();
        match item.typ {
            ITEM_NAME => {
                if !name.is_empty() {
                    return Err(item.errorf("Multiple word query name not allowed.").into());
                }
                item.val.clone_into(&mut name);
            }
            ITEM_LEFT_ROUND => {
                if name.is_empty() {
                    return Err(item
                        .errorf("Variables can be defined only in named queries.")
                        .into());
                }
                parse_dql_variables(it, vmap)?;
                check_value_type(vmap)?;
            }
            ITEM_LEFT_CURL => {
                return get_query(it);
            }
            _ => {}
        }
    }
    // Go returns a nil query here when the loop exhausts; the callers only
    // reach this on inputs the lexer already rejected, so error instead.
    Err(it.errorf("Invalid query").into())
}

/// Builds the query tree: root plus `{...}` / directives.
fn get_query(it: &mut ItemIterator) -> Result<GraphQuery, DqlError> {
    // First, get the root.
    let mut gq = get_root(it)?;

    let mut seen_filter = false;
    loop {
        // Recurse to deeper levels through godeep.
        if !it.advance() {
            return Err(it
                .errorf("Expecting more lexer items while parsing query")
                .into());
        }
        let item = it.item();
        match item.typ {
            ITEM_LEFT_CURL => {
                godeep(it, &mut gq)?;
                return Ok(gq);
            }
            ITEM_AT => {
                it.advance();
                let item = it.item();
                if item.typ != ITEM_NAME {
                    // Go falls out of the switch and returns gq.
                    return Ok(gq);
                }
                match item.val.to_ascii_lowercase().as_str() {
                    "filter" => {
                        if seen_filter {
                            return Err(item.errorf("Repeated filter at root").into());
                        }
                        seen_filter = true;
                        gq.filter = parse_filter(it)?.map(Box::new);
                    }
                    "normalize" => gq.normalize = true,
                    "cascade" => parse_cascade(it, &mut gq)?,
                    "groupby" => {
                        gq.is_groupby = true;
                        parse_groupby(it, &mut gq)?;
                    }
                    "ignorereflex" => gq.ignore_reflex = true,
                    "recurse" => {
                        gq.recurse = true;
                        super::directives::parse_recurse_args(it, &mut gq)?;
                    }
                    _ => {
                        return Err(item
                            .errorf(format!("Unknown directive [{}]", item.val))
                            .into());
                    }
                }
            }
            ITEM_RIGHT_CURL => {
                // Do nothing.
                return Ok(gq);
            }
            ITEM_NAME => {
                it.prev();
                return Ok(gq);
            }
            _ => {
                return Err(item
                    .errorf(format!("Malformed Query. Missing {{. Got {}", item.val))
                    .into());
            }
        }
    }
}

/// Parses a fragment definition (not a reference).
fn get_fragment(
    it: &mut ItemIterator,
    // (fragment collection lives in the caller)
) -> Result<super::ast::FragmentNode, DqlError> {
    let mut name = String::new();
    loop {
        if !it.advance() {
            break;
        }
        let item = it.item();
        match item.typ {
            ITEM_NAME => {
                let trimmed = item.val.trim();
                if !trimmed.is_empty() && name.is_empty() {
                    // The first nontrivial token is the fragment name;
                    // everything after is ignored until the left curl.
                    trimmed.clone_into(&mut name);
                }
            }
            ITEM_LEFT_CURL => break,
            _ => {
                return Err(item
                    .errorf(format!(
                        "Unexpected item in fragment: {} {}",
                        item.typ, item.val
                    ))
                    .into());
            }
        }
    }
    if name.is_empty() {
        return Err(it.errorf("Empty fragment name").into());
    }
    let mut gq = GraphQuery {
        args: HashMap::new(),
        ..GraphQuery::default()
    };
    godeep(it, &mut gq)?;
    Ok(super::ast::FragmentNode { name, gq })
}

/// Parses `[a, b]` item names (excluding the left square).
fn parse_list_item_names(it: &mut ItemIterator) -> Result<Vec<String>, DqlError> {
    let mut items = Vec::new();
    while it.advance() {
        let item = it.item();
        match item.typ {
            ITEM_RIGHT_SQUARE => return Ok(items),
            ITEM_NAME => {
                items.push(super::args::collect_name(it, &item.val));
            }
            ITEM_COMMA => {
                it.advance();
                let item = it.item();
                if item.typ != ITEM_NAME {
                    return Err(item.errorf("Invalid scheam block").into());
                }
                items.push(super::args::collect_name(it, &item.val));
            }
            _ => return Err(item.errorf("Invalid schema block").into()),
        }
    }
    Err(it
        .errorf("Expecting ] to end list but none was found")
        .into())
}

/// Parses `pred: ...` / `type: ...` schema selectors.
fn parse_schema_preds_or_types(
    it: &mut ItemIterator,
    schema: &mut SchemaRequest,
) -> Result<(), DqlError> {
    // pred or type should be followed by colon.
    it.advance();
    let item = it.item();
    // NOTE: Go's `&&` here is dead (`item.Val == ...` implies `itemName`);
    // the effective check is the value comparison.
    if item.typ != ITEM_NAME && !(item.val == "pred" || item.val == "type") {
        return Err(item.errorf("Invalid schema block").into());
    }
    let parse_types = item.val == "type";

    it.advance();
    let item = it.item();
    if item.typ != ITEM_COLON {
        return Err(item.errorf("Invalid schema block").into());
    }

    // Can be `a` or `[a, b]`.
    it.advance();
    let item = it.item();
    match item.typ {
        ITEM_NAME => {
            if parse_types {
                schema.types.push(item.val.clone());
            } else {
                schema.predicates.push(item.val.clone());
            }
        }
        ITEM_LEFT_SQUARE => {
            let names = parse_list_item_names(it)?;
            if parse_types {
                schema.types = names;
            } else {
                schema.predicates = names;
            }
        }
        _ => return Err(item.errorf("Invalid schema block").into()),
    }

    it.advance();
    let item = it.item();
    if item.typ == ITEM_RIGHT_ROUND {
        return Ok(());
    }
    Err(item.errorf("Invalid schema blocks").into())
}

/// Parses schema `{ field ... }` bodies.
fn parse_schema_fields(it: &mut ItemIterator, schema: &mut SchemaRequest) -> Result<(), DqlError> {
    while it.advance() {
        let item = it.item();
        match item.typ {
            ITEM_RIGHT_CURL => return Ok(()),
            ITEM_NAME => schema.fields.push(item.val.clone()),
            _ => return Err(item.errorf("Invalid schema block.").into()),
        }
    }
    Err(it
        .errorf("Expecting } to end fields list, but none was found")
        .into())
}

fn get_schema(it: &mut ItemIterator) -> Result<SchemaRequest, DqlError> {
    let mut schema = SchemaRequest::default();
    let mut left_round_seen = false;
    while it.advance() {
        let item = it.item();
        match item.typ {
            ITEM_LEFT_CURL => {
                parse_schema_fields(it, &mut schema)?;
                return Ok(schema);
            }
            ITEM_LEFT_ROUND => {
                if left_round_seen {
                    return Err(item.errorf("Too many left rounds in schema block").into());
                }
                left_round_seen = true;
                parse_schema_preds_or_types(it, &mut schema)?;
            }
            _ => return Err(item.errorf("Invalid schema block").into()),
        }
    }
    Err(it.errorf("Invalid schema block.").into())
}
