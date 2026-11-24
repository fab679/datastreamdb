//! Stemming filters. Port of `tok/stemmers.go`.
//!
//! Snowball languages delegate to `rust-stemmers` (same spec as bleve's
//! `snowballstem`); `en` uses our exact [`porter`](super::porter) port; light/custom
//! stemmers (de/es/fr/it/pt, ar/hi/ckb) are ported rule-for-rule from bleve;
//! `cjk_bigram` ports bleve's shingler.

#![warn(missing_docs)]

use rust_stemmers::{Algorithm, Stemmer};

use super::analyzer::Token;
use super::porter::stem_without_lower_casing;

/// Stems terms in place by language; unknown languages pass through.
/// Port of `filterStemmers`.
pub fn filter_stemmers(lang: &str, tokens: Vec<Token>) -> Vec<Token> {
    match lang {
        "da" => snowball(tokens, Algorithm::Danish),
        "fi" => snowball(tokens, Algorithm::Finnish),
        "hu" => snowball(tokens, Algorithm::Hungarian),
        "nl" => snowball(tokens, Algorithm::Dutch),
        "no" => snowball(tokens, Algorithm::Norwegian),
        "ro" => snowball(tokens, Algorithm::Romanian),
        "ru" => snowball(tokens, Algorithm::Russian),
        "sv" => snowball(tokens, Algorithm::Swedish),
        "tr" => snowball(tokens, Algorithm::Turkish),
        "en" => map_terms(tokens, stem_without_lower_casing),
        "de" => map_terms(tokens, light_de),
        "es" => map_terms(tokens, light_es),
        "fr" => map_terms(tokens, light_fr),
        "it" => map_terms(tokens, light_it),
        "pt" => map_terms(tokens, light_pt),
        "ar" => map_terms(tokens, light_ar),
        "hi" => map_terms(tokens, stem_hi),
        "ckb" => map_terms(tokens, stem_ckb),
        "ja" | "ko" | "zh" => cjk_bigram(tokens),
        _ => tokens,
    }
}

fn map_terms(tokens: Vec<Token>, f: impl Fn(&str) -> String) -> Vec<Token> {
    tokens
        .into_iter()
        .map(|mut token| {
            let stemmed = f(&token.term);
            token.term = stemmed;
            token
        })
        .collect()
}

fn snowball(tokens: Vec<Token>, algorithm: Algorithm) -> Vec<Token> {
    let stemmer = Stemmer::create(algorithm);
    map_terms(tokens, |term| stemmer.stem(term).into_owned())
}

// --- light stemmers (faithful bleve rule ports) ---

fn light_de(term: &str) -> String {
    let mut runes: Vec<char> = term.chars().collect();
    for r in &mut runes {
        *r = match *r {
            'ä' | 'à' | 'á' | 'â' => 'a',
            'ö' | 'ò' | 'ó' | 'ô' => 'o',
            'ï' | 'ì' | 'í' | 'î' => 'i',
            'ü' | 'ù' | 'ú' | 'û' => 'u',
            _ => *r,
        };
    }
    de_step1(&mut runes);
    de_step2(&mut runes);
    runes.into_iter().collect()
}

fn de_st_ending(ch: char) -> bool {
    matches!(
        ch,
        'b' | 'd' | 'f' | 'g' | 'h' | 'k' | 'l' | 'm' | 'n' | 't'
    )
}

fn de_step1(runes: &mut Vec<char>) {
    let l = runes.len();
    if l > 5 && runes[l - 3] == 'e' && runes[l - 2] == 'r' && runes[l - 1] == 'n' {
        runes.truncate(l - 3);
        return;
    }
    if l > 4 && runes[l - 2] == 'e' && matches!(runes[l - 1], 'm' | 'n' | 'r' | 's') {
        runes.truncate(l - 2);
        return;
    }
    if l > 3 && runes[l - 1] == 'e' {
        runes.truncate(l - 1);
        return;
    }
    if l > 3 && runes[l - 1] == 's' && de_st_ending(runes[l - 2]) {
        runes.truncate(l - 1);
    }
}

