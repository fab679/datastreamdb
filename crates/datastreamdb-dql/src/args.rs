//! Root/child argument lists and small iterator helpers.

#![warn(missing_docs)]

use datastreamdb_lex::{Item, ItemIterator, ItemType};

use super::ast::{GraphQuery, VALUE_VAR, VarContext};
use super::state::{
    ITEM_AT, ITEM_COLON, ITEM_COMMA, ITEM_DOLLAR, ITEM_MATH_OP, ITEM_NAME, ITEM_RIGHT_ROUND,
};
use crate::DqlError;

/// One parsed `key: value` argument.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Pair {
    /// Argument name.
    pub key: String,
    /// Argument value.
    pub val: String,
}

/// Whether the key is valid at root.
pub(crate) fn valid_key_at_root(key: &str) -> bool {
    matches!(
        key,
        "func"
            | "orderasc"
            | "orderdesc"
            | "first"
            | "offset"
            | "after"
            | "from"
            | "to"
            | "numpaths"
            | "minweight"
            | "maxweight"
            | "maxfrontiersize"
            | "depth"
    )
}

/// Whether the key is valid at non-root nodes.
pub(crate) fn valid_key(key: &str) -> bool {
    matches!(key, "orderasc" | "orderdesc" | "first" | "offset" | "after")
}

/// Splits `attr@lang:lang2` into attribute and languages.
pub(crate) fn attr_and_lang(attr_data: &str) -> (&str, Vec<String>) {
    match attr_data.find('@') {
        None => (attr_data, Vec::new()),
        Some(idx) => (
            &attr_data[..idx],
            attr_data[idx + 1..].split(':').map(str::to_owned).collect(),
        ),
    }
}

/// Whether the key is a sort key.
pub(crate) fn is_sort_key(key: &str) -> bool {
    key == "orderasc" || key == "orderdesc"
}

/// Reassembles dash-separated names (the lexer splits them).
pub(crate) fn collect_name(it: &mut ItemIterator, val: &str) -> String {
    let mut out = val.to_owned();
    // False while expecting a dash, true while expecting a name.
    let mut dash_or_name = false;
    while let Ok(items) = it.peek(1) {
        let next_is_name = items[0].typ == ITEM_NAME && dash_or_name;
        let next_is_dash = items[0].val == "-" && !dash_or_name;
        if next_is_name || next_is_dash {
            it.advance();
            out.push_str(&it.item().val);
            dash_or_name = !dash_or_name;
        } else {
            break;
        }
    }
    out
}

/// Consumes the next item when it has the type.
pub(crate) fn try_parse_item_type(it: &mut ItemIterator, typ: ItemType) -> Option<Item> {
    let (item, ok) = it.peek_one();
    if !ok || item.typ != typ {
        return None;
    }
    it.advance();
    Some(item)
}

/// Consumes the next item when it has the value.
pub(crate) fn try_skip_item_val(it: &mut ItemIterator, val: &str) -> bool {
    let (item, ok) = it.peek_one();
    if !ok || item.val != val {
        return false;
    }
    it.advance();
    true
}

/// Consumes the next item when it has the type.
pub(crate) fn try_skip_item_type(it: &mut ItemIterator, typ: ItemType) -> bool {
    try_parse_item_type(it, typ).is_some()
}

/// Reads one argument value (`$var`, signed literal, or dashed name).
fn arg_value_head(
    it: &mut ItemIterator,
    mut item: datastreamdb_lex::Item,
) -> Result<String, DqlError> {
    let mut val = String::new();
    if item.typ == ITEM_DOLLAR {
        val.push('$');
        it.advance();
        item = it.item();
        if item.typ != ITEM_NAME {
            return Err(item
                .errorf(format!("Expecting argument value. Got: {item}"))
                .into());
        }
    } else if item.typ == ITEM_MATH_OP {
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
    } else if item.typ != ITEM_NAME {
        return Err(item
            .errorf(format!("Expecting argument value. Got: {item}"))
            .into());
    }
    Ok(collect_name(it, &(val + &item.val)))
}

