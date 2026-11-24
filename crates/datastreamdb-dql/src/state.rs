//! DQL lexer states. Port of `dql/state.go`.

#![warn(missing_docs)]

use datastreamdb_lex::{EOF, ITEM_EOF, ItemType, Lexer, StateFn, iri_ref, is_end_of_line};

pub(crate) const ITEM_TEXT: ItemType = 5;
pub(crate) const ITEM_LEFT_CURL: ItemType = 6;
pub(crate) const ITEM_RIGHT_CURL: ItemType = 7;
pub(crate) const ITEM_EQUAL: ItemType = 8;
pub(crate) const ITEM_NAME: ItemType = 9;
pub(crate) const ITEM_OP_TYPE: ItemType = 10;
pub(crate) const ITEM_LEFT_ROUND: ItemType = 11;
pub(crate) const ITEM_RIGHT_ROUND: ItemType = 12;
pub(crate) const ITEM_COLON: ItemType = 13;
pub(crate) const ITEM_AT: ItemType = 14;
pub(crate) const ITEM_PERIOD: ItemType = 15;
pub(crate) const ITEM_DOLLAR: ItemType = 16;
pub(crate) const ITEM_REGEX: ItemType = 17;
pub(crate) const ITEM_MUTATION_OP: ItemType = 18;
pub(crate) const ITEM_MUTATION_OP_CONTENT: ItemType = 19;
pub(crate) const ITEM_UPSERT_BLOCK: ItemType = 20;
pub(crate) const ITEM_UPSERT_BLOCK_OP: ItemType = 21;
pub(crate) const ITEM_UPSERT_BLOCK_OP_CONTENT: ItemType = 22;
pub(crate) const ITEM_LEFT_SQUARE: ItemType = 23;
pub(crate) const ITEM_RIGHT_SQUARE: ItemType = 24;
pub(crate) const ITEM_COMMA: ItemType = 25;
pub(crate) const ITEM_MATH_OP: ItemType = 26;
pub(crate) const ITEM_STAR: ItemType = 27;

/// Formats a rune Go `%#U` style (`U+0041 'A'`).
fn show_rune(r: Option<char>) -> String {
    match r {
        Some(c) => format!("U+{:04X} {c:?}", c as u32),
        None => "U+FFFFFFFF <EOF>".to_owned(),
    }
}

/// Lexes the upsert block.
fn lex_upsert_block(l: &mut Lexer) -> Option<StateFn> {
    l.mode = Some(StateFn(lex_upsert_block));
    loop {
        match l.next() {
            Some('}') => {
                l.block_depth -= 1;
                l.emit(ITEM_RIGHT_CURL);
                if l.block_depth == 0 {
                    return Some(StateFn(lex_top_level));
                }
            }
            Some('{') => {
                l.block_depth += 1;
                l.emit(ITEM_LEFT_CURL);
            }
            Some(r) if is_space(r) || is_end_of_line(Some(r)) => l.ignore(),
            Some(r) if is_name_begin(r) => return Some(StateFn(lex_name_upsert_op)),
            Some('#') => return Some(StateFn(lex_comment)),
            None => return l.errorf("Unclosed upsert block"),
            Some(r) => {
                return l.errorf(format!(
                    "Unrecognized character in upsert block: {}",
                    show_rune(Some(r))
                ));
            }
        }
    }
}

/// Parses operation names inside an upsert block.
fn lex_name_upsert_op(l: &mut Lexer) -> Option<StateFn> {
    // The caller already checked isNameBegin, and absorbed one rune.
    l.accept_run(is_name_suffix);
    let word = l.input[l.start..l.pos].to_owned();
    match word.as_str() {
        "query" | "fragment" => {
            l.emit(ITEM_UPSERT_BLOCK_OP);
            Some(StateFn(lex_block_content))
        }
        "mutation" => {
            l.emit(ITEM_UPSERT_BLOCK_OP);
            Some(StateFn(lex_inside_mutation))
        }
        _ => l.errorf(format!("Invalid operation type: {word}")),
    }
}

/// Lexes text inside a brace-covered block.
fn lex_block_content(l: &mut Lexer) -> Option<StateFn> {
    lex_content(l, '{', '}', StateFn(lex_upsert_block))
}