fn de_step2(runes: &mut Vec<char>) {
    let l = runes.len();
    if l > 5 && runes[l - 3] == 'e' && runes[l - 2] == 's' && runes[l - 1] == 't' {
        runes.truncate(l - 3);
        return;
    }
    if l > 4 && runes[l - 2] == 'e' && matches!(runes[l - 1], 'r' | 'n') {
        runes.truncate(l - 2);
        return;
    }
    if l > 4 && runes[l - 2] == 's' && runes[l - 1] == 't' && de_st_ending(runes[l - 3]) {
        runes.truncate(l - 2);
    }
}

fn light_es(term: &str) -> String {
    let mut runes: Vec<char> = term.chars().collect();
    let l = runes.len();
    if l < 5 {
        return term.to_owned();
    }
    match runes[l - 1] {
        'o' | 'a' | 'e' => {
            runes.truncate(l - 1);
        }
        's' => {
            if runes[l - 2] == 'e' && runes[l - 3] == 's' && runes[l - 4] == 'e' {
                runes.truncate(l - 2);
            } else if runes[l - 2] == 'e' && runes[l - 3] == 'c' {
                runes[l - 3] = 'z';
                runes.truncate(l - 2);
            } else if matches!(runes[l - 2], 'o' | 'a' | 'e') {
                runes.truncate(l - 2);
            }
        }
        _ => {}
    }
    runes.into_iter().collect()
}

fn ends_with(runes: &[char], suffix: &str) -> bool {
    let suffix: Vec<char> = suffix.chars().collect();
    runes.len() >= suffix.len() && runes[runes.len() - suffix.len()..] == suffix[..]
}

fn light_fr(term: &str) -> String {
    let mut runes: Vec<char> = term.chars().collect();
    let mut len = runes.len();
    if len > 5 && runes[len - 1] == 'x' {
        if runes[len - 3] == 'a' && runes[len - 2] == 'u' && runes[len - 4] != 'e' {
            runes[len - 2] = 'l';
        }
        runes.truncate(len - 1);
        len = runes.len();
    }
    if len > 3 && runes[len - 1] == 'x' {
        runes.truncate(len - 1);
        len = runes.len();
    }
    if len > 3 && runes[len - 1] == 's' {
        runes.truncate(len - 1);
    }
    if let Some(stemmed) = fr_verb_endings(&mut runes) {
        return stemmed;
    }
    if let Some(stemmed) = fr_noun_endings(&mut runes) {
        return stemmed;
    }
    fr_norm(runes)
}

