//! Scalar reads over a list. Port of the `Value`/`PostingFor`/`Facets`
//! family from `posting/list.go`: default values, language resolution,
//! facets, and posting lookup.

#![warn(missing_docs)]

use datastreamdb_proto::{Facets, Posting, PostingList, PostingType};
use datastreamdb_types::{TypeId, Val};

use crate::errors::{PostingError, Result};
use crate::iter::iterate;
use crate::list::List;

impl List {
    /// Default value posting list at `read_ts` (newest visible entries).
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage failure.
    pub fn static_value(&self, read_ts: u64) -> Result<Option<PostingList>> {
        let inner = self.inner.read();
        Ok(find_static_value(&inner, read_ts))
    }

    /// Default (untagged) value at `read_ts`.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError::NoValue`] when absent, [`PostingError`] on
    /// storage failure.
    pub fn value(&self, read_ts: u64) -> Result<Val> {
        let inner = self.inner.read();
        let (val, found) = find_value(&inner, self.store.as_ref(), read_ts, u64::MAX)?;
        if !found {
            return Err(PostingError::NoValue);
        }
        Ok(val)
    }

    /// Value by preferred language list (see Go `ValueFor` for the fallback
    /// chain: preferred order, untagged, smallest lang UID with `.`).
    ///
    /// # Errors
    ///
    /// Returns [`PostingError::NoValue`] when absent, [`PostingError`] on
    /// storage failure.
    pub fn value_for(&self, read_ts: u64, langs: &[String]) -> Result<Val> {
        let posting = self.posting_for(read_ts, langs)?;
        Ok(value_to_types_val(&posting))
    }

    /// Posting by preferred language list.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError::NoValue`] when absent, [`PostingError`] on
    /// storage failure.
    pub fn posting_for(&self, read_ts: u64, langs: &[String]) -> Result<Posting> {
        let inner = self.inner.read();
        posting_for_langs(&inner, self.store.as_ref(), read_ts, langs)
    }

    /// Value for one language tag.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError::NoValue`] when absent, [`PostingError`] on
    /// storage failure.
    pub fn value_for_tag(&self, read_ts: u64, tag: &str) -> Result<Val> {
        let inner = self.inner.read();
        let posting = posting_for_tag(&inner, self.store.as_ref(), read_ts, tag)?;
        Ok(value_to_types_val(&posting))
    }

    /// All values in the list.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage failure.
    pub fn all_values(&self, read_ts: u64) -> Result<Vec<Val>> {
        let mut vals = Vec::new();
        self.iterate(read_ts, 0, &mut |p| {
            vals.push(value_to_types_val(p));
            Ok(())
        })?;
        Ok(vals)
    }

    /// All untagged values in the list.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage failure.
    pub fn all_untagged_values(&self, read_ts: u64) -> Result<Vec<Val>> {
        let mut vals = Vec::new();
        self.iterate(read_ts, 0, &mut |p| {
            if p.lang_tag.is_empty() {
                vals.push(value_to_types_val(p));
            }
            Ok(())
        })?;
        Ok(vals)
    }

    /// Language tags of every posting in the list.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage failure.
    pub fn lang_tags(&self, read_ts: u64) -> Result<Vec<String>> {
        let mut tags = Vec::new();
        self.iterate(read_ts, 0, &mut |p| {
            tags.push(String::from_utf8_lossy(&p.lang_tag).into_owned());
            Ok(())
        })?;
        Ok(tags)
    }

    /// Facets for the value posting (all untagged postings for lists).
    ///
    /// # Errors
    ///
    /// Returns [`PostingError::NoValue`] when absent, [`PostingError`] on
    /// storage failure.
    pub fn facets(
        &self,
        read_ts: u64,
        param: Option<&datastreamdb_proto::FacetParams>,
        langs: &[String],
        list_type: bool,
    ) -> Result<Vec<Facets>> {
        let params = to_tok_params(param);
        if list_type {
            let inner = self.inner.read();
            let mut out = Vec::new();
            iterate(&inner, self.store.as_ref(), read_ts, 0, &mut |p| {
                if p.lang_tag.is_empty() {
                    out.push(Facets {
                        facets: copy_proto_facets(&p.facets, &params),
                    });
                }
                Ok(())
            })?;
            return Ok(out);
        }
        let posting = self.posting_for(read_ts, langs)?;
        Ok(vec![Facets {
            facets: copy_proto_facets(&posting.facets, &params),
        }])
    }

    /// Finds one UID's posting.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage failure.
    pub fn find_posting(&self, read_ts: u64, uid: u64) -> Result<(bool, Option<Posting>)> {
        let inner = self.inner.read();
        find_posting(&inner, self.store.as_ref(), read_ts, uid)
    }
}

/// Converts a posting into a typed value (borrows the value bytes).
pub(crate) fn value_to_types_val(p: &Posting) -> Val {
    Val {
        tid: TypeId::try_from(p.val_type).unwrap_or(TypeId::Default),
        value: datastreamdb_types::Value::Binary(p.value.clone()),
    }
}

/// Newest visible entries without iterating (Go `findStaticValue`).
fn find_static_value(inner: &crate::list::ListInner, read_ts: u64) -> Option<PostingList> {
    let layer = &inner.mutation_map;
    if layer.current_entries().is_some() && layer.read_ts() == read_ts {
        return layer.current_entries().cloned();
    }
    if inner.max_ts <= read_ts {
        if let Some(last) = layer.last_entry() {
            return Some(last.clone());
        }
        if let Some(mutation) = layer.get(inner.max_ts) {
            return Some(mutation.clone());
        }
    }
    let mut best: Option<(u64, PostingList)> = None;
    layer.iterate(
        &mut |_, candidate| {
            let ts = candidate.commit_ts;
            if best.as_ref().is_none_or(|(found, _)| ts > *found) {
                best = Some((ts, candidate.clone()));
            }
        },
        read_ts,
    );
    if let Some((_, mutation)) = best {
        return Some(mutation);
    }
    if !inner.plist.postings.is_empty() {
        return Some(inner.plist.clone());
    }
    None
}

