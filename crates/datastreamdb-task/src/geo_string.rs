//! Geo and string post-filters. Port of `filterGeoFunction` /
//! `filterStringFunction` / `getValsForUID` from `worker/task.go` and the
//! matchers from `worker/stringfilter.go`.
//!
//! Delta from Go: geo filtering runs sequentially (Go fans out).

#![warn(missing_docs)]

use datastreamdb_proto::{TaskQuery, TaskResult};
use datastreamdb_schema::SchemaState;
use datastreamdb_types::{TypeId, Val};

use crate::context::{FuncType, FunctionContext, QueryState};
use crate::errors::{Result, TaskError};
use crate::parse::lang_for_func;

/// Whether string post-filtering applies to this task.
pub(crate) fn needs_string_filtering(
    schema: &SchemaState,
    src_fn: &FunctionContext,
    langs: &[String],
    attr: &str,
) -> bool {
    if !src_fn.is_string_fn {
        return false;
    }
    if !schema.has_lang(attr) {
        return false;
    }
    lang_for_func(langs) != "."
        && matches!(
            src_fn.fn_type,
            FuncType::Standard
                | FuncType::Has
                | FuncType::FullTextSearch
                | FuncType::CompareAttr
                | FuncType::CustomIndex
                | FuncType::Ngram
        )
}

/// String matcher kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MatchKind {
    /// Token-overlap match.
    Default,
    /// Comparison match.
    Ineq,
}

/// String filter state.
struct StringFilter {
    /// Lowercased function name.
    func_name: String,
    /// Language tag.
    lang: String,
    /// Query tokens.
    tokens: Vec<Vec<u8>>,
    /// Matcher kind.
    match_kind: MatchKind,
    /// Comparison values.
    eq_vals: Vec<Val>,
    /// Tokenizer name for value tokenization.
    tok_name: String,
}

/// Keeps the UIDs whose values match the filter.
fn match_strings(uids: &[u64], values: &[Vec<Val>], filter: &StringFilter) -> Vec<u64> {
    let mut kept = Vec::new();
    for (i, row) in values.iter().enumerate() {
        for value in row {
            let matched = match filter.match_kind {
                MatchKind::Default => default_match(value, filter),
                MatchKind::Ineq => ineq_match(value, filter),
            };
            if matched {
                kept.push(uids[i]);
                break;
            }
        }
    }
    kept
}

/// Token-overlap match (`allof*` needs every token).
fn default_match(value: &Val, filter: &StringFilter) -> bool {
    let mut token_map = std::collections::HashMap::new();
    for token in &filter.tokens {
        token_map.insert(token.clone(), false);
    }
    let mut count = 0usize;
    for token in tokenize_value(value, filter) {
        if let Some(seen) = token_map.get_mut(&token) {
            if !*seen {
                *seen = true;
                count += 1;
            }
        }
    }
    if filter.func_name.starts_with("allof") {
        count == filter.tokens.len()
    } else {
        count > 0
    }
}

/// Comparison match over converted values.
fn ineq_match(value: &Val, filter: &StringFilter) -> bool {
    if filter.func_name == "eq" {
        return filter
            .eq_vals
            .iter()
            .any(|other| datastreamdb_types::compare_vals("eq", value, other).unwrap_or(false));
    }
    if filter.func_name == "between" {
        return datastreamdb_types::compare_vals("ge", value, &filter.eq_vals[0]).unwrap_or(false)
            && datastreamdb_types::compare_vals("le", value, &filter.eq_vals[1]).unwrap_or(false);
    }
    datastreamdb_types::compare_vals(&filter.func_name, value, &filter.eq_vals[0]).unwrap_or(false)
}

/// Tokenizes one value with the filter's tokenizer.
fn tokenize_value(value: &Val, filter: &StringFilter) -> Vec<Vec<u8>> {
    let Some(tokenizer) = datastreamdb_tok::tok::get_tokenizer(&filter.tok_name) else {
        return Vec::new();
    };
    let tokenizer = datastreamdb_tok::text::tokenizer_for_lang(&filter.tok_name, &filter.lang)
        .unwrap_or(tokenizer);
    datastreamdb_tok::tok::build_tokens(value, tokenizer.as_ref()).unwrap_or_default()
}

