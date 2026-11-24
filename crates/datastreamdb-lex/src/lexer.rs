//! Rob Pike-style state-function lexer.
//!
//! Rust port of the upstream Go `lex` package (`lexer.go`): the [`Lexer`] turns raw
//! input into [`Item`]s via caller-supplied [`StateFn`] state functions, and
//! [`ItemIterator`] lets parsers walk the emitted items with save/restore and
//! lookahead.
//!
//! # Port deltas
//!
//! - Go's `rune` + `EOF == -1` becomes `Option<char>`; the [`EOF`] constant
//!   (`None`) keeps call sites shaped like the Go (`r == EOF` still reads).
//! - `ItemIterator::Next` is named [`ItemIterator::advance`] because
//!   `Iterator::next` already means something else in Rust.
//! - Positions are `usize` byte offsets; line/column are `i32` (synthetic
//!   out-of-range items use `-1`, as in Go).
//! - Rust `&str` is guaranteed UTF-8, so there is no `RuneError` path.
//! - `lex_quoted_string` reports a trailing backslash at EOF as
//!   "Unexpected end of input." (Go formats `%c` of `EOF`, which is garbage).
//! - `Lexer::width` is kept for shape parity but, as in Go, is never written.

use std::fmt;

use crate::LexError;

/// End-of-input sentinel, returned by [`Lexer::next`] past the input end.
///
/// This is `None`, so ported call sites keep their Go shape: `r == EOF`.
pub const EOF: Option<char> = None;

/// Token type. `0` is EOF, `1` is error; downstream lexers start at `5`.
pub type ItemType = i32;

/// [`ItemType`] emitted when the end of the input is reached.
pub const ITEM_EOF: ItemType = 0;
/// [`ItemType`] emitted when lexing fails.
pub const ITEM_ERROR: ItemType = 1;

/// Next-state function of the lexer.
///
/// Go spells this as a recursive `func(*Lexer) StateFn` where `nil` stops the
/// run; Rust cannot name that recursive alias, so states are `fn` pointers
/// returning `None` to stop.
#[derive(Clone, Copy, Debug)]
pub struct StateFn(pub fn(&mut Lexer) -> Option<StateFn>);

/// Unit emitted by the lexer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// Token type.
    pub typ: ItemType,
    /// Token text (`input[start..pos]` at emission).
    pub val: String,
    line: i32,
    column: i32,
}

impl Item {
    /// 1-based line number where the item starts (`-1` when out of range).
    #[must_use]
    pub fn line(&self) -> i32 {
        self.line
    }

    /// Byte column where the item starts (`-1` when out of range).
    #[must_use]
    pub fn column(&self) -> i32 {
        self.column
    }

    /// Error message prefixed with the item's line and column.
    pub fn errorf(&self, msg: impl fmt::Display) -> LexError {
        LexError(format!(
            "line {} column {}: {}",
            self.line, self.column, msg
        ))
    }

    /// Out-of-range placeholder (Go zero `Item`: type EOF, line/column `-1`).
    fn out_of_range() -> Self {
        Self {
            typ: ITEM_EOF,
            val: String::new(),
            line: -1,
            column: -1,
        }
    }
}

impl fmt::Display for Item {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.typ == ITEM_EOF {
            return write!(f, "EOF");
        }
        write!(
            f,
            "lex.Item [{}] {:?} at {}:{}",
            self.typ, self.val, self.line, self.column
        )
    }
}

/// Consecutive run of equally-wide runes, for correct multi-step [`Lexer::backup`].
#[derive(Clone, Copy, Debug)]
struct RuneWidth {
    width: usize,
    /// Always `>= 1`; the entry is popped instead of reaching `0`.
    count: usize,
}