/// Lexes a whole `@if(...)` directive in a mutation block.
fn lex_if_content(l: &mut Lexer) -> Option<StateFn> {
    if l.next() != Some('@') {
        let r = l.peek();
        return l.errorf(format!("Expected [@], found; [{}]", show_rune(r)));
    }
    l.accept_run(is_name_suffix);
    let word = l.input[l.start..l.pos].to_owned();
    if word != "@if" {
        return l.errorf(format!("Expected @if, found [{word}]"));
    }
    lex_content(l, '(', ')', StateFn(lex_inside_mutation))
}

fn lex_content(l: &mut Lexer, left: char, right: char, return_to: StateFn) -> Option<StateFn> {
    let mut depth = 0;
    loop {
        match l.next() {
            None => return l.errorf("Matching brackets not found"),
            Some('"') => {
                if let Err(err) = l.lex_quoted_string() {
                    return l.errorf(err.to_string());
                }
            }
            Some(r) if r == left => depth += 1,
            Some(r) if r == right => {
                depth -= 1;
                if depth < 0 {
                    return l.errorf(format!("Unopened {right} found"));
                }
                if depth == 0 {
                    l.emit(ITEM_UPSERT_BLOCK_OP_CONTENT);
                    return Some(return_to);
                }
            }
            Some(_) => {}
        }
    }
}

fn lex_inside_mutation(l: &mut Lexer) -> Option<StateFn> {
    l.mode = Some(StateFn(lex_inside_mutation));
    loop {
        match l.next() {
            Some('@') => {
                l.backup();
                return Some(StateFn(lex_if_content));
            }
            Some('}') => {
                l.depth -= 1;
                l.emit(ITEM_RIGHT_CURL);
                if l.depth == 0 {
                    return Some(StateFn(lex_top_level));
                }
            }
            Some('{') => {
                l.depth += 1;
                l.emit(ITEM_LEFT_CURL);
                if l.depth >= 2 {
                    return Some(StateFn(lex_text_mutation));
                }
            }
            Some(r) if is_space(r) || is_end_of_line(Some(r)) => l.ignore(),
            Some(r) if is_name_begin(r) => return Some(StateFn(lex_name_mutation)),
            Some('#') => return Some(StateFn(lex_comment)),
            None => return l.errorf("Unclosed mutation action"),
            Some(r) => {
                return l.errorf(format!(
                    "Unrecognized character inside mutation: {}",
                    show_rune(Some(r))
                ));
            }
        }
    }
}

fn lex_inside_schema(l: &mut Lexer) -> Option<StateFn> {
    l.mode = Some(StateFn(lex_inside_schema));
    loop {
        match l.next() {
            Some(')') => l.emit(ITEM_RIGHT_ROUND),
            Some('(') => l.emit(ITEM_LEFT_ROUND),
            Some('}') => {
                l.depth -= 1;
                l.emit(ITEM_RIGHT_CURL);
                if l.depth == 0 {
                    return Some(StateFn(lex_top_level));
                }
            }
            Some('{') => {
                l.depth += 1;
                l.emit(ITEM_LEFT_CURL);
            }
            Some('[') => l.emit(ITEM_LEFT_SQUARE),
            Some(']') => l.emit(ITEM_RIGHT_SQUARE),
            Some(r) if is_space(r) || is_end_of_line(Some(r)) => l.ignore(),
            Some('<') => return Some(StateFn(lex_iri_ref)),
            Some(r) if is_name_begin(r) => return Some(StateFn(lex_arg_name)),
            Some('#') => return Some(StateFn(lex_comment)),
            Some(':') => l.emit(ITEM_COLON),
            Some(',') => l.emit(ITEM_COMMA),
            None => return l.errorf("Unclosed schema action"),
            Some(r) => {
                return l.errorf(format!(
                    "Unrecognized character inside schema: {}",
                    show_rune(Some(r))
                ));
            }
        }
    }
}