impl QueryState {
    /// Verifies geo-index candidates against stored geometries.
    pub(crate) fn filter_geo_function(
        &self,
        query: &TaskQuery,
        src_fn: &FunctionContext,
        out: &mut TaskResult,
    ) -> Result<()> {
        let merged = datastreamdb_algo::uidlist::merge_sorted(&out.uid_matrix);
        let Some(geo_query) = &src_fn.geo_query else {
            return Ok(());
        };
        let mut kept = Vec::new();
        for uid in &merged.uids {
            let list = self
                .cache
                .get(&datastreamdb_x::data_key(&query.attr, *uid))?;
            let mut matched = false;
            list.iterate(query.read_ts, 0, &mut |posting| {
                if datastreamdb_types::geofilter::match_geo(&posting.value, geo_query) {
                    matched = true;
                    return Err(datastreamdb_posting::PostingError::StopIteration);
                }
                Ok(())
            })?;
            if matched {
                kept.push(*uid);
            }
        }
        intersect_uid_matrix(out, &kept);
        Ok(())
    }

    /// Filters string rows by language-aware token match.
    pub(crate) fn filter_string_function(
        &self,
        query: &TaskQuery,
        src_fn: &FunctionContext,
        out: &mut TaskResult,
    ) -> Result<()> {
        let merged = datastreamdb_algo::uidlist::merge_sorted(&out.uid_matrix);
        let lang = lang_for_func(&query.langs).to_owned();
        let mut values: Vec<Vec<Val>> = Vec::new();
        let mut present = Vec::new();
        for uid in &merged.uids {
            let vals = match self.get_vals_for_uid(&query.attr, &lang, *uid, query.read_ts) {
                Ok(vals) => vals,
                Err(TaskError::Posting(datastreamdb_posting::PostingError::NoValue)) => continue,
                Err(err) => return Err(err),
            };
            let mut strings = Vec::new();
            for val in &vals {
                if let Ok(text) = datastreamdb_types::convert(val, TypeId::String) {
                    strings.push(text);
                }
            }
            if !strings.is_empty() {
                values.push(strings);
                present.push(*uid);
            }
        }

        let mut filter = StringFilter {
            func_name: src_fn.fname.clone(),
            lang,
            tokens: Vec::new(),
            match_kind: MatchKind::Default,
            eq_vals: Vec::new(),
            tok_name: String::new(),
        };
        let mut filtered = present.clone();
        match src_fn.fn_type {
            // `has` needs no value matching (language filtering already ran).
            FuncType::CompareAttr => {
                filter.eq_vals.clone_from(&src_fn.eq_tokens);
                filter.match_kind = MatchKind::Ineq;
                filtered = match_strings(&present, &values, &filter);
            }
            FuncType::Ngram
            | FuncType::FullTextSearch
            | FuncType::Standard
            | FuncType::CustomIndex => {
                let tok_name = match src_fn.fn_type {
                    FuncType::Ngram => "ngram",
                    FuncType::FullTextSearch => "fulltext",
                    FuncType::Standard => "term",
                    _ => query
                        .src_func
                        .as_ref()
                        .map_or("", |func| func.args.first().map_or("", String::as_str)),
                };
                filter.tokens.clone_from(&src_fn.tokens);
                tok_name.clone_into(&mut filter.tok_name);
                filtered = match_strings(&present, &values, &filter);
            }
            _ => {}
        }
        intersect_uid_matrix(out, &filtered);
        Ok(())
    }

    /// Values of one UID honoring the language tag.
    pub(crate) fn get_vals_for_uid(
        &self,
        attr: &str,
        lang: &str,
        uid: u64,
        read_ts: u64,
    ) -> Result<Vec<Val>> {
        let list = self.cache.get(&datastreamdb_x::data_key(attr, uid))?;
        if lang.is_empty() {
            if self.engine.schema().is_list(attr) {
                return Ok(list.all_values(read_ts)?);
            }
            return Ok(vec![list.value(read_ts)?]);
        }
        Ok(vec![list.value_for_tag(read_ts, lang)?])
    }
}

/// Intersects every UID row with the filtered set.
fn intersect_uid_matrix(out: &mut TaskResult, filtered: &[u64]) {
    for row in &mut out.uid_matrix {
        let mut merged = Vec::new();
        datastreamdb_algo::uidlist::intersect_with(&row.uids, filtered, &mut merged);
        row.uids = merged;
    }
}
