//! Source-function parsing. Port of `parseSrcFn` plus the tokenizer
//! helpers from `worker/task.go` / `worker/tokens.go`: argument validation,
//! token generation, and inequality-token planning.

#![warn(missing_docs)]

use std::sync::Arc;

use datastreamdb_posting::PostingEngine;
use datastreamdb_proto::{SrcFunction, TaskQuery};
use datastreamdb_tok::tok::{Tokenizer, get_tokenizer};
use datastreamdb_types::{TypeId, Val, Value};

use crate::context::{
    FN_BETWEEN, FN_EQ, FuncType, FunctionContext, convert_value, could_apply_aggregator_on,
    needs_intersect, parse_func_type,
};
use crate::errors::{Result, TaskError};

/// Uid lists below this size filter directly instead of via the index.
pub const TYPE_FILTER_UID_LIMIT: u64 = 10;

/// Index tokens plus the boundary tokens of an inequality.
type TokenPair = (Vec<Vec<u8>>, Vec<Vec<u8>>);

/// Validates the argument count.
fn ensure_args_count(src_func: &SrcFunction, expected: usize) -> Result<()> {
    if src_func.args.len() != expected {
        return Err(TaskError::Invalid(format!(
            "function '{}' requires {expected} arguments, but got {} ({:?})",
            src_func.name,
            src_func.args.len(),
            src_func.args
        )));
    }
    Ok(())
}

/// Marks root functions (no input UID list).
fn check_root(query: &TaskQuery, fc: &mut FunctionContext) {
    match &query.uid_list {
        None => {
            fc.n = 0;
            fc.is_func_at_root = true;
        }
        Some(uids) => {
            fc.n = uids.uids.len();
        }
    }
}

/// Single language for function evaluation (`.` passes through).
pub(crate) fn lang_for_func(langs: &[String]) -> &str {
    langs.first().map_or("", String::as_str)
}

/// Whether any UID is zero.
fn check_uid_zero(uids: &[u64]) -> bool {
    uids.contains(&0)
}

/// Parses a base-0 integer (Go `strconv.ParseInt(s, 0, 64)`).
fn parse_int_auto(arg: &str) -> Result<i64> {
    let (negative, digits) = match arg.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, arg.strip_prefix('+').unwrap_or(arg)),
    };
    let (base, digits) = if let Some(hex) = digits.strip_prefix("0x") {
        (16u32, hex)
    } else if let Some(oct) = digits.strip_prefix("0o") {
        (8u32, oct)
    } else if let Some(bin) = digits.strip_prefix("0b") {
        (2u32, bin)
    } else if digits.len() > 1 && digits.starts_with('0') {
        (8u32, digits)
    } else {
        (10u32, digits)
    };
    let magnitude = i64::from_str_radix(digits, base)
        .map_err(|_| TaskError::Invalid(format!("value {arg:?} is not a number")))?;
    Ok(if negative { -magnitude } else { magnitude })
}

/// Parses a base-0 UID (Go `strconv.ParseUint(s, 0, 64)`).
fn parse_uid_auto(name: &str, arg: &str) -> Result<u64> {
    if arg.starts_with('-') {
        return Err(TaskError::Invalid(format!(
            "value {arg:?} in {name} is not a number"
        )));
    }
    let digits = arg.strip_prefix('+').unwrap_or(arg);
    let (base, digits) = if let Some(hex) = digits.strip_prefix("0x") {
        (16u32, hex)
    } else if let Some(oct) = digits.strip_prefix("0o") {
        (8u32, oct)
    } else if let Some(bin) = digits.strip_prefix("0b") {
        (2u32, bin)
    } else if digits.len() > 1 && digits.starts_with('0') {
        (8u32, digits)
    } else {
        (10u32, digits)
    };
    u64::from_str_radix(digits, base)
        .map_err(|_| TaskError::Invalid(format!("value {arg:?} in {name} is not a number")))
}

/// Chooses index-vs-data filtering for `eq` filters.
fn plan_for_eq_filter(
    engine: &PostingEngine,
    fc: &mut FunctionContext,
    pred: &str,
    uid_list: &[u64],
) {
    if check_uid_zero(uid_list) {
        fc.n = fc.tokens.len();
        return;
    }
    if (uid_list.len() as u64) < TYPE_FILTER_UID_LIMIT {
        fc.tokens.clear();
        fc.n = uid_list.len();
        return;
    }

    let mut estimated = 0u64;
    let mut got_estimate = false;
    for token in &fc.tokens {
        let count = engine.memory().stats().process_eq_predicate(pred, token);
        if count != u64::MAX && count != 0 {
            estimated += count;
            got_estimate = true;
        } else {
            break;
        }
    }
    if got_estimate && estimated == 0 {
        got_estimate = false;
    }
    if got_estimate && (uid_list.len() as u64) < estimated / TYPE_FILTER_UID_LIMIT {
        fc.tokens.clear();
        fc.n = uid_list.len();
        return;
    }
    fc.n = fc.tokens.len();
}

