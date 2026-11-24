//! String tokenizers: term, exact, full-text, n-gram, trigram, hash.
//! Port of the `tok/tok.go` string half.

#![warn(missing_docs)]

use std::collections::HashSet;
use std::sync::Arc;

use datastreamdb_types::{Val, Value};

use super::analyzer::{Token, analyze, unique_terms};
use super::lang::{DEFAULT_LANG, lang_base};
use super::stemmers::filter_stemmers;
use super::stopwords::filter_stopwords;
use super::tok::{
    IDENT_DELIMITER, IDENT_EXACT, IDENT_EXACT_LANG, IDENT_FULLTEXT, IDENT_HASH, IDENT_NGRAM,
    IDENT_SHA, IDENT_TERM, IDENT_TRIGRAM, TokError, Tokenizer, build_tokens, encode_token,
    get_tokenizer, require_string,
};

/// Languages tokenized by whitespace split (no spaces as delimiters).
fn is_cjk_split_lang(lang: &str) -> bool {
    matches!(lang, "zh" | "ja" | "th" | "lo" | "my" | "bo" | "km" | "kxm")
}

/// blake2b-256 digest (Go `blake2b.Sum256`).
fn blake2b_256(data: &[u8]) -> Vec<u8> {
    use blake2::digest::consts::U32;
    use blake2::{Blake2b, Digest};
    let mut hasher = Blake2b::<U32>::new();
    hasher.update(data);
    hasher.finalize().to_vec()
}

/// Term tokenizer (optional language for CJK splitting).
#[derive(Debug, Default)]
pub struct TermTokenizer {
    /// Language tag (base extracted per value).
    pub lang: String,
}

/// Exact tokenizer (optional language collation).
#[derive(Default)]
pub struct ExactTokenizer {
    lang_base: String,
    collator: Option<icu_collator::CollatorBorrowed<'static>>,
}

impl ExactTokenizer {
    /// Exact tokenizer with language collation (falls back to English).
    ///
    /// # Panics
    ///
    /// Never panics in practice: the English locale always parses and compiled
    /// collation data is always present.
    #[must_use]
    pub fn for_lang(lang: &str) -> Self {
        let lang_base = lang_base(lang);
        let normalized = lang.replace('_', "-");
        let locale = icu_locale_core::Locale::try_from_str(normalized.as_str())
            .or_else(|_| icu_locale_core::Locale::try_from_str(DEFAULT_LANG))
            .expect("en locale parses");
        let prefs = icu_collator::CollatorPreferences::from(&locale);
        let options = icu_collator::options::CollatorOptions::default();
        let collator = icu_collator::CollatorBorrowed::try_new(prefs, options)
            .expect("compiled collation data present");
        Self {
            lang_base,
            collator: Some(collator),
        }
    }

    /// Index prefix (identifier, plus language for collated instances).
    #[must_use]
    pub fn prefix(&self) -> Vec<u8> {
        if self.collator.is_none() {
            return vec![IDENT_EXACT];
        }
        let mut prefix = vec![IDENT_EXACT_LANG];
        prefix.extend_from_slice(self.lang_base.as_bytes());
        prefix.push(IDENT_DELIMITER);
        prefix
    }
}

/// Full-text tokenizer (optional language).
#[derive(Debug, Default)]
pub struct FullTextTokenizer {
    /// Language tag.
    pub lang: String,
}

/// N-gram tokenizer (optional language).
#[derive(Debug, Default)]
pub struct NGramTokenizer {
    /// Language tag.
    pub lang: String,
}

/// Trigram tokenizer.
#[derive(Debug, Default)]
pub struct TrigramTokenizer;
/// Hash tokenizer.
#[derive(Debug, Default)]
pub struct HashTokenizer;
/// SHA tokenizer.
#[derive(Debug, Default)]
pub struct Sha256Tokenizer;

