//! Schema text lexer states. Port of Go's `schema/state.go`.

#![warn(missing_docs)]

use datastreamdb_lex::{EOF, ITEM_EOF, ItemType, Lexer, StateFn, iri_ref, is_end_of_line};

pub(crate) const ITEM_TEXT: ItemType = 5;
pub(crate) const ITEM_NUMBER: ItemType = 6;
pub(crate) const ITEM_LEFT_CURL: ItemType = 7;
pub(crate) const ITEM_RIGHT_CURL: ItemType = 8;
pub(crate) const ITEM_COLON: ItemType = 9;
pub(crate) const ITEM_LEFT_ROUND: ItemType = 10;
pub(crate) const ITEM_RIGHT_ROUND: ItemType = 11;
pub(crate) const ITEM_AT: ItemType = 12;
pub(crate) const ITEM_COMMA: ItemType = 13;
pub(crate) const ITEM_NEW_LINE: ItemType = 14;
pub(crate) const ITEM_DOT: ItemType = 15;
pub(crate) const ITEM_LEFT_SQUARE: ItemType = 16;
pub(crate) const ITEM_RIGHT_SQUARE: ItemType = 17;
pub(crate) const ITEM_EXCLAMATION: ItemType = 18;
#[allow(dead_code, reason = "Go parity: defined but never emitted")]
pub(crate) const ITEM_QUOTE: ItemType = 19;
pub(crate) const ITEM_QUOTED_TEXT: ItemType = 20;

pub(crate) fn lex_text(l: &mut Lexer) -> Option<StateFn> {
    loop {
        match l.next() {
            None => break,
            Some(r) if is_name_begin(r) => {
                l.backup();
                return Some(StateFn(lex_word));
            }
            Some(r) if is_space(r) => l.ignore(),
            Some(r) if is_end_of_line(Some(r)) => l.emit(ITEM_NEW_LINE),
            Some('.') => l.emit(ITEM_DOT),
            Some('#') => return Some(StateFn(lex_text_comment)),
            Some(',') => l.emit(ITEM_COMMA),
            Some('<') => {
                if let Err(err) = iri_ref(l, ITEM_TEXT) {
                    return l.errorf(format!("Invalid schema: {err}"));
                }
            }
            Some('{') => l.emit(ITEM_LEFT_CURL),
            Some('}') => l.emit(ITEM_RIGHT_CURL),
            Some('(') => l.emit(ITEM_LEFT_ROUND),
            Some(')') => l.emit(ITEM_RIGHT_ROUND),
            Some(':') => l.emit(ITEM_COLON),
            Some('@') => l.emit(ITEM_AT),
            Some('[') => l.emit(ITEM_LEFT_SQUARE),
            Some(']') => l.emit(ITEM_RIGHT_SQUARE),
            Some('!') => l.emit(ITEM_EXCLAMATION),
            // Predicates can start with '_'.
            Some('_') => return Some(StateFn(lex_word)),
            Some(r) if is_digit(r) => {
                let [r0, r1] = l.peek_two();
                if r == '0' && r0.is_some_and(is_hex_separator) && r1.is_some_and(is_hexadecimal) {
                    l.backup();
                    return Some(StateFn(lex_hex_number));
                }
                l.backup();
                return Some(StateFn(lex_number));
            }
            Some('"') => {
                if let Err(err) = l.lex_quoted_string() {
                    return l.errorf(format!("Invalid schema: {err}"));
                }
                l.emit(ITEM_QUOTED_TEXT);
            }
            Some(_) => {
                let text = l.input[l.start..l.pos].to_owned();
                return l.errorf(format!("Invalid schema. Unexpected {text}"));
            }
        }
    }
    if l.pos > l.start {
        l.emit(ITEM_TEXT);
    }
    l.emit(ITEM_EOF);
    None
}

#[allow(clippy::unnecessary_wraps, reason = "all states share one fn type")]
fn lex_word(l: &mut Lexer) -> Option<StateFn> {
    loop {
        // The caller already checked the first rune and absorbed one rune.
        match l.next() {
            Some(r) if is_name_suffix(r) => {}
            _ => {
                l.backup();
                l.emit(ITEM_TEXT);
                break;
            }
        }
    }
    Some(StateFn(lex_text))
}

#[allow(clippy::unnecessary_wraps, reason = "all states share one fn type")]
fn lex_number(l: &mut Lexer) -> Option<StateFn> {
    loop {
        // The caller already checked is_digit and absorbed one rune.
        match l.next() {
            Some(r) if is_digit(r) => {}
            _ => {
                l.backup();
                l.emit(ITEM_NUMBER);
                break;
            }
        }
    }
    Some(StateFn(lex_text))
}

#[allow(clippy::unnecessary_wraps, reason = "all states share one fn type")]
fn lex_hex_number(l: &mut Lexer) -> Option<StateFn> {
    // It satisfies 0[xX]; process the input as hexadecimal.
    l.next();
    l.next(); // Absorb 0[xX].
    loop {
        // The caller already checked is_hexadecimal and absorbed one rune.
        match l.next() {
            Some(r) if is_hexadecimal(r) => {}
            _ => {
                l.backup();
                l.emit(ITEM_NUMBER);
                break;
            }
        }
    }
    Some(StateFn(lex_text))
}

/// Lexes a comment inside a schema.
#[allow(clippy::unnecessary_wraps, reason = "all states share one fn type")]
fn lex_text_comment(l: &mut Lexer) -> Option<StateFn> {
    loop {
        let r = l.next();
        if r == EOF {
            l.ignore();
            l.emit(ITEM_EOF);
            break;
        }
        if !is_end_of_line(r) {
            continue;
        }
        l.ignore();
        l.emit(ITEM_NEW_LINE);
        break;
    }
    Some(StateFn(lex_text))
}

/// Whether the rune can start a name (an alphabet).
fn is_name_begin(r: char) -> bool {
    r.is_ascii_alphabetic()
}

fn is_name_suffix(r: char) -> bool {
    // '_' | '.' | '-' are used by freebase.
    is_name_begin(r) || r.is_ascii_digit() || matches!(r, '_' | '.' | '-')
}

/// Whether the rune is hexadecimal.
fn is_hexadecimal(r: char) -> bool {
    r.is_ascii_hexdigit()
}

fn is_hex_separator(r: char) -> bool {
    r == 'x' || r == 'X'
}

/// Whether the rune is a digit.
fn is_digit(r: char) -> bool {
    r.is_ascii_digit()
}

/// Whether the rune is a tab or space.
fn is_space(r: char) -> bool {
    r == '\u{0009}' || r == '\u{0020}'
}
