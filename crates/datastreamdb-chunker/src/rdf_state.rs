//! RDF N-Quad lexer states. Port of `rdf_state.go`.

#![warn(missing_docs)]

use datastreamdb_lex::{EOF, ITEM_EOF, ItemType, Lexer, StateFn, has_uchars, has_xchars, iri_ref};

pub(crate) const ITEM_TEXT: ItemType = 5;
pub(crate) const ITEM_SUBJECT: ItemType = 6;
pub(crate) const ITEM_PREDICATE: ItemType = 7;
pub(crate) const ITEM_OBJECT: ItemType = 8;
pub(crate) const ITEM_LABEL: ItemType = 9;
pub(crate) const ITEM_LITERAL: ItemType = 10;
pub(crate) const ITEM_LANGUAGE: ItemType = 11;
pub(crate) const ITEM_OBJECT_TYPE: ItemType = 12;
pub(crate) const ITEM_VALID_END: ItemType = 13;
pub(crate) const ITEM_COMMENT: ItemType = 14;
pub(crate) const ITEM_COMMA: ItemType = 15;
pub(crate) const ITEM_EQUAL: ItemType = 16;
pub(crate) const ITEM_LEFT_ROUND: ItemType = 17;
pub(crate) const ITEM_RIGHT_ROUND: ItemType = 18;
pub(crate) const ITEM_STAR: ItemType = 19;
pub(crate) const ITEM_SUBJECT_FUNC: ItemType = 20;
pub(crate) const ITEM_OBJECT_FUNC: ItemType = 21;
pub(crate) const ITEM_VAR_NAME: ItemType = 22;

/// Depth trackers while parsing an N-Quad.
const AT_SUBJECT: i32 = 0;
const AT_PREDICATE: i32 = 1;
const AT_OBJECT: i32 = 2;
const AT_LABEL: i32 = 3;
const AT_FACET: i32 = 4;

/// Formats a rune for `%c`-style errors (`None` renders as `EOF`).
fn show(r: Option<char>) -> String {
    r.map_or_else(|| "EOF".to_owned(), |c| c.to_string())
}

/// Inspects the next rune and calls the appropriate state function.
pub(crate) fn lex_text(l: &mut Lexer) -> Option<StateFn> {
    loop {
        match l.next() {
            Some(r @ ('<' | '_')) => {
                let next = match l.depth {
                    AT_SUBJECT => lex_subject,
                    AT_PREDICATE => lex_predicate,
                    AT_OBJECT => lex_object,
                    AT_LABEL => lex_label,
                    _ => return l.errorf(format!("Invalid input: {r} at lexText")),
                };
                l.backup();
                l.emit(ITEM_TEXT); // emit whatever we have so far.
                return Some(StateFn(next));
            }
            Some('"') => {
                if l.depth != AT_OBJECT {
                    return l.errorf("Invalid quote for non-object.");
                }
                l.backup();
                l.emit(ITEM_TEXT);
                return Some(StateFn(lex_object));
            }
            Some('#') => {
                if l.depth != AT_SUBJECT {
                    return l.errorf(format!("Invalid input: {} at lexText", show(Some('#'))));
                }
                return Some(StateFn(lex_comment));
            }
            Some('*') => {
                l.depth += 1;
                l.emit(ITEM_STAR);
            }
            Some('(') => {
                if l.depth > AT_OBJECT {
                    l.backup();
                    l.emit(ITEM_TEXT);
                    return Some(StateFn(lex_facets));
                }
                return l.errorf(format!("Invalid input: {} at Facet", show(Some('('))));
            }
            None => {
                l.emit(ITEM_EOF);
                return None;
            }
            Some('.') => {
                if l.depth > AT_OBJECT {
                    l.emit(ITEM_VALID_END);
                    l.depth = AT_SUBJECT;
                }
            }
            // UID or val function: just check for u or v.
            Some(r @ ('u' | 'v')) => {
                if l.depth != AT_SUBJECT && l.depth != AT_OBJECT {
                    return l.errorf(format!("Unexpected char '{r}'"));
                }
                l.backup();
                l.emit(ITEM_TEXT);
                return Some(StateFn(lex_variable));
            }
            Some(r) if is_space_rune(r) => {}
            Some(r) => {
                // Like Go, the error is recorded and lexing continues; the
                // run fails validation afterwards.
                l.errorf(format!("Invalid input: {r} at lexText"));
            }
        }
    }
}

