//! Directives: `@filter` sites, `@cascade`, `@groupby`, `@facets`, languages.

#![warn(missing_docs)]

use datastreamdb_lex::ItemIterator;
use datastreamdb_types::strconv::{parse_go_bool, parse_go_uint_base0};

use super::args::{collect_name, try_parse_item_type, try_skip_item_type};
use super::ast::{GraphQuery, GroupByAttr};
use super::facets::parse_facets;
use super::filter::parse_filter;
use super::state::{
    ITEM_AT, ITEM_COLON, ITEM_COMMA, ITEM_DOLLAR, ITEM_LEFT_CURL, ITEM_LEFT_ROUND, ITEM_NAME,
    ITEM_PERIOD, ITEM_RIGHT_CURL, ITEM_RIGHT_ROUND, ITEM_STAR,
};
use crate::DqlError;

/// Parses a `$name...` variable name (caller consumed `$`).
pub(crate) fn parse_var_name(it: &mut ItemIterator) -> Result<String, DqlError> {
    let mut val = String::from("$");
    let mut consumed = false;
    loop {
        let items = it.peek(1).map_err(DqlError::from)?;
        if items[0].typ != ITEM_NAME {
            if !consumed {
                return Err(it.errorf("Expected variable name after $").into());
            }
            break;
        }
        consumed = true;
        val.push_str(&items[0].val);
        // Consume the current item.
        if !it.advance() {
            break;
        }
    }
    Ok(val)
}

/// Parses `@recurse(depth: ..., loop: ...)` arguments.
pub(crate) fn parse_recurse_args(
    it: &mut ItemIterator,
    gq: &mut GraphQuery,
) -> Result<(), DqlError> {
    if !try_skip_item_type(it, ITEM_LEFT_ROUND) {
        // No '(': nothing to do.
        return Ok(());
    }

    while it.advance() {
        let item = it.item();
        if item.typ != ITEM_NAME {
            return Err(item.errorf("Expected key inside @recurse()").into());
        }
        let key = item.val.to_ascii_lowercase();

        if !try_skip_item_type(it, ITEM_COLON) {
            return Err(it.errorf(format!("Expected colon(:) after {key}")).into());
        }
        if !it.advance() {
            return Err(it.errorf("Expected argument").into());
        }

        // Consume the next item.
        let item = it.item();
        let val = item.val.clone();
        match key.as_str() {
            "depth" => {
                // Variable or value?
                if item.typ == ITEM_DOLLAR {
                    let var_name = parse_var_name(it)?;
                    gq.recurse_args.var_map.insert("depth".to_owned(), var_name);
                } else {
                    if item.typ != ITEM_NAME {
                        return Err(item
                            .errorf(format!("Expected value inside @recurse() for key: {key}"))
                            .into());
                    }
                    match parse_go_uint_base0(&val) {
                        Ok(depth) => gq.recurse_args.depth = depth,
                        Err(_) => {
                            return Err(DqlError::message(
                                "Value inside depth should be type of integer",
                            ));
                        }
                    }
                }
            }
            "loop" => {
                if item.typ == ITEM_DOLLAR {
                    let var_name = parse_var_name(it)?;
                    gq.recurse_args.var_map.insert("loop".to_owned(), var_name);
                } else {
                    match parse_go_bool(&val) {
                        Ok(allow) => gq.recurse_args.allow_loop = allow,
                        Err(_) => {
                            return Err(DqlError::message(
                                "Value inside loop should be type of boolean",
                            ));
                        }
                    }
                }
            }
            _ => {
                return Err(item
                    .errorf(format!("Unexpected key: [{key}] inside @recurse block"))
                    .into());
            }
        }

        if try_parse_item_type(it, ITEM_RIGHT_ROUND).is_some() {
            return Ok(());
        }
        if try_parse_item_type(it, ITEM_COMMA).is_none() {
            return Err(it
                .errorf(format!(
                    "Expected comma after value: {val} inside recurse block"
                ))
                .into());
        }
    }
    Ok(())
}

