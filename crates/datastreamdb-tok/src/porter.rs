//! Original Porter stemmer. Port of `blevesearch/go-porterstemmer`
//! (MIT, Charles Iliya Krempeaux), which bleve's `stemmer_porter` uses.
//! `StemWithoutLowerCasing` semantics: words of length ≤ 2 pass through.

// Step functions mutate length (truncate/push), so they need `Vec`, not slices.
#![allow(clippy::ptr_arg)]
#![warn(missing_docs)]

fn is_consonant(word: &[char], i: usize) -> bool {
    match word[i] {
        'a' | 'e' | 'i' | 'o' | 'u' => false,
        'y' => {
            if i == 0 {
                true
            } else {
                !is_consonant(word, i - 1)
            }
        }
        _ => true,
    }
}

fn measure(word: &[char]) -> usize {
    if word.is_empty() {
        return 0;
    }
    let mut i = 0;
    while i < word.len() && is_consonant(word, i) {
        i += 1;
    }
    let mut result = 0;
    'outer: while i < word.len() {
        while i < word.len() && !is_consonant(word, i) {
            i += 1;
            if i >= word.len() {
                break 'outer;
            }
        }
        while i < word.len() && is_consonant(word, i) {
            i += 1;
            if i >= word.len() {
                result += 1;
                break 'outer;
            }
        }
        result += 1;
    }
    result
}

/// Whether `word` ends with `suffix` with at least one char to spare.
fn has_suffix(word: &[char], suffix: &[char]) -> bool {
    if word.len() <= suffix.len() {
        return false;
    }
    word.ends_with(suffix)
}

fn contains_vowel(word: &[char]) -> bool {
    (0..word.len()).any(|i| !is_consonant(word, i))
}

fn has_double_consonant(word: &[char]) -> bool {
    word.len() >= 2
        && word[word.len() - 1] == word[word.len() - 2]
        && is_consonant(word, word.len() - 1)
}

fn has_cvc(word: &[char]) -> bool {
    word.len() >= 3
        && is_consonant(word, word.len() - 3)
        && !is_consonant(word, word.len() - 2)
        && is_consonant(word, word.len() - 1)
}

fn step1a(word: &mut Vec<char>) {
    if has_suffix(word, &['s', 's', 'e', 's']) || has_suffix(word, &['i', 'e', 's']) {
        word.truncate(word.len() - 2);
    } else if has_suffix(word, &['s', 's']) {
        // keep
    } else if has_suffix(word, &['s']) {
        word.truncate(word.len() - 1);
    }
}

/// Double consonant that is not l/s/z (Porter step-1b rule).
fn is_double_not_lsz(word: &[char], stem_len: usize) -> bool {
    let c = word[stem_len - 1];
    c != 'l' && c != 's' && c != 'z' && has_double_consonant(&word[..stem_len])
}

fn step1b_helper(word: &mut Vec<char>, stem_len: usize) {
    let ends = |suffix: &[char]| has_suffix(&word[..stem_len], suffix);
    if ends(&['a', 't']) || ends(&['b', 'l']) || ends(&['i', 'z']) {
        word.truncate(stem_len);
        word.push('e');
    } else if is_double_not_lsz(word, stem_len) {
        word.truncate(stem_len - 1);
    } else if measure(&word[..stem_len]) == 1
        && has_cvc(&word[..stem_len])
        && !matches!(word[stem_len - 1], 'w' | 'x' | 'y')
    {
        word.truncate(stem_len);
        word.push('e');
    } else {
        word.truncate(stem_len);
    }
}

fn step1b(word: &mut Vec<char>) {
    if has_suffix(word, &['e', 'e', 'd']) {
        if measure(&word[..word.len() - 3]) > 0 {
            word.truncate(word.len() - 1);
        }
    } else if has_suffix(word, &['e', 'd']) {
        let stem_len = word.len() - 2;
        if contains_vowel(&word[..stem_len]) {
            step1b_helper(word, stem_len);
        }
    } else if has_suffix(word, &['i', 'n', 'g']) {
        let stem_len = word.len() - 3;
        if contains_vowel(&word[..stem_len]) {
            step1b_helper(word, stem_len);
        }
    }
}

