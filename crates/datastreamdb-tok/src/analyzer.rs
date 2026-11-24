//! Text analysis: word segmentation, lowercasing, NFKC.
//!
//! Replaces bleve's `unicode` tokenizer + `to_lower` + `normalize_unicode`
//! filters. Segmentation follows UAX #29 via `unicode-segmentation` (the same
//! standard as bleve's `segment` package); CJK ideographic detection uses
//! script ranges.

#![warn(missing_docs)]

use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

/// One analyzed token: normalized term, byte offset, and CJK class.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    /// Lowercased, NFKC-normalized term text.
    pub term: String,
    /// Byte offset of the term in the source.
    pub position: usize,
    /// Whether the term is CJK ideographic (Han, Hiragana, Katakana).
    pub ideographic: bool,
}

impl Token {
    /// Plain (non-ideographic) token for tests and term lists.
    #[must_use]
    pub fn alpha(term: &str) -> Self {
        Self {
            term: term.to_owned(),
            position: 0,
            ideographic: false,
        }
    }
}

/// Whether `text` is CJK ideographic (all Han/Hiragana/Katakana).
fn is_ideographic(text: &str) -> bool {
    fn ideographic_char(c: char) -> bool {
        matches!(c,
            '\u{3400}'..='\u{4DBF}' // CJK ext A
            | '\u{4E00}'..='\u{9FFF}' // CJK unified
            | '\u{F900}'..='\u{FAFF}' // CJK compat
            | '\u{20000}'..='\u{2A6DF}' // CJK ext B
            | '\u{2A6E0}'..='\u{2B739}' // CJK ext C-E
            | '\u{2B740}'..='\u{2B81D}' // CJK ext F
            | '\u{2B820}'..='\u{2CEAF}' // CJK ext G
            | '\u{2CEB0}'..='\u{2EBE0}' // CJK ext H
            | '\u{2F800}'..='\u{2FA1D}' // CJK compat supplement
            | '\u{3040}'..='\u{309F}' // Hiragana
            | '\u{30A0}'..='\u{30FF}' // Katakana
            | '\u{31F0}'..='\u{31FF}' // Katakana ext
            | '\u{FF66}'..='\u{FF9F}' // halfwidth Katakana
        )
    }
    !text.is_empty() && text.chars().all(ideographic_char)
}

/// Segments `text` into normalized word tokens (drops spaces/punctuation).
#[must_use]
pub fn analyze(text: &str) -> Vec<Token> {
    text.split_word_bound_indices()
        .filter(|(_, word)| word.chars().any(char::is_alphanumeric))
        .map(|(position, word)| {
            let term: String = word.to_lowercase().nfkc().collect();
            Token {
                term: term.clone(),
                position,
                ideographic: is_ideographic(&term),
            }
        })
        .collect()
}

/// Sorts + dedups terms (Go `x.RemoveDuplicates`).
#[must_use]
pub fn unique_terms(mut terms: Vec<String>) -> Vec<String> {
    terms.sort();
    terms.dedup();
    terms
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_and_normalizes() {
        let tokens = analyze("Hello, WORLD! 123");
        assert_eq!(
            tokens.iter().map(|t| t.term.as_str()).collect::<Vec<_>>(),
            ["hello", "world", "123"]
        );
        assert_eq!(tokens[0].position, 0);
        // Contractions stay together (UAX #29).
        assert_eq!(analyze("don't")[0].term, "don't");
        // Full-width forms normalize.
        assert_eq!(analyze("Ａ")[0].term, "a");
    }

    #[test]
    fn classifies_cjk() {
        assert!(analyze("他")[0].ideographic);
        assert!(analyze("あ")[0].ideographic);
        assert!(analyze("ア")[0].ideographic);
        assert!(!analyze("한")[0].ideographic); // Hangul: word, not ideographic
        assert!(!analyze("fox")[0].ideographic);
        assert!(analyze("").is_empty());
    }

    #[test]
    fn dedups_sorted() {
        assert_eq!(
            unique_terms(vec!["b".to_owned(), "a".to_owned(), "b".to_owned()]),
            ["a", "b"]
        );
    }
}
