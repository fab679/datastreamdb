//! IRI lexing (`IRIREF ::= '<' ... '>'`, `\u`/`\U`/`\x` escapes).
//!
//! Rust port of the upstream Go `lex/iri.go`.

use crate::{ItemType, LexError, Lexer};

/// Emits an IRIREF (without `<`/`>`), or errors on invalid input.
///
/// The caller must have just read the opening `<` (it is ignored here).
///
/// # Errors
///
/// Errors on end of input before `>` or on an unexpected character.
pub fn iri_ref(l: &mut Lexer, styp: ItemType) -> Result<(), LexError> {
    l.ignore(); // ignore '<'
    l.accept_run_rec(is_iri_ref_char);
    l.emit(styp); // will emit without '<' and '>'
    match l.next() {
        None => Err(LexError::from("Unexpected end of IRI")),
        Some('>') => {
            l.ignore(); // ignore '>'
            Ok(())
        }
        Some(r) => Err(LexError::from(format!(
            "Unexpected character {r:?} while parsing IRI"
        ))),
    }
}

/// Whether the rune may appear in an `IRIRef`.
///
/// ``IRIREF ::= '<' ([^#x00-#x20<>"{}|^`\\] | UCHAR)* '>'``
fn is_iri_ref_char(r: char, l: &mut Lexer) -> bool {
    if (r as u32) <= 32 {
        // No chars between 0x00 and 0x20 inclusive.
        return false;
    }
    match r {
        '<' | '>' | '"' | '{' | '}' | '|' | '^' | '`' => false,
        '\\' => match l.next() {
            Some(r2) if r2 == 'u' || r2 == 'U' => has_uchars(Some(r2), l),
            _ => {
                l.backup();
                false
            }
        },
        _ => true,
    }
}

/// Whether the lexer is at the start of an escaped Unicode character.
///
/// `UCHAR ::= '\u' HEX HEX HEX HEX | '\U' HEX HEX HEX HEX HEX HEX HEX HEX`
#[must_use]
pub fn has_uchars(r: Option<char>, l: &mut Lexer) -> bool {
    let times = match r {
        Some('u') => 4,
        Some('U') => 8,
        _ => return false,
    };
    times == l.accept_run_times(is_hex, times)
}

/// Whether the lexer is at the start of an escaped hex byte (e.g. `\xFE`).
///
/// `XCHAR ::= '\x' HEX HEX`
#[must_use]
pub fn has_xchars(r: Option<char>, l: &mut Lexer) -> bool {
    if r != Some('x') {
        return false;
    }
    2 == l.accept_run_times(is_hex, 2)
}

/// `HEX ::= [0-9] | [A-F] | [a-f]`
fn is_hex(r: char) -> bool {
    r.is_ascii_hexdigit()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Port of Go `TestHasUChars`.
    #[test]
    fn has_uchars_vectors() {
        let cases = [
            ("u1def", true),
            ("UADEFABCD", true),
            ("uYDQW", false),
            ("Uydvxypqt", false),
            ("abc", false),
            ("uabcdg", true),
        ];
        for (input, expected) in cases {
            let mut lexer = Lexer::with_input(input);
            let r = lexer.next();
            assert_eq!(has_uchars(r, &mut lexer), expected, "input {input:?}");
        }
    }

    /// Port of Go `TestHasXChars`.
    #[test]
    fn has_xchars_vectors() {
        let cases = [
            ("xad", true),
            ("xAD", true),
            ("xYD", false),
            ("xyd", false),
            ("abc", false),
        ];
        for (input, expected) in cases {
            let mut lexer = Lexer::with_input(input);
            let r = lexer.next();
            assert_eq!(has_xchars(r, &mut lexer), expected, "input {input:?}");
        }
    }

    /// Port of Go `TestIsHex`.
    #[test]
    fn is_hex_vectors() {
        let cases = [
            ('x', false),
            ('X', false),
            ('L', false),
            ('A', true),
            ('b', true),
        ];
        for (input, expected) in cases {
            assert_eq!(is_hex(input), expected, "input {input:?}");
        }
    }

    /// Port of Go `TestIsIRIRefChar`.
    #[test]
    fn is_iri_ref_char_vectors() {
        let cases = [
            ("\\u1def", true),
            ("\\UADEFABCD", true),
            ("A", true),
            ("a", true),
            ("<", false),
            (">", false),
            ("{", false),
            ("`", false),
            ("\t", false),
            ("\\abc", false),
        ];
        for (input, expected) in cases {
            let mut lexer = Lexer::with_input(input);
            let r = lexer.next().expect("test inputs are non-empty");
            assert_eq!(is_iri_ref_char(r, &mut lexer), expected, "input {input:?}");
        }
    }

    #[test]
    fn iri_ref_roundtrip() {
        let mut lexer = Lexer::with_input("<http://example.org/x>");
        lexer.next(); // '<', as callers do
        iri_ref(&mut lexer, 5).unwrap();
        let mut it = lexer.new_iterator();
        assert!(it.advance());
        assert_eq!(it.item().val, "http://example.org/x");
    }

    #[test]
    fn iri_ref_errors() {
        let mut unterminated = Lexer::with_input("<abc");
        unterminated.next();
        assert_eq!(
            iri_ref(&mut unterminated, 5).unwrap_err().to_string(),
            "Unexpected end of IRI"
        );
        let mut bad_char = Lexer::with_input("<a b>");
        bad_char.next();
        assert_eq!(
            iri_ref(&mut bad_char, 5).unwrap_err().to_string(),
            "Unexpected character ' ' while parsing IRI"
        );
    }
}
