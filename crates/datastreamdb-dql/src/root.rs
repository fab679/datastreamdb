//! Root blocks, nesting (`godeep`), fragments, and schema blocks.

#![warn(missing_docs)]

use datastreamdb_lex::ItemIterator;
use datastreamdb_proto::Order;
use datastreamdb_types::strconv::parse_go_uint_base0;

use super::args::{
    attr_and_lang, collect_name, is_sort_key, parse_arguments, parse_type_list, parse_var_list,
    valid_key, valid_key_at_root,
};
use super::ast::{
    COUNT_FUNC, Function, GraphQuery, LIST_VAR, UID_FUNC, VALUE_FUNC, VALUE_VAR, is_aggregator,
    is_expand_func, is_math_block,
};
use super::directives::{parse_directive, parse_language_list};
use super::funcs::{parse_function, valid_func_name};
use super::math::parse_math_func;
use super::state::{
    ITEM_AT, ITEM_COLON, ITEM_COMMA, ITEM_DOLLAR, ITEM_LEFT_CURL, ITEM_LEFT_ROUND, ITEM_MATH_OP,
    ITEM_NAME, ITEM_PERIOD, ITEM_RIGHT_CURL, ITEM_RIGHT_ROUND,
};
use crate::DqlError;

/// Expand-compat error (Go's `errExpandType`).
pub(crate) const EXPAND_TYPE_ERROR: &str = "expand is only compatible with type filters";

/// Parses ID lists (`1`, `[1, 2]`) for DQL variables.
pub(crate) fn parse_id(val: &str) -> Result<Vec<u64>, DqlError> {
    let val = datastreamdb_x::strip_space_tab(val);
    if val.is_empty() {
        return Err(DqlError::message("ID can't be empty"));
    }
    if !val.starts_with('[') {
        let uid = parse_go_uint_base0(&val).map_err(|e| DqlError::message(e.to_string()))?;
        return Ok(vec![uid]);
    }
    if !val.ends_with(']') {
        return Err(DqlError::message(format!(
            "Invalid id list at root. Got: {val}"
        )));
    }
    let mut uids = Vec::new();
    let mut buf = String::new();
    for c in val[1..].chars() {
        if c == ',' || c == ']' {
            if buf.is_empty() {
                continue;
            }
            let uid = parse_go_uint_base0(&buf).map_err(|e| DqlError::message(e.to_string()))?;
            uids.push(uid);
            buf.clear();
            continue;
        }
        if c == '[' || c == ')' {
            return Err(DqlError::message(format!(
                "Invalid id list at root. Got: {val}"
            )));
        }
        buf.push(c);
    }
    Ok(uids)
}