fn step1c(word: &mut Vec<char>) {
    if word.len() < 2 {
        return;
    }
    let last = word.len() - 1;
    if (word[last] == 'y' || word[last] == 'Y') && contains_vowel(&word[..last]) {
        word[last] = if word[last] == 'y' { 'i' } else { 'I' };
    }
}

/// (suffix, measure threshold, replacement action) step-2 table.
fn step2(word: &mut Vec<char>) {
    let rules: &[(&[char], usize, &[char])] = &[
        (&['a', 't', 'i', 'o', 'n', 'a', 'l'], 0, &['a', 't', 'e']),
        (&['t', 'i', 'o', 'n', 'a', 'l'], 0, &['t', 'i', 'o', 'n']),
        (&['e', 'n', 'c', 'i'], 0, &['e', 'n', 'c', 'e']),
        (&['a', 'n', 'c', 'i'], 0, &['a', 'n', 'c', 'e']),
        (&['i', 'z', 'e', 'r'], 0, &['i', 'z', 'e']),
        (&['b', 'l', 'i'], 0, &['b', 'l', 'e']),
        (&['a', 'l', 'l', 'i'], 0, &['a', 'l']),
        (&['e', 'n', 't', 'l', 'i'], 0, &['e', 'n', 't']),
        (&['e', 'l', 'i'], 0, &['e']),
        (&['o', 'u', 's', 'l', 'i'], 0, &['o', 'u', 's']),
        (&['i', 'z', 'a', 't', 'i', 'o', 'n'], 0, &['i', 'z', 'e']),
        (&['a', 't', 'i', 'o', 'n'], 0, &['a', 't', 'e']),
        (&['a', 't', 'o', 'r'], 0, &['a', 't', 'e']),
        (&['a', 'l', 'i', 's', 'm'], 0, &['a', 'l']),
        (&['i', 'v', 'e', 'n', 'e', 's', 's'], 0, &['i', 'v', 'e']),
        (&['f', 'u', 'l', 'n', 'e', 's', 's'], 0, &['f', 'u', 'l']),
        (&['o', 'u', 's', 'n', 'e', 's', 's'], 0, &['o', 'u', 's']),
        (&['a', 'l', 'i', 't', 'i'], 0, &['a', 'l']),
        (&['i', 'v', 'i', 't', 'i'], 0, &['i', 'v', 'e']),
        (&['b', 'i', 'l', 'i', 't', 'i'], 0, &['b', 'l', 'e']),
        (&['l', 'o', 'g', 'i'], 0, &['l', 'o', 'g']),
    ];
    for (suffix, threshold, replacement) in rules {
        if has_suffix(word, suffix) && measure(&word[..word.len() - suffix.len()]) > *threshold {
            let at = word.len() - suffix.len();
            word.truncate(at);
            word.extend_from_slice(replacement);
            return;
        }
    }
}

fn step3(word: &mut Vec<char>) {
    let rules: &[(&[char], &[char])] = &[
        (&['i', 'c', 'a', 't', 'e'], &['i', 'c']),
        (&['a', 't', 'i', 'v', 'e'], &[]),
        (&['a', 'l', 'i', 'z', 'e'], &['a', 'l']),
        (&['i', 'c', 'i', 't', 'i'], &['i', 'c']),
        (&['i', 'c', 'a', 'l'], &['i', 'c']),
        (&['f', 'u', 'l'], &[]),
        (&['n', 'e', 's', 's'], &[]),
    ];
    for (suffix, replacement) in rules {
        if has_suffix(word, suffix) && measure(&word[..word.len() - suffix.len()]) > 0 {
            let at = word.len() - suffix.len();
            word.truncate(at);
            word.extend_from_slice(replacement);
            return;
        }
    }
}