fn lex_func_or_arg(l: &mut Lexer) -> Option<StateFn> {
    l.mode = Some(StateFn(lex_func_or_arg));
    // NOTE: `empty` is per-invocation: every return through `Mode` restarts
    // this function fresh, which is exactly how Go tracks comma state.
    let mut empty = false;
    loop {
        match l.next() {
            Some('@') => {
                l.emit(ITEM_AT);
                return Some(StateFn(lex_directive_or_lang_list));
            }
            Some(r) if is_name_begin(r) || is_number(r) => return Some(StateFn(lex_arg_name)),
            // A '/' starting an argument is a regex, otherwise division.
            Some('/') if empty => return Some(StateFn(lex_regex)),
            Some(r) if is_math_op(r) => l.emit(ITEM_MATH_OP),
            Some(r) if is_inequality_op(r) => {
                if r == '=' && !l.peek().is_some_and(is_inequality_op) {
                    l.emit(ITEM_EQUAL);
                    continue;
                }
                if r == '<' && !is_space(l.peek().unwrap_or(' ')) && l.peek() != Some('=') {
                    // As long as it is not '=' or ' '.
                    return Some(StateFn(lex_iri_ref));
                }
                if l.peek().is_some_and(is_inequality_op) {
                    l.next();
                }
                l.emit(ITEM_MATH_OP);
            }
            Some('(') => {
                l.emit(ITEM_LEFT_ROUND);
                l.arg_depth += 1;
            }
            Some(')') => {
                if l.arg_depth == 0 {
                    return l.errorf("Unexpected right round bracket");
                }
                l.arg_depth -= 1;
                l.emit(ITEM_RIGHT_ROUND);
                if empty {
                    return l.errorf("Empty Argument");
                }
                if l.arg_depth == 0 {
                    return Some(StateFn(lex_query));
                }
            }
            None => return l.errorf("Unclosed Brackets"),
            Some(r) if is_space(r) || is_end_of_line(Some(r)) => l.ignore(),
            Some(',') => {
                if empty {
                    return l.errorf("Consecutive commas not allowed.");
                }
                empty = true;
                l.emit(ITEM_COMMA);
            }
            Some(r) if is_dollar(r) => l.emit(ITEM_DOLLAR),
            Some(':') => l.emit(ITEM_COLON),
            Some('"') => {
                empty = false;
                if let Err(err) = l.lex_quoted_string() {
                    return l.errorf(err.to_string());
                }
                l.emit(ITEM_NAME);
            }
            Some(r) if is_end_literal(r) => {
                empty = false;
                l.accept_until(is_end_literal); // Backs up the ending ".
                l.next(); // Consume the ".
                l.emit(ITEM_NAME);
            }
            Some('[') => l.emit(ITEM_LEFT_SQUARE),
            Some(']') => l.emit(ITEM_RIGHT_SQUARE),
            Some('{') => {
                empty = false;
                l.emit(ITEM_LEFT_CURL);
                // Design decision: emit brace tokens without affecting ArgDepth
                // tracking. The parser validates legality in context.
            }
            Some('}') => {
                l.emit(ITEM_RIGHT_CURL);
                // Don't decrement ArgDepth for braces; let parser validate.
            }
            Some('#') => return Some(StateFn(lex_comment)),
            Some('.') => l.emit(ITEM_PERIOD),
            Some(r) => {
                return l.errorf(format!(
                    "Unrecognized character inside a func: {}",
                    show_rune(Some(r))
                ));
            }
        }
    }
}

pub(crate) fn lex_top_level(l: &mut Lexer) -> Option<StateFn> {
    // Only the upsert block nests blocks right now; BlockDepth tracks that.
    if l.block_depth != 0 {
        return Some(StateFn(lex_upsert_block));
    }
    l.mode = Some(StateFn(lex_top_level));
    loop {
        match l.next() {
            Some('{') => {
                l.depth += 1;
                l.emit(ITEM_LEFT_CURL);
                return Some(StateFn(lex_identify_mutation_or_query));
            }
            Some('}') => return l.errorf("Too many right curl"),
            None => break,
            Some('#') => return Some(StateFn(lex_comment)),
            Some('(') => {
                l.backup();
                l.emit(ITEM_TEXT);
                l.next();
                l.emit(ITEM_LEFT_ROUND);
                l.arg_depth += 1;
                return Some(StateFn(lex_query));
            }
            Some(r) if is_space(r) || is_end_of_line(Some(r)) => l.ignore(),
            Some(r) if is_name_begin(r) => {
                l.backup();
                return Some(StateFn(lex_operation_type));
            }
            // NOTE: Go has no default here: anything else is silently skipped.
            Some(_) => {}
        }
    }
    if l.pos > l.start {
        l.emit(ITEM_TEXT);
    }
    l.emit(ITEM_EOF);
    None
}