/// Assumes the caller has consumed the initial `<`.
fn lex_iri_ref(l: &mut Lexer, styp: ItemType, next: StateFn) -> Option<StateFn> {
    if let Err(err) = iri_ref(l, styp) {
        return l.errorf(err.to_string());
    }
    Some(next)
}

fn lex_uid_node(l: &mut Lexer, styp: ItemType, next: StateFn) -> Option<StateFn> {
    l.accept_until(is_space_rune);
    let r = l.peek();
    if r == EOF {
        return l.errorf("Unexpected end of uid subject");
    }
    let input = l.input[l.start..l.pos].to_owned();
    if datastreamdb_types::strconv::parse_go_uint_base0(&input).is_err() {
        return l.errorf(format!("Unable to convert '{input}' to UID"));
    }
    if r.is_some_and(is_space_rune) {
        l.emit(styp);
        return Some(next);
    }
    l.errorf(format!(
        "Invalid character '{}' found for UID node itemType: {styp}",
        show(r)
    ))
}

/// Assumes the caller has consumed `_`.
/// `BLANK_NODE_LABEL ::= '_:' (PN_CHARS_U | [0-9]) ((PN_CHARS | '.')* PN_CHARS)?`
fn lex_blank_node(l: &mut Lexer, styp: ItemType, next: StateFn) -> Option<StateFn> {
    let r = l.next();
    if r != Some(':') {
        return l.errorf(format!(
            "Invalid character after _. Expected :, found '{}'",
            show(r)
        ));
    }
    let r = l.next();
    if r == EOF {
        return l.errorf("Unexpected end of subject");
    }
    if !r.is_some_and(|c| is_pn_chars_u(c) || c.is_ascii_digit()) {
        return l.errorf(format!(
            "Invalid character in {styp} after _: , Got '{}'",
            show(r)
        ));
    }
    let (last, valid) = l.accept_run(|r| r == '.' || is_pn_char(r));
    if valid && last == Some('.') {
        return l.errorf(format!("Can not end {styp} with '.'"));
    }
    let r = l.peek();
    if r == EOF {
        return l.errorf(format!("Unexpected end of {styp}"));
    }
    if r.is_some_and(is_space_rune) {
        l.emit(styp);
        return Some(next);
    }
    l.errorf(format!(
        "Invalid character '{}' found for itemType: {styp}",
        show(r)
    ))
}

fn lex_subject(l: &mut Lexer) -> Option<StateFn> {
    match l.next() {
        // The subject is an IRI, so we lex till we encounter '>'.
        Some('<') => {
            l.depth += 1;
            lex_iri_ref(l, ITEM_SUBJECT, StateFn(lex_text))
        }
        // The subject represents a blank node.
        Some('_') => {
            l.depth += 1;
            lex_blank_node(l, ITEM_SUBJECT, StateFn(lex_text))
        }
        // See if it is a uid.
        _ => lex_uid_node(l, ITEM_SUBJECT, StateFn(lex_text)),
    }
}

fn lex_predicate(l: &mut Lexer) -> Option<StateFn> {
    // The predicate can only be an IRI according to the spec.
    let r = l.next();
    if r != Some('<') {
        return l.errorf(format!("Invalid character in lexPredicate: '{}'", show(r)));
    }
    l.depth += 1;
    lex_iri_ref(l, ITEM_PREDICATE, StateFn(lex_text))
}

fn lex_language(l: &mut Lexer) -> Option<StateFn> {
    if l.next() != Some('@') {
        return l.errorf("Expected @ prefix for lexLanguage");
    }
    l.ignore();
    let r = l.next();
    if !r.is_some_and(is_lang_tag_prefix) {
        return l.errorf(format!("Invalid language tag prefix: '{}'", show(r)));
    }
    let (last, valid) = l.accept_run(is_lang_tag);
    if valid && last == Some('-') {
        return l.errorf("Invalid character - at the end of language literal.");
    }
    l.emit(ITEM_LANGUAGE);
    Some(StateFn(lex_text))
}

