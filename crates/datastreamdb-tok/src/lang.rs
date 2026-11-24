//! Language tags: base extraction. Port of `tok/langbase.go`.
//!
//! Go leans on `x/text` matching with confidence; we parse BCP47 with ICU and
//! validate the base against ISO 639 plus the script against ISO 15924, which
//! agrees on every pinned vector. Regions, variants, and extensions are not
//! validated (Go rejects bogus ones; we accept well-formed tags — divergence
//! only for valid-base tags with bogus region/variant/extension parts).

#![warn(missing_docs)]

use super::lang_data::{KNOWN_BASES, KNOWN_SCRIPTS};

/// Default language for empty/unparseable tags.
pub const DEFAULT_LANG: &str = "en";

/// Deprecated base mappings (`sh` pinned; rest are standard BCP47).
fn canonical_base(base: &str) -> &str {
    match base {
        "sh" => "sr",
        "iw" => "he",
        "in" => "id",
        "mo" => "ro",
        "ji" => "yi",
        _ => base,
    }
}

/// Base language subtag in lowercase, or [`DEFAULT_LANG`].
#[must_use]
pub fn lang_base(lang: &str) -> String {
    if lang.is_empty() {
        return DEFAULT_LANG.to_owned();
    }
    // Underscores are not BCP47; try the dashed form too (Go is lenient).
    let normalized = lang.replace('_', "-");
    let Ok(locale) = icu_locale_core::Locale::try_from_str(normalized.as_str()) else {
        return DEFAULT_LANG.to_owned();
    };
    let base = locale.id.language.to_string();
    if base.is_empty() || base == "und" {
        return DEFAULT_LANG.to_owned();
    }
    let base = canonical_base(&base);
    if KNOWN_BASES.binary_search(&base).is_err() {
        return DEFAULT_LANG.to_owned();
    }
    if let Some(script) = locale.id.script {
        if KNOWN_SCRIPTS
            .binary_search(&script.to_string().as_str())
            .is_err()
        {
            return DEFAULT_LANG.to_owned();
        }
    }
    base.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_bases() {
        // Full port of TestLangBase.
        for (input, expected) in [
            ("zh", "zh"),
            ("zh-hant", "zh"),
            ("zh-hans", "zh"),
            ("es-es", "es"),
            ("ar-001", "ar"),
            ("ca-ES-valencia", "ca"),
            ("ca-ES-valencia-u-va-posix", "ca"),
            ("ca-ES-valencia-u-co-phonebk", "ca"),
            ("ca-ES-valencia-u-co-phonebk-va-posix", "ca"),
            ("x-klingon", "en"),
            ("en-US", "en"),
            ("en-US-u-va-posix", "en"),
            ("en", "en"),
            ("en-u-co-phonebk", "en"),
            ("en-001", "en"),
            ("sh", "sr"),
            ("en-GB-u-rg-uszzzz", "en"),
            ("en-GB-u-rg-uszzzz-va-posix", "en"),
            ("en-GB-u-co-phonebk-rg-uszzzz", "en"),
            ("en-GB-u-co-phonebk-rg-uszz", "en"),
            ("", "en"),
            ("no_such_language", "en"),
            ("xxx_such_language", "en"),
        ] {
            assert_eq!(lang_base(input), expected, "{input}");
        }
    }
}