fn lex_identify_mutation_or_query(l: &mut Lexer) -> Option<StateFn> {
    l.mode = Some(StateFn(lex_identify_mutation_or_query));
    loop {
        match l.next() {
            Some(r) if is_space(r) || is_end_of_line(Some(r)) => l.ignore(),
            Some(r) if is_name_begin(r) => {
                l.accept_run(is_name_suffix);
                let op = l.input[l.start..l.pos].to_owned();
                if op == "set" || op == "delete" || op == "del" {
                    l.emit(ITEM_MUTATION_OP);
                    return Some(StateFn(lex_inside_mutation));
                }
                l.emit(ITEM_NAME);
                return Some(StateFn(lex_query));
            }
            Some('<') => {
                l.backup();
                return Some(StateFn(lex_query));
            }
            Some('#') => return Some(StateFn(lex_comment)),
            None => return l.errorf("Invalid DQL"),
            Some(r) => {
                return l.errorf(format!(
                    "Unexpected character while lexing DQL: {}",
                    show_rune(Some(r))
                ));
            }
        }
    }
}

/// Lexes the query body, dispatching to argument/directive lexers.
fn lex_query(l: &mut Lexer) -> Option<StateFn> {
    l.mode = Some(StateFn(lex_query));
    loop {
        match l.next() {
            Some('.') => l.emit(ITEM_PERIOD),
            Some('}') => {
                l.depth -= 1;
                l.emit(ITEM_RIGHT_CURL);
                if l.depth == 0 {
                    return Some(StateFn(lex_top_level));
                }
            }
            Some('{') => {
                l.depth += 1;
                l.emit(ITEM_LEFT_CURL);
            }
            None => return l.errorf("Unclosed action"),
            Some(r) if is_space(r) || is_end_of_line(Some(r)) => l.ignore(),
            Some(',') => l.emit(ITEM_COMMA),
            Some(r) if is_name_begin(r) => return Some(StateFn(lex_name)),
            Some('#') => return Some(StateFn(lex_comment)),
            Some('-') => l.emit(ITEM_MATH_OP),
            Some('(') => {
                l.emit(ITEM_LEFT_ROUND);
                l.accept_run(is_space);
                l.ignore();
                l.arg_depth += 1;
                return Some(StateFn(lex_func_or_arg));
            }
            Some(':') => l.emit(ITEM_COLON),
            Some('@') => {
                l.emit(ITEM_AT);
                return Some(StateFn(lex_directive_or_lang_list));
            }
            Some('<') => return Some(StateFn(lex_iri_ref)),
            Some('*') => l.emit(ITEM_STAR),
            Some(r) => {
                return l.errorf(format!(
                    "Unrecognized character in lexText: {}",
                    show_rune(Some(r))
                ));
            }
        }
    }
}

fn lex_iri_ref(l: &mut Lexer) -> Option<StateFn> {
    if let Err(err) = iri_ref(l, ITEM_NAME) {
        return l.errorf(err.to_string());
    }
    l.mode
}

/// Called right after `@`.
fn lex_directive_or_lang_list(l: &mut Lexer) -> Option<StateFn> {
    let r = l.next();
    // Check first character.
    if !r.is_some_and(is_name_begin) && r != Some('.') && r != Some('*') {
        return l.errorf(format!(
            "Unrecognized character in lexDirective: {}",
            show_rune(r)
        ));
    }
    l.backup();
    loop {
        let r = l.next();
        if r == Some('.') {
            l.emit(ITEM_NAME);
            return l.mode;
        }
        if r.is_some_and(is_lang_or_directive) {
            continue;
        }
        l.backup();
        l.emit(ITEM_NAME);
        break;
    }
    l.mode
}

fn lex_name(l: &mut Lexer) -> Option<StateFn> {
    l.accept_run(is_name_suffix);
    l.emit(ITEM_NAME);
    l.mode
}

/// Lexes a comment.
fn lex_comment(l: &mut Lexer) -> Option<StateFn> {
    loop {
        let r = l.next();
        if is_end_of_line(r) {
            l.ignore();
            return l.mode;
        }
        if r == EOF {
            break;
        }
    }
    l.ignore();
    l.emit(ITEM_EOF);
    l.mode
}

/// Lexes a `set`/`delete` mutation op word.
fn lex_name_mutation(l: &mut Lexer) -> Option<StateFn> {
    loop {
        // The caller already checked isNameBegin, and absorbed one rune.
        let r = l.next();
        if r.is_some_and(is_name_suffix) {
            continue;
        }
        l.backup();
        l.emit(ITEM_MUTATION_OP);
        break;
    }
    l.mode
}