/// Parses the `(key: value, ...)` arguments of a query root.
pub(crate) fn parse_arguments(
    it: &mut ItemIterator,
    gq: &mut GraphQuery,
) -> Result<Vec<Pair>, DqlError> {
    let mut result = Vec::new();
    let mut expect_arg = true;
    let mut order_count = 0;
    loop {
        if !it.advance() {
            break;
        }
        let mut pair = Pair {
            key: String::new(),
            val: String::new(),
        };
        // Get key.
        let item = it.item();
        match item.typ {
            ITEM_NAME => {
                if !expect_arg {
                    return Err(item
                        .errorf(format!("Expecting a comma. But got: {}", item.val))
                        .into());
                }
                pair.key = collect_name(it, &item.val);
                if is_sort_key(&pair.key) {
                    order_count += 1;
                }
                expect_arg = false;
            }
            ITEM_RIGHT_ROUND => {
                if expect_arg {
                    return Err(item.errorf("Expected argument but got ')'.").into());
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

        it.advance();
        let item = it.item();
        if item.typ != ITEM_COLON {
            return Err(item
                .errorf(format!("Expecting a colon. Got: {item} in {}", gq.attr))
                .into());
        }

        // Get value.
        it.advance();
        let item = it.item();
        if item.val == super::ast::VALUE_FUNC {
            let count = parse_var_list(it, gq)?;
            if count != 1 {
                return Err(item
                    .errorf(format!("Only one variable expected. Got {count}"))
                    .into());
            }
            if let Some(last) = gq.needs_var.last_mut() {
                last.typ = VALUE_VAR;
                last.name.clone_into(&mut pair.val);
            }
            result.push(pair.clone());
            if is_sort_key(&pair.key) && order_count > 1 {
                return Err(item
                    .errorf(format!(
                        "Multiple sorting only allowed by predicates. Got: {}",
                        pair.val
                    ))
                    .into());
            }
            continue;
        }

        pair.val = arg_value_head(it, item)?;

        // Get language list, if present.
        if let Ok(items) = it.peek(1) {
            if items[0].typ == ITEM_AT {
                it.advance(); // consume '@'
                it.advance(); // move forward
                let langs = super::directives::parse_language_list(it)?;
                pair.val = format!("{}@{}", pair.val, langs.join(":"));
            }
        }

        result.push(pair);
    }
    Ok(result)
}

/// Parses `var(...)` variable lists, recording `NeedsVar` entries.
pub(crate) fn parse_var_list(
    it: &mut ItemIterator,
    gq: &mut GraphQuery,
) -> Result<usize, DqlError> {
    let mut count = 0;
    let mut expect_arg = true;
    it.advance();
    let item = it.item();
    if item.typ != super::state::ITEM_LEFT_ROUND {
        return Err(item.errorf("Expected a left round after var").into());
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
                    return Err(item.errorf("Expected a variable but got comma").into());
                }
                expect_arg = true;
            }
            ITEM_NAME => {
                if !expect_arg {
                    return Err(item
                        .errorf(format!("Expected a variable but got {}", item.val))
                        .into());
                }
                count += 1;
                gq.needs_var.push(VarContext {
                    name: item.val.clone(),
                    typ: super::ast::UID_VAR,
                });
                expect_arg = false;
            }
            _ => {}
        }
    }
    if expect_arg {
        return Err(it.errorf("Unnecessary comma in val()").into());
    }
    Ok(count)
}

/// Parses `expand` type lists (`T1, T2`), leaving the `)` for the caller.
pub(crate) fn parse_type_list(it: &mut ItemIterator, gq: &mut GraphQuery) -> Result<(), DqlError> {
    let mut type_list = it.item().val.clone();
    let mut expect_arg = false;
    loop {
        if !it.advance() {
            break;
        }
        let item = it.item();
        match item.typ {
            ITEM_RIGHT_ROUND => {
                it.prev();
                break;
            }
            ITEM_COMMA => {
                if expect_arg {
                    return Err(item.errorf("Expected a variable but got comma").into());
                }
                expect_arg = true;
            }
            ITEM_NAME => {
                if !expect_arg {
                    return Err(item
                        .errorf(format!("Expected a variable but got {}", item.val))
                        .into());
                }
                type_list = format!("{type_list},{}", item.val);
                expect_arg = false;
            }
            _ => {
                return Err(item
                    .errorf(format!(
                        "Unexpected token {} when reading a type list",
                        item.val
                    ))
                    .into());
            }
        }
    }
    if expect_arg {
        return Err(it.item().errorf("Unnecessary comma in val()").into());
    }
    gq.expand = type_list;
    Ok(())
}