impl Tokenizer for TermTokenizer {
    fn name(&self) -> &'static str {
        "term"
    }
    fn tokenizer_type(&self) -> &'static str {
        "string"
    }
    fn tokens(&self, value: &Val) -> Result<Vec<Vec<u8>>, TokError> {
        let Value::String(text) = &value.value else {
            return Ok(vec![Vec::new()]);
        };
        if text.is_empty() {
            return Ok(vec![Vec::new()]);
        }
        if is_cjk_split_lang(&lang_base(&self.lang)) {
            return Ok(unique_terms(text.split(' ').map(str::to_owned).collect())
                .into_iter()
                .map(String::into_bytes)
                .collect());
        }
        Ok(
            unique_terms(analyze(text).into_iter().map(|t| t.term).collect())
                .into_iter()
                .map(String::into_bytes)
                .collect(),
        )
    }
    fn identifier(&self) -> u8 {
        IDENT_TERM
    }
    fn is_sortable(&self) -> bool {
        false
    }
    fn is_lossy(&self) -> bool {
        true
    }
}

impl Tokenizer for ExactTokenizer {
    fn name(&self) -> &'static str {
        "exact"
    }
    fn tokenizer_type(&self) -> &'static str {
        "string"
    }
    fn tokens(&self, value: &Val) -> Result<Vec<Vec<u8>>, TokError> {
        let Value::String(text) = &value.value else {
            return Err(TokError::ExactNonString);
        };
        match &self.collator {
            None => Ok(vec![text.as_bytes().to_vec()]),
            Some(collator) => {
                let mut key = Vec::new();
                collator
                    .write_sort_key_to(text, &mut key)
                    .expect("vec sink is infallible");
                let mut term = self.lang_base.as_bytes().to_vec();
                term.push(IDENT_DELIMITER);
                term.extend_from_slice(&key);
                Ok(vec![term])
            }
        }
    }
    fn identifier(&self) -> u8 {
        if self.collator.is_none() {
            IDENT_EXACT
        } else {
            IDENT_EXACT_LANG
        }
    }
    fn is_sortable(&self) -> bool {
        true
    }
    fn is_lossy(&self) -> bool {
        false
    }
}

impl Tokenizer for FullTextTokenizer {
    fn name(&self) -> &'static str {
        "fulltext"
    }
    fn tokenizer_type(&self) -> &'static str {
        "string"
    }
    fn tokens(&self, value: &Val) -> Result<Vec<Vec<u8>>, TokError> {
        let Value::String(text) = &value.value else {
            return Ok(Vec::new());
        };
        if text.is_empty() {
            return Ok(Vec::new());
        }
        let lang = lang_base(&self.lang);
        let tokens = analyze(text);
        let tokens = filter_stopwords(&lang, tokens);
        let tokens = filter_stemmers(&lang, tokens);
        Ok(unique_terms(tokens.into_iter().map(|t| t.term).collect())
            .into_iter()
            .map(String::into_bytes)
            .collect())
    }
    fn identifier(&self) -> u8 {
        IDENT_FULLTEXT
    }
    fn is_sortable(&self) -> bool {
        false
    }
    fn is_lossy(&self) -> bool {
        true
    }
}

impl NGramTokenizer {
    fn process_tokens(&self, value: &Val) -> Vec<Token> {
        let Value::String(text) = &value.value else {
            return Vec::new();
        };
        if text.is_empty() {
            return Vec::new();
        }
        let lang = lang_base(&self.lang);
        let tokens = analyze(text);
        let tokens = filter_stopwords(&lang, tokens);
        filter_stemmers(&lang, tokens)
    }

    fn generate_ngrams(
        tokens: &[Token],
        grams: impl Fn(&[Token], &mut dyn FnMut(String)),
    ) -> Vec<Vec<u8>> {
        if tokens.is_empty() {
            return Vec::new();
        }
        let mut shingled = HashSet::new();
        let mut add = |token: String| {
            // Go hashes by BYTE length; our terms are UTF-8 so this matches.
            if token.len() < 30 {
                shingled.insert(token.into_bytes());
            } else {
                shingled.insert(blake2b_256(token.as_bytes()));
            }
        };
        grams(tokens, &mut add);
        shingled.into_iter().collect()
    }