/// Default value lookup by UID.
fn find_value(
    inner: &crate::list::ListInner,
    store: &dyn datastreamdb_store::Backend,
    read_ts: u64,
    uid: u64,
) -> Result<(Val, bool)> {
    let (found, posting) = find_posting(inner, store, read_ts, uid)?;
    match (found, posting) {
        (true, Some(posting)) => Ok((value_to_types_val(&posting), true)),
        _ => Ok((
            Val {
                tid: TypeId::Default,
                value: datastreamdb_types::Value::Binary(Vec::new()),
            },
            false,
        )),
    }
}

/// Language fallback chain (Go `postingForLangs`).
fn posting_for_langs(
    inner: &crate::list::ListInner,
    store: &dyn datastreamdb_store::Backend,
    read_ts: u64,
    langs: &[String],
) -> Result<Posting> {
    let mut any = false;
    for lang in langs {
        if lang == "." {
            any = true;
            break;
        }
        if let Ok(posting) = posting_for_tag(inner, store, read_ts, lang) {
            return Ok(posting);
        }
    }

    if any || langs.is_empty() {
        let (found, posting) = find_posting(inner, store, read_ts, u64::MAX)?;
        if let (true, Some(posting)) = (found, posting) {
            return Ok(posting);
        }
    }

    if any {
        let mut found = None;
        iterate(inner, store, read_ts, 0, &mut |p| {
            if p.posting_type == PostingType::ValueLang as i32 {
                found = Some(p.clone());
                return Err(PostingError::StopIteration);
            }
            Ok(())
        })?;
        if let Some(posting) = found {
            return Ok(posting);
        }
    }

    Err(PostingError::NoValue)
}

/// Posting for one language tag.
fn posting_for_tag(
    inner: &crate::list::ListInner,
    store: &dyn datastreamdb_store::Backend,
    read_ts: u64,
    tag: &str,
) -> Result<Posting> {
    let uid = crate::list::fingerprint64(tag.as_bytes());
    let (found, posting) = find_posting(inner, store, read_ts, uid)?;
    match (found, posting) {
        (true, Some(posting)) => Ok(posting),
        _ => Err(PostingError::NoValue),
    }
}

/// UID lookup across the mutable layer, then the immutable pack.
pub(crate) fn find_posting(
    inner: &crate::list::ListInner,
    store: &dyn datastreamdb_store::Backend,
    read_ts: u64,
    uid: u64,
) -> Result<(bool, Option<Posting>)> {
    let (search_further, posting) = inner.mutation_map.find_posting(read_ts, uid);
    if let Some(posting) = posting {
        return Ok((true, Some(posting)));
    }
    if !search_further {
        return Ok((false, None));
    }
    // Seek the immutable iterator just before `uid` and compare.
    let mut found = None;
    let before = uid.saturating_sub(1);
    iterate(inner, store, read_ts, before, &mut |p| {
        if p.uid == uid {
            found = Some(p.clone());
        }
        Err(PostingError::StopIteration)
    })?;
    Ok((found.is_some(), found))
}

/// Converts wire facet params into the tokenizer mirror.
fn to_tok_params(
    params: Option<&datastreamdb_proto::FacetParams>,
) -> datastreamdb_tok::facets::FacetParams {
    params.map_or(datastreamdb_tok::facets::FacetParams::default(), |p| {
        datastreamdb_tok::facets::FacetParams {
            all_keys: p.all_keys,
            allowed_keys: p.param.iter().map(|fp| fp.key.clone()).collect(),
            aliases: p
                .param
                .iter()
                .filter(|fp| !fp.alias.is_empty())
                .map(|fp| (fp.key.clone(), fp.alias.clone()))
                .collect(),
        }
    })
}

/// Filters stored facets through [`copy_facets`](datastreamdb_tok::facets::copy_facets).
#[must_use]
pub fn filter_facets(
    facets: &[datastreamdb_proto::api::Facet],
    params: Option<&datastreamdb_proto::FacetParams>,
) -> Vec<datastreamdb_proto::api::Facet> {
    copy_proto_facets(facets, &to_tok_params(params))
}

/// Filters stored facets through [`copy_facets`](datastreamdb_tok::facets::copy_facets).
fn copy_proto_facets(
    facets: &[datastreamdb_proto::api::Facet],
    params: &datastreamdb_tok::facets::FacetParams,
) -> Vec<datastreamdb_proto::api::Facet> {
    use datastreamdb_tok::facets::FacetValType;
    let mirror: Vec<datastreamdb_tok::facets::Facet> = facets
        .iter()
        .map(|f| datastreamdb_tok::facets::Facet {
            key: f.key.clone(),
            value: f.value.clone(),
            val_type: match f.val_type {
                1 => FacetValType::Int,
                2 => FacetValType::Float,
                3 => FacetValType::Bool,
                4 => FacetValType::Datetime,
                _ => FacetValType::String,
            },
            tokens: f.tokens.clone(),
            alias: f.alias.clone(),
        })
        .collect();
    datastreamdb_tok::facets::copy_facets(&mirror, params)
        .into_iter()
        .map(|f| datastreamdb_proto::api::Facet {
            key: f.key,
            value: f.value,
            val_type: f.val_type as i32,
            tokens: f.tokens,
            alias: f.alias,
        })
        .collect()
}