/// French verb-family endings.
fn fr_verb_endings(runes: &mut Vec<char>) -> Option<String> {
    let mut len = runes.len();
    if len > 9 && ends_with(runes, "issement") {
        runes.truncate(len - 6);
        len = runes.len();
        runes[len - 1] = 'r';
        return Some(fr_norm(std::mem::take(runes)));
    }
    if len > 8 && ends_with(runes, "issant") {
        runes.truncate(len - 4);
        len = runes.len();
        runes[len - 1] = 'r';
        return Some(fr_norm(std::mem::take(runes)));
    }
    if len > 6 && ends_with(runes, "ement") {
        runes.truncate(len - 4);
        len = runes.len();
        if len > 3 && ends_with(runes, "ive") {
            runes.truncate(len - 1);
            len = runes.len();
            runes[len - 1] = 'f';
        }
        return Some(fr_norm(std::mem::take(runes)));
    }
    if len > 11 && ends_with(runes, "ficatrice") {
        runes.truncate(len - 5);
        len = runes.len();
        runes[len - 2] = 'e';
        runes[len - 1] = 'r';
        return Some(fr_norm(std::mem::take(runes)));
    }
    if len > 10 && ends_with(runes, "ficateur") {
        runes.truncate(len - 4);
        len = runes.len();
        runes[len - 2] = 'e';
        runes[len - 1] = 'r';
        return Some(fr_norm(std::mem::take(runes)));
    }
    if len > 9 && ends_with(runes, "catrice") {
        runes.truncate(len - 3);
        len = runes.len();
        runes[len - 4] = 'q';
        runes[len - 3] = 'u';
        runes[len - 2] = 'e';
        return Some(fr_norm(std::mem::take(runes)));
    }
    if len > 8 && ends_with(runes, "cateur") {
        runes.truncate(len - 2);
        len = runes.len();
        runes[len - 4] = 'q';
        runes[len - 3] = 'u';
        runes[len - 2] = 'e';
        runes[len - 1] = 'r';
        return Some(fr_norm(std::mem::take(runes)));
    }
    if len > 8 && ends_with(runes, "atrice") {
        runes.truncate(len - 4);
        len = runes.len();
        runes[len - 2] = 'e';
        runes[len - 1] = 'r';
        return Some(fr_norm(std::mem::take(runes)));
    }
    if len > 7 && ends_with(runes, "ateur") {
        runes.truncate(len - 3);
        len = runes.len();
        runes[len - 2] = 'e';
        runes[len - 1] = 'r';
        return Some(fr_norm(std::mem::take(runes)));
    }
    if len > 6 && ends_with(runes, "trice") {
        runes.truncate(len - 1);
        len = runes.len();
        runes[len - 3] = 'e';
        runes[len - 2] = 'u';
        runes[len - 1] = 'r';
    }
    None
}

/// French noun-family endings.
fn fr_noun_endings(runes: &mut Vec<char>) -> Option<String> {
    let mut len = runes.len();
    if len > 5 && ends_with(runes, "ième") {
        return Some(fr_norm(runes[..len - 4].to_vec()));
    }
    if len > 7 && ends_with(runes, "teuse") {
        runes.truncate(len - 2);
        len = runes.len();
        runes[len - 1] = 'r';
        return Some(fr_norm(std::mem::take(runes)));
    }
    if len > 6 && ends_with(runes, "teur") {
        runes.truncate(len - 1);
        len = runes.len();
        runes[len - 1] = 'r';
        return Some(fr_norm(std::mem::take(runes)));
    }
    if len > 5 && ends_with(runes, "euse") {
        return Some(fr_norm(runes[..len - 2].to_vec()));
    }
    if len > 8 && ends_with(runes, "ère") {
        runes.truncate(len - 1);
        len = runes.len();
        runes[len - 2] = 'e';
        return Some(fr_norm(std::mem::take(runes)));
    }
    if len > 7 && ends_with(runes, "ive") {
        runes.truncate(len - 1);
        len = runes.len();
        runes[len - 1] = 'f';
        return Some(fr_norm(std::mem::take(runes)));
    }
    if len > 4 && (ends_with(runes, "folle") || ends_with(runes, "molle")) {
        runes.truncate(len - 2);
        len = runes.len();
        runes[len - 1] = 'u';
        return Some(fr_norm(std::mem::take(runes)));
    }
    if len > 9 && ends_with(runes, "nnelle") {
        return Some(fr_norm(runes[..len - 5].to_vec()));
    }
    if len > 9 && ends_with(runes, "nnel") {
        return Some(fr_norm(runes[..len - 3].to_vec()));
    }
    if len > 4 && ends_with(runes, "ète") {
        runes.truncate(len - 1);
        len = runes.len();
        runes[len - 2] = 'e';
    }
    if len > 8 && ends_with(runes, "ique") {
        runes.truncate(len - 4);
        len = runes.len();
    }
    if len > 8 && ends_with(runes, "esse") {
        return Some(fr_norm(runes[..len - 3].to_vec()));
    }
    if len > 7 && ends_with(runes, "inage") {
        return Some(fr_norm(runes[..len - 3].to_vec()));
    }
    if len > 9 && ends_with(runes, "isation") {
        runes.truncate(len - 7);
        len = runes.len();
        if len > 5 && ends_with(runes, "ual") {
            runes[len - 2] = 'e';
        }
        return Some(fr_norm(std::mem::take(runes)));
    }
    if len > 9 && ends_with(runes, "isateur") {
        return Some(fr_norm(runes[..len - 7].to_vec()));
    }
    if len > 8 && ends_with(runes, "ation") {
        return Some(fr_norm(runes[..len - 5].to_vec()));
    }
    if len > 8 && ends_with(runes, "ition") {
        return Some(fr_norm(runes[..len - 5].to_vec()));
    }
    None
}