/// Assumes `"` has already been encountered.
/// `literal ::= STRING_LITERAL_QUOTE ('^^' IRIREF | LANGTAG)?`
fn lex_literal(l: &mut Lexer) -> Option<StateFn> {
    loop {
        let r = l.next();
        if r == Some('\\') {
            // Backslash.
            let r = l.next();
            if l.is_esc_char(r) || has_uchars(r, l) || has_xchars(r, l) {
                continue; // This skips over the escaped rune.
            }
            return l.errorf(format!(
                "Invalid escape character : '{}' in literal",
                show(r)
            ));
        }
        // 0x22 ('"') ends the literal.
        if r == Some('\u{5c}') || r == Some('\u{a}') || r == Some('\u{d}') {
            return l.errorf(format!("Invalid character '{}' in literal.", show(r)));
        }
        if r == EOF || r.is_some_and(is_end_literal) {
            break;
        }
    }
    l.emit(ITEM_LITERAL);
    l.depth += 1;
    match l.peek() {
        Some('@') => lex_language(l),
        Some('^') => lex_object_type(l),
        _ => Some(StateFn(lex_text)),
    }
}

fn lex_object_type(l: &mut Lexer) -> Option<StateFn> {
    if l.next() != Some('^') {
        return l.errorf("Expected ^ for lexObjectType");
    }
    if l.next() != Some('^') {
        return l.errorf("Expected ^^ for lexObjectType");
    }
    l.ignore();
    if l.next() != Some('<') {
        return l.errorf("Expected < for lexObjectType");
    }
    lex_iri_ref(l, ITEM_OBJECT_TYPE, StateFn(lex_text))
}

fn lex_object(l: &mut Lexer) -> Option<StateFn> {
    // The object can be an IRI, blank node, literal.
    match l.next() {
        Some('<') => {
            l.depth += 1;
            lex_iri_ref(l, ITEM_OBJECT, StateFn(lex_text))
        }
        Some('_') => {
            l.depth += 1;
            lex_blank_node(l, ITEM_OBJECT, StateFn(lex_text))
        }
        Some('"') => lex_literal(l),
        r => l.errorf(format!("Invalid char: '{}' at lexObject", show(r))),
    }
}

fn lex_label(l: &mut Lexer) -> Option<StateFn> {
    // Graph label can either be an IRI or a blank node according to spec.
    match l.next() {
        Some('<') => {
            l.depth += 1;
            lex_iri_ref(l, ITEM_LABEL, StateFn(lex_text))
        }
        Some('_') => {
            l.depth += 1;
            lex_blank_node(l, ITEM_LABEL, StateFn(lex_text))
        }
        r => l.errorf(format!("Invalid char: '{}' at lexLabel", show(r))),
    }
}

/// Parses facet key-value pairs, e.g.
/// `(key1 = "value1", key2=13, key3=, key4=2.4, key5=2006-01-02T15:04:05)`.
fn lex_facets(l: &mut Lexer) -> Option<StateFn> {
    let r = l.next();
    if r != Some('(') {
        return l.errorf(format!("Expected '(' but found '{}' at Facet.", show(r)));
    }
    l.emit(ITEM_LEFT_ROUND);
    // We can come here from lexObject too, so set ahead of atFacet explicitly.
    l.depth = AT_FACET + 1;
    loop {
        match l.next() {
            Some(r) if is_space_rune(r) => l.ignore(),
            Some('=') => l.emit(ITEM_EQUAL),
            Some(',') => l.emit(ITEM_COMMA),
            Some(')') => {
                l.emit(ITEM_RIGHT_ROUND);
                break;
            }
            None => {
                l.emit(ITEM_EOF);
                return None;
            }
            Some('"') => {
                if let Err(err) = l.lex_quoted_string() {
                    return l.errorf(err.to_string());
                }
                l.emit(ITEM_TEXT);
            }
            Some(_) => {
                l.accept_run(|r| r != '=' && !is_space_rune(r) && r != ')' && r != ',');
                l.emit(ITEM_TEXT);
            }
        }
    }
    Some(StateFn(lex_text))
}