    /// Query-time trigrams (or fewer for short input).
    ///
    /// # Errors
    ///
    /// Returns [`TokError`] on tokenizer failure (never today; kept for symmetry).
    #[allow(
        clippy::unnecessary_wraps,
        reason = "API symmetry with Tokenizer::tokens"
    )]
    pub fn query_tokens(&self, value: &Val) -> Result<Vec<Vec<u8>>, TokError> {
        let tokens = self.process_tokens(value);
        if tokens.is_empty() {
            return Ok(Vec::new());
        }
        Ok(Self::generate_ngrams(&tokens, |tokens, add| {
            let gram = tokens.len().min(3);
            for window in tokens.windows(gram) {
                add(window
                    .iter()
                    .map(|t| t.term.as_str())
                    .collect::<Vec<_>>()
                    .join(" "));
            }
        }))
    }
}

impl Tokenizer for NGramTokenizer {
    fn name(&self) -> &'static str {
        "ngram"
    }
    fn tokenizer_type(&self) -> &'static str {
        "string"
    }
    fn tokens(&self, value: &Val) -> Result<Vec<Vec<u8>>, TokError> {
        let tokens = self.process_tokens(value);
        if tokens.is_empty() {
            return Ok(Vec::new());
        }
        Ok(Self::generate_ngrams(&tokens, |tokens, add| {
            let n = tokens.len();
            for i in 0..n {
                add(tokens[i].term.clone());
                if i + 1 < n {
                    add(format!("{} {}", tokens[i].term, tokens[i + 1].term));
                }
                if i + 2 < n {
                    add(format!(
                        "{} {} {}",
                        tokens[i].term,
                        tokens[i + 1].term,
                        tokens[i + 2].term
                    ));
                }
                if i + 3 < n {
                    add(format!(
                        "{} {} {} {}",
                        tokens[i].term,
                        tokens[i + 1].term,
                        tokens[i + 2].term,
                        tokens[i + 3].term
                    ));
                }
            }
        }))
    }
    fn identifier(&self) -> u8 {
        IDENT_NGRAM
    }
    fn is_sortable(&self) -> bool {
        false
    }
    fn is_lossy(&self) -> bool {
        true
    }
}

impl Tokenizer for TrigramTokenizer {
    fn name(&self) -> &'static str {
        "trigram"
    }
    fn tokenizer_type(&self) -> &'static str {
        "string"
    }
    fn tokens(&self, value: &Val) -> Result<Vec<Vec<u8>>, TokError> {
        let text = require_string("trigram", value).map_err(|_| TokError::TrigramNonString)?;
        let bytes = text.as_bytes();
        if bytes.len() < 3 {
            return Ok(Vec::new());
        }
        let mut tokens: Vec<Vec<u8>> = (0..=bytes.len() - 3)
            .map(|i| bytes[i..i + 3].to_vec())
            .collect();
        tokens.sort();
        tokens.dedup();
        Ok(tokens)
    }
    fn identifier(&self) -> u8 {
        IDENT_TRIGRAM
    }
    fn is_sortable(&self) -> bool {
        false
    }
    fn is_lossy(&self) -> bool {
        true
    }
}

impl Tokenizer for HashTokenizer {
    fn name(&self) -> &'static str {
        "hash"
    }
    fn tokenizer_type(&self) -> &'static str {
        "string"
    }
    fn tokens(&self, value: &Val) -> Result<Vec<Vec<u8>>, TokError> {
        let text = require_string("hash", value).map_err(|_| TokError::HashNonString)?;
        Ok(vec![blake2b_256(text.as_bytes())])
    }
    fn identifier(&self) -> u8 {
        IDENT_HASH
    }
    fn is_sortable(&self) -> bool {
        false
    }
    fn is_lossy(&self) -> bool {
        false
    }
}

impl Tokenizer for Sha256Tokenizer {
    fn name(&self) -> &'static str {
        "sha256"
    }
    fn tokenizer_type(&self) -> &'static str {
        "string"
    }
    fn tokens(&self, value: &Val) -> Result<Vec<Vec<u8>>, TokError> {
        match &value.value {
            Value::String(text) if text.len() >= 64 => Ok(vec![text.as_bytes()[..64].to_vec()]),
            _ => Ok(Vec::new()),
        }
    }
    fn identifier(&self) -> u8 {
        IDENT_SHA
    }
    fn is_sortable(&self) -> bool {
        false
    }
    fn is_lossy(&self) -> bool {
        false
    }
}