fn fr_norm(mut runes: Vec<char>) -> String {
    if runes.len() > 4 {
        for r in &mut runes {
            *r = match *r {
                'à' | 'á' | 'â' => 'a',
                'ô' => 'o',
                'è' | 'é' | 'ê' => 'e',
                'ù' | 'û' => 'u',
                'î' => 'i',
                'ç' => 'c',
                _ => *r,
            };
        }
        // Collapse consecutive duplicate letters (Go interleaves this per
        // char; the fixpoint is identical).
        let mut deduped: Vec<char> = Vec::with_capacity(runes.len());
        for r in runes {
            if deduped.last() == Some(&r) && r.is_alphabetic() {
                continue;
            }
            deduped.push(r);
        }
        runes = deduped;
    }
    if runes.len() > 4 && ends_with(&runes, "ie") {
        runes.truncate(runes.len() - 2);
    }
    if runes.len() > 4 {
        // Go strips one trailing r, then e, then e (guarded by the outer
        // length check); equivalent loop below.
        for _ in 0..1 {
            let last = runes.len() - 1;
            if runes[last] == 'r' {
                runes.truncate(last);
            } else {
                break;
            }
        }
        for _ in 0..2 {
            let last = runes.len() - 1;
            if runes[last] == 'e' {
                runes.truncate(last);
            } else {
                break;
            }
        }
        let last = runes.len() - 1;
        if runes[last] == runes[last - 1] && runes[last].is_alphabetic() {
            runes.truncate(last);
        }
    }
    runes.into_iter().collect()
}

fn light_it(term: &str) -> String {
    let mut runes: Vec<char> = term.chars().collect();
    if runes.len() < 6 {
        return term.to_owned();
    }
    for r in &mut runes {
        *r = match *r {
            'à' | 'á' | 'â' | 'ä' => 'a',
            'ò' | 'ó' | 'ô' | 'ö' => 'o',
            'è' | 'é' | 'ê' | 'ë' => 'e',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            'ì' | 'í' | 'î' | 'ï' => 'i',
            _ => *r,
        };
    }
    let len = runes.len();
    match runes[len - 1] {
        'e' | 'i' => {
            if matches!(runes[len - 2], 'i' | 'h') {
                runes.truncate(len - 2);
            } else {
                runes.truncate(len - 1);
            }
        }
        'a' | 'o' => {
            if runes[len - 2] == 'i' {
                runes.truncate(len - 2);
            } else {
                runes.truncate(len - 1);
            }
        }
        _ => {}
    }
    runes.into_iter().collect()
}

fn light_pt(term: &str) -> String {
    let mut runes: Vec<char> = term.chars().collect();
    if runes.len() < 4 {
        return term.to_owned();
    }
    pt_remove_suffix(&mut runes);
    let mut len = runes.len();
    if len > 3 && runes[len - 1] == 'a' {
        pt_norm_feminine(&mut runes);
        len = runes.len();
    }
    if len > 4 && matches!(runes[len - 1], 'e' | 'a' | 'o') {
        runes.truncate(len - 1);
        len = runes.len();
    }
    for r in runes.iter_mut().take(len) {
        *r = match *r {
            'à' | 'á' | 'â' | 'ä' | 'ã' => 'a',
            'ò' | 'ó' | 'ô' | 'ö' | 'õ' => 'o',
            'è' | 'é' | 'ê' | 'ë' => 'e',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            'ì' | 'í' | 'î' | 'ï' => 'i',
            'ç' => 'c',
            _ => *r,
        };
    }
    runes.truncate(len);
    runes.into_iter().collect()
}