/// Creates the root block by parsing `(...)` arguments.
#[allow(clippy::too_many_lines, reason = "exact port of Go getRoot")]
pub(crate) fn get_root(it: &mut ItemIterator) -> Result<GraphQuery, DqlError> {
    let mut gq = GraphQuery {
        args: std::collections::HashMap::new(),
        ..GraphQuery::default()
    };
    if !it.advance() {
        return Err(it.errorf("Invalid query").into());
    }
    let item = it.item();
    if item.typ != ITEM_NAME {
        return Err(item
            .errorf(format!("Expected some name. Got: {item}"))
            .into());
    }

    let peeked = it
        .peek(1)
        .map_err(|_| DqlError::message(it.errorf("Invalid Query").to_string()))?;
    if peeked[0].typ == ITEM_NAME && peeked[0].val.eq_ignore_ascii_case("as") {
        item.val.clone_into(&mut gq.var);
        it.advance(); // Consume the "AS".
        it.advance();
    }
    // Re-read: either the var name was consumed or `item` still holds it.
    let item = it.item();
    item.val.clone_into(&mut gq.alias);
    if !it.advance() {
        return Err(it.errorf("Invalid query").into());
    }
    let item = it.item();
    if item.typ != ITEM_LEFT_ROUND {
        return Err(item
            .errorf(format!("Expected Left round brackets. Got: {item}"))
            .into());
    }

    let mut expect_arg = true;
    let mut order: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Parse in KV fashion. Depending on the key, decide the path.
    loop {
        if !it.advance() {
            break;
        }
        let key: String;
        // Get key.
        let item = it.item();
        match item.typ {
            ITEM_NAME => {
                if !expect_arg {
                    return Err(item
                        .errorf(format!("Not expecting argument. Got: {item}"))
                        .into());
                }
                key = item.val.clone();
                expect_arg = false;
            }
            ITEM_RIGHT_ROUND => {
                if is_empty(&gq) {
                    // Aggregation at root, fetched in another block.
                    gq.is_empty = true;
                }
                break;
            }
            ITEM_COMMA => {
                if expect_arg {
                    return Err(item.errorf("Expected Argument but got comma.").into());
                }
                expect_arg = true;
                continue;
            }
            _ => {
                return Err(item
                    .errorf(format!("Expecting argument name. Got: {item}"))
                    .into());
            }
        }

        if !valid_key_at_root(&key) {
            return Err(item
                .errorf(format!("Got invalid keyword: {key} at root"))
                .into());
        }

        if !it.advance() {
            return Err(it.errorf("Invalid query").into());
        }
        let item = it.item();
        if item.typ != ITEM_COLON {
            return Err(item
                .errorf(format!("Expecting a colon. Got: {item}"))
                .into());
        }

        match key.as_str() {
            "func" => {
                // Store the generator function.
                if gq.func.is_some() {
                    return Err(item.errorf("Only one function allowed at root").into());
                }
                let parsed_f = parse_function(it, Some(&mut gq))?;
                if !valid_func_name(&parsed_f.name) {
                    return Err(item
                        .errorf(format!("Function name: {} is not valid.", parsed_f.name))
                        .into());
                }
                let needs = parsed_f.needs_var.clone();
                gq.func = Some(parsed_f);
                gq.needs_var.extend(needs);
            }
            "from" | "to" => {
                parse_root_endpoint(it, &mut gq, &key, &item)?;
            }
            _ => {
                parse_root_kv(it, &mut gq, &key, &mut order)?;
            }
        }
    }
    Ok(gq)
}

fn assign_shortest_path_fn(gq: &mut GraphQuery, func: Function, key: &str) {
    match key {
        "from" => gq.shortest_path_args.from = Some(func),
        _ => gq.shortest_path_args.to = Some(func),
    }
}

/// Parses a `from`/`to` shortest-path endpoint (UID or `uid()` function).
fn parse_root_endpoint(
    it: &mut ItemIterator,
    gq: &mut GraphQuery,
    key: &str,
    item: &datastreamdb_lex::Item,
) -> Result<(), DqlError> {
    if gq.alias != "shortest" {
        return Err(item
            .errorf("from/to only allowed for shortest path queries")
            .into());
    }
    let mut func = Function::default();
    let peeked = it
        .peek(1)
        .map_err(|_| DqlError::message(it.errorf("Invalid query").to_string()))?;
    if peeked[0].val == UID_FUNC {
        let parsed_f = parse_function(it, Some(gq))?;
        func.needs_var = parsed_f.needs_var;
        func.name = parsed_f.name;
        assign_shortest_path_fn(gq, func, key);
        return Ok(());
    }
    // Not a uid function: an actual uid.
    it.advance();
    let item = it.item();
    let val = collect_name(it, &item.val);
    match parse_go_uint_base0(&val) {
        Ok(uid) => func.uid.push(uid),
        Err(e) => {
            if matches!(e, datastreamdb_types::strconv::ParseUintError::Range { .. }) {
                return Err(item
                    .errorf(format!("The uid value {val:?} is too large."))
                    .into());
            }
            return Err(item
                .errorf(format!(
                    "from/to in shortest path can only accept uid function or an uid. Got: {val}"
                ))
                .into());
        }
    }
    assign_shortest_path_fn(gq, func, key);
    Ok(())
}