/// Tokenizer with language applied (fresh instances; no shared mutation).
/// Port of `GetTokenizerForLang` (by name to avoid downcasting).
#[must_use]
pub fn tokenizer_for_lang(name: &str, lang: &str) -> Option<Arc<dyn Tokenizer>> {
    if lang.is_empty() {
        return get_tokenizer(name);
    }
    match name {
        "fulltext" => Some(Arc::new(FullTextTokenizer {
            lang: lang.to_owned(),
        })),
        "term" => Some(Arc::new(TermTokenizer {
            lang: lang.to_owned(),
        })),
        "ngram" => Some(Arc::new(NGramTokenizer {
            lang: lang.to_owned(),
        })),
        "exact" => Some(Arc::new(ExactTokenizer::for_lang(lang))),
        _ => get_tokenizer(name),
    }
}

/// N-gram tokens for one value. Port of `GetNGramTokens`
/// (empty args error instead of Go's index panic).
///
/// # Errors
///
/// Returns [`TokError`] on empty args or tokenizer failure.
pub fn get_ngram_tokens(args: &[Val], lang: &str) -> Result<Vec<Vec<u8>>, TokError> {
    let first = args.first().ok_or(TokError::Arity(0))?;
    build_tokens(
        first,
        &NGramTokenizer {
            lang: lang.to_owned(),
        },
    )
}

/// Query-time n-gram tokens. Port of `GetNGramQueryTokens`
/// (empty args error instead of Go's index panic).
///
/// # Errors
///
/// Returns [`TokError`] on empty args or tokenizer failure.
pub fn get_ngram_query_tokens(args: &[Val], lang: &str) -> Result<Vec<Vec<u8>>, TokError> {
    let first = args.first().ok_or(TokError::Arity(0))?;
    let tokenizer = NGramTokenizer {
        lang: lang.to_owned(),
    };
    let id = tokenizer.identifier();
    Ok(tokenizer
        .query_tokens(first)?
        .into_iter()
        .map(|t| encode_token(&t, id))
        .collect())
}