/// Lexes a comment line.
fn lex_comment(l: &mut Lexer) -> Option<StateFn> {
    l.backup();
    loop {
        let r = l.next();
        if datastreamdb_lex::is_end_of_line(r) || r == EOF {
            break;
        }
    }
    l.emit(ITEM_COMMENT);
    l.emit(ITEM_EOF);
    None // Stop the run loop.
}

fn lex_variable(l: &mut Lexer) -> Option<StateFn> {
    let function = if l.next() == Some('v') { "val" } else { "uid" };
    l.backup();
    for c in function.chars() {
        let r = l.next();
        if r != Some(c) {
            return l.errorf(format!(
                "Unexpected char '{}' when parsing uid keyword",
                show(r)
            ));
        }
    }
    if l.depth == AT_OBJECT {
        l.emit(ITEM_OBJECT_FUNC);
    } else if l.depth == AT_SUBJECT {
        l.emit(ITEM_SUBJECT_FUNC);
    }
    l.ignore_run(is_space_rune);
    let r = l.next();
    if r != Some('(') {
        return l.errorf(format!(
            "Expected '(' after uid keyword, found: '{}'",
            show(r)
        ));
    }
    l.emit(ITEM_LEFT_ROUND);
    l.ignore_run(is_space_rune);
    // TODO(Aman): we support all characters in variable names except space and
    // right bracket. we should support only limited characters in variable names.
    // For now, this is fine because variables names must be used once in query
    // block before they can be used here. And, we throw an error if number of
    // used variables are different than number of defined variables.
    if !l.accept_run(|r| !(is_space_rune(r) || r == ')')).1 {
        return l.errorf("Unexpected end of input while reading variable name");
    }
    l.emit(ITEM_VAR_NAME);
    l.ignore_run(is_space_rune);
    let r = l.next();
    if r != Some(')') {
        return l.errorf(format!(
            "Expected ')' while reading function found: '{}'",
            show(r)
        ));
    }
    l.emit(ITEM_RIGHT_ROUND);
    l.depth += 1;
    Some(StateFn(lex_text))
}

/// Whether the rune is a tab or space.
fn is_space_rune(r: char) -> bool {
    r == '\u{0009}' || r == '\u{0020}'
}

fn is_end_literal(r: char) -> bool {
    r == '"' || r == '\u{000d}' || r == '\u{000a}'
}

fn is_lang_tag_prefix(r: char) -> bool {
    r.is_ascii_alphabetic()
}

/// Whether the rune is allowed in a language tag by the RDF spec.
fn is_lang_tag(r: char) -> bool {
    is_lang_tag_prefix(r) || r == '-' || r.is_ascii_digit()
}

/// `PN_CHARS_BASE` ranges from the RDF spec.
fn is_pn_chars_base(r: char) -> bool {
    let r = r as u32;
    matches!(r,
        0x61..=0x7A | 0x41..=0x5A | 0xC0..=0xD6 | 0xD8..=0xF6 | 0xF8..=0x2FF
        | 0x370..=0x37D | 0x37F..=0x1FFF | 0x200C..=0x200D | 0x2070..=0x218F
        | 0x2C00..=0x2FEF | 0x3001..=0xD7FF | 0xF900..=0xFDCF | 0xFDF0..=0xFFFD
        | 0x10000..=0xEFFFF)
}

/// `PN_CHARS_U ::= PN_CHARS_BASE | '_' | ':'`
fn is_pn_chars_u(r: char) -> bool {
    r == '_' || r == ':' || is_pn_chars_base(r)
}

/// `PN_CHARS ::= PN_CHARS_U | '-' | [0-9] | #x00B7 | [#x0300-#x036F] | [#x203F-#x2040]`
fn is_pn_char(r: char) -> bool {
    let v = r as u32;
    r == '-'
        || r.is_ascii_digit()
        || v == 0xB7
        || matches!(v, 0x300..=0x36F | 0x203F..=0x2040)
        || is_pn_chars_u(r)
}