/// Converts raw input into [`Item`]s.
///
/// The zero value matches Go's `lex.Lexer{}` (line `0`); call [`Lexer::reset`]
/// before use, which sets line `1` and reuses the buffers.
#[derive(Debug, Default)]
pub struct Lexer {
    /// String being scanned.
    pub input: String,
    /// Start position (bytes) of the current item.
    pub start: usize,
    /// Current position (bytes) of the current item.
    pub pos: usize,
    /// Width of the last rune read. Never written, exactly as in Go.
    pub width: usize,
    width_stack: Vec<RuneWidth>,
    items: Vec<Item>,
    /// Nesting of `{}`.
    pub depth: i32,
    /// Nesting of blocks (e.g. mutation block inside upsert block).
    pub block_depth: i32,
    /// Nesting of `()`.
    pub arg_depth: i32,
    /// Default state to go back to after reading a token.
    pub mode: Option<StateFn>,
    /// Current line number corresponding to `start`.
    pub line: i32,
    /// Current byte column corresponding to `start`.
    pub column: i32,
}

/// Predicate signature for accepting valid runes.
pub type CheckRune = fn(char) -> bool;
/// [`CheckRune`] with the lexer as an extra argument (for recursive checks).
pub type CheckRuneRec = fn(char, &mut Lexer) -> bool;

impl Lexer {
    /// Fresh lexer over `input` (equivalent to default + [`Lexer::reset`]).
    pub fn with_input(input: impl Into<String>) -> Self {
        let mut lexer = Self::default();
        lexer.reset(input);
        lexer
    }

    /// Resets the lexer over new input, reusing already allocated buffers.
    pub fn reset(&mut self, input: impl Into<String>) {
        let mut items = std::mem::take(&mut self.items);
        let mut width_stack = std::mem::take(&mut self.width_stack);
        items.clear();
        width_stack.clear();
        *self = Self {
            input: input.into(),
            items,
            width_stack,
            line: 1,
            ..Self::default()
        };
    }

    /// Verifies the whole input lexed without errors.
    ///
    /// # Errors
    ///
    /// Returns the first [`ITEM_ERROR`] item's message, if any.
    pub fn validate_result(&self) -> Result<(), LexError> {
        let mut it = self.new_iterator();
        while it.advance() {
            let item = it.item();
            if item.typ == ITEM_ERROR {
                return Err(LexError(item.val));
            }
        }
        Ok(())
    }

    /// Executes state functions until one returns `None`.
    pub fn run(&mut self, first: StateFn) -> &mut Self {
        let mut state = Some(first);
        while let Some(next) = state {
            state = (next.0)(self);
        }
        self
    }

    /// Emits an [`ITEM_ERROR`] item and stops the run (returns `None`).
    pub fn errorf(&mut self, msg: impl fmt::Display) -> Option<StateFn> {
        self.items.push(Item {
            typ: ITEM_ERROR,
            val: format!(
                "while lexing {} at line {} column {}: {}",
                self.input, self.line, self.column, msg
            ),
            line: self.line,
            column: self.column,
        });
        None
    }

    /// Emits the current `input[start..pos]` slice with its type.
    pub fn emit(&mut self, typ: ItemType) {
        if typ != ITEM_EOF && self.pos < self.start {
            // Let ITEM_EOF go through.
            return;
        }
        let item = Item {
            typ,
            val: self.input[self.start..self.pos].to_owned(),
            line: self.line,
            column: self.column,
        };
        self.items.push(item);
        self.move_start_to_pos();
    }

    fn push_width(&mut self, width: usize) {
        if let Some(last) = self.width_stack.last_mut() {
            if last.width == width {
                last.count += 1;
                return;
            }
        }
        self.width_stack.push(RuneWidth { width, count: 1 });
    }

    /// Reads the next rune, advancing `pos` (`None` at end of input).
    #[allow(clippy::should_implement_trait, reason = "Go parity: lex.Lexer.Next")]
    pub fn next(&mut self) -> Option<char> {
        match self.input[self.pos..].chars().next() {
            None => {
                self.push_width(0);
                None
            }
            Some(r) => {
                self.push_width(r.len_utf8());
                self.pos += r.len_utf8();
                Some(r)
            }
        }
    }