fn pt_remove_suffix(runes: &mut Vec<char>) {
    let len = runes.len();
    if len > 4 && ends_with(runes, "es") && matches!(runes[len - 3], 'r' | 's' | 'l' | 'z') {
        runes.truncate(len - 2);
        return;
    }
    if len > 3 && ends_with(runes, "ns") {
        runes[len - 2] = 'm';
        runes.truncate(len - 1);
        return;
    }
    if len > 4 && (ends_with(runes, "eis") || ends_with(runes, "éis")) {
        runes[len - 3] = 'e';
        runes[len - 2] = 'l';
        runes.truncate(len - 1);
        return;
    }
    if len > 4 && ends_with(runes, "ais") {
        runes[len - 2] = 'l';
        runes.truncate(len - 1);
        return;
    }
    if len > 4 && ends_with(runes, "óis") {
        runes[len - 3] = 'o';
        runes[len - 2] = 'l';
        runes.truncate(len - 1);
        return;
    }
    if len > 4 && ends_with(runes, "is") {
        runes[len - 1] = 'l';
        return;
    }
    if len > 3 && (ends_with(runes, "ões") || ends_with(runes, "ães")) {
        runes.truncate(len - 1);
        let len = runes.len();
        runes[len - 2] = 'ã';
        runes[len - 1] = 'o';
        return;
    }
    if len > 6 && ends_with(runes, "mente") {
        runes.truncate(len - 5);
        return;
    }
    if len > 3 && runes[len - 1] == 's' {
        runes.truncate(len - 1);
    }
}

fn pt_norm_feminine(runes: &mut Vec<char>) {
    let len = runes.len();
    if len > 7 && (ends_with(runes, "inha") || ends_with(runes, "iaca") || ends_with(runes, "eira"))
    {
        runes[len - 1] = 'o';
        return;
    }
    if len > 6 {
        if ends_with(runes, "osa")
            || ends_with(runes, "ica")
            || ends_with(runes, "ida")
            || ends_with(runes, "ada")
            || ends_with(runes, "iva")
            || ends_with(runes, "ama")
        {
            runes[len - 1] = 'o';
            return;
        }
        if ends_with(runes, "ona") {
            runes[len - 3] = 'ã';
            runes[len - 2] = 'o';
            runes.truncate(len - 1);
            return;
        }
        if ends_with(runes, "ora") {
            runes.truncate(len - 1);
            return;
        }
        if ends_with(runes, "esa") {
            runes[len - 3] = 'ê';
            runes.truncate(len - 1);
            return;
        }
        if ends_with(runes, "na") {
            runes[len - 1] = 'o';
        }
    }
}

fn light_ar(term: &str) -> String {
    const PREFIXES: &[&str] = &["ال", "وال", "بال", "كال", "فال", "لل", "و"];
    const SUFFIXES: &[&str] = &["ها", "ان", "ات", "ون", "ين", "يه", "ية", "ه", "ة", "ي"];
    let mut runes: Vec<char> = term.chars().collect();
    for prefix in PREFIXES {
        let prefix: Vec<char> = prefix.chars().collect();
        let ok = (prefix.len() != 1 || runes.len() >= 4)
            && runes.len() >= prefix.len() + 2
            && runes.starts_with(&prefix);
        if ok {
            runes = runes[prefix.len()..].to_vec();
            break;
        }
    }
    for suffix in SUFFIXES {
        let suffix: Vec<char> = suffix.chars().collect();
        if runes.len() >= suffix.len() + 2 && ends_with(&runes, &suffix.iter().collect::<String>())
        {
            runes.truncate(runes.len() - suffix.len());
        }
    }
    runes.into_iter().collect()
}

