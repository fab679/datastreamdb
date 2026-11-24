//! Function parsing (`eq`, `uid`, `has`, geo, `similar_to`, ...).

#![warn(missing_docs)]

use datastreamdb_lex::ItemIterator;

use super::ast::{
    Arg, COUNT_FUNC, Function, GraphQuery, LEN_FUNC, SIMILAR_TO_FUNC, TYPE_FUNC, UID_FUNC,
    UID_IN_FUNC, UID_VAR, VALUE_FUNC, VALUE_VAR, is_geo_func, is_inequality_fn,
};
use super::state::{
    ITEM_AT, ITEM_COLON, ITEM_COMMA, ITEM_DOLLAR, ITEM_LEFT_CURL, ITEM_LEFT_ROUND,
    ITEM_LEFT_SQUARE, ITEM_MATH_OP, ITEM_NAME, ITEM_REGEX, ITEM_RIGHT_CURL, ITEM_RIGHT_ROUND,
    ITEM_RIGHT_SQUARE,
};
use super::vars::unquote_if_quoted;
use crate::DqlError;

/// Reassembles bracketed geo tokens into one argument.
fn parse_geo_args(it: &mut ItemIterator, func: &mut Function) -> Result<(), DqlError> {
    let mut buf = String::from("[");
    let mut depth = 1;
    loop {
        if !it.advance() {
            return Err(it.errorf("Got EOF while parsing Geo tokens").into());
        }
        let item = it.item();
        match item.typ {
            ITEM_LEFT_SQUARE => {
                buf.push_str(&item.val);
                depth += 1;
            }
            ITEM_RIGHT_SQUARE => {
                buf.push_str(&item.val);
                depth -= 1;
            }
            ITEM_MATH_OP | ITEM_COMMA | ITEM_NAME => {
                buf.push_str(&item.val);
            }
            _ => {
                return Err(item
                    .errorf(format!(
                        "Found invalid item: {} while parsing geo arguments.",
                        item.val
                    ))
                    .into());
            }
        }
        if !(0..=4).contains(&depth) {
            return Err(item.errorf("Invalid bracket sequence").into());
        }
        if depth == 0 {
            break;
        }
    }
    // Append the concatenated geo token to args.
    // TODO - See if we can directly encode to Geo format.
    func.args.push(Arg {
        value: buf,
        ..Arg::default()
    });
    let peeked = it.peek(1).map_err(DqlError::from)?;
    if peeked[0].typ != ITEM_RIGHT_ROUND && peeked[0].typ != ITEM_COMMA {
        return Err(peeked[0]
            .errorf(format!(
                "Expected right round or comma. Got: {:?}",
                peeked[0]
            ))
            .into());
    }
    Ok(())
}

/// Parses `[...]` function arguments (`$var` becomes a DQL variable).
fn parse_func_args(it: &mut ItemIterator, func: &mut Function) -> Result<(), DqlError> {
    let mut expect_arg = true;
    let mut is_dollar = false;
    while it.advance() {
        let item = it.item();
        match item.typ {
            ITEM_RIGHT_SQUARE => return Ok(()),
            ITEM_DOLLAR => {
                if !expect_arg {
                    return Err(item
                        .errorf("Missing comma in argument list declaration")
                        .into());
                }
                let (peeked, ok) = it.peek_one();
                if !ok || peeked.typ != ITEM_NAME {
                    return Err(peeked
                        .errorf(format!("Expecting a variable name. Got: {peeked}"))
                        .into());
                }
                is_dollar = true;
                continue;
            }
            ITEM_NAME => {
                if is_dollar {
                    // A $variable expanded later.
                    func.args.push(Arg {
                        value: format!("${}", item.val),
                        is_dql_var: true,
                        ..Arg::default()
                    });
                } else {
                    // Not a $variable, just add the value.
                    let val = get_value_arg(&item.val)?;
                    func.args.push(Arg {
                        value: val,
                        ..Arg::default()
                    });
                }
            }
            ITEM_COMMA => {
                if expect_arg {
                    return Err(item.errorf("Invalid comma in argument list").into());
                }
                expect_arg = true;
                continue;
            }
            _ => return Err(item.errorf("Invalid arg list").into()),
        }
        expect_arg = false;
        is_dollar = false;
    }
    Err(it
        .errorf(format!(
            "Expecting ] to end list but got {} instead",
            it.item().val
        ))
        .into())
}

/// Trims and unquotes a function argument value.
fn get_value_arg(val: &str) -> Result<String, DqlError> {
    unquote_if_quoted(val.trim())
}