/// Parses one non-func root `key: value` pair.
fn parse_root_kv(
    it: &mut ItemIterator,
    gq: &mut GraphQuery,
    key: &str,
    order: &mut std::collections::HashSet<String>,
) -> Result<(), DqlError> {
    let mut val = String::new();
    if !it.advance() {
        return Err(it.errorf("Invalid query").into());
    }
    let mut item = it.item();

    if item.typ == ITEM_DOLLAR {
        it.advance();
        item = it.item();
        if item.typ == ITEM_NAME {
            val = format!("${}", item.val);
        } else {
            return Err(item
                .errorf(format!("Expecting a variable name. Got: {item}"))
                .into());
        }
        return assign_root_arg(it, gq, key, val);
    }
    if item.typ == ITEM_MATH_OP {
        if item.val != "+" && item.val != "-" {
            return Err(item
                .errorf(format!(
                    "Only Plus and minus are allowed unary ops. Got: {}",
                    item.val
                ))
                .into());
        }
        val.clone_from(&item.val);
        it.advance();
        item = it.item();
    }

    if val.is_empty() && item.val == VALUE_FUNC {
        let count = parse_var_list(it, gq)?;
        if count != 1 {
            return Err(item
                .errorf(format!("Expected only one variable but got: {count}"))
                .into());
        }
        // Modify the NeedsVar context here.
        // NOTE: unlike parseArguments, val stays empty and is filled from
        // NeedsVar below (which also enforces single-sort).
        if let Some(last) = gq.needs_var.last_mut() {
            last.typ = VALUE_VAR;
        }
    } else {
        val = collect_name(it, &(val + &item.val));
        // Get language list, if present.
        if let Ok(items) = it.peek(1) {
            if items[0].typ == ITEM_LEFT_ROUND
                && (key == "orderasc" || key == "orderdesc")
                && val != VALUE_FUNC
            {
                return Err(it
                    .errorf(format!("Expected val(). Got {val}() with order."))
                    .into());
            }
            if items[0].typ == ITEM_AT {
                it.advance(); // consume '@'
                it.advance(); // move forward
                let langs = parse_language_list(it)?;
                val = format!("{val}@{}", langs.join(":"));
            }
        }
    }

    // TODO - Allow only order by one of variable/predicate for now.
    if val.is_empty() {
        // Only happens for `orderasc: val(c)` style.
        let Some(last) = gq.needs_var.last() else {
            return Err(it
                .errorf("unable to get value when parsing key value pairs")
                .into());
        };
        val.clone_from(&last.name);
        // Only one sort by a variable for now.
        if !gq.order.is_empty() && is_sort_key(key) {
            return Err(it
                .errorf(format!(
                    "Multiple sorting only allowed by predicates. Got: {val}"
                ))
                .into());
        }
    }
    if is_sort_key(key) {
        return push_root_order(it, gq, key, &val, order);
    }

    assign_root_arg(it, gq, key, val)
}

/// Records one root ordering, enforcing the single-sort rules.
fn push_root_order(
    it: &mut ItemIterator,
    gq: &mut GraphQuery,
    key: &str,
    val: &str,
    order: &mut std::collections::HashSet<String>,
) -> Result<(), DqlError> {
    // Variable + predicate sorting is unsupported.
    for o in &gq.order {
        for need in &gq.needs_var {
            if need.name == o.attr {
                return Err(it
                    .errorf(format!(
                        "Val() is not allowed in multiple sorting. Got: [{}]",
                        need.name
                    ))
                    .into());
            }
        }
    }
    if !order.insert(val.to_owned()) {
        return Err(it
            .errorf(format!(
                "Sorting by an attribute: [{val}] can only be done once"
            ))
            .into());
    }
    let (attr, langs) = attr_and_lang(val);
    if langs.len() > 1 {
        return Err(it
            .errorf(format!(
                "Sorting by an attribute: [{val}] can only be done on one language"
            ))
            .into());
    }
    gq.order.push(Order {
        attr: attr.to_owned(),
        desc: key == "orderdesc",
        langs,
    });
    Ok(())
}

fn assign_root_arg(
    it: &mut ItemIterator,
    gq: &mut GraphQuery,
    key: &str,
    val: String,
) -> Result<(), DqlError> {
    if gq.args.contains_key(key) {
        return Err(it.errorf(format!("Repeated key {key:?} at root")).into());
    }
    gq.args.insert(key.to_owned(), val);
    Ok(())
}

/// Whether the block has no starting function, vars, or args.
fn is_empty(gq: &GraphQuery) -> bool {
    gq.func.is_none()
        && gq.needs_var.is_empty()
        && gq.args.is_empty()
        && gq.shortest_path_args.from.is_none()
        && gq.shortest_path_args.to.is_none()
}