/// Parses `@cascade` / `@cascade(pred, ...)`.
pub(crate) fn parse_cascade(it: &mut ItemIterator, gq: &mut GraphQuery) -> Result<(), DqlError> {
    let item = it.item();
    let items = it.peek(1).map_err(|_| {
        DqlError::message(
            item.errorf("Unable to peek lexer after cascade")
                .to_string(),
        )
    })?;

    // Bare `@cascade` before `{`, `}`, `@`, or a name.
    if matches!(
        items[0].typ,
        ITEM_LEFT_CURL | ITEM_RIGHT_CURL | ITEM_AT | ITEM_NAME
    ) {
        // `__all__` implies values for all children are mandatory.
        gq.cascade.push("__all__".to_owned());
        return Ok(());
    }

    let mut count = 0;
    let mut expect_arg = true;
    it.advance();
    let item = it.item();
    if item.typ != ITEM_LEFT_ROUND {
        return Err(item
            .errorf(format!("Expected a left round after cascade, got: {item}"))
            .into());
    }

    loop {
        if !it.advance() {
            break;
        }
        let item = it.item();
        match item.typ {
            ITEM_RIGHT_ROUND => break,
            ITEM_COMMA => {
                if expect_arg {
                    return Err(item.errorf("Expected a predicate but got comma").into());
                }
                expect_arg = true;
            }
            ITEM_NAME => {
                if !expect_arg {
                    return Err(item
                        .errorf(format!(
                            "Expected a comma or right round but got: {}",
                            item.val
                        ))
                        .into());
                }
                gq.cascade.push(collect_name(it, &item.val));
                count += 1;
                expect_arg = false;
            }
            _ => {
                return Err(item
                    .errorf(format!("Unexpected item while parsing: {}", item.val))
                    .into());
            }
        }
    }
    if expect_arg {
        // Use the initial item for error positions.
        return Err(item.errorf("Unnecessary comma in cascade()").into());
    }
    if count == 0 {
        return Err(item
            .errorf("At least one predicate required in parameterized cascade()")
            .into());
    }
    Ok(())
}

/// Parses `@groupby(pred, alias: pred@lang, ...)`.
pub(crate) fn parse_groupby(it: &mut ItemIterator, gq: &mut GraphQuery) -> Result<(), DqlError> {
    let mut count = 0;
    let mut expect_arg = true;
    it.advance();
    let item = it.item();
    let mut alias = String::new();
    if item.typ != ITEM_LEFT_ROUND {
        return Err(item.errorf("Expected a left round after groupby").into());
    }

    loop {
        if !it.advance() {
            break;
        }
        let item = it.item();
        match item.typ {
            ITEM_RIGHT_ROUND => break,
            ITEM_COMMA => {
                if expect_arg {
                    return Err(item.errorf("Expected a predicate but got comma").into());
                }
                expect_arg = true;
            }
            ITEM_NAME => {
                if !expect_arg {
                    return Err(item
                        .errorf(format!(
                            "Expected a comma or right round but got: {}",
                            item.val
                        ))
                        .into());
                }
                let val = collect_name(it, &item.val);
                let peeked = it.peek(1).map_err(DqlError::from)?;
                if peeked[0].typ == ITEM_COLON {
                    if !alias.is_empty() {
                        return Err(item
                            .errorf(format!("Expected predicate after {alias}:"))
                            .into());
                    }
                    if super::args::valid_key(&val) {
                        return Err(item
                            .errorf(format!("Can't use keyword {val} as alias in groupby"))
                            .into());
                    }
                    alias = val;
                    it.advance(); // Consume the colon.
                    continue;
                }

                let mut langs = Vec::new();
                if let Ok(items) = it.peek(1) {
                    if items[0].typ == ITEM_AT {
                        it.advance(); // consume '@'
                        it.advance(); // move forward
                        langs = parse_language_list(it)?;
                    }
                }
                gq.groupby_attrs.push(GroupByAttr {
                    attr: val,
                    alias: std::mem::take(&mut alias),
                    langs,
                });
                count += 1;
                expect_arg = false;
            }
            // NOTE: Go has no default here: anything else is silently skipped.
            _ => {}
        }
    }
    if expect_arg {
        // Use the initial item for error positions.
        return Err(item.errorf("Unnecessary comma in groupby()").into());
    }
    if count == 0 {
        return Err(item
            .errorf("Expected atleast one attribute in groupby")
            .into());
    }
    Ok(())
}

