//! `@facets` list and filter parsing.

#![warn(missing_docs)]

use std::collections::{HashMap, HashSet};

use datastreamdb_lex::ItemIterator;
use datastreamdb_proto::{FacetParam, FacetParams};

use super::args::{try_parse_item_type, try_skip_item_val};
use super::ast::{FacetOrder, FilterTree};
use super::filter::parse_filter;
use super::state::{ITEM_COLON, ITEM_COMMA, ITEM_LEFT_ROUND, ITEM_NAME, ITEM_RIGHT_ROUND};
use crate::DqlError;

/// `@facets` parse result: selection, filter, var map, or orderings.
pub(crate) struct FacetRes {
    /// Facet selection (list form).
    pub f: Option<FacetParams>,
    /// Facet filter (filter form).
    pub ft: Option<FilterTree>,
    /// Facet-name to variable mappings.
    pub vmap: HashMap<String, String>,
    /// Facet orderings.
    pub facets_order: Vec<FacetOrder>,
}

struct FacetItem {
    name: String,
    alias: String,
    var_name: String,
    ordered: bool,
    order_desc: bool,
}

/// Parses `@facets...` as either a list or a filter.
pub(crate) fn parse_facets(it: &mut ItemIterator) -> Result<FacetRes, DqlError> {
    let (res, ok) = try_parse_facet_list(it)?;
    if ok {
        return Ok(res);
    }
    let filter = parse_filter(it)?;
    Ok(FacetRes {
        f: None,
        ft: filter,
        vmap: HashMap::new(),
        facets_order: Vec::new(),
    })
}

/// Parses `[order|alias:] [var as] name`. Returns `None` (restoring the
/// iterator) when the input is not a facet item.
fn try_parse_facet_item(it: &mut ItemIterator) -> Result<Option<FacetItem>, DqlError> {
    // We parse this:
    // [{orderdesc|orderasc|alias}:] [varname as] name
    let save = it.save();
    let mut res = FacetItem {
        name: String::new(),
        alias: String::new(),
        var_name: String::new(),
        ordered: false,
        order_desc: false,
    };

    let Some(item) = try_parse_item_name(it) else {
        it.restore(save);
        return Ok(None);
    };
    let is_order_asc = item.val == "orderasc";
    if try_skip_item_colon(it) {
        if is_order_asc || item.val == "orderdesc" {
            res.ordered = true;
            res.order_desc = !is_order_asc;
        } else {
            item.val.clone_into(&mut res.alias);
        }
        // Step past colon.
        let Some(item) = try_parse_item_name(it) else {
            let (peeked, _) = it.peek_one();
            return Err(peeked.errorf("Expected name after colon").into());
        };
        let name1 = item.val.clone();
        return finish_facet_item(it, save, res, name1);
    }

    // No colon: the consumed name starts the item.
    let name1 = item.val.clone();
    finish_facet_item(it, save, res, name1)
}

fn try_parse_item_name(it: &mut ItemIterator) -> Option<datastreamdb_lex::Item> {
    let (item, ok) = it.peek_one();
    if !ok || item.typ != ITEM_NAME {
        return None;
    }
    it.advance();
    Some(item)
}

fn try_skip_item_colon(it: &mut ItemIterator) -> bool {
    let (item, ok) = it.peek_one();
    if !ok || item.typ != ITEM_COLON {
        return false;
    }
    it.advance();
    true
}

fn finish_facet_item(
    it: &mut ItemIterator,
    save: isize,
    mut res: FacetItem,
    name1: String,
) -> Result<Option<FacetItem>, DqlError> {
    // Now try to consume "as".
    if !try_skip_item_val(it, "as") {
        res.name = super::args::collect_name(it, &name1);
        return Ok(Some(res));
    }
    let Some(item) = try_parse_item_name(it) else {
        let (peeked, _) = it.peek_one();
        return Err(peeked.errorf("Expected name in facet list").into());
    };
    res.name = super::args::collect_name(it, &item.val);
    res.var_name = name1;
    let _ = save;
    Ok(Some(res))
}