/// Whether the function name is valid at root.
pub(crate) fn valid_func_name(name: &str) -> bool {
    if is_geo_func(name) || is_inequality_fn(name) {
        return true;
    }
    matches!(
        name,
        "regexp"
            | "anyofterms"
            | "allofterms"
            | "alloftext"
            | "anyoftext"
            | "ngram"
            | "has"
            | "uid"
            | "uid_in"
            | "anyof"
            | "allof"
            | "type"
            | "match"
            | "similar_to"
    )
}

/// Split regex literal (`/expr/flags`).
pub(crate) struct RegexArgs {
    /// Expression between slashes (`\/` unescaped).
    pub expr: String,
    /// Trailing flags.
    pub flags: String,
}

/// Splits `/expr/flags`, validating slash placement.
pub(crate) fn parse_regex_args(val: &str) -> Result<RegexArgs, DqlError> {
    if val.is_empty() {
        return Err(DqlError::message(format!("Found empty regex: [{val}]")));
    }
    let Some(end) = val.rfind('/') else {
        return Err(DqlError::message(format!("Found invalid regex: [{val}]")));
    };
    if end < 1 || !val.starts_with('/') {
        return Err(DqlError::message(format!("Found invalid regex: [{val}]")));
    }
    Ok(RegexArgs {
        expr: val[1..end].replace("\\/", "/"),
        flags: if end + 1 < val.len() {
            val[end + 1..].to_owned()
        } else {
            String::new()
        },
    })
}

/// Assigns one collected argument: attribute, language, value, or UID.
#[allow(
    clippy::too_many_arguments,
    reason = "mirrors Go's parseFunction locals"
)]
fn finish_function_arg(
    it: &mut ItemIterator,
    function: &mut Function,
    gq: Option<&mut GraphQuery>,
    item_in_func: &datastreamdb_lex::Item,
    mut val: String,
    flags: &mut ArgFlags,
    is_dollar: &mut bool,
) -> Result<(), DqlError> {
    let (peeked, ok) = it.peek_one();
    if !ok {
        return Err(peeked.errorf(format!("Unexpected item: {peeked}")).into());
    }
    // Part of function continue.
    if peeked.typ == ITEM_LEFT_ROUND {
        return Ok(());
    }
    if !flags.expect_arg && !flags.expect_lang {
        return Err(item_in_func
            .errorf(format!(
                "Expected comma or language but got: {}",
                item_in_func.val
            ))
            .into());
    }

    let collected = super::args::collect_name(it, &item_in_func.val);
    // TODO - Move this to a function.
    let trimmed = collected.trim_matches([' ', '\t']);
    let unquoted = unquote_if_quoted(trimmed)?;
    val.push_str(&unquoted);

    if *is_dollar {
        val = format!("${val}");
        *is_dollar = false;
        if function.name == UID_FUNC {
            if let Some(gq) = gq {
                if gq.args.get("id").is_some_and(|v| !v.is_empty()) {
                    return Err(item_in_func
                        .errorf("Only one DQL variable allowed inside uid function.")
                        .into());
                }
                gq.args.insert("id".to_owned(), val.clone());
            } else {
                function.args.push(Arg {
                    value: val.clone(),
                    is_dql_var: true,
                    ..Arg::default()
                });
            }
        } else {
            function.args.push(Arg {
                value: val.clone(),
                is_dql_var: true,
                ..Arg::default()
            });
        }
        flags.expect_arg = false;
        return Ok(());
    }

    // Unlike other functions, uid has no attribute; all is args.
    if function.attr.is_empty() && function.name != UID_FUNC && function.name != TYPE_FUNC {
        if item_in_func.val.contains('"') {
            return Err(item_in_func
                .errorf(format!(
                    "Attribute in function must not be quoted with \": {}",
                    item_in_func.val
                ))
                .into());
        }
        if function.name == UID_IN_FUNC && peeked.typ == ITEM_RIGHT_ROUND {
            return Err(item_in_func
                .errorf("uid_in function expects an argument, got none")
                .into());
        }
        function.attr.clone_from(&val);
        flags.attr_items_ago = 0;
    } else if flags.expect_lang {
        if val == "*" {
            return Err(DqlError::message(
                "The * symbol cannot be used as a valid language inside functions",
            ));
        }
        function.lang.clone_from(&val);
        flags.expect_lang = false;
    } else if function.name != UID_FUNC {
        // For UID function, we set g.UID.
        function.args.push(Arg {
            value: val.clone(),
            ..Arg::default()
        });
    }

    if function.name == "var" {
        return Err(item_in_func
            .errorf("Unexpected var(). Maybe you want to try using uid()")
            .into());
    }

    flags.expect_arg = false;
    collect_function_name_arg(function, gq, item_in_func, &val)
}