/// Requires a string index of the function's kind.
fn verify_string_index(engine: &PostingEngine, attr: &str, fn_type: FuncType) -> Result<String> {
    let required = match fn_type {
        FuncType::Ngram => "ngram",
        FuncType::FullTextSearch => "fulltext",
        FuncType::Match => "trigram",
        _ => "term",
    };
    let schema = engine.schema();
    if !schema.is_indexed(attr, false) {
        return Err(TaskError::Invalid(format!(
            "attribute {} is not indexed with type {required}",
            datastreamdb_x::parse_attr(attr)
        )));
    }
    let want = get_tokenizer(required).map(|tok| tok.identifier());
    for tok in schema.tokenizers(attr, false) {
        if Some(tok.identifier()) == want {
            return Ok(required.to_owned());
        }
    }
    Err(TaskError::Invalid(format!(
        "attribute {} is not indexed with type {required}",
        datastreamdb_x::parse_attr(attr)
    )))
}

/// Requires a custom tokenizer index by name.
fn verify_custom_index(engine: &PostingEngine, attr: &str, name: &str) -> bool {
    use datastreamdb_tok::tok::IDENT_CUSTOM;
    let schema = engine.schema();
    if !schema.is_indexed(attr, false) {
        return false;
    }
    schema
        .tokenizers(attr, false)
        .iter()
        .any(|tok| tok.identifier() >= IDENT_CUSTOM && tok.name() == name)
}

/// Tokenizes function arguments with the function's tokenizer.
fn get_string_tokens(args: &[String], lang: &str, fn_type: FuncType) -> Result<Vec<Vec<u8>>> {
    let lang = if lang == "." { "en" } else { lang };
    let vals: Vec<Val> = args
        .iter()
        .map(|arg| Val {
            tid: TypeId::String,
            value: Value::String(arg.clone()),
        })
        .collect();
    if fn_type == FuncType::FullTextSearch {
        return Ok(datastreamdb_tok::text::get_full_text_tokens(&vals, lang)?);
    }
    if fn_type == FuncType::Ngram {
        return Ok(datastreamdb_tok::text::get_ngram_query_tokens(&vals, lang)?);
    }
    Ok(datastreamdb_tok::tok::get_term_tokens(&vals)?)
}

/// Picks the comparison tokenizer (non-lossy for `eq`, sortable otherwise).
pub(crate) fn pick_tokenizer(
    engine: &PostingEngine,
    attr: &str,
    fname: &str,
) -> Result<Arc<dyn Tokenizer>> {
    let schema = engine.schema();
    if !schema.is_indexed(attr, false) {
        return Err(TaskError::Invalid(format!(
            "attribute {attr} is not indexed"
        )));
    }
    let tokenizers = schema.tokenizers(attr, false);
    if tokenizers.is_empty() {
        return Err(TaskError::Invalid(format!(
            "schema state not found for {attr}"
        )));
    }
    for tok in &tokenizers {
        if fname == FN_EQ {
            if !tok.is_lossy() {
                return Ok(tok.clone());
            }
        } else if tok.is_sortable() {
            return Ok(tok.clone());
        }
    }
    if fname != FN_EQ {
        return Err(TaskError::Invalid(format!(
            "attribute:{attr} does not have proper index for comparison"
        )));
    }
    if schema.type_of(attr).is_ok_and(|typ| typ == TypeId::String) {
        for tok in &tokenizers {
            if tok.identifier() != datastreamdb_tok::tok::IDENT_TRIGRAM {
                return Ok(tok.clone());
            }
        }
    }
    Ok(tokenizers[0].clone())
}