/// Lexes and absorbs text inside a mutation operation block.
fn lex_text_mutation(l: &mut Lexer) -> Option<StateFn> {
    loop {
        match l.next() {
            None => return l.errorf("Unclosed mutation text"),
            Some('"') => return Some(StateFn(lex_mutation_value)),
            Some('{') => return l.errorf("Invalid character '{' inside mutation text"),
            Some('}') => {
                l.backup();
                l.emit(ITEM_MUTATION_OP_CONTENT);
                break;
            }
            Some(_) => {}
        }
    }
    Some(StateFn(lex_inside_mutation))
}

/// Absorbs the object value inside mutation text.
fn lex_mutation_value(l: &mut Lexer) -> Option<StateFn> {
    loop {
        match l.next() {
            None => return l.errorf("Unclosed mutation value"),
            Some('"') => break,
            Some('\\') => {
                l.next(); // skip one.
            }
            Some(_) => {}
        }
    }
    Some(StateFn(lex_text_mutation))
}

fn lex_regex(l: &mut Lexer) -> Option<StateFn> {
    loop {
        match l.next() {
            None => return l.errorf("Unclosed regexp"),
            Some('\\') => {
                l.next();
            }
            Some('/') => break,
            Some(_) => {}
        }
    }
    l.accept_run(is_regex_flag);
    l.emit(ITEM_REGEX);
    l.mode
}

/// Lexes a query/mutation/schema/fragment/upsert operation type.
fn lex_operation_type(l: &mut Lexer) -> Option<StateFn> {
    l.accept_run(is_name_suffix);
    // Pos is the index of the end of the operation type + 1.
    let word = l.input[l.start..l.pos].to_owned();
    match word.as_str() {
        "mutation" => {
            l.emit(ITEM_OP_TYPE);
            Some(StateFn(lex_inside_mutation))
        }
        "fragment" | "query" => {
            l.emit(ITEM_OP_TYPE);
            Some(StateFn(lex_query))
        }
        "schema" => {
            l.emit(ITEM_OP_TYPE);
            Some(StateFn(lex_inside_schema))
        }
        "upsert" => {
            l.emit(ITEM_UPSERT_BLOCK);
            Some(StateFn(lex_upsert_block))
        }
        _ => l.errorf(format!("Invalid operation type: {word}")),
    }
}

/// Lexes and emits the name part of an argument.
fn lex_arg_name(l: &mut Lexer) -> Option<StateFn> {
    l.accept_run(is_name_suffix);
    l.emit(ITEM_NAME);
    l.mode
}

/// Whether the rune is a dollar sign.
fn is_dollar(r: char) -> bool {
    r == '$'
}

/// Whether the rune is a tab or space.
fn is_space(r: char) -> bool {
    r == '\u{0009}' || r == '\u{0020}'
}

/// Whether the rune ends a literal (quote, CR, or LF).
fn is_end_literal(r: char) -> bool {
    r == '"' || r == '\u{000d}' || r == '\u{000a}'
}

fn is_lang_or_directive(r: char) -> bool {
    is_name_begin(r) || r == '-' || r.is_ascii_digit() || r == '*'
}

/// Whether the rune can start a name (alphabet, `_`, or `~`).
fn is_name_begin(r: char) -> bool {
    r.is_ascii_alphabetic() || r == '_' || r == '~'
}

fn is_math_op(r: char) -> bool {
    matches!(r, '+' | '-' | '*' | '/' | '%')
}

fn is_inequality_op(r: char) -> bool {
    matches!(r, '<' | '>' | '=' | '!')
}

/// Whether the rune is a digit.
fn is_number(r: char) -> bool {
    r.is_ascii_digit()
}

fn is_name_suffix(r: char) -> bool {
    if is_math_op(r) {
        return false;
    }
    if is_name_begin(r) || is_number(r) {
        return true;
    }
    // '.' is used by freebase.
    r == '.'
}

fn is_regex_flag(r: char) -> bool {
    r.is_ascii_alphabetic()
}

#[cfg(test)]
mod tests {
    use super::*;
    use datastreamdb_lex::{ITEM_ERROR, Lexer};