/// Records `val`/`len`/`uid` names as variables (or root/filter UIDs).
fn collect_function_name_arg(
    function: &mut Function,
    gq: Option<&mut GraphQuery>,
    item_in_func: &datastreamdb_lex::Item,
    val: &str,
) -> Result<(), DqlError> {
    match function.name.as_str() {
        VALUE_FUNC => {
            // E.g. @filter(gt(val(a), 10)).
            function.needs_var.push(super::ast::VarContext {
                name: val.to_owned(),
                typ: VALUE_VAR,
            });
        }
        LEN_FUNC => {
            // E.g. @filter(gt(len(a), 10)).
            // TODO(Aman): type could be ValueVar too!
            function.needs_var.push(super::ast::VarContext {
                name: val.to_owned(),
                typ: UID_VAR,
            });
        }
        UID_FUNC => {
            // Uid takes variables as well as actual uids: parsable
            // values are uids, otherwise variables.
            match datastreamdb_types::strconv::parse_go_uint_base0(val) {
                Ok(uid) => {
                    if let Some(gq) = gq {
                        gq.uid.push(uid);
                    } else {
                        function.uid.push(uid);
                    }
                    return Ok(());
                }
                Err(e) => {
                    if matches!(e, datastreamdb_types::strconv::ParseUintError::Range { .. }) {
                        return Err(item_in_func
                            .errorf(format!("The uid value {val:?} is too large."))
                            .into());
                    }
                }
            }
            // E.g. @filter(uid(a, b, c)).
            function.needs_var.push(super::ast::VarContext {
                name: val.to_owned(),
                typ: UID_VAR,
            });
        }
        _ => {}
    }
    Ok(())
}

/// Handles a bare argument token: `similar_to` named options or a name
/// check. Returns whether the loop should continue (option consumed).
fn parse_bare_function_arg(
    it: &mut ItemIterator,
    function: &mut Function,
    similar_to_seen: &mut std::collections::HashSet<String>,
    flags: &mut ArgFlags,
    item_in_func: &datastreamdb_lex::Item,
) -> Result<bool, DqlError> {
    // similar_to named options after the positional args.
    if item_in_func.typ == ITEM_NAME
        && function.name == SIMILAR_TO_FUNC
        && !function.attr.is_empty()
        && function.args.len() >= 2
    {
        let (next, ok) = it.peek_one();
        if ok && next.typ == ITEM_COLON {
            parse_similar_to_option(it, function, similar_to_seen, item_in_func)?;
            flags.expect_arg = false;
            return Ok(true);
        }
        // Disallow extra positional args after (k, vec).
        return Err(item_in_func
            .errorf("Expected named parameter in similar_to options (e.g. ef: 64)")
            .into());
    }
    if item_in_func.typ != ITEM_NAME {
        return Err(item_in_func
            .errorf(format!(
                "Expected arg after func [{}], but got item {item_in_func}",
                function.name
            ))
            .into());
    }
    Ok(false)
}

/// Folds a nested `val`/`len`/`count`/`uid` call into its parent function.
fn apply_nested_function(
    nested: Function,
    function: &mut Function,
    item: &datastreamdb_lex::Item,
) -> Result<(), DqlError> {
    match nested.name.as_str() {
        VALUE_FUNC => {
            if nested.needs_var.len() > 1 {
                return Err(item
                    .errorf("Multiple variables not allowed in a function")
                    .into());
            }
            if function.attr.is_empty() {
                // Variable in place of attribute, eq(val(a), 5).
                function.attr.clone_from(&nested.needs_var[0].name);
                function.is_value_var = true;
            } else {
                // eq(name, val(a)).
                function.args.push(Arg {
                    value: nested.needs_var[0].name.clone(),
                    is_value_var: true,
                    ..Arg::default()
                });
            }
            function.needs_var.extend(nested.needs_var);
            if let Some(first) = function.needs_var.first_mut() {
                first.typ = VALUE_VAR;
            }
        }
        LEN_FUNC => {
            if nested.needs_var.len() > 1 {
                return Err(item
                    .errorf("Multiple variables not allowed in len function")
                    .into());
            }
            if !is_inequality_fn(&function.name) {
                return Err(item
                    .errorf("len function only allowed inside inequality function")
                    .into());
            }
            if function.attr.is_empty() {
                // eq(len(a), 1).
                function.attr.clone_from(&nested.needs_var[0].name);
                function.is_len_var = true;
            } else {
                // eq(1, len(a)).
                return Err(item
                    .errorf("incorrect order, the first argument should be len function")
                    .into());
            }
            function.needs_var.extend(nested.needs_var);
        }
        COUNT_FUNC => {
            function.attr.clone_from(&nested.attr);
            function.is_count = true;
        }
        UID_FUNC => {
            // TODO (Anurag): support uid(1,2,3) nested like
            // @filter(uid_in(predicate, uid())).
            if nested.needs_var.len() != 1 {
                return Err(item
                    .errorf(format!(
                        "Nested uid fn expects 1 uid variable, got {}",
                        nested.needs_var.len()
                    ))
                    .into());
            }
            if !nested.uid.is_empty() {
                return Err(item
                    .errorf("Nested uid fn expects only uid variable, got UID")
                    .into());
            }
            function.needs_var.extend(nested.needs_var);
            if let Some(first) = function.needs_var.first_mut() {
                first.typ = UID_VAR;
            }
            let name = function.needs_var[0].name.clone();
            function.args.push(Arg {
                value: name,
                ..Arg::default()
            });
        }
        other => {
            return Err(item
                .errorf(format!(
                    "Only val/count/len/uid allowed as function within another. Got: {other}"
                ))
                .into());
        }
    }
    Ok(())
}