/// Parses an `@...` directive (or language list) at a child block.
/// `None` targets internal nodes that take no directives (Go passes nil).
pub(crate) fn parse_directive(
    it: &mut ItemIterator,
    curp: Option<&mut GraphQuery>,
) -> Result<(), DqlError> {
    let mut valid = true;
    it.prev();
    let item = it.item();
    if item.typ == ITEM_LEFT_CURL {
        // Ideally we should check that curp was created at current depth.
        valid = false;
    }
    it.advance();

    let is_expand = curp.as_ref().is_some_and(|c| !c.expand.is_empty());
    // No directive is allowed on internal subgraphs like expand-all (except
    // type filters), value variables, etc.
    if !valid || curp.is_none() || curp.as_ref().is_some_and(|c| c.is_internal && !is_expand) {
        return Err(item.errorf("Invalid use of directive.").into());
    }
    let curp = curp.expect("checked");

    it.advance();
    let item = it.item();
    let peeked = it.peek(1).map_err(DqlError::from)?;
    if item.typ != ITEM_NAME {
        return Err(item.errorf("Expected directive or language list").into());
    }

    if is_expand && item.val != "filter" {
        return Err(item.errorf(super::root::EXPAND_TYPE_ERROR).into());
    }

    if item.val == "facets" {
        // @facets can come with or without '()'.
        let res = parse_facets(it)?;
        if let Some(f) = res.f {
            curp.facet_var = res.vmap;
            curp.facets_order = res.facets_order;
            if curp.facets.is_some() {
                return Err(item.errorf("Only one facets allowed").into());
            }
            curp.facets = Some(f);
        } else if let Some(ft) = res.ft {
            if curp.facets_filter.is_some() {
                return Err(item.errorf("Only one facets filter allowed").into());
            }
            if super::filter::has_vars(&ft) {
                return Err(item
                    .errorf("variables are not allowed in facets filter.")
                    .into());
            }
            curp.facets_filter = Some(Box::new(ft));
        } else {
            return Err(item.errorf("Facets parsing failed.").into());
        }
        return Ok(());
    }
    if item.val == "cascade" {
        return parse_cascade(it, curp);
    }
    if item.val == "normalize" {
        curp.normalize = true;
        return Ok(());
    }
    if peeked[0].typ == ITEM_LEFT_ROUND {
        // This is a directive.
        return parse_paren_directive(it, curp, &item);
    }
    if !curp.attr.is_empty() && curp.langs.is_empty() {
        // This is a language list.
        let langs = parse_language_list(it)?;
        curp.langs = langs;
        if curp.langs.is_empty() {
            return Err(item
                .errorf(format!(
                    "Expected at least 1 language in list for {}",
                    curp.attr
                ))
                .into());
        }
        return Ok(());
    }
    Err(item
        .errorf(format!(
            "Expected directive or language list, got @{}",
            item.val
        ))
        .into())
}

/// Parses `@filter(...)` / `@groupby(...)` (parenthesized directives).
fn parse_paren_directive(
    it: &mut ItemIterator,
    curp: &mut GraphQuery,
    item: &datastreamdb_lex::Item,
) -> Result<(), DqlError> {
    let is_expand = !curp.expand.is_empty();
    match item.val.as_str() {
        "filter" => {
            if curp.filter.is_some() {
                return Err(item
                    .errorf("Use AND, OR and round brackets instead of multiple filter directives.")
                    .into());
            }
            let filter = parse_filter(it)?;
            if is_expand
                && let Some(f) = filter.as_ref()
                && let Some(func) = f.func.as_ref()
                && func.name != "type"
            {
                return Err(item.errorf(super::root::EXPAND_TYPE_ERROR).into());
            }
            curp.filter = filter.map(Box::new);
            Ok(())
        }
        "groupby" => {
            if curp.is_groupby {
                return Err(item.errorf("Only one group by directive allowed.").into());
            }
            curp.is_groupby = true;
            parse_groupby(it, curp)
        }
        _ => Err(item
            .errorf(format!("Unknown directive [{}]", item.val))
            .into()),
    }
}

/// Parses language lists (`en:de`, `.`, `*`).
pub(crate) fn parse_language_list(it: &mut ItemIterator) -> Result<Vec<String>, DqlError> {
    let mut item = it.item();
    let mut langs = Vec::new();
    while item.typ == ITEM_NAME || item.typ == ITEM_PERIOD || item.typ == ITEM_STAR {
        langs.push(item.val.clone());
        it.advance();
        if it.item().typ == ITEM_COLON {
            it.advance();
        } else {
            break;
        }
        item = it.item().clone();
    }
    if it.item().typ == ITEM_PERIOD {
        let peeked = it.peek(1).map_err(DqlError::from)?;
        if peeked[0].typ == ITEM_PERIOD {
            return Err(it
                .errorf("Expected only one dot(.) while parsing language list.")
                .into());
        }
    }
    it.prev();

    for lang in &langs {
        if lang == "*" && langs.len() > 1 {
            // Go formats the slice as `[a b]`.
            return Err(DqlError::message(format!(
                "If * is used, no other languages are allowed in the language list. Found [{}]",
                langs.join(" ")
            )));
        }
    }
    Ok(langs)
}