    /// Moves the lexer back to its previous position.
    ///
    /// # Panics
    ///
    /// Panics when the width stack is empty (Go asserts the same).
    pub fn backup(&mut self) {
        assert!(
            !self.width_stack.is_empty(),
            "Backup should not be called when the width tracker stack is empty"
        );
        let last = self.width_stack.len() - 1;
        let width = self.width_stack[last].width;
        if self.width_stack[last].count == 1 {
            self.width_stack.pop();
        } else {
            self.width_stack[last].count -= 1;
        }
        self.pos -= width;
    }

    /// Returns the next rune without advancing the lexer.
    pub fn peek(&mut self) -> Option<char> {
        let r = self.next();
        self.backup();
        r
    }

    /// Returns the next two runes without advancing the lexer.
    pub fn peek_two(&mut self) -> [Option<char>; 2] {
        let r1 = self.next();
        if r1 == EOF {
            self.backup();
            return [r1, EOF];
        }
        let r2 = self.next();
        self.backup();
        self.backup();
        [r1, r2]
    }

    fn move_start_to_pos(&mut self) {
        // Check if we are about to move Start to a new line.
        // Note: CRLF advances the line twice, exactly as in Go.
        let mut offset = self.start;
        while offset < self.pos {
            let r = self.input[offset..]
                .chars()
                .next()
                .expect("offset stays on char boundaries");
            offset += r.len_utf8();
            if is_end_of_line_char(r) {
                self.line += 1;
                self.column = 0;
            } else {
                self.column += i32::try_from(r.len_utf8()).expect("UTF-8 width fits i32");
            }
        }
        self.start = self.pos;
    }

    /// Skips the current token (comments and other syntax-free text).
    pub fn ignore(&mut self) {
        self.move_start_to_pos();
    }

    /// Accepts runes while `check` holds; returns the last rune and whether
    /// any rune was accepted.
    pub fn accept_run(&mut self, check: CheckRune) -> (Option<char>, bool) {
        let mut last = None;
        let mut valid = false;
        loop {
            match self.next() {
                Some(r) if check(r) => {
                    valid = true;
                    last = Some(r);
                }
                _ => break,
            }
        }
        self.backup();
        (last, valid)
    }

    /// Accepts runes while `check` holds (recursive variant).
    pub fn accept_run_rec(&mut self, check: CheckRuneRec) {
        loop {
            match self.next() {
                Some(r) if check(r, self) => {}
                _ => break,
            }
        }
        self.backup();
    }

    /// Accepts runes until `check` holds.
    pub fn accept_until(&mut self, check: CheckRune) {
        loop {
            match self.next() {
                Some(r) if !check(r) => {}
                _ => break,
            }
        }
        self.backup();
    }

    /// Accepts runes with `check` up to `times`; returns the accepted count.
    ///
    /// Note: like Go, this backs up exactly once (the first rejected rune or
    /// the EOF lookahead), not once per accepted rune.
    pub fn accept_run_times(&mut self, check: CheckRune, times: usize) -> usize {
        let mut accepted = 0;
        while accepted < times {
            match self.next() {
                Some(r) if check(r) => accepted += 1,
                _ => break,
            }
        }
        self.backup();
        accepted
    }

    /// Ignores all the runes accepted by `check`.
    pub fn ignore_run(&mut self, check: CheckRune) {
        self.accept_run(check);
        self.ignore();
    }

    /// Whether the rune is an escape character (`ECHAR ::= '\' [uvtbnrf"'\\]`).
    #[must_use]
    #[allow(clippy::unused_self, reason = "Go parity: lex.Lexer.IsEscChar method")]
    pub fn is_esc_char(&self, r: Option<char>) -> bool {
        matches!(
            r,
            Some('u' | 'v' | 't' | 'b' | 'n' | 'r' | 'f' | '"' | '\'' | '\\')
        )
    }

