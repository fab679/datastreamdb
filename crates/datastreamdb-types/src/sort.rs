//! Multi-key sorting with top-N selection. Port of `types/sort.go` +
//! `types/select.go`.
//!
//! Deltas from Go: language collation uses ICU4X instead of `x/text`
//! (same tailorings, e.g. German `ö` < `z`, Swedish `ö` > `z`); the final
//! partial sort clamps `n` instead of panicking when `n` exceeds the length.

#![warn(missing_docs)]

use std::cmp::Ordering;

use super::compare::{equal, less_inner, mismatched_less};
use super::scalar::TypeId;
use super::value::{Val, Value};

/// Language collator: ICU when the tag parses, bytewise otherwise (like Go).
pub struct Collator {
    inner: Option<icu_collator::CollatorBorrowed<'static>>,
}

impl Collator {
    /// Collator for `lang` (BCP47); empty/unparseable tags sort bytewise.
    #[must_use]
    pub fn for_lang(lang: &str) -> Self {
        if lang.is_empty() {
            return Self { inner: None };
        }
        let locale: Result<icu_locale_core::Locale, _> = lang.parse();
        let Ok(locale) = locale else {
            return Self { inner: None };
        };
        let prefs = icu_collator::CollatorPreferences::from(&locale);
        let options = icu_collator::options::CollatorOptions::default();
        match icu_collator::CollatorBorrowed::try_new(prefs, options) {
            Ok(collator) => Self {
                inner: Some(collator),
            },
            Err(_) => Self { inner: None },
        }
    }

    /// Compares two strings under this collator.
    #[must_use]
    pub fn compare(&self, left: &str, right: &str) -> Ordering {
        match &self.inner {
            Some(collator) => collator.compare(left, right),
            None => left.cmp(right),
        }
    }
}

/// Sort error.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SortError {
    /// A sort key has an unsortable type.
    #[error("Value of type: {0} isn't sortable")]
    Unsortable(TypeId),
}

/// Whether values of `tid` can sort.
#[must_use]
pub fn is_sortable(tid: TypeId) -> bool {
    matches!(
        tid,
        TypeId::DateTime
            | TypeId::Int
            | TypeId::Float
            | TypeId::String
            | TypeId::Default
            | TypeId::BigFloat
    )
}

fn check_sortable(first: &[Val]) -> Result<(), SortError> {
    for val in first {
        if !is_sortable(val.tid) {
            return Err(SortError::Unsortable(val.tid));
        }
    }
    Ok(())
}

/// Single-key less-than with collation for same-tag strings.
fn less_key(a: &Val, b: &Val, collator: &Collator) -> bool {
    if a.tid != b.tid {
        return mismatched_less(a, b);
    }
    if matches!(a.tid, TypeId::String | TypeId::Default) {
        if let (Value::String(x), Value::String(y)) = (&a.value, &b.value) {
            return collator.compare(x, y) == Ordering::Less;
        }
        return false;
    }
    less_inner(a, b)
}

/// Row ordering across sort keys (`Null` sorts last in both directions).
fn compare_rows(first: &[Val], second: &[Val], desc: &[bool], collator: &Collator) -> Ordering {
    for (vidx, a) in first.iter().enumerate() {
        let b = second.get(vidx);
        // Missing keys behave like Null (Go would panic on ragged input).
        let b_null = b.is_none_or(|v| matches!(v.value, Value::Null));
        match (&a.value, b_null) {
            (Value::Null, true) => return Ordering::Equal,
            (Value::Null, false) => return Ordering::Greater,
            (_, true) => return Ordering::Less,
            _ => {}
        }
        let b = b.expect("non-null above");
        if equal(a, b) {
            continue;
        }
        // Two-sided evaluation replicates Go's Less pair exactly.
        let ord = match (less_key(a, b, collator), less_key(b, a, collator)) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            _ => Ordering::Equal,
        };
        return if desc.get(vidx).copied().unwrap_or(false) {
            ord.reverse()
        } else {
            ord
        };
    }
    Ordering::Equal
}

/// Sortable view over parallel value/uid/facet arrays.
struct SortData<'a, T> {
    values: &'a mut [Vec<Val>],
    uids: &'a mut [u64],
    facets: Option<&'a mut [T]>,
    desc: &'a [bool],
    collator: &'a Collator,
}