/// Per-argument flags threaded through the function-arg loop.
struct ArgFlags {
    expect_arg: bool,
    expect_lang: bool,
    attr_items_ago: i32,
}

/// Parses one function call (`gq` is set for root functions, else filter use).
#[allow(clippy::too_many_lines, reason = "single Go switch port; arms map 1:1")]
pub(crate) fn parse_function(
    it: &mut ItemIterator,
    gq: Option<&mut GraphQuery>,
) -> Result<Function, DqlError> {
    let mut function = Function::default();
    let mut flags = ArgFlags {
        expect_arg: false,
        expect_lang: false,
        attr_items_ago: -1,
    };
    let mut seen_func_arg = false;
    let mut is_dollar = false;
    // Root UID/DQL-var collection when parsing at root.
    let mut gq = gq;
    'outer: loop {
        if !it.advance() {
            break;
        }
        let item = it.item();
        if item.typ != ITEM_NAME {
            return Err(item
                .errorf(format!("Expected a function but got {:?}", item.val))
                .into());
        }

        let name = super::args::collect_name(it, &item.val);
        function.name = name.to_ascii_lowercase();
        let mut similar_to_seen = std::collections::HashSet::new();
        if super::args::try_parse_item_type(it, ITEM_LEFT_ROUND).is_none() {
            return Err(it
                .errorf(format!("Expected ( after func name [{}]", function.name))
                .into());
        }

        flags.expect_arg = true;
        while it.advance() {
            let mut item_in_func = it.item();
            if flags.attr_items_ago >= 0 {
                flags.attr_items_ago += 1;
            }
            let mut val = String::new();
            match item_in_func.typ {
                ITEM_RIGHT_ROUND => break 'outer,
                ITEM_COMMA => {
                    if flags.expect_arg {
                        return Err(item_in_func.errorf("Invalid use of comma.").into());
                    }
                    if is_dollar {
                        return Err(item_in_func
                            .errorf("Invalid use of comma after dollar.")
                            .into());
                    }
                    flags.expect_arg = true;
                    continue;
                }
                ITEM_LEFT_ROUND => {
                    // Function inside a function.
                    if seen_func_arg {
                        return Err(item_in_func
                            .errorf("Multiple functions as arguments not allowed")
                            .into());
                    }
                    it.prev();
                    it.prev();
                    let nested = parse_function(it, None)?;
                    seen_func_arg = true;
                    apply_nested_function(nested, &mut function, &item_in_func)?;
                    flags.expect_arg = false;
                    continue;
                }
                ITEM_AT => {
                    if flags.attr_items_ago != 1 {
                        return Err(item_in_func
                            .errorf("Invalid usage of '@' in function argument, must only appear immediately after attr.")
                            .into());
                    }
                    flags.expect_lang = true;
                    continue;
                }
                ITEM_MATH_OP => {
                    item_in_func.val.clone_into(&mut val);
                    it.advance();
                    item_in_func = it.item();
                }
                ITEM_DOLLAR => {
                    if is_dollar {
                        return Err(item_in_func.errorf("Invalid use of $ in func args").into());
                    }
                    is_dollar = true;
                    continue;
                }
                ITEM_REGEX => {
                    let parsed = parse_regex_args(&item_in_func.val)?;
                    function.args.push(Arg {
                        value: parsed.expr,
                        ..Arg::default()
                    });
                    function.args.push(Arg {
                        value: parsed.flags,
                        ..Arg::default()
                    });
                    flags.expect_arg = false;
                    continue;
                }
                // Reassemble geo tokens.
                ITEM_LEFT_SQUARE => {
                    let result = if is_geo_func(&function.name) {
                        parse_geo_args(it, &mut function)
                    } else if is_inequality_fn(&function.name)
                        || function.name == "uid_in"
                        || function.name == SIMILAR_TO_FUNC
                    {
                        parse_func_args(it, &mut function)
                    } else {
                        Err(item_in_func
                            .errorf("Unexpected character [ while parsing request.")
                            .into())
                    };
                    result?;
                    flags.expect_arg = false;
                    continue;
                }
                ITEM_RIGHT_SQUARE => {
                    if it.peek(1).is_err() {
                        return Err(item_in_func
                            .errorf("Unexpected EOF while parsing args")
                            .into());
                    }
                    flags.expect_arg = false;
                    continue;
                }
                ITEM_LEFT_CURL => {
                    return Err(item_in_func
                        .errorf("Unrecognized character inside a func: U+007B '{'")
                        .into());
                }
                ITEM_RIGHT_CURL => {
                    // Stray '}' is never valid in function arguments.
                    return Err(item_in_func
                        .errorf("Unrecognized character inside a func: U+007D '}'")
                        .into());
                }
                _ => {
                    if parse_bare_function_arg(
                        it,
                        &mut function,
                        &mut similar_to_seen,
                        &mut flags,
                        &item_in_func,
                    )? {
                        continue;
                    }
                }
            }

            finish_function_arg(
                it,
                &mut function,
                gq.as_deref_mut(),
                &item_in_func,
                val,
                &mut flags,
                &mut is_dollar,
            )?;
        }
    }

    if function.name != UID_FUNC && function.name != TYPE_FUNC && function.attr.is_empty() {
        return Err(it
            .errorf(format!("Got empty attr for function: [{}]", function.name))
            .into());
    }
    if function.name == TYPE_FUNC && function.args.len() != 1 {
        return Err(it
            .errorf(format!(
                "type function only supports one argument. Got: {:?}",
                function.args
            ))
            .into());
    }
    Ok(function)
}