    /// Consumes a quoted string, honouring escape characters.
    ///
    /// The caller must have just read the opening quote (it is backed up and
    /// re-read here, as in Go).
    ///
    /// # Errors
    ///
    /// Errors when the quote is missing, the input ends mid-string, or an
    /// escape is invalid.
    pub fn lex_quoted_string(&mut self) -> Result<(), LexError> {
        self.backup();
        if self.next() != Some('"') {
            return Err(LexError::from("String should start with quote."));
        }
        loop {
            match self.next() {
                None => return Err(LexError::from("Unexpected end of input.")),
                Some('\\') => match self.next() {
                    Some(r) if self.is_esc_char(Some(r)) => {}
                    Some(r) => {
                        return Err(LexError::from(format!("Not a valid escape char: '{r}'")));
                    }
                    // Go formats %c of EOF here (garbage); report the EOF.
                    None => return Err(LexError::from("Unexpected end of input.")),
                },
                Some('"') => break,
                Some(_) => {}
            }
        }
        Ok(())
    }

    /// Returns an iterator over the emitted items.
    #[must_use]
    pub fn new_iterator(&self) -> ItemIterator<'_> {
        ItemIterator {
            lexer: self,
            idx: -1,
        }
    }
}

/// Whether the rune ends a line (line feed or carriage return).
#[must_use]
pub fn is_end_of_line(r: Option<char>) -> bool {
    r.is_some_and(is_end_of_line_char)
}

fn is_end_of_line_char(r: char) -> bool {
    r == '\u{000A}' || r == '\u{000D}'
}

/// Iterates over the items emitted by a [`Lexer`].
#[derive(Debug)]
pub struct ItemIterator<'a> {
    lexer: &'a Lexer,
    idx: isize,
}