fn stem_hi(term: &str) -> String {
    const RULES: &[(&[&str], usize, usize)] = &[
        (
            &["ाएंगी", "ाएंगे", "ाऊंगी", "ाऊंगा", "ाइयाँ", "ाइयों", "ाइयां"],
            6,
            5,
        ),
        (
            &[
                "ाएगी",
                "ाएगा",
                "ाओगी",
                "ाओगे",
                "एंगी",
                "ेंगी",
                "एंगे",
                "ेंगे",
                "ूंगी",
                "ूंगा",
                "ातीं",
                "नाओं",
                "नाएं",
                "ताओं",
                "ताएं",
                "ियाँ",
                "ियों",
                "ियां",
            ],
            5,
            4,
        ),
        (
            &[
                "ाकर",
                "ाइए",
                "ाईं",
                "ाया",
                "ेगी",
                "ेगा",
                "ोगी",
                "ोगे",
                "ाने",
                "ाना",
                "ाते",
                "ाती",
                "ाता",
                "तीं",
                "ाओं",
                "ाएं",
                "ुओं",
                "ुएं",
                "ुआं",
            ],
            4,
            3,
        ),
        (
            &[
                "कर", "ाओ", "िए", "ाई", "ाए", "ने", "नी", "ना", "ते", "ीं", "ती", "ता", "ाँ", "ां", "ों",
                "ें",
            ],
            3,
            2,
        ),
        (&["ो", "े", "ू", "ु", "ी", "ि", "ा"], 2, 1),
    ];
    let len = term.chars().count();
    for (suffixes, min_len, cut) in RULES {
        if len > *min_len && suffixes.iter().any(|s| term.ends_with(s)) {
            return term.chars().take(len - cut).collect();
        }
    }
    term.to_owned()
}

fn stem_ckb(term: &str) -> String {
    fn truncate(term: &str, n: usize) -> String {
        let len = term.chars().count();
        term.chars().take(len - n).collect()
    }
    let mut text: String = term.to_owned();
    let mut len = text.chars().count();
    if len > 5 && text.ends_with("دا") {
        text = truncate(&text, 2);
        len = text.chars().count();
    } else if len > 4 && text.ends_with("نا") {
        text = truncate(&text, 1);
        len = text.chars().count();
    } else if len > 6 && text.ends_with("ەوە") {
        text = truncate(&text, 3);
        len = text.chars().count();
    }
    if len > 6 && (text.ends_with("مان") || text.ends_with("یان") || text.ends_with("تان"))
    {
        text = truncate(&text, 3);
        len = text.chars().count();
    }
    if len > 6 && text.ends_with("ێکی") {
        return truncate(&text, 3);
    } else if len > 7 && text.ends_with("یەکی") {
        return truncate(&text, 4);
    }
    if len > 5 && text.ends_with("ێک") {
        return truncate(&text, 2);
    } else if len > 6 && (text.ends_with("یەک") || text.ends_with("ەکە")) {
        return truncate(&text, 3);
    } else if len > 5 && text.ends_with("کە") {
        return truncate(&text, 2);
    } else if len > 7 && text.ends_with("ەکان") {
        return truncate(&text, 4);
    } else if len > 6 && text.ends_with("کان") {
        return truncate(&text, 3);
    } else if len > 7 && text.ends_with("یانی") {
        return truncate(&text, 4);
    } else if len > 6 && (text.ends_with("انی") || text.ends_with("یان")) {
        return truncate(&text, 3);
    } else if len > 5 && text.ends_with("ان") {
        return truncate(&text, 2);
    } else if len > 7 && text.ends_with("یانە") {
        return truncate(&text, 4);
    } else if len > 6 && text.ends_with("انە") {
        return truncate(&text, 3);
    } else if len > 5 && (text.ends_with("ایە") || text.ends_with("ەیە")) {
        return truncate(&text, 2);
    } else if len > 4 && (text.ends_with("ە") || text.ends_with("ی")) {
        return truncate(&text, 1);
    }
    text
}

