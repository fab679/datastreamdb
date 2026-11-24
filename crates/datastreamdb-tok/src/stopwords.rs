//! Stopword tables + filtering. Port of `tok/stopwords.go`.
//!
//! Tables live in generated `stopwords_*` modules (bleve snowball lists).

#![warn(missing_docs)]

use std::{collections::HashMap, sync::OnceLock};

use super::analyzer::Token;

#[path = "stopwords/stopwords_a.rs"]
mod stopwords_a;
#[path = "stopwords/stopwords_b.rs"]
mod stopwords_b;
#[path = "stopwords/stopwords_c.rs"]
mod stopwords_c;
#[path = "stopwords/stopwords_d.rs"]
mod stopwords_d;

/// Looks up a language table across all generated modules.
fn lookup(lang: &str) -> Option<&'static [&'static str]> {
    stopwords_a::lookup(lang)
        .or_else(|| stopwords_b::lookup(lang))
        .or_else(|| stopwords_c::lookup(lang))
        .or_else(|| stopwords_d::lookup(lang))
}

fn sets() -> &'static HashMap<&'static str, std::collections::HashSet<&'static str>> {
    static SETS: OnceLock<HashMap<&'static str, std::collections::HashSet<&'static str>>> =
        OnceLock::new();
    SETS.get_or_init(|| {
        let mut map = HashMap::new();
        for lang in [
            "ar", "bg", "ca", "ckb", "cs", "da", "de", "el", "en", "es", "eu", "fa", "fi", "fr",
            "ga", "gl", "hi", "hu", "hy", "id", "it", "nl", "no", "pt", "ro", "ru", "sv", "tr",
        ] {
            if let Some(words) = lookup(lang) {
                map.insert(lang, words.iter().copied().collect());
            }
        }
        map
    })
}

/// Removes exact-match stopwords (unknown languages pass through).
/// Port of `filterStopwords`.
#[must_use]
pub fn filter_stopwords(lang: &str, tokens: Vec<Token>) -> Vec<Token> {
    let Some(set) = sets().get(lang) else {
        return tokens;
    };
    tokens
        .into_iter()
        .filter(|t| !set.contains(t.term.as_str()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(terms: &[&str]) -> Vec<Token> {
        terms.iter().map(|t| Token::alpha(t)).collect()
    }

    fn terms(tokens: &[Token]) -> Vec<&str> {
        tokens.iter().map(|t| t.term.as_str()).collect()
    }

    #[test]
    fn removes_stopwords() {
        // Port of TestFilterStopwords.
        let cases: &[(&str, &[&str], &[&str])] = &[
            (
                "en",
                &[
                    "the", "quick", "brown", "foxes", "jump", "over", "the", "big", "dogs",
                ],
                &["quick", "brown", "foxes", "jump", "big", "dogs"],
            ),
            (
                "es",
                &[
                    "deseándoles",
                    "muchas",
                    "alegrías",
                    "a",
                    "las",
                    "señoritas",
                    "y",
                    "los",
                    "señores",
                    "programadores",
                    "de",
                    "DataStreamDB",
                ],
                &[
                    "deseándoles",
                    "muchas",
                    "alegrías",
                    "señoritas",
                    "señores",
                    "programadores",
                    "DataStreamDB",
                ],
            ),
            ("x-klingon", &["tlhIngan", "maH!"], &["tlhIngan", "maH!"]),
            ("en", &["same"], &[]),
            ("en", &[], &[]),
            ("", &[""], &[""]),
        ];
        for (lang, input, expected) in cases {
            assert_eq!(
                terms(&filter_stopwords(lang, toks(input))),
                *expected,
                "{lang}"
            );
        }
    }
}