fn step4(word: &mut Vec<char>) {
    for suffix in [
        ['a', 'l'].as_slice(),
        &['a', 'n', 'c', 'e'],
        &['e', 'n', 'c', 'e'],
        &['e', 'r'],
        &['i', 'c'],
        &['a', 'b', 'l', 'e'],
        &['i', 'b', 'l', 'e'],
        &['a', 'n', 't'],
        &['e', 'm', 'e', 'n', 't'],
        &['m', 'e', 'n', 't'],
        &['e', 'n', 't'],
        &['o', 'u'],
        &['i', 's', 'm'],
        &['a', 't', 'e'],
        &['i', 't', 'i'],
        &['o', 'u', 's'],
        &['i', 'v', 'e'],
        &['i', 'z', 'e'],
    ] {
        if has_suffix(word, suffix) && measure(&word[..word.len() - suffix.len()]) > 1 {
            word.truncate(word.len() - suffix.len());
            return;
        }
    }
    if has_suffix(word, &['i', 'o', 'n']) {
        let stem = &word[..word.len() - 3];
        if measure(stem) > 1 && matches!(stem[stem.len() - 1], 's' | 't') {
            word.truncate(word.len() - 3);
        }
    }
}

fn step5a(word: &mut Vec<char>) {
    if word[word.len() - 1] != 'e' {
        return;
    }
    let stem = &word[..word.len() - 1];
    let m = measure(stem);
    if m > 1 {
        word.truncate(word.len() - 1);
    } else if m == 1 {
        let c = stem[stem.len() - 1];
        if !has_cvc(stem) || matches!(c, 'w' | 'x' | 'y') {
            word.truncate(word.len() - 1);
        }
    }
}

fn step5b(word: &mut Vec<char>) {
    if word.len() > 2 && word[word.len() - 2] == 'l' && word[word.len() - 1] == 'l' {
        let stem = &word[..word.len() - 1];
        if measure(stem) > 1 {
            word.truncate(word.len() - 1);
        }
    }
}

/// Stems a word without lowercasing (Go `StemWithoutLowerCasing`).
/// Words shorter than 3 chars pass through.
#[must_use]
pub fn stem_without_lower_casing(word: &str) -> String {
    let mut chars: Vec<char> = word.chars().collect();
    if chars.len() <= 2 {
        return word.to_owned();
    }
    step1a(&mut chars);
    step1b(&mut chars);
    step1c(&mut chars);
    step2(&mut chars);
    step3(&mut chars);
    step4(&mut chars);
    step5a(&mut chars);
    step5b(&mut chars);
    chars.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porter_vectors() {
        // Go-test vectors plus canonical step-1 examples from Porter's paper.
        for (input, expected) in [
            ("foxes", "fox"),
            ("dogs", "dog"),
            ("surprise", "surpris"),
            ("efficiency", "effici"),
            ("stemming", "stem"),
            ("works", "work"),
            ("running", "run"),
            ("quickly", "quickli"),
            ("tokenizer", "token"),
            ("jumps", "jump"),
            ("lazy", "lazi"),
            ("caresses", "caress"),
            ("ponies", "poni"),
            ("caress", "caress"),
            ("cats", "cat"),
            ("feed", "feed"),
            ("agreed", "agre"),
            ("plastered", "plaster"),
            ("bled", "bled"),
            ("motoring", "motor"),
            ("sing", "sing"),
            ("conflated", "conflat"),
            ("troubled", "troubl"),
            ("sized", "size"),
            ("hopping", "hop"),
            ("tanned", "tan"),
            ("falling", "fall"),
            ("hissing", "hiss"),
            ("fizzed", "fizz"),
            ("failing", "fail"),
            ("filing", "file"),
            ("rational", "ration"),
            ("a", "a"),
            ("to", "to"),
        ] {
            assert_eq!(stem_without_lower_casing(input), expected, "{input}");
        }
    }
}