/// `count` keyword state inside `godeep`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CountState {
    /// Default value.
    NotSeen,
    /// Saw the `count` keyword.
    Seen,
    /// Saw a predicate within `count`.
    SeenWithPred,
}

/// Tracks which node directives/children attach to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cursor {
    /// The block itself.
    Root,
    /// The most recently appended child.
    Child,
    /// No node (internal blocks that take no children).
    Nil,
}

/// Resolves the cursor to a mutable node.
fn cursor_node(gq: &mut GraphQuery, cursor: Cursor) -> Option<&mut GraphQuery> {
    match cursor {
        Cursor::Root => Some(gq),
        Cursor::Child => gq.children.last_mut(),
        Cursor::Nil => None,
    }
}

/// Parses `(...)` child arguments into the target block.
fn apply_child_args(it: &mut ItemIterator, target: &mut GraphQuery) -> Result<(), DqlError> {
    let args = parse_arguments(it, target)?;
    // Stores args in GraphQuery, used later while retrieving results.
    let mut order: std::collections::HashSet<String> = std::collections::HashSet::new();
    for pair in args {
        if !valid_key(&pair.key) {
            return Err(it
                .errorf(format!("Got invalid keyword: {}", pair.key))
                .into());
        }
        if target.args.contains_key(&pair.key) {
            return Err(it
                .errorf(format!(
                    "Got repeated key {:?} at level {:?}",
                    pair.key, target.attr
                ))
                .into());
        }
        if pair.val.is_empty() {
            return Err(it.errorf("Got empty argument").into());
        }
        if pair.key == "orderasc" || pair.key == "orderdesc" {
            if !order.insert(pair.val.clone()) {
                return Err(it
                    .errorf(format!(
                        "Sorting by an attribute: [{}] can only be done once",
                        pair.val
                    ))
                    .into());
            }
            let (attr, langs) = attr_and_lang(&pair.val);
            if langs.len() > 1 {
                return Err(it
                    .errorf(format!(
                        "Sorting by an attribute: [{}] can only be done on one language",
                        pair.val
                    ))
                    .into());
            }
            target.order.push(Order {
                attr: attr.to_owned(),
                desc: pair.key == "orderdesc",
                langs,
            });
            continue;
        }
        target.args.insert(pair.key, pair.val);
    }
    Ok(())
}

/// Validates aggregation-only block items (`math`/`min`/..., not predicates).
fn validate_empty_block_item(it: &mut ItemIterator, val: &str) -> Result<(), DqlError> {
    let save = it.save();
    let result = validate_empty_block_item_inner(it, val);
    it.restore(save);
    result
}

fn validate_empty_block_item_inner(it: &mut ItemIterator, val: &str) -> Result<(), DqlError> {
    let mut fname = val.to_owned();
    // Could have alias so peek forward to get actual function name.
    if super::args::try_skip_item_type(it, ITEM_COLON) {
        let Some(item) = super::args::try_parse_item_type(it, ITEM_NAME) else {
            let (peeked, _) = it.peek_one();
            return Err(peeked
                .errorf(format!("Expected name. Got: {}", peeked.val))
                .into());
        };
        item.val.clone_into(&mut fname);
    }
    let skipped = super::args::try_skip_item_type(it, ITEM_LEFT_ROUND);
    if !skipped || (!is_math_block(&fname) && !is_aggregator(&fname)) {
        return Err(it
            .errorf(format!(
                "Only aggregation/math functions allowed inside empty blocks. Got: {fname}"
            ))
            .into());
    }
    Ok(())
}

/// Parses a `...fragment` spread (or errors on a short dot run).
fn parse_fragment_spread(
    it: &mut ItemIterator,
    gq: &mut GraphQuery,
    item: &datastreamdb_lex::Item,
) -> Result<(), DqlError> {
    // Looking for `...`.
    let mut dots = 1;
    for _ in 0..2 {
        if it.advance() && it.item().typ == ITEM_PERIOD {
            dots += 1;
        }
    }
    if dots == 3 {
        it.advance();
        let item = it.item();
        if item.typ == ITEM_NAME {
            // Unlike itemName, there is no nesting: keep cursor.
            gq.children.push(GraphQuery {
                fragment: item.val.clone(),
                ..GraphQuery::default()
            });
        }
    } else {
        return Err(item
            .errorf(format!("Expected 3 periods (\"...\"), got {dots}."))
            .into());
    }
    Ok(())
}