/// Full-text tokens for one value. Port of `GetFullTextTokens`.
///
/// # Errors
///
/// Returns [`TokError`] on wrong arity or tokenizer failure.
pub fn get_full_text_tokens(args: &[Val], lang: &str) -> Result<Vec<Vec<u8>>, TokError> {
    if args.len() != 1 {
        return Err(TokError::Arity(args.len()));
    }
    build_tokens(
        &args[0],
        &FullTextTokenizer {
            lang: lang.to_owned(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use datastreamdb_types::{TypeId, Val, Value};

    fn text(value: &str) -> Val {
        Val {
            tid: TypeId::String,
            value: Value::String(value.to_owned()),
        }
    }

    fn as_strings(tokens: &[Vec<u8>]) -> Vec<String> {
        tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).into_owned())
            .collect()
    }

    fn encoded(terms: &[&str], id: u8) -> Vec<Vec<u8>> {
        terms
            .iter()
            .map(|t| encode_token(t.as_bytes(), id))
            .collect()
    }

    #[test]
    fn fulltext_vectors() {
        // Port of TestFullTextTokenizer.
        let tokenizer = FullTextTokenizer {
            lang: String::new(),
        };
        let tokens = build_tokens(&text("Stemming works!"), &tokenizer).expect("tokens");
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens, encoded(&["stem", "work"], IDENT_FULLTEXT));
    }

    #[test]
    fn fulltext_lang_vectors() {
        // Port of TestFullTextTokenizerLang (sorted + unique).
        let tokenizer = FullTextTokenizer {
            lang: "de".to_owned(),
        };
        let tokens = build_tokens(&text("Katzen und Auffassung und Auffassung"), &tokenizer)
            .expect("tokens");
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens, encoded(&["auffassung", "katz"], IDENT_FULLTEXT));
    }

    #[test]
    fn fulltext_get_vectors() {
        // Ports of TestGetFullTextTokens/1/InvalidLang.
        let val = "Our chief weapon is surprise...surprise and fear...fear and surprise...."
            .to_owned()
            + "Our two weapons are fear and surprise...and ruthless efficiency.... "
            + "Our three weapons are fear, surprise, and ruthless efficiency...";
        let tokens = FullTextTokenizer {
            lang: "en".to_owned(),
        }
        .tokens(&text(&val))
        .expect("tokens");
        let mut expected = [
            "chief", "weapon", "surpris", "fear", "ruthless", "effici", "two", "three",
        ]
        .to_vec();
        expected.sort_unstable();
        assert_eq!(as_strings(&tokens), expected);

        let tokens = get_full_text_tokens(&[text("Quick brown fox")], "en").expect("tokens");
        assert_eq!(tokens.len(), 3);
        let tokens =
            get_full_text_tokens(&[text("Quick brown fox")], "xxx_such_language").expect("tokens");
        assert_eq!(tokens.len(), 3);
    }

    #[test]
    fn term_vectors() {
        // Port of TestTermTokenizer.
        let tokens = build_tokens(&text("Tokenizer works works!"), &TermTokenizer::default())
            .expect("tokens");
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens, encoded(&["tokenizer", "works"], IDENT_TERM));
    }

    #[test]
    fn trigram_vectors() {
        // Port of TestTrigramTokenizer.
        let tokens = build_tokens(&text("Search rocks!"), &TrigramTokenizer).expect("tokens");
        assert_eq!(tokens.len(), 11);
        let mut expected = [
            " ro", "Sea", "arc", "ch ", "cks", "ear", "h r", "ks!", "ock", "rch", "roc",
        ]
        .to_vec();
        expected.sort_unstable();
        assert_eq!(tokens, encoded(&expected, IDENT_TRIGRAM));
        assert!(
            TrigramTokenizer
                .tokens(&text("ab"))
                .expect("short")
                .is_empty()
        );
    }

    #[test]
    fn cjk_fulltext_vectors() {
        // Ports of TestFullTextTokenizerCJK{Chinese,Korean,Japanese}.
        let id = IDENT_FULLTEXT;
        let zh = FullTextTokenizer {
            lang: "zh".to_owned(),
        };
        let chinese = build_tokens(&text("他是一个薪水很高的商人"), &zh).expect("tokens");
        let mut expected = [
            "他是", "是一", "一个", "个薪", "薪水", "水很", "很高", "高的", "的商", "商人",
        ]
        .to_vec();
        expected.sort_unstable();
        assert_eq!(chinese, encoded(&expected, id));

        let ko = FullTextTokenizer {
            lang: "ko".to_owned(),
        };
        let korean = build_tokens(&text("그는 큰 급여를 가진 사업가입니다."), &ko).expect("tokens");
        let mut expected = ["그는", "큰", "급여를", "가진", "사업가입니다"].to_vec();
        expected.sort_unstable();
        assert_eq!(korean, encoded(&expected, id));

        let ja = FullTextTokenizer {
            lang: "ja".to_owned(),
        };
        let japanese = build_tokens(&text("彼は大きな給与を持つ実業家です"), &ja).expect("tokens");
        let mut expected = [
            "彼は", "は大", "大き", "きな", "な給", "給与", "与を", "を持", "持つ", "つ実", "実業",
            "業家", "家で", "です",
        ]
        .to_vec();
        expected.sort_unstable();
        assert_eq!(japanese, encoded(&expected, id));
    }

    #[test]
    fn cjk_term_vectors() {
        // Port of TestTermTokenizeCJKChinese.
        let id = IDENT_TERM;
        let tokens =
            build_tokens(&text("第一轮 第二轮 第一轮"), &TermTokenizer::default()).expect("tokens");
        let mut expected = ["第", "一", "轮", "二"].to_vec();
        expected.sort_unstable();
        assert_eq!(tokens, encoded(&expected, id));

        let lang = tokenizer_for_lang("term", "zh").expect("zh");
        let tokens = build_tokens(&text("第一轮 第二轮 第一轮"), lang.as_ref()).expect("tokens");
        let mut expected = ["第一轮", "第二轮"].to_vec();
        expected.sort_unstable();
        assert_eq!(tokens, encoded(&expected, id));
    }

    #[test]
    fn ngram_vectors() {
        // Ports of TestNGramTokenizer/Empty/SingleWord/Stopwords/Stemming.
        let id = IDENT_NGRAM;
        let lang = tokenizer_for_lang("ngram", "en").expect("en");
        let tokens = build_tokens(&text("quick brown fox"), lang.as_ref()).expect("tokens");
        assert!(!tokens.is_empty());
        for term in [
            "quick",
            "brown",
            "fox",
            "quick brown",
            "brown fox",
            "quick brown fox",
        ] {
            assert!(
                tokens.contains(&encode_token(term.as_bytes(), id)),
                "{term}"
            );
        }
        assert_unique(&tokens);

        assert!(
            build_tokens(&text(""), lang.as_ref())
                .expect("empty")
                .is_empty()
        );
        assert!(
            build_tokens(&text("   "), lang.as_ref())
                .expect("spaces")
                .is_empty()
        );
        let tokens = build_tokens(&text("hello"), lang.as_ref()).expect("single");
        assert!(tokens.contains(&encode_token(b"hello", id)));

        let tokens = build_tokens(&text("the quick brown fox"), lang.as_ref()).expect("stops");
        assert!(!tokens.contains(&encode_token(b"the", id)));
        assert!(!tokens.contains(&encode_token(b"the quick", id)));
        for term in ["quick", "brown", "fox"] {
            assert!(
                tokens.contains(&encode_token(term.as_bytes(), id)),
                "{term}"
            );
        }

        let tokens = build_tokens(&text("running quickly"), lang.as_ref()).expect("stemming");
        assert!(!tokens.is_empty());
        for term in ["run", "quickli", "run quickli"] {
            assert!(
                tokens.contains(&encode_token(term.as_bytes(), id)),
                "{term}"
            );
        }
        assert_unique(&tokens);
    }

    #[test]
    fn ngram_long_text() {
        // Port of TestNGramTokenizerLongText (Go tokenizes expectations too).
        let lang = tokenizer_for_lang("ngram", "en").expect("en");
        let tokens = build_tokens(
            &text("The quick brown fox jumps over the lazy dog. This is a test sentence with multiple words."),
            lang.as_ref(),
        )
        .expect("tokens");
        assert!(tokens.len() > 10);
        for phrase in [
            "quick",
            "brown",
            "fox",
            "quick brown",
            "brown fox",
            "quick brown fox",
            "sentence multiple words",
            "lazy dog test sentence",
        ] {
            // Longest token of the tokenized phrase must appear (Go's rule).
            let phrase_tokens = build_tokens(&text(phrase), lang.as_ref()).expect("phrase");
            let longest = phrase_tokens.iter().max_by_key(|t| t.len()).expect("token");
            assert!(tokens.contains(longest), "{phrase}");
        }
        assert_unique(&tokens);
    }

    #[test]
    fn ngram_query_tokens() {
        // Ports of TestNGramTokenizerQueryTokens/VariousInputs.
        let id = IDENT_NGRAM;
        let query = get_ngram_query_tokens(&[text("quick brown fox")], "en").expect("query");
        assert!(!query.is_empty());
        assert!(query.contains(&encode_token(b"quick brown fox", id)));
        let query = get_ngram_query_tokens(&[text("hello")], "en").expect("query");
        assert!(query.contains(&encode_token(b"hello", id)));
        assert!(
            get_ngram_query_tokens(&[text("")], "en")
                .expect("empty")
                .is_empty()
        );

        let cases: &[(&str, usize, &[&str])] = &[
            ("quick brown fox", 1, &["quick brown fox"]),
            ("hello world", 1, &["hello world"]),
            ("single", 1, &["single"]),
            (
                "one two three four",
                2,
                &["one two three", "two three four"],
            ),
            (
                "the quick brown fox jumps",
                2,
                &["quick brown fox", "brown fox jump"],
            ),
        ];
        let tokenizer = NGramTokenizer {
            lang: "en".to_owned(),
        };
        for (input, count, expected) in cases {
            let query = get_ngram_query_tokens(&[text(input)], "en").expect("query");
            assert_eq!(query.len(), *count, "{input}");
            for term in *expected {
                // Go tokenizes each expectation and seeks any resulting token.
                let stemmed = tokenizer.tokens(&text(term)).expect("stem");
                assert!(
                    stemmed.iter().any(|t| query.contains(&encode_token(t, id))),
                    "{term} in {input}"
                );
            }
        }
    }

    #[test]
    fn ngram_language_stemming_and_hashing() {
        // Ports of TestNGramTokenizerLanguageSupport/Stemming/LongTokenHashing/NonStringInput.
        for lang in ["en", "es", "fr", "de"] {
            let tokenizer = tokenizer_for_lang("ngram", lang).expect("lang");
            let tokens = build_tokens(&text("hello world"), tokenizer.as_ref()).expect("tokens");
            assert!(!tokens.is_empty(), "{lang}");
            assert_unique(&tokens);
        }

        let id = IDENT_NGRAM;
        let lang = tokenizer_for_lang("ngram", "en").expect("en");
        let tokens = build_tokens(
            &text("supercalifragilisticexpialidocious antidisestablishmentarianism pneumonoultramicroscopicsilicovolcanoconiosiss"),
            lang.as_ref(),
        )
        .expect("long");
        assert!(!tokens.is_empty());
        assert_unique(&tokens);
        assert!(tokens.iter().all(|t| t[0] == id));

        let non_string = Val {
            tid: TypeId::Int,
            value: Value::Int(123),
        };
        assert!(
            build_tokens(&non_string, lang.as_ref())
                .expect("non-string")
                .is_empty()
        );
        assert!(get_ngram_tokens(&[], "").is_err());
    }

    fn assert_unique(tokens: &[Vec<u8>]) {
        let mut set = std::collections::HashSet::new();
        for token in tokens {
            assert!(set.insert(token), "duplicate token");
        }
    }

    #[test]
    fn hash_and_sha_vectors() {
        let hash = HashTokenizer.tokens(&text("abc")).expect("hash")[0].clone();
        assert_eq!(hash.len(), 32);
        // blake2b-256("abc") test vector (via Python hashlib ground truth).
        let hex: String = hash.iter().fold(String::new(), |mut acc, b| {
            use std::fmt::Write as _;
            let _ = write!(acc, "{b:02x}");
            acc
        });
        assert_eq!(
            hex,
            "bddd813c634239723171ef3fee98579b94964e3bb1cb3e427262c8c068d52319"
        );
        assert!(
            HashTokenizer
                .tokens(&Val {
                    tid: TypeId::Int,
                    value: Value::Int(1)
                })
                .is_err()
        );
        let long = "x".repeat(100);
        assert_eq!(
            Sha256Tokenizer.tokens(&text(&long)).expect("sha")[0].len(),
            64
        );
        assert!(
            Sha256Tokenizer
                .tokens(&text("short"))
                .expect("short")
                .is_empty()
        );
    }

    #[test]
    fn exact_and_geo_tokens() {
        let exact = ExactTokenizer::default();
        assert_eq!(
            exact.tokens(&text("abc")).expect("exact"),
            [b"abc".to_vec()]
        );
        assert_eq!(exact.identifier(), IDENT_EXACT);
        assert!(
            exact
                .tokens(&Val {
                    tid: TypeId::Int,
                    value: Value::Int(1)
                })
                .is_err()
        );

        let lang = ExactTokenizer::for_lang("de");
        assert_eq!(lang.identifier(), IDENT_EXACT_LANG);
        let tokens = lang.tokens(&text("öffnen")).expect("collated");
        assert_eq!(tokens.len(), 1);
        assert!(tokens[0].starts_with(b"de\x1f"));

        let geo = crate::tok::GeoTokenizer;
        let geom =
            datastreamdb_types::geo::geo_from_json_text("{'type':'Point','coordinates':[1,2]}")
                .expect("geom");
        let tokens = geo
            .tokens(&Val {
                tid: TypeId::Geo,
                value: Value::Geo(geom),
            })
            .expect("geo");
        assert!(!tokens.is_empty());
        assert!(
            tokens
                .iter()
                .all(|t| t.starts_with(b"p/") || t.starts_with(b"c/"))
        );
    }
}