/// Tokens at/around the comparison boundary, plus the boundary tokens.
#[allow(clippy::too_many_lines)]
fn get_inequality_tokens(
    engine: &PostingEngine,
    read_ts: u64,
    attr: &str,
    fname: &str,
    lang: &str,
    ineq_values: &[Val],
) -> Result<TokenPair> {
    let tokenizer = pick_tokenizer(engine, attr, fname)?;
    let tokenizer =
        datastreamdb_tok::text::tokenizer_for_lang(tokenizer.name(), lang).unwrap_or(tokenizer);

    let mut ineq_final: Vec<Vec<u8>> = Vec::new();
    for value in ineq_values {
        let tokens = datastreamdb_tok::tok::build_tokens(value, tokenizer.as_ref())?;
        if tokens.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        let ident = tokenizer.identifier();
        let multi_ok = fname == FN_EQ
            && (ident == datastreamdb_tok::tok::IDENT_TERM
                || ident == datastreamdb_tok::tok::IDENT_FULLTEXT);
        if tokens.len() > 1 && !multi_ok {
            return Err(TaskError::Invalid(format!(
                "attribute {attr} does not have a valid tokenizer"
            )));
        }
        ineq_final.push(tokens[0].clone());
        if fname == FN_EQ {
            return Ok((vec![tokens[0].clone()], ineq_final));
        }
    }

    // Scan this tokenizer's index keys around the boundary token.
    let seek = datastreamdb_x::index_key(attr, &ineq_final[0]);
    let prefix = datastreamdb_x::index_key(attr, &[tokenizer.identifier()]);
    let mut pairs = engine
        .store()
        .scan_prefix(datastreamdb_posting::store::PARTITION, &prefix)
        .map_err(datastreamdb_posting::PostingError::from)?;
    let forward = fname == "ge" || fname == "gt" || fname == FN_BETWEEN;
    if forward {
        pairs.retain(|(key, _)| key.as_slice() >= seek.as_slice());
    } else {
        pairs.retain(|(key, _)| key.as_slice() <= seek.as_slice());
        pairs.reverse();
    }

    let bound1 = &ineq_final[0];
    let bound2 = if fname == FN_BETWEEN {
        Some(&ineq_final[1])
    } else {
        None
    };
    let mut out: Vec<Vec<u8>> = Vec::new();
    for (key, _) in &pairs {
        let Ok(parsed) = datastreamdb_x::parse(key) else {
            continue;
        };
        if parsed.has_start_uid {
            continue;
        }
        // Only entries visible at read_ts participate.
        if !chain_visible(engine, key, read_ts)? {
            continue;
        }
        let term = &parsed.term;
        if tokenizer.is_lossy() {
            if fname == FN_BETWEEN && bound2.is_some_and(|b| term > b) {
                break;
            }
            out.push(term.clone());
            continue;
        }
        match fname {
            "gt" => {
                if term > bound1 {
                    out.push(term.clone());
                }
            }
            "lt" => {
                if term < bound1 {
                    out.push(term.clone());
                }
            }
            FN_BETWEEN => {
                let bound2 = bound2.expect("between bounds");
                if term >= bound1 && term <= bound2 {
                    out.push(term.clone());
                } else {
                    break;
                }
            }
            _ => out.push(term.clone()),
        }
    }
    Ok((out, ineq_final))
}

/// Whether a chain holds anything visible at `read_ts`.
fn chain_visible(engine: &PostingEngine, key: &[u8], read_ts: u64) -> Result<bool> {
    use datastreamdb_posting::store::read_chain;
    let Some(chain) = read_chain(engine.store().as_ref(), key)? else {
        return Ok(false);
    };
    if chain.deltas.iter().any(|d| d.commit_ts <= read_ts) {
        return Ok(true);
    }
    Ok(chain
        .base
        .is_some_and(|base| !base.empty && base.commit_ts <= read_ts))
}

