//! Regex and fuzzy matching. Port of `handleRegexFunction` /
//! `handleMatchFunction` / `matchFuzzy` from `worker/task.go` plus the
//! trigram-candidate logic from `worker/trigram.go` / `worker/match.go`.
//!
//! Delta from Go: trigram candidate extraction (`cindex.RegexpQuery`) is
//! deferred, so root regex/match scans the predicate's lists instead of
//! requiring a trigram index. Results are identical; large scans are slower.
//! Matching itself uses the `regex` crate (RE2-class, like `cregexp`).

#![warn(missing_docs)]

use datastreamdb_posting::PostingError;
use datastreamdb_proto::{List as ProtoList, TaskQuery, TaskResult};
use datastreamdb_types::{TypeId, Val, Value};

use crate::context::{FunctionContext, QueryState};
use crate::errors::{Result, TaskError};
use crate::parse::lang_for_func;

impl QueryState {
    /// Filters UIDs by regex match over stored values.
    pub(crate) fn handle_regex_function(
        &self,
        query: &TaskQuery,
        src_fn: &FunctionContext,
        out: &mut TaskResult,
    ) -> Result<()> {
        let schema = self.engine.schema().clone();
        let typ = schema
            .type_of(&query.attr)
            .map_err(|_| TaskError::Invalid(format!("attribute not scalar: {}", query.attr)))?;
        if !typ.is_scalar() || typ != TypeId::String {
            return Err(TaskError::Invalid(
                "regex match is allowed only on string type".to_owned(),
            ));
        }
        let Some(regex) = src_fn.regex.clone() else {
            return Err(TaskError::Invalid("missing regex".to_owned()));
        };

        let uids = match &query.uid_list {
            Some(list) => list.uids.clone(),
            None => self.scan_attr_uids(query)?,
        };
        out.uid_matrix.push(ProtoList { uids: uids.clone() });

        let is_list = schema.is_list(&query.attr);
        let lang = lang_for_func(&query.langs).to_owned();
        let mut filtered = Vec::new();
        for uid in &uids {
            let list = self
                .cache
                .get(&datastreamdb_x::data_key(&query.attr, *uid))?;
            let vals = read_match_vals(&list, query.read_ts, &lang, is_list)?;
            for val in vals {
                let Ok(text) = datastreamdb_types::convert(&val, TypeId::String) else {
                    continue;
                };
                if match_regex(&text, &regex) {
                    filtered.push(*uid);
                    break;
                }
            }
        }
        intersect_uid_matrix(out, &filtered);
        Ok(())
    }

    /// Filters UIDs by fuzzy match over stored values.
    pub(crate) fn handle_match_function(
        &self,
        query: &TaskQuery,
        src_fn: &FunctionContext,
        out: &mut TaskResult,
    ) -> Result<()> {
        if !src_fn.atype.is_scalar() {
            return Err(TaskError::Invalid(format!(
                "attribute not scalar: {}",
                query.attr
            )));
        }
        if src_fn.atype != TypeId::String {
            return Err(TaskError::Invalid(
                "fuzzy match is allowed only on string type".to_owned(),
            ));
        }
        let uids = match &query.uid_list {
            Some(list) if !list.uids.is_empty() => list.uids.clone(),
            _ => self.scan_attr_uids(query)?,
        };
        out.uid_matrix.push(ProtoList { uids: uids.clone() });

        let schema = self.engine.schema().clone();
        let is_list = schema.is_list(&query.attr);
        let lang = lang_for_func(&query.langs).to_owned();
        let match_query: Vec<u8> = src_fn.tokens.concat();
        let match_query = String::from_utf8_lossy(&match_query).into_owned();
        let max = usize::try_from(src_fn.threshold[0]).unwrap_or(usize::MAX);
        let mut filtered = Vec::new();
        for uid in &uids {
            let list = self
                .cache
                .get(&datastreamdb_x::data_key(&query.attr, *uid))?;
            let vals = read_match_vals(&list, query.read_ts, &lang, is_list)?;
            for val in vals {
                let Ok(text) = datastreamdb_types::convert(&val, TypeId::String) else {
                    continue;
                };
                if let Value::String(text) = text.value {
                    if match_fuzzy(&match_query, &text, max) {
                        filtered.push(*uid);
                        break;
                    }
                }
            }
        }
        intersect_uid_matrix(out, &filtered);
        Ok(())
    }

    /// Collects every non-empty UID of the predicate (scan fallback).
    fn scan_attr_uids(&self, query: &TaskQuery) -> Result<Vec<u64>> {
        let parsed = datastreamdb_x::parse(&datastreamdb_x::data_key(&query.attr, 1))
            .map_err(datastreamdb_posting::PostingError::from)?;
        let mut uids = Vec::new();
        let mut args = datastreamdb_posting::mem::IterateDiskArgs {
            prefix: parsed.data_prefix(),
            read_ts: query.read_ts,
            reverse: false,
            start_key: datastreamdb_x::data_key(&query.attr, query.after_uid.saturating_add(1)),
            check_inclusion: None,
            function: &mut |_, pk| {
                uids.push(pk.uid);
                Ok(())
            },
        };
        self.engine
            .memory()
            .iterate_disk(self.engine.store(), &mut args)?;
        Ok(uids)
    }
}

/// Reads match-candidate values (tagged, untagged-list, or default).
fn read_match_vals(
    list: &datastreamdb_posting::List,
    read_ts: u64,
    lang: &str,
    is_list: bool,
) -> Result<Vec<Val>> {
    if !lang.is_empty() {
        match list.value_for_tag(read_ts, lang) {
            Ok(val) => Ok(vec![val]),
            Err(PostingError::NoValue) => Ok(Vec::new()),
            Err(err) => Err(err.into()),
        }
    } else if is_list {
        Ok(list.all_untagged_values(read_ts)?)
    } else {
        match list.value(read_ts) {
            Ok(val) => Ok(vec![val]),
            Err(PostingError::NoValue) => Ok(Vec::new()),
            Err(err) => Err(err.into()),
        }
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

/// Whether a converted value is a non-empty regex match.
fn match_regex(value: &Val, regex: &regex::Regex) -> bool {
    match &value.value {
        Value::String(text) => !text.is_empty() && regex.is_match(text),
        _ => false,
    }
}

/// Whether `val` is within Levenshtein `max` of the query.
fn match_fuzzy(query: &str, val: &str, max: usize) -> bool {
    if val.is_empty() {
        return false;
    }
    levenshtein_distance(val, query) <= max
}

/// Edit distance in O(min(m,n)) space (port of `levenshteinDistance`).
fn levenshtein_distance(first: &str, second: &str) -> usize {
    let (short, long) = if first.len() > second.len() {
        (second, first)
    } else {
        (first, second)
    };
    let short: Vec<char> = short.chars().collect();
    let long: Vec<char> = long.chars().collect();
    let mut column: Vec<usize> = (0..=short.len()).collect();
    for (x, long_char) in long.iter().enumerate() {
        column[0] = x + 1;
        let mut last_diag = x;
        for (y, short_char) in short.iter().enumerate() {
            let old_diag = column[y + 1];
            let cost = usize::from(short_char != long_char);
            column[y + 1] = (column[y + 1] + 1).min(column[y] + 1).min(last_diag + cost);
            last_diag = old_diag;
        }
    }
    column[short.len()]
}