impl<T> SortData<'_, T> {
    fn len(&self) -> usize {
        self.values.len()
    }

    fn swap(&mut self, i: usize, j: usize) {
        self.values.swap(i, j);
        self.uids.swap(i, j);
        if let Some(facets) = self.facets.as_mut() {
            facets.swap(i, j);
        }
    }

    fn less(&self, i: usize, j: usize) -> bool {
        compare_rows(&self.values[i], &self.values[j], self.desc, self.collator) == Ordering::Less
    }

    fn is_nil(&self, i: usize) -> bool {
        self.values[i]
            .first()
            .is_none_or(|v| matches!(v.value, Value::Null))
    }
}

/// Applies `to` (old position → new position) in place via swaps.
fn apply_permutation<T>(slice: &mut [T], to: &mut [usize]) {
    debug_assert_eq!(slice.len(), to.len());
    for i in 0..slice.len() {
        while to[i] != i {
            let j = to[i];
            slice.swap(i, j);
            to.swap(i, j);
        }
    }
}

/// Sorts `values[lo..hi]` (plus parallel arrays) by the row comparator.
fn sort_window<T>(data: &mut SortData<'_, T>, lo: usize, hi: usize) {
    let mut order: Vec<usize> = (lo..hi).collect();
    order
        .sort_by(|&i, &j| compare_rows(&data.values[i], &data.values[j], data.desc, data.collator));
    // Invert new→old into old→new for the swap applier.
    let mut to = vec![0usize; hi - lo];
    for (new, &old) in order.iter().enumerate() {
        to[old - lo] = new;
    }
    apply_permutation(&mut data.values[lo..hi], &mut to.clone());
    apply_permutation(&mut data.uids[lo..hi], &mut to.clone());
    if let Some(facets) = data.facets.as_mut() {
        apply_permutation(&mut facets[lo..hi], &mut to);
    }
}

// --- quickselect (port of select.go) ---

fn insertion_sort<T>(data: &mut SortData<'_, T>, a: usize, b: usize) {
    for i in a + 1..b {
        let mut j = i;
        while j > a && data.less(j, j - 1) {
            data.swap(j, j - 1);
            j -= 1;
        }
    }
}

fn order2<T>(data: &SortData<'_, T>, a: usize, b: usize) -> (usize, usize) {
    if data.less(b, a) { (b, a) } else { (a, b) }
}

fn median<T>(data: &SortData<'_, T>, a: usize, b: usize, c: usize) -> usize {
    let (a, mut b) = order2(data, a, b);
    (b, _) = order2(data, b, c);
    (_, b) = order2(data, a, b);
    b
}

fn median_adjacent<T>(data: &SortData<'_, T>, a: usize) -> usize {
    median(data, a - 1, a, a + 1)
}

fn choose_pivot<T>(data: &SortData<'_, T>, lo: usize, hi: usize) -> usize {
    const SHORTEST_NINTHER: usize = 50;
    let len = hi - lo;
    let (mut pi, mut pj, mut pk) = (lo + len / 4, lo + len / 4 * 2, lo + len / 4 * 3);
    if len >= 8 {
        if len >= SHORTEST_NINTHER {
            pi = median_adjacent(data, pi);
            pj = median_adjacent(data, pj);
            pk = median_adjacent(data, pk);
        }
        pj = median(data, pi, pj, pk);
    }
    pj
}

fn partition<T>(data: &mut SortData<'_, T>, a: usize, b: usize, pivot: usize) -> usize {
    let mut index = a;
    data.swap(pivot, b);
    for i in a..b {
        if data.less(i, b) {
            data.swap(i, index);
            index += 1;
        }
    }
    data.swap(index, b);
    index
}

fn quick_select<T>(data: &mut SortData<'_, T>, mut low: usize, mut high: usize, k: usize) {
    loop {
        if low >= high {
            return;
        } else if high - low <= 8 {
            insertion_sort(data, low, high + 1);
            return;
        }
        let mut pivot = choose_pivot(data, low, high);
        pivot = partition(data, low, high, pivot);
        match k.cmp(&pivot) {
            std::cmp::Ordering::Less => high = pivot - 1,
            std::cmp::Ordering::Greater => low = pivot + 1,
            std::cmp::Ordering::Equal => return,
        }
    }
}

/// Sorts all rows in place, carrying UIDs (and `facets`) along.
/// Port of `SortWithFacet`.
///
/// # Errors
///
/// Returns [`SortError`] when a sort key is unsortable.
pub fn sort_with_facet<T>(
    values: &mut [Vec<Val>],
    uids: &mut [u64],
    facets: &mut [T],
    desc: &[bool],
    lang: &str,
) -> Result<(), SortError> {
    if values.is_empty() || values[0].is_empty() {
        return Ok(());
    }
    check_sortable(&values[0])?;
    let collator = Collator::for_lang(lang);
    let mut data = SortData {
        values,
        uids,
        facets: Some(facets),
        desc,
        collator: &collator,
    };
    let len = data.len();
    sort_window(&mut data, 0, len);
    Ok(())
}