impl ItemIterator<'_> {
    fn len(&self) -> isize {
        isize::try_from(self.lexer.items.len()).expect("item count fits in isize")
    }

    /// Converts a bounds-checked position to a slice index.
    fn at(idx: isize) -> usize {
        usize::try_from(idx).expect("index bounds-checked by caller")
    }

    /// Error message using the location of the next item.
    pub fn errorf(&self, msg: impl fmt::Display) -> LexError {
        let (next_item, _) = self.peek_one();
        next_item.errorf(msg)
    }

    /// Advances the iterator by one; `false` when exhausted.
    pub fn advance(&mut self) -> bool {
        self.idx += 1;
        self.idx < self.len()
    }

    /// Returns the current item (out-of-range placeholder when exhausted).
    #[must_use]
    pub fn item(&self) -> Item {
        if self.idx < 0 || self.idx >= self.len() {
            return Item::out_of_range();
        }
        self.lexer.items[Self::at(self.idx)].clone()
    }

    /// Moves the index back by one; `false` when already at the start.
    pub fn prev(&mut self) -> bool {
        if self.idx > 0 {
            self.idx -= 1;
            return true;
        }
        false
    }

    /// Restores the iterator to a position from [`ItemIterator::save`].
    ///
    /// # Panics
    ///
    /// Panics unless `-1 <= pos <= len` (Go asserts the same).
    pub fn restore(&mut self, pos: isize) {
        assert!(
            pos <= self.len() && pos >= -1,
            "restore position out of range: {pos}"
        );
        self.idx = pos;
    }

    /// Returns the current position for later [`ItemIterator::restore`].
    #[must_use]
    pub fn save(&self) -> isize {
        self.idx
    }

    /// Returns the next `num` items without consuming them.
    ///
    /// # Errors
    ///
    /// Errors with "Out of range for peek" past the item end.
    pub fn peek(&self, num: usize) -> Result<&[Item], LexError> {
        let num = isize::try_from(num).map_err(|_| LexError::from("Out of range for peek"))?;
        if self.idx + num + 1 > self.len() {
            return Err(LexError::from("Out of range for peek"));
        }
        let lo = Self::at(self.idx + 1);
        let hi = Self::at(self.idx + num + 1);
        Ok(&self.lexer.items[lo..hi])
    }

    /// Returns the next item without consuming it.
    #[must_use]
    pub fn peek_one(&self) -> (Item, bool) {
        if self.idx + 1 >= self.len() {
            return (Item::out_of_range(), false);
        }
        (self.lexer.items[Self::at(self.idx + 1)].clone(), true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ITEM_TEXT: ItemType = 5;

    fn lex_two_text(l: &mut Lexer) -> Option<StateFn> {
        l.accept_run(|r| r != ' ');
        l.emit(ITEM_TEXT);
        l.ignore_run(|r| r == ' ');
        if l.peek() == EOF {
            l.emit(ITEM_EOF);
            return None;
        }
        Some(StateFn(lex_two_text))
    }

    #[test]
    fn run_emits_items_and_eof() {
        let mut lexer = Lexer::with_input("hello world");
        lexer.run(StateFn(lex_two_text));
        assert!(lexer.validate_result().is_ok());
        let mut it = lexer.new_iterator();
        assert!(it.advance());
        assert_eq!(it.item().val, "hello");
        assert_eq!((it.item().line(), it.item().column()), (1, 0));
        assert!(it.advance());
        assert_eq!(it.item().val, "world");
        assert_eq!((it.item().line(), it.item().column()), (1, 6));
        assert!(it.advance());
        assert_eq!(it.item().typ, ITEM_EOF);
        assert!(!it.advance());
    }

    #[test]
    fn backup_tracks_mixed_widths() {
        // 1-byte, 1-byte, 3-byte: backups must unwind 3, 1, 1.
        let mut lexer = Lexer::with_input("ab\u{20AC}");
        assert_eq!(lexer.next(), Some('a'));
        assert_eq!(lexer.next(), Some('b'));
        assert_eq!(lexer.next(), Some('\u{20AC}'));
        assert_eq!(lexer.pos, 5);
        lexer.backup();
        assert_eq!(lexer.pos, 2);
        lexer.backup();
        assert_eq!(lexer.pos, 1);
        lexer.backup();
        assert_eq!(lexer.pos, 0);
        assert_eq!(lexer.next(), Some('a'));
    }

    #[test]
    fn eof_next_and_backup_roundtrip() {
        let mut lexer = Lexer::with_input("x");
        assert_eq!(lexer.next(), Some('x'));
        assert_eq!(lexer.next(), EOF);
        lexer.backup(); // pops the zero-width EOF entry
        assert_eq!(lexer.pos, 1);
        assert_eq!(lexer.next(), EOF);
    }

    #[test]
    fn peek_does_not_advance() {
        let mut lexer = Lexer::with_input("xy");
        assert_eq!(lexer.peek(), Some('x'));
        assert_eq!(lexer.peek_two(), [Some('x'), Some('y')]);
        assert_eq!(lexer.pos, 0);
        assert_eq!(lexer.next(), Some('x'));
    }

    #[test]
    fn peek_two_at_eof() {
        let mut lexer = Lexer::with_input("x");
        lexer.next();
        assert_eq!(lexer.peek_two(), [EOF, EOF]);
        assert_eq!(lexer.pos, 1);
    }

    #[test]
    fn line_and_column_track_newlines() {
        let mut lexer = Lexer::with_input("ab\ncde\nf");
        lexer.accept_run(|r| r != '\n');
        lexer.emit(ITEM_TEXT);
        lexer.ignore_run(|r| r == '\n');
        lexer.accept_run(|r| r != '\n');
        lexer.emit(ITEM_TEXT);
        let mut it = lexer.new_iterator();
        it.advance();
        assert_eq!((it.item().line(), it.item().column()), (1, 0));
        it.advance();
        assert_eq!((it.item().line(), it.item().column()), (2, 0));
    }

    #[test]
    fn accept_run_times_backs_up_once() {
        let mut lexer = Lexer::with_input("ab12");
        let is_alpha = |r: char| r.is_ascii_alphabetic();
        assert_eq!(lexer.accept_run_times(is_alpha, 4), 2);
        // Consumed "ab" plus the '1' lookahead, backed up once.
        assert_eq!(lexer.pos, 2);
        assert_eq!(lexer.next(), Some('1'));
    }

    #[test]
    fn errorf_stops_run_and_fails_validation() {
        fn fail(l: &mut Lexer) -> Option<StateFn> {
            l.errorf("boom")
        }
        let mut lexer = Lexer::with_input("input");
        lexer.run(StateFn(fail));
        let err = lexer.validate_result().unwrap_err();
        assert_eq!(
            err.to_string(),
            "while lexing input at line 1 column 0: boom"
        );
    }

    #[test]
    fn reset_reuses_and_relines() {
        let mut lexer = Lexer::with_input("a\nb");
        lexer.run(StateFn(lex_two_text));
        lexer.reset("z");
        assert_eq!(lexer.line, 1);
        assert_eq!(lexer.column, 0);
        assert_eq!(lexer.pos, 0);
        lexer.run(StateFn(lex_two_text));
        let mut it = lexer.new_iterator();
        it.advance();
        assert_eq!(it.item().val, "z");
    }

    #[test]
    fn iterator_save_restore_peek() {
        let mut lexer = Lexer::with_input("a b c");
        lexer.run(StateFn(lex_two_text));
        let mut it = lexer.new_iterator();
        assert!(it.advance());
        let saved = it.save();
        assert!(it.advance());
        assert_eq!(it.item().val, "b");
        assert!(it.prev());
        assert_eq!(it.item().val, "a");
        it.restore(saved);
        assert_eq!(it.item().val, "a");
        let (next, ok) = it.peek_one();
        assert!(ok);
        assert_eq!(next.val, "b");
        let pair = it.peek(2).unwrap();
        assert_eq!(pair.len(), 2);
        assert_eq!(pair[1].val, "c");
        assert!(it.peek(99).is_err());
    }

    #[test]
    fn iterator_out_of_range_item() {
        let lexer = Lexer::with_input("");
        let mut it = lexer.new_iterator();
        assert!(!it.advance());
        let item = it.item();
        assert_eq!((item.line(), item.column()), (-1, -1));
        let (_, ok) = it.peek_one();
        assert!(!ok);
    }

    #[test]
    fn item_display() {
        let mut lexer = Lexer::with_input("hi");
        lexer.run(StateFn(lex_two_text));
        let mut it = lexer.new_iterator();
        it.advance();
        assert_eq!(it.item().to_string(), "lex.Item [5] \"hi\" at 1:0");
        it.advance();
        assert_eq!(it.item().to_string(), "EOF");
    }

    #[test]
    fn quoted_string_with_escapes() {
        let mut lexer = Lexer::with_input(r#""a\nb\"""#);
        lexer.next(); // opening quote, as callers do
        assert!(lexer.lex_quoted_string().is_ok());
        assert_eq!(lexer.pos, lexer.input.len());
    }

    #[test]
    fn quoted_string_errors() {
        let mut lexer = Lexer::with_input("noquote");
        lexer.next();
        assert!(lexer.lex_quoted_string().is_err());
        let mut unterminated = Lexer::with_input("\"abc");
        unterminated.next();
        assert_eq!(
            unterminated.lex_quoted_string().unwrap_err().to_string(),
            "Unexpected end of input."
        );
        let mut bad_escape = Lexer::with_input(r#""\q""#);
        bad_escape.next();
        assert_eq!(
            bad_escape.lex_quoted_string().unwrap_err().to_string(),
            "Not a valid escape char: 'q'"
        );
    }
}