/// Builds the child subgraph under `gq`.
pub(crate) fn godeep(it: &mut ItemIterator, gq: &mut GraphQuery) -> Result<(), DqlError> {
    let mut count = CountState::NotSeen;
    let mut alias = String::new();
    let mut var_name = String::new();
    // Tracks the current node for nesting.
    let mut cursor = Cursor::Root;
    while it.advance() {
        let item = it.item();
        match item.typ {
            datastreamdb_lex::ITEM_EOF | ITEM_RIGHT_CURL => return Ok(()),
            ITEM_PERIOD => {
                parse_fragment_spread(it, gq, &item)?;
            }
            ITEM_NAME => {
                let peeked = it
                    .peek(1)
                    .map_err(|_| DqlError::message(item.errorf("Invalid query").to_string()))?;
                if peeked[0].typ == ITEM_NAME && peeked[0].val.eq_ignore_ascii_case("as") {
                    item.val.clone_into(&mut var_name);
                    it.advance(); // "As" was checked before.
                    continue;
                }

                let val = collect_name(it, &item.val);
                let val_lower = val.to_ascii_lowercase();

                let peeked = it.peek(1).map_err(DqlError::from)?;
                if peeked[0].typ == ITEM_COLON {
                    if !alias.is_empty() {
                        return Err(item.errorf("Invalid colon after alias declaration").into());
                    }
                    alias = val;
                    it.advance(); // Consume the colon.
                    continue;
                }

                if gq.is_groupby
                    && !is_aggregator(&val)
                    && val != COUNT_FUNC
                    && count != CountState::Seen
                {
                    // Only aggregator or count allowed inside the groupby block.
                    return Err(it
                        .errorf(format!(
                            "Only aggregator/count functions allowed inside @groupby. Got: {val}"
                        ))
                        .into());
                }

                if gq.is_empty {
                    validate_empty_block_item(it, &val_lower)?;
                }

                // Returns true when control falls to the plain-predicate path.
                godeep_name(
                    it,
                    gq,
                    &val,
                    &val_lower,
                    &mut var_name,
                    &mut alias,
                    &mut count,
                    &mut cursor,
                )?;
            }
            ITEM_LEFT_CURL => {
                let Some(target) = cursor_node(gq, cursor) else {
                    return Err(it.errorf("Query syntax invalid.").into());
                };
                if !target.langs.is_empty() {
                    return Err(it
                        .errorf(format!(
                            "Cannot have children for attr: {} with lang tags: {:?}",
                            target.attr, target.langs
                        ))
                        .into());
                }
                godeep(it, target)?;
            }
            ITEM_LEFT_ROUND => {
                let Some(target) = cursor_node(gq, cursor) else {
                    return Err(it.errorf("Query syntax invalid.").into());
                };
                if target.attr.is_empty() {
                    return Err(it.errorf("Predicate name cannot be empty.").into());
                }
                apply_child_args(it, target)?;
            }
            ITEM_AT => {
                let target = cursor_node(gq, cursor);
                parse_directive(it, target)?;
            }
            ITEM_RIGHT_ROUND => {
                if count != CountState::SeenWithPred {
                    return Err(it.errorf("Invalid mention of brackets").into());
                }
                count = CountState::NotSeen;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Handles one `itemName` in `godeep`. Returns `true` when the outer loop
/// should `continue` (all paths except the plain-predicate fallthrough).
#[allow(clippy::too_many_arguments, reason = "mirrors Go's godeep locals")]
#[allow(clippy::too_many_lines, reason = "single Go switch port")]
fn godeep_name(
    it: &mut ItemIterator,
    gq: &mut GraphQuery,
    val: &str,
    val_lower: &str,
    var_name: &mut String,
    alias: &mut String,
    count: &mut CountState,
    cursor: &mut Cursor,
) -> Result<(), DqlError> {
    if val_lower == "checkpwd" {
        let mut child = GraphQuery {
            args: std::collections::HashMap::new(),
            var: std::mem::take(var_name),
            alias: std::mem::take(alias),
            ..GraphQuery::default()
        };
        it.prev();
        let mut func = parse_function(it, Some(gq))?;
        let attr = func.attr.clone();
        func.args.push(super::ast::Arg {
            value: attr.clone(),
            ..super::ast::Arg::default()
        });
        child.attr = attr;
        child.func = Some(func);
        gq.children.push(child);
        *cursor = Cursor::Nil;
        return Ok(());
    }
    if is_aggregator(val_lower) {
        let mut child = GraphQuery {
            attr: VALUE_FUNC.to_owned(),
            args: std::collections::HashMap::new(),
            var: std::mem::take(var_name),
            is_internal: true,
            alias: std::mem::take(alias),
            ..GraphQuery::default()
        };
        it.advance();
        if it.item().typ == ITEM_LEFT_ROUND {
            it.advance();
            if gq.is_groupby {
                let item = it.item();
                let attr = collect_name(it, &item.val);
                // Get language list, if present.
                if let Ok(items) = it.peek(1) {
                    if items[0].typ == ITEM_AT {
                        it.advance(); // consume '@'
                        it.advance(); // move forward
                        child.langs = parse_language_list(it)?;
                    }
                }
                child.attr = attr;
                child.is_internal = false;
            } else {
                if it.item().val != VALUE_FUNC {
                    return Err(it
                        .errorf(format!(
                            "Only variables allowed in aggregate functions. Got: {}",
                            it.item().val
                        ))
                        .into());
                }
                let n = super::args::parse_var_list(it, &mut child)?;
                if n != 1 {
                    return Err(it
                        .errorf(format!(
                            "Expected one variable inside val() of aggregator but got {n}"
                        ))
                        .into());
                }
                if let Some(last) = child.needs_var.last_mut() {
                    last.typ = VALUE_VAR;
                }
            }
            child.func = Some(Function {
                name: val_lower.to_owned(),
                // Go shares this slice between child and func; a clone is
                // equivalent (read-only afterwards).
                needs_var: child.needs_var.clone(),
                ..Function::default()
            });
            it.advance(); // Skip the closing ')'.
            gq.children.push(child);
            *cursor = Cursor::Nil;
            return Ok(());
        }
    }
    if is_math_block(val_lower) {
        if var_name.is_empty() && alias.is_empty() {
            return Err(it
                .errorf("Function math should be used with a variable or have an alias")
                .into());
        }
        let (math_tree, again) = parse_math_func(it, false)?;
        if again {
            return Err(it
                .errorf("Comma encountered in math() at unexpected place.")
                .into());
        }
        gq.children.push(GraphQuery {
            attr: val.to_owned(),
            alias: std::mem::take(alias),
            args: std::collections::HashMap::new(),
            var: std::mem::take(var_name),
            math_exp: Some(math_tree),
            is_internal: true,
            ..GraphQuery::default()
        });
        *cursor = Cursor::Nil;
        return Ok(());
    }
    if is_expand_func(val_lower) {
        if !var_name.is_empty() {
            return Err(it
                .errorf(format!("expand() cannot be used with a variable: {val}"))
                .into());
        }
        if !alias.is_empty() {
            return Err(it.errorf("expand() cannot have an alias").into());
        }
        it.advance(); // Consume the '('.
        if it.item().typ != ITEM_LEFT_ROUND {
            return Err(it.errorf("Invalid use of expand()").into());
        }
        it.advance();
        let item = it.item();
        let mut child = GraphQuery {
            attr: val.to_owned(),
            args: std::collections::HashMap::new(),
            is_internal: true,
            ..GraphQuery::default()
        };
        match item.val.as_str() {
            VALUE_FUNC => {
                let n = super::args::parse_var_list(it, &mut child)?;
                if n != 1 {
                    return Err(item
                        .errorf("Invalid use of expand(). Exactly one variable expected.")
                        .into());
                }
                if let Some(last) = child.needs_var.last_mut() {
                    last.typ = LIST_VAR;
                    child.expand.clone_from(&last.name);
                }
            }
            "_all_" => "_all_".clone_into(&mut child.expand),
            "_forward_" => {
                return Err(item.errorf("Argument _forward_ has been deprecated").into());
            }
            "_reverse_" => {
                return Err(item.errorf("Argument _reverse_ has been deprecated").into());
            }
            _ => {
                parse_type_list(it, &mut child)?;
            }
        }
        it.advance(); // Consume ')'.
        gq.children.push(child);
        // Note: cursor is not set to nil, so it can have children, filters, etc.
        *cursor = Cursor::Child;
        return Ok(());
    }
    if val_lower == COUNT_FUNC {
        if *count != CountState::NotSeen {
            return Err(it.errorf("Invalid mention of function count").into());
        }
        *count = CountState::Seen;
        it.advance();
        let item = it.item();
        if item.typ == ITEM_LEFT_ROUND {
            let peeked = it.peek(2).map_err(DqlError::from)?;
            if peeked[0].typ == ITEM_RIGHT_ROUND {
                return Err(it
                    .errorf("Cannot use count(), please use count(uid)")
                    .into());
            }
            if peeked[0].val == UID_FUNC && peeked[1].typ == ITEM_RIGHT_ROUND {
                if gq.is_groupby {
                    // count(uid) inside @groupby.
                    it.advance();
                    it.advance();
                    fall_child(it, gq, UID_FUNC, var_name, alias, count, cursor)?;
                    return Ok(());
                }
                *count = CountState::NotSeen;
                gq.children.push(GraphQuery {
                    attr: "uid".to_owned(),
                    alias: std::mem::take(alias),
                    var: std::mem::take(var_name),
                    is_count: true,
                    is_internal: true,
                    ..GraphQuery::default()
                });
                it.advance();
                it.advance();
            }
            return Ok(());
        }
        it.prev();
        *count = CountState::NotSeen;
    }
    if val_lower == VALUE_FUNC {
        if !var_name.is_empty() {
            return Err(it.errorf("Cannot assign a variable to val()").into());
        }
        if *count == CountState::Seen {
            return Err(it.errorf("Count of a variable is not allowed").into());
        }
        let peeked = it.peek(1).map_err(DqlError::from)?;
        if peeked[0].typ == ITEM_LEFT_ROUND {
            let mut child = GraphQuery {
                attr: val.to_owned(),
                args: std::collections::HashMap::new(),
                is_internal: true,
                alias: std::mem::take(alias),
                ..GraphQuery::default()
            };
            let n = super::args::parse_var_list(it, &mut child)?;
            if n != 1 {
                return Err(it
                    .errorf("Invalid use of val(). Exactly one variable expected.")
                    .into());
            }
            // Only value vars can be retrieved.
            if let Some(last) = child.needs_var.last_mut() {
                last.typ = VALUE_VAR;
            }
            gq.children.push(child);
            *cursor = Cursor::Nil;
            return Ok(());
        }
    }
    if val_lower == UID_FUNC {
        if *count == CountState::Seen {
            return Err(it.errorf("Count of a variable is not allowed").into());
        }
        let peeked = it.peek(1).map_err(DqlError::from)?;
        if peeked[0].typ == ITEM_LEFT_ROUND {
            return Err(it.errorf("Cannot do uid() of a variable").into());
        }
    }
    // Plain predicate child (Go's `Fall` label).
    fall_child(it, gq, val, var_name, alias, count, cursor)
}

/// Appends a plain predicate child (Go's `Fall` block in `godeep`).
#[allow(clippy::too_many_arguments, reason = "mirrors Go's Fall block")]
fn fall_child(
    it: &mut ItemIterator,
    gq: &mut GraphQuery,
    val: &str,
    var_name: &mut String,
    alias: &mut String,
    count: &mut CountState,
    cursor: &mut Cursor,
) -> Result<(), DqlError> {
    if *count == CountState::SeenWithPred {
        return Err(it
            .errorf("Multiple predicates not allowed in single count.")
            .into());
    }
    gq.children.push(GraphQuery {
        args: std::collections::HashMap::new(),
        attr: val.to_owned(),
        is_count: *count == CountState::Seen,
        var: std::mem::take(var_name),
        alias: std::mem::take(alias),
        ..GraphQuery::default()
    });
    if gq.is_count {
        return Err(it
            .errorf("Cannot have children attributes when asking for count.")
            .into());
    }
    *cursor = Cursor::Child;
    if *count == CountState::Seen {
        *count = CountState::SeenWithPred;
    }
    Ok(())
}