/// Sorts all rows in place, carrying UIDs along. Port of `Sort`.
///
/// # Errors
///
/// Returns [`SortError`] when a sort key is unsortable.
pub fn sort_values(
    values: &mut [Vec<Val>],
    uids: &mut [u64],
    desc: &[bool],
    lang: &str,
) -> Result<(), SortError> {
    if values.is_empty() || values[0].is_empty() {
        return Ok(());
    }
    check_sortable(&values[0])?;
    let collator = Collator::for_lang(lang);
    let mut data = SortData::<()> {
        values,
        uids,
        facets: None,
        desc,
        collator: &collator,
    };
    let len = data.len();
    sort_window(&mut data, 0, len);
    Ok(())
}

/// Partially sorts so the first `n` rows hold the top-N (plus UIDs).
/// Port of `SortTopN`.
///
/// # Errors
///
/// Returns [`SortError`] when a sort key is unsortable.
pub fn sort_top_n(
    values: &mut [Vec<Val>],
    uids: &mut [u64],
    desc: &[bool],
    lang: &str,
    n: usize,
) -> Result<(), SortError> {
    if values.is_empty() || values[0].is_empty() {
        return Ok(());
    }
    check_sortable(&values[0])?;
    let collator = Collator::for_lang(lang);
    let mut data = SortData::<()> {
        values,
        uids,
        facets: None,
        desc,
        collator: &collator,
    };
    let mut nul = 0;
    for i in 0..data.uids.len() {
        if data.is_nil(i) {
            continue;
        }
        if i != nul {
            data.swap(i, nul);
        }
        nul += 1;
    }
    let n = n.min(data.len());
    if nul > n {
        quick_select(&mut data, 0, nul - 1, n);
    }
    sort_window(&mut data, 0, n);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Value, convert};

    fn input(tid: TypeId, texts: &[&str]) -> Vec<Vec<Val>> {
        texts
            .iter()
            .map(|s| {
                vec![
                    convert(
                        &Val {
                            tid: TypeId::String,
                            value: Value::Binary(s.as_bytes().to_vec()),
                        },
                        tid,
                    )
                    .expect("convert test input"),
                ]
            })
            .collect()
    }

    fn uids(n: usize) -> Vec<u64> {
        (1..=n as u64).map(|i| i * 100).collect()
    }

    fn as_strings(values: &[Vec<Val>]) -> Vec<String> {
        values
            .iter()
            .map(
                |row| match crate::marshal(row.first().expect("key"), TypeId::String) {
                    Ok(val) => match val.value {
                        Value::String(s) => s,
                        _ => panic!("expected string"),
                    },
                    Err(e) => panic!("marshal: {e}"),
                },
            )
            .collect()
    }

    #[test]
    fn sorts_strings() {
        // Port of TestSortStrings.
        let mut list = input(TypeId::String, &["bb", "aaa", "aa", "bab"]);
        let mut ul = uids(4);
        sort_values(&mut list, &mut ul, &[false], "").expect("sort");
        assert_eq!(ul, [300, 200, 400, 100]);
        assert_eq!(as_strings(&list), ["aa", "aaa", "bab", "bb"]);
    }

    #[test]
    fn sorts_with_language() {
        // Port of TestSortLanguage: German ö sorts like o, Swedish ö sorts last.
        let mut list = input(TypeId::String, &["öffnen", "zumachen"]);
        let mut ul = uids(2);
        sort_values(&mut list, &mut ul, &[false], "de").expect("sort");
        assert_eq!(ul, [100, 200]);
        assert_eq!(as_strings(&list), ["öffnen", "zumachen"]);

        let mut list = input(TypeId::String, &["öppna", "zon"]);
        let mut ul = uids(2);
        sort_values(&mut list, &mut ul, &[false], "sv").expect("sort");
        assert_eq!(ul, [200, 100]);
        assert_eq!(as_strings(&list), ["zon", "öppna"]);
    }

    #[test]
    fn sorts_numbers_and_dates() {
        // Ports of TestSortInts/Floats/FloatsDesc/DateTimes/IntAndFloat.
        let mut list = input(TypeId::Int, &["22", "111", "11", "212"]);
        let mut ul = uids(4);
        sort_values(&mut list, &mut ul, &[false], "").expect("sort");
        assert_eq!(ul, [300, 100, 200, 400]);

        let mut list = input(TypeId::Float, &["22.2", "11.2", "11.5", "2.12"]);
        let mut ul = uids(4);
        sort_values(&mut list, &mut ul, &[false], "").expect("sort");
        assert_eq!(ul, [400, 200, 300, 100]);
        assert_eq!(as_strings(&list), ["2.12", "11.2", "11.5", "22.2"]);

        let mut list = input(TypeId::Float, &["22.2", "11.2", "11.5", "2.12"]);
        let mut ul = uids(4);
        sort_values(&mut list, &mut ul, &[true], "").expect("sort");
        assert_eq!(ul, [100, 300, 200, 400]);

        let mut list = input(
            TypeId::DateTime,
            &[
                "2016-01-02T15:04:05",
                "2006-01-02T15:04:05",
                "2006-01-02T15:04:06",
                "2006-01-02T15:04:01",
            ],
        );
        let mut ul = uids(4);
        sort_values(&mut list, &mut ul, &[false], "").expect("sort");
        assert_eq!(ul, [400, 200, 300, 100]);

        let mut list = vec![
            vec![Val {
                tid: TypeId::Int,
                value: Value::Int(55),
            }],
            vec![Val {
                tid: TypeId::Float,
                value: Value::Float(21.5),
            }],
            vec![Val {
                tid: TypeId::Int,
                value: Value::Int(100),
            }],
        ];
        let mut ul = uids(3);
        sort_values(&mut list, &mut ul, &[false], "").expect("sort");
        assert_eq!(ul, [200, 100, 300]);
    }

    #[test]
    fn sorts_mismatched_types_by_group() {
        // Port of TestSortMismatchedTypes.
        let mut list = vec![
            vec![Val {
                tid: TypeId::String,
                value: Value::String("cat".to_owned()),
            }],
            vec![Val {
                tid: TypeId::Int,
                value: Value::Int(55),
            }],
            vec![Val {
                tid: TypeId::Bool,
                value: Value::Bool(true),
            }],
            vec![Val {
                tid: TypeId::Float,
                value: Value::Float(21.5),
            }],
            vec![Val {
                tid: TypeId::String,
                value: Value::String("aardvark".to_owned()),
            }],
            vec![Val {
                tid: TypeId::String,
                value: Value::String("buffalo".to_owned()),
            }],
            vec![Val {
                tid: TypeId::Float,
                value: Value::Float(33.33),
            }],
        ];
        let mut ul = uids(7);
        sort_values(&mut list, &mut ul, &[false], "").expect("sort");
        let pos = |uid: u64| ul.iter().position(|&u| u == uid).expect("uid");
        assert!(pos(500) < pos(600) && pos(600) < pos(100));
        assert!(pos(400) < pos(700) && pos(700) < pos(200));
    }

    #[test]
    fn top_n_partitions() {
        // Port of TestQuickSelect (deterministic data).
        let texts: Vec<String> = (0..10000)
            .map(|i| format!("{}", (i * 7919) % 100_000))
            .collect();
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let mut list = input(TypeId::Int, &refs);
        let mut ul = uids(10000);
        sort_top_n(&mut list, &mut ul, &[false], "", 10).expect("top-n");
        let head: Vec<i64> = list
            .iter()
            .take(10)
            .map(|row| match row[0].value {
                Value::Int(v) => v,
                _ => panic!("int"),
            })
            .collect();
        let tail: Vec<i64> = list
            .iter()
            .skip(10)
            .map(|row| match row[0].value {
                Value::Int(v) => v,
                _ => panic!("int"),
            })
            .collect();
        for h in &head {
            for t in &tail {
                assert!(h <= t, "{h} <= {t}");
            }
        }
        // Head itself is sorted.
        let mut sorted = head.clone();
        sorted.sort_unstable();
        assert_eq!(head, sorted);
    }

    #[test]
    fn rejects_unsortable() {
        let mut list = input(TypeId::Bool, &["true"]);
        let mut ul = uids(1);
        assert_eq!(
            sort_values(&mut list, &mut ul, &[false], ""),
            Err(SortError::Unsortable(TypeId::Bool))
        );
        // Empty inputs are no-ops.
        let mut empty: Vec<Vec<Val>> = Vec::new();
        sort_values(&mut empty, &mut [], &[false], "").expect("empty");
    }
}