/// CJK bigram shingler. Port of bleve's `cjk_bigram` filter
/// (`outputUnigram=false`): adjacent ideographic tokens fuse into overlapping
/// 2-char shingles; anything else flushes the buffer.
#[must_use]
pub fn cjk_bigram(tokens: Vec<Token>) -> Vec<Token> {
    let mut out = Vec::new();
    let mut buf: Vec<Token> = Vec::new();
    let flush = |buf: &mut Vec<Token>, out: &mut Vec<Token>| {
        if buf.len() >= 2 {
            for window in buf.windows(2) {
                let mut term = window[0].term.clone();
                term.push_str(&window[1].term);
                out.push(Token {
                    term,
                    position: window[0].position,
                    ideographic: true,
                });
            }
        }
        buf.clear();
    };
    for token in tokens {
        if token.ideographic {
            buf.push(token);
        } else {
            flush(&mut buf, &mut out);
            out.push(token);
        }
    }
    flush(&mut buf, &mut out);
    out
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
    fn filter_vectors() {
        // Exact port of TestFilterStemmers.
        let cases: &[(&str, &[&str], &[&str])] = &[
            (
                "en",
                &[
                    "the", "quick", "brown", "foxes", "jump", "over", "the", "big", "dogs",
                ],
                &[
                    "the", "quick", "brown", "fox", "jump", "over", "the", "big", "dog",
                ],
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
                    "deseándol",
                    "much",
                    "alegrí",
                    "a",
                    "las",
                    "señorit",
                    "y",
                    "los",
                    "señor",
                    "programador",
                    "de",
                    "DataStreamDB",
                ],
            ),
            ("x-klingon", &["tlhIngan", "maH!"], &["tlhIngan", "maH!"]),
            ("en", &[], &[]),
            ("", &[""], &[""]),
        ];
        for (lang, input, expected) in cases {
            assert_eq!(
                terms(&filter_stemmers(lang, toks(input))),
                *expected,
                "{lang}"
            );
        }
    }

    #[test]
    fn german_light_vectors() {
        // de rules verified through the fulltext pipeline vectors in tok tests;
        // direct filter checks (pipeline lowercases first).
        let input = toks(&["katzen", "auffassung"]);
        assert_eq!(terms(&filter_stemmers("de", input)), ["katz", "auffassung"]);
    }

    #[test]
    fn cjk_shingles() {
        // Port of TestCJKBigramFilter (ideographic flags as the analyzer sets).
        let chars = [
            "他", "是", "一", "个", "薪", "水", "很", "高", "的", "商", "人",
        ];
        let input: Vec<Token> = chars
            .iter()
            .enumerate()
            .map(|(i, c)| Token {
                term: (*c).to_owned(),
                position: i,
                ideographic: true,
            })
            .collect();
        assert_eq!(
            terms(&cjk_bigram(input)),
            [
                "他是", "是一", "一个", "个薪", "薪水", "水很", "很高", "高的", "的商", "商人"
            ]
        );
        // Non-ideographic tokens flush and pass through.
        let mixed = vec![
            Token {
                term: "a".to_owned(),
                position: 0,
                ideographic: false,
            },
            Token {
                term: "他".to_owned(),
                position: 1,
                ideographic: true,
            },
            Token {
                term: "是".to_owned(),
                position: 2,
                ideographic: true,
            },
        ];
        assert_eq!(terms(&cjk_bigram(mixed)), ["a", "他是"]);
    }
}