/// Parses one `key: value` option inside `similar_to(...)`.
fn parse_similar_to_option(
    it: &mut ItemIterator,
    function: &mut Function,
    seen: &mut std::collections::HashSet<String>,
    at: &datastreamdb_lex::Item,
) -> Result<(), DqlError> {
    let key = super::args::collect_name(it, &at.val).to_ascii_lowercase();
    if key != "ef" && key != "distance_threshold" {
        return Err(at
            .errorf(format!("Unknown option {key:?} in similar_to"))
            .into());
    }
    if !seen.insert(key.clone()) {
        return Err(at
            .errorf(format!("Duplicate key {key:?} in similar_to options"))
            .into());
    }
    if !super::args::try_skip_item_type(it, ITEM_COLON) {
        return Err(it.errorf(format!("Expected colon(:) after {key}")).into());
    }
    if !it.advance() {
        return Err(it.errorf(format!("Expected value for {key}")).into());
    }
    let mut val_item = it.item();
    if val_item.typ == ITEM_DOLLAR {
        let var_name = super::directives::parse_var_name(it)?;
        function.args.push(Arg {
            value: key,
            ..Arg::default()
        });
        function.args.push(Arg {
            value: var_name,
            is_dql_var: true,
            ..Arg::default()
        });
        return Ok(());
    }
    let mut prefix = String::new();
    if val_item.typ == ITEM_MATH_OP {
        // Signed numeric literals, e.g. distance_threshold: -0.5.
        val_item.val.clone_into(&mut prefix);
        if !it.advance() {
            return Err(it
                .errorf(format!("Expected value after {prefix} for {key}"))
                .into());
        }
        val_item = it.item();
        if val_item.typ != ITEM_NAME {
            return Err(val_item.errorf(format!("Expected value for {key}")).into());
        }
    } else if val_item.typ != ITEM_NAME {
        return Err(val_item.errorf(format!("Expected value for {key}")).into());
    }
    let collected = super::args::collect_name(it, &val_item.val);
    let unquoted = unquote_if_quoted(collected.trim_matches([' ', '\t']))?;
    function.args.push(Arg {
        value: key,
        ..Arg::default()
    });
    function.args.push(Arg {
        value: format!("{prefix}{unquoted}"),
        ..Arg::default()
    });
    Ok(())
}