/// Sorts and deduplicates on `)`, reporting whether the list closed.
fn try_close_facet_list(it: &mut ItemIterator, facets: &mut FacetParams) -> bool {
    if try_parse_item_type(it, ITEM_RIGHT_ROUND).is_none() {
        return false;
    }
    facets.param.sort_by(|a, b| a.key.cmp(&b.key));
    // Deduplicate facets, keeping the last of each key run like Go.
    let mut out: Vec<FacetParam> = Vec::with_capacity(facets.param.len());
    for param in std::mem::take(&mut facets.param) {
        if out.last().is_some_and(|prev| prev.key == param.key) {
            *out.last_mut().expect("checked") = param;
        } else {
            out.push(param);
        }
    }
    facets.param = out;
    true
}

/// Parses `(...)`, `()`, or bare `@facets`. Returns `None` (restoring the
/// iterator) when a filter parse should be attempted instead.
fn try_parse_facet_list(it: &mut ItemIterator) -> Result<(FacetRes, bool), DqlError> {
    let save = it.save();
    let empty = || FacetRes {
        f: None,
        ft: None,
        vmap: HashMap::new(),
        facets_order: Vec::new(),
    };

    // Skip past '('.
    if try_parse_item_type(it, ITEM_LEFT_ROUND).is_none() {
        it.restore(save);
        return Ok((
            FacetRes {
                f: Some(FacetParams {
                    all_keys: true,
                    param: Vec::new(),
                }),
                ft: None,
                vmap: HashMap::new(),
                facets_order: Vec::new(),
            },
            true,
        ));
    }

    let mut facet_var: HashMap<String, String> = HashMap::new();
    let mut facets = FacetParams {
        all_keys: false,
        param: Vec::new(),
    };
    let mut facets_order: Vec<FacetOrder> = Vec::new();

    if try_parse_item_type(it, ITEM_RIGHT_ROUND).is_some() {
        // `@facets()` parses to an empty set of facets.
        return Ok((
            FacetRes {
                f: Some(facets),
                ft: None,
                vmap: facet_var,
                facets_order,
            },
            true,
        ));
    }

    let mut order_keys = HashSet::new();
    loop {
        // We've just consumed a leftRound or a comma.
        let parsed = try_parse_facet_item(it)?;
        let Some(facet_item) = parsed else {
            it.restore(save);
            return Ok((empty(), false));
        };

        if !facet_item.var_name.is_empty() {
            if facet_var.contains_key(&facet_item.name) {
                // Go aliases the live iterator here, so the error points at
                // the current position.
                return Err(it
                    .errorf(format!(
                        "Duplicate variable mappings for facet {}",
                        facet_item.name
                    ))
                    .into());
            }
            facet_var.insert(facet_item.name.clone(), facet_item.var_name.clone());
        }
        facets.param.push(FacetParam {
            key: facet_item.name.clone(),
            alias: facet_item.alias.clone(),
        });
        if facet_item.ordered {
            if !order_keys.insert(facet_item.name.clone()) {
                return Err(it
                    .errorf(format!(
                        "Sorting by facet: [{}] can only be done once",
                        facet_item.name
                    ))
                    .into());
            }
            facets_order.push(FacetOrder {
                key: facet_item.name.clone(),
                desc: facet_item.order_desc,
            });
        }

        // Now what? Either close-paren or a comma.
        if try_close_facet_list(it, &mut facets) {
            return Ok((
                FacetRes {
                    f: Some(facets),
                    ft: None,
                    vmap: facet_var,
                    facets_order,
                },
                true,
            ));
        }
        if try_parse_item_type(it, ITEM_COMMA).is_none() {
            if facets.param.len() < 2 {
                // Only consumed `'@facets' '(' <facetItem>`: a filter parse
                // might succeed. Return no-parse, no-error.
                it.restore(save);
                return Ok((empty(), false));
            }
            // Definitely not a filter: error.
            let (peeked, _) = it.peek_one();
            return Err(peeked
                .errorf(format!("Expected ',' or ')' in facet list: {}", peeked.val))
                .into());
        }
    }
}