/// Parses a task's source function into an executable context.
///
/// # Errors
///
/// Returns [`TaskError`] on bad arguments, missing indexes, or unsupported
/// (vector) functions.
#[allow(clippy::too_many_lines)]
pub fn parse_src_fn(engine: &PostingEngine, query: &mut TaskQuery) -> Result<FunctionContext> {
    let (fn_type, fname) = parse_func_type(query.src_func.as_ref());
    let attr = query.attr.clone();
    let mut fc = FunctionContext {
        fn_type,
        fname: fname.clone(),
        ..FunctionContext::default()
    };
    let schema = engine.schema().clone();
    let is_indexed = schema.is_indexed(&attr, false);

    if let Ok(typ) = schema.type_of(&attr) {
        if fn_type != FuncType::NotAFunction && typ == TypeId::String {
            fc.is_string_fn = true;
        }
    }

    match fn_type {
        FuncType::NotAFunction => {
            fc.n = query.uid_list.as_ref().map_or(0, |list| list.uids.len());
        }
        FuncType::Aggregator => {
            let typ = schema.type_of(&attr).map_err(|_| {
                TaskError::Invalid(format!("attribute {attr:?} is not scalar-type"))
            })?;
            if !could_apply_aggregator_on(&fname, typ) {
                return Err(TaskError::Invalid(format!(
                    "aggregator {fname:?} could not apply on {}",
                    datastreamdb_x::parse_attr(&attr)
                )));
            }
            fc.n = query.uid_list.as_ref().map_or(0, |list| list.uids.len());
        }
        FuncType::CompareAttr => {
            let Some(src_func) = query.src_func.clone() else {
                return Err(TaskError::Invalid("missing src function".to_owned()));
            };
            let args = src_func.args.clone();
            if fname == FN_EQ {
                if args.is_empty() {
                    return Err(TaskError::Invalid(
                        "eq expects atleast 1 argument.".to_owned(),
                    ));
                }
            } else if fname == FN_BETWEEN {
                if args.len() != 2 {
                    return Err(TaskError::Invalid(
                        "between expects exactly 2 argument.".to_owned(),
                    ));
                }
            } else if args.len() != 1 {
                return Err(TaskError::Invalid(format!(
                    "{fname} expects only 1 argument. Got: {args:?}"
                )));
            }

            let mut generate_tokens = true;
            if fname != FN_EQ {
                if let Some(uids) = &query.uid_list {
                    if (uids.uids.len() as u64) < TYPE_FILTER_UID_LIMIT
                        && !check_uid_zero(&uids.uids)
                    {
                        fc.n = uids.uids.len();
                        generate_tokens = false;
                    }
                }
            }

            let mut idx = 0;
            while idx < args.len() {
                let value = convert_value(&schema, &attr, &args[idx])?;
                let mut ineq_values = vec![value.clone()];
                fc.eq_tokens.push(value);
                if fname == FN_BETWEEN {
                    let value2 = convert_value(&schema, &attr, &args[idx + 1])?;
                    ineq_values.push(value2.clone());
                    fc.eq_tokens.push(value2);
                    idx += 1;
                }
                idx += 1;
                if !is_indexed || !generate_tokens {
                    continue;
                }
                let lang = query.langs.first().cloned().unwrap_or_default();
                let (tokens, boundary) = get_inequality_tokens(
                    engine,
                    query.read_ts,
                    &attr,
                    &fname,
                    &lang,
                    &ineq_values,
                )?;
                fc.ineq_value_token = boundary;
                fc.tokens.extend(tokens);
            }

            if !generate_tokens {
                return Ok(fc);
            }
            match (&query.uid_list, is_indexed, fname.as_str()) {
                (Some(uids), false, _) => {
                    fc.n = uids.uids.len();
                }
                (Some(uids), true, name) if name != FN_EQ && fc.tokens.len() > uids.uids.len() => {
                    fc.tokens.clear();
                    fc.n = uids.uids.len();
                }
                (Some(uids), _, name) if name == FN_EQ && !fc.tokens.is_empty() => {
                    plan_for_eq_filter(engine, &mut fc, &attr, &uids.uids);
                }
                _ => {
                    fc.n = fc.tokens.len();
                }
            }
        }
        FuncType::CompareScalar => {
            let Some(src_func) = query.src_func.clone() else {
                return Err(TaskError::Invalid("missing src function".to_owned()));
            };
            let want = if src_func.name == FN_BETWEEN { 2 } else { 1 };
            ensure_args_count(&src_func, want)?;
            let mut thresholds = Vec::new();
            for arg in &src_func.args {
                let threshold = parse_int_auto(arg).map_err(|_| {
                    TaskError::Invalid(format!(
                        "compare {}({}) require digits, but got invalid num",
                        src_func.name, src_func.args[0]
                    ))
                })?;
                if !(0..=i64::from(u32::MAX)).contains(&threshold) {
                    return Err(TaskError::Invalid(format!(
                        "compare {}: threshold value {threshold} must be between 0 and {}",
                        src_func.name,
                        u32::MAX
                    )));
                }
                thresholds.push(threshold);
            }
            fc.threshold = thresholds;
            check_root(query, &mut fc);
        }
        FuncType::Geo => {
            let Some(src_func) = query.src_func.clone() else {
                return Err(TaskError::Invalid("missing src function".to_owned()));
            };
            let (mut tokens, geo_query) =
                datastreamdb_types::geofilter::get_geo_tokens(&src_func.name, &src_func.args)
                    .map_err(|err| TaskError::Invalid(err.to_string()))?;
            let mut raw: Vec<Vec<u8>> = tokens
                .drain(..)
                .map(std::string::String::into_bytes)
                .collect();
            datastreamdb_tok::tok::encode_geo_tokens(&mut raw);
            fc.tokens = raw;
            fc.geo_query = Some(geo_query);
            fc.n = fc.tokens.len();
        }
        FuncType::Password => {
            let Some(src_func) = query.src_func.clone() else {
                return Err(TaskError::Invalid("missing src function".to_owned()));
            };
            ensure_args_count(&src_func, 2)?;
            fc.n = query.uid_list.as_ref().map_or(0, |list| list.uids.len());
        }
        FuncType::Standard | FuncType::FullTextSearch | FuncType::Ngram => {
            let Some(src_func) = query.src_func.clone() else {
                return Err(TaskError::Invalid("missing src function".to_owned()));
            };
            ensure_args_count(&src_func, 1)?;
            verify_string_index(engine, &attr, fn_type)?;
            fc.tokens = get_string_tokens(&src_func.args, lang_for_func(&query.langs), fn_type)?;
            fc.intersect_dest = needs_intersect(&fname);
            fc.n = fc.tokens.len();
        }
        FuncType::Match => {
            let Some(src_func) = query.src_func.clone() else {
                return Err(TaskError::Invalid("missing src function".to_owned()));
            };
            ensure_args_count(&src_func, 2)?;
            verify_string_index(engine, &attr, fn_type)?;
            fc.intersect_dest = needs_intersect(&fname);
            let max = src_func.args[1].parse::<i64>().map_err(|_| {
                TaskError::Invalid(format!(
                    "levenshtein distance value must be an int, got {}",
                    src_func.args[1]
                ))
            })?;
            if max < 0 {
                return Err(TaskError::Invalid(format!(
                    "levenshtein distance value must be greater than 0, got {max}"
                )));
            }
            fc.threshold = vec![max];
            fc.tokens = vec![src_func.args[0].as_bytes().to_vec()];
            if let Some(func) = &mut query.src_func {
                func.args.truncate(1);
            }
            fc.n = fc.tokens.len();
        }
        FuncType::CustomIndex => {
            let Some(src_func) = query.src_func.clone() else {
                return Err(TaskError::Invalid("missing src function".to_owned()));
            };
            ensure_args_count(&src_func, 2)?;
            let name = src_func.args[0].clone();
            if !verify_custom_index(engine, &attr, &name) {
                return Err(TaskError::Invalid(format!(
                    "attribute {} is not indexed with custom tokenizer {name}",
                    datastreamdb_x::parse_attr(&attr)
                )));
            }
            let value = convert_value(&schema, &attr, &src_func.args[1])?;
            let tokenizer = get_tokenizer(&name)
                .ok_or_else(|| TaskError::Invalid(format!("no tokenizer {name:?}")))?;
            let lang_tok =
                datastreamdb_tok::text::tokenizer_for_lang(&name, lang_for_func(&query.langs))
                    .unwrap_or(tokenizer);
            fc.tokens = datastreamdb_tok::tok::build_tokens(&value, lang_tok.as_ref())?;
            fc.intersect_dest = needs_intersect(&fname);
            fc.n = fc.tokens.len();
        }
        FuncType::Regex => {
            let Some(src_func) = query.src_func.clone() else {
                return Err(TaskError::Invalid("missing src function".to_owned()));
            };
            ensure_args_count(&src_func, 2)?;
            let mut pattern = String::from("(?m)");
            if !src_func.args[1].is_empty() {
                if src_func.args[1] != "i" {
                    return Err(TaskError::Invalid(format!(
                        "invalid regexp modifier: {}",
                        src_func.args[1]
                    )));
                }
                pattern = String::from("(?i)(?m)");
            }
            pattern.push_str(&src_func.args[0]);
            fc.regex = Some(regex::Regex::new(&pattern)?);
            fc.n = 0;
        }
        FuncType::Has => {
            let Some(src_func) = query.src_func.clone() else {
                return Err(TaskError::Invalid("missing src function".to_owned()));
            };
            ensure_args_count(&src_func, 0)?;
            check_root(query, &mut fc);
        }
        FuncType::SimilarTo => {
            return Err(TaskError::Unsupported(
                "similar_to (vector milestone)".to_owned(),
            ));
        }
        FuncType::UidIn => {
            let Some(src_func) = query.src_func.clone() else {
                return Err(TaskError::Invalid("missing src function".to_owned()));
            };
            for arg in &src_func.args {
                fc.uids_present.push(parse_uid_auto(&src_func.name, arg)?);
            }
            fc.uids_present.sort_unstable();
            check_root(query, &mut fc);
            if fc.is_func_at_root {
                return Err(TaskError::Invalid(
                    "uid_in function not allowed at root".to_owned(),
                ));
            }
        }
    }
    Ok(fc)
}