    /// Runs the full top-level lexer, asserting no item errors.
    fn lex_ok(input: &str) {
        let mut lexer = Lexer::default();
        lexer.input = input.to_owned();
        lexer.run(StateFn(lex_top_level));
        let mut it = lexer.new_iterator();
        while it.advance() {
            let item = it.item();
            assert_ne!(item.typ, ITEM_ERROR, "item: {item}");
        }
    }

    /// Runs the full top-level lexer, returning the last item type.
    fn lex_last(input: &str) -> ItemType {
        let mut lexer = Lexer::default();
        lexer.input = input.to_owned();
        lexer.run(StateFn(lex_top_level));
        let mut it = lexer.new_iterator();
        let mut typ = ITEM_EOF;
        while it.advance() {
            typ = it.item().typ;
        }
        typ
    }

    #[test]
    fn query_lexing() {
        // Port of TestQueryLexing (starts mid-query like Go).
        let input = "
query {
\tme(_xid_: rick, id:10 ) {
\t\tname0 # my name
\t\t_city, # 0what would fail lex.
\t\tprofilePic(width: 100, height: 100)
\t\tfriends {
\t\t\tname
\t\t}
\t}
}";
        let mut lexer = Lexer::default();
        lexer.reset(input);
        lexer.run(StateFn(lex_query));
        let mut it = lexer.new_iterator();
        while it.advance() {
            assert_ne!(it.item().typ, ITEM_ERROR);
        }
    }

    #[test]
    fn mutation_lexing() {
        lex_ok(
            "
mutation {
\tset {
\t\tWhat is <this> .
\t\tWhy is this #!!?
\t\tHow is this?
\t}
\tdelete {
\t\tWhy is this
\t}
}
query {
\tme(_xid_: rick) {
\t\t_city
\t}
}",
        );
    }

    #[test]
    fn new_schema_query() {
        lex_ok(
            "
schema {
\tpred
\ttype
}
schema( pred : name ) {
\tpred
\ttype
}
schema( pred : name,hi ) { #This is valid in lexer, parser would throw error
\tpred
\ttype
}
schema( pred : [name,hi] ) {
\tpred
\ttype
}",
        );
    }

    #[test]
    fn abrupt_schema_query() {
        assert_eq!(lex_last("\nschema {\n\tpred\n"), ITEM_ERROR);
    }

    #[test]
    fn abrupt_mutation() {
        assert_eq!(
            lex_last(
                "\nmutation {\n\tset {\n\t\tWhat is <this> .\n\t\tWhy is this #!!?\n\t\tHow is this?\n}"
            ),
            ITEM_ERROR
        );
    }

    #[test]
    fn variables_lex() {
        lex_ok("\nquery testQuery($username: String!) {\n\tme(_xid_: rick) {\n\t\t_city\n\t}\n}");
        lex_ok(
            "\nquery testQuery ($username: String, $id: int, $email: string) {\n\tme(_xid_: rick) {\n\t\t_city\n\t}\n}",
        );
        lex_ok(
            "\nquery testQuery ($username: string = abc, $id: int = 5, $email: string) {\n\tme(_xid_: rick) {\n\t\t_city\n\t}\n}",
        );
    }

    #[test]
    fn iri_ref_lexing() {
        lex_ok("{\n\t<user-query>(func: uid(0x1)) {\n\t\tname\n\t}\n}");
        lex_ok(
            "\nquery testQuery {\n\tme(id : <http://helloworld.com/how/are/you>) {\n\t        <http://verygood.com/what/about/you>\n\t}\n}",
        );
    }

    #[test]
    fn lang_lexing() {
        lex_ok("\nquery {\n\tme(id: test) {\n\t\tname@en\n\t}\n}\n");
        lex_ok("\nquery {\n\tme(id: test) {\n\t\tname@en, name@en:ru:fr:de\n\t}\n}\n");
    }

    #[test]
    fn number_in_lang() {
        // Port of TestNumberInLang (also pins the `es-419` token).
        let input = "\n{\n\tq(func: eq(name@es-419, \"aoeu\")) {\n\t\tname@.\n\t}\n}\n";
        let mut lexer = Lexer::default();
        lexer.input = input.to_owned();
        lexer.run(StateFn(lex_top_level));
        let mut it = lexer.new_iterator();
        let mut got_es = false;
        while it.advance() {
            let item = it.item();
            assert_ne!(item.typ, ITEM_ERROR);
            if item.val == "es-419" {
                got_es = true;
            }
        }
        assert!(got_es);
    }
}
