//! Stream chunking for the bulk/live loaders. Port of `chunk.go`.
//!
//! [`Chunker`] splits an input [`Cursor`] into [`Chunk`]s (line batches for
//! RDF, top-level maps for JSON) and parses them into an [`NQuadBuffer`].
//!
//! `FileReader`/`StreamReader` (gzip + encryption) are loader-milestone work
//! (see `TODO.md`); chunking here works over in-memory bytes.

#![warn(missing_docs)]

use super::json_parser::{NQuadBuffer, Op};
use super::rdf_parser::parse_rdf;
use crate::ChunkError;
use datastreamdb_lex::Lexer;

/// Input format for [`new_chunker`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputFormat {
    /// Unsupported format.
    Unknown = 0,
    /// RDF N-Quads.
    Rdf = 1,
    /// JSON mutations.
    Json = 2,
}

/// One chunk of input plus whether the stream ended with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chunk {
    /// Chunk bytes (RDF lines, or a `[`-wrapped JSON map list).
    pub data: Vec<u8>,
    /// Whether the stream ended with this chunk (Go's `io.EOF` signal).
    pub eof: bool,
}

/// Byte cursor with one-rune pushback (covers `bufio.Reader`'s rune API).
///
/// Invalid UTF-8 decodes as `U+FFFD` consuming one byte, like Go's `ReadRune`.
#[derive(Clone, Debug)]
pub struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
    pushed: Option<char>,
}

impl<'a> Cursor<'a> {
    /// Cursor over the given bytes.
    #[must_use]
    pub fn new(buf: &'a [u8]) -> Self {
        Self {
            buf,
            pos: 0,
            pushed: None,
        }
    }

    /// Remaining bytes.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    /// Reads the next rune (`None` at end of input).
    pub fn read_rune(&mut self) -> Option<char> {
        if let Some(c) = self.pushed.take() {
            return Some(c);
        }
        let rest = self.buf.get(self.pos..)?;
        if rest.is_empty() {
            return None;
        }
        let (c, width) = decode_at(rest);
        self.pos += width;
        Some(c)
    }

    /// Pushes one rune back (infallible: chunking never double-unreads).
    pub fn unread_rune(&mut self, c: char) {
        self.pushed = Some(c);
    }

    /// Reads through the next `\n` (inclusive); `None` only when exhausted.
    /// Returns the line and whether a newline terminated it.
    pub fn read_line(&mut self) -> Option<(&'a [u8], bool)> {
        // Line and rune reads never mix on one cursor (the RDF chunker only
        // reads lines), so no pushback can be pending here.
        debug_assert!(self.pushed.is_none(), "mixed rune/line reads");
        if self.pos >= self.buf.len() {
            return None;
        }
        let start = self.pos;
        if let Some(rel) = self.buf[start..].iter().position(|b| *b == b'\n') {
            self.pos = start + rel + 1;
            Some((&self.buf[start..self.pos], true))
        } else {
            self.pos = self.buf.len();
            Some((&self.buf[start..], false))
        }
    }
}

/// Decodes one rune at the slice start (invalid input yields `U+FFFD`, width 1).
fn decode_at(buf: &[u8]) -> (char, usize) {
    let b0 = buf[0];
    let width = if b0 < 0x80 {
        1
    } else if b0 >> 5 == 0b110 {
        2
    } else if b0 >> 4 == 0b1110 {
        3
    } else if b0 >> 3 == 0b11110 {
        4
    } else {
        return ('\u{FFFD}', 1);
    };
    if buf.len() < width {
        return ('\u{FFFD}', 1);
    }
    match std::str::from_utf8(&buf[..width]) {
        Ok(s) => (s.chars().next().unwrap_or('\u{FFFD}'), width),
        Err(_) => ('\u{FFFD}', 1),
    }
}

/// RDF/JSON chunker. Port of Go's `Chunker` interface plus implementations.
pub enum Chunker {
    /// RDF line chunker.
    Rdf(RdfChunker),
    /// JSON map chunker.
    Json(JsonChunker),
}

/// Returns a chunker for the format.
///
/// # Panics
///
/// Panics on [`InputFormat::Unknown`], like Go's `x.Panic`.
#[must_use]
pub fn new_chunker(format: InputFormat, batch_size: i64) -> Chunker {
    match format {
        InputFormat::Rdf => Chunker::Rdf(RdfChunker {
            lexer: Lexer::default(),
            nqs: NQuadBuffer::new(batch_size),
        }),
        InputFormat::Json => Chunker::Json(JsonChunker {
            nqs: NQuadBuffer::new(batch_size),
            in_list: false,
        }),
        InputFormat::Unknown => panic!("unknown input format"),
    }
}

impl Chunker {
    /// Reads the next [`Chunk`] from the cursor.
    ///
    /// # Errors
    ///
    /// Returns [`ChunkError`] on malformed input.
    pub fn chunk(&mut self, cursor: &mut Cursor) -> Result<Chunk, ChunkError> {
        match self {
            Chunker::Rdf(_) => Ok(RdfChunker::chunk(cursor)),
            Chunker::Json(c) => c.chunk(cursor),
        }
    }

    /// Parses a chunk into the N-Quad buffer.
    ///
    /// # Errors
    ///
    /// Returns [`ChunkError`] on malformed quads.
    pub fn parse(&mut self, data: &[u8]) -> Result<(), ChunkError> {
        match self {
            Chunker::Rdf(c) => c.parse(data),
            Chunker::Json(c) => c.parse(data),
        }
    }

    /// The N-Quad buffer parsed so far.
    #[must_use]
    pub fn nquads(&self) -> &NQuadBuffer {
        match self {
            Chunker::Rdf(c) => &c.nqs,
            Chunker::Json(c) => &c.nqs,
        }
    }

    /// The N-Quad buffer parsed so far (mutable).
    pub fn nquads_mut(&mut self) -> &mut NQuadBuffer {
        match self {
            Chunker::Rdf(c) => &mut c.nqs,
            Chunker::Json(c) => &mut c.nqs,
        }
    }
}

/// RDF line chunker.
pub struct RdfChunker {
    lexer: Lexer,
    nqs: NQuadBuffer,
}

/// Lines per RDF chunk.
const RDF_CHUNK_LINES: usize = 100_000;

impl RdfChunker {
    /// Reads up to 100k lines or EOF.
    fn chunk(cursor: &mut Cursor) -> Chunk {
        let mut batch = Vec::with_capacity(1 << 20);
        for _ in 0..RDF_CHUNK_LINES {
            match cursor.read_line() {
                None => {
                    return Chunk {
                        data: batch,
                        eof: true,
                    };
                }
                Some((line, _)) => batch.extend_from_slice(line),
            }
        }
        Chunk {
            data: batch,
            eof: false,
        }
    }

    /// Parses a chunk line by line. Not thread-safe (reuses the lexer).
    fn parse(&mut self, data: &[u8]) -> Result<(), ChunkError> {
        if data.is_empty() {
            return Ok(());
        }
        for piece in data.split_inclusive(|b| *b == b'\n') {
            let text = String::from_utf8_lossy(piece);
            match parse_rdf(&text, &mut self.lexer) {
                Err(ChunkError::Empty) => {} // blank line or comment
                Err(err) => {
                    return Err(ChunkError::message(format!(
                        "while parsing line {text:?}: {err}"
                    )));
                }
                Ok(nq) => self.nqs.push(nq),
            }
        }
        Ok(())
    }
}

/// JSON map chunker.
pub struct JsonChunker {
    nqs: NQuadBuffer,
    in_list: bool,
}

/// JSON chunk size threshold (bytes).
const JSON_CHUNK_BYTES: usize = 100_000;

impl JsonChunker {
    /// Consumes top-level maps until the size threshold or EOF.
    fn chunk(&mut self, cursor: &mut Cursor) -> Result<Chunk, ChunkError> {
        let Some(ch) = next_rune(cursor) else {
            return Ok(Chunk {
                data: Vec::new(),
                eof: true,
            });
        };
        // If the file starts with '[', set inList and keep consuming maps
        // until the threshold.
        if ch == '[' {
            self.in_list = true;
        } else if ch == '{' {
            // Put the rune back for consumeMap.
            cursor.unread_rune(ch);
        } else {
            return Err(ChunkError::message("file is not JSON"));
        }

        let mut out = Vec::new();
        out.push(b'[');
        let mut has_maps_before = false;
        while out.len() < JSON_CHUNK_BYTES {
            if has_maps_before {
                out.push(b',');
            }
            match consume_map(cursor, &mut out)? {
                MapOutcome::EofInString => {
                    // Go drops the partial buffer and surfaces raw io.EOF.
                    return Ok(Chunk {
                        data: Vec::new(),
                        eof: true,
                    });
                }
                MapOutcome::Done => {}
            }
            has_maps_before = true;

            // Handle the legal terminations via the rune after the map.
            match next_rune(cursor) {
                None => {
                    if self.in_list {
                        return Err(ChunkError::message("JSON file ends abruptly, expecting ]"));
                    }
                    out.push(b']');
                    return Ok(Chunk {
                        data: out,
                        eof: true,
                    });
                }
                Some(']') => {
                    if !self.in_list {
                        return Err(ChunkError::message(
                            "JSON map is followed by an extraneous ]",
                        ));
                    }
                    // No non-space chars may follow the ']'.
                    if !slurp_space(cursor) {
                        return Err(ChunkError::message("not all of JSON file consumed"));
                    }
                    out.push(b']');
                    return Ok(Chunk {
                        data: out,
                        eof: true,
                    });
                }
                Some(ch) => {
                    if out.len() == 1 {
                        // 1 is the '[' inserted before the loop.
                        return Err(ChunkError::message(format!(
                            "illegal rune found \"{ch}\", expecting {{"
                        )));
                    }
                    if ch != ',' {
                        return Err(ChunkError::message(format!(
                            "JSON map is followed by illegal rune \"{ch}\""
                        )));
                    }
                }
            }
        }
        out.push(b']');
        Ok(Chunk {
            data: out,
            eof: false,
        })
    }

    /// Parses a chunk (always as a set mutation, like Go).
    fn parse(&mut self, data: &[u8]) -> Result<(), ChunkError> {
        if data.is_empty() {
            return Ok(());
        }
        self.nqs.parse_json(data, Op::Set)
    }
}

/// Next non-space rune (`None` at EOF).
fn next_rune(cursor: &mut Cursor) -> Option<char> {
    slurp_space(cursor);
    cursor.read_rune()
}

/// Skips Go whitespace; returns whether the cursor hit EOF.
fn slurp_space(cursor: &mut Cursor) -> bool {
    loop {
        match cursor.read_rune() {
            None => return true,
            Some(c) if crate::is_go_space(c) => {}
            Some(c) => {
                cursor.unread_rune(c);
                return false;
            }
        }
    }
}

/// Outcome of [`consume_map`].
enum MapOutcome {
    /// A map was consumed (or no `{` started one).
    Done,
    /// EOF truncated a quoted string (Go surfaces raw `io.EOF` there).
    EofInString,
}

/// Consumes one `{...}` map into `out`.
///
/// After ignoring spaces, a non-`{` start consumes nothing.
fn consume_map(cursor: &mut Cursor, out: &mut Vec<u8>) -> Result<MapOutcome, ChunkError> {
    // Just find the matching closing brace. The JSON parser worries about
    // whether everything in between is valid JSON.
    let mut depth = 0;
    loop {
        let Some(ch) = next_rune(cursor) else {
            return Err(ChunkError::message("Malformed JSON"));
        };
        if depth == 0 && ch != '{' {
            // Unread and return without consuming anything.
            cursor.unread_rune(ch);
            return Ok(MapOutcome::Done);
        }
        let mut buf = [0u8; 4];
        out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
        match ch {
            '{' => depth += 1,
            '}' => depth -= 1,
            '"' => {
                let complete = slurp_quoted(cursor, out);
                if !complete {
                    return Ok(MapOutcome::EofInString);
                }
            }
            _ => {}
        }
        if depth <= 0 {
            break;
        }
    }
    Ok(MapOutcome::Done)
}

/// Copies a quoted string verbatim (escapes consume one extra rune).
/// Returns `false` when EOF truncates the string.
fn slurp_quoted(cursor: &mut Cursor, out: &mut Vec<u8>) -> bool {
    loop {
        let Some(ch) = cursor.read_rune() else {
            return false;
        };
        let mut buf = [0u8; 4];
        out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
        if ch == '\\' {
            // Pick one more rune.
            let Some(esc) = cursor.read_rune() else {
                return false;
            };
            out.extend_from_slice(esc.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        if ch == '"' {
            return true;
        }
    }
}

/// Detects the data format from the filename or an explicit option.
/// The file extension has precedence.
#[must_use]
#[allow(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "both sides are lowercased above"
)]
pub fn data_format(filename: &str, format: &str) -> InputFormat {
    let format = format.to_ascii_lowercase();
    let lowered = filename.to_ascii_lowercase();
    let filename = lowered.strip_suffix(".gz").unwrap_or(&lowered);
    if filename.ends_with(".rdf") || format == "rdf" {
        InputFormat::Rdf
    } else if filename.ends_with(".json") || format == "json" {
        InputFormat::Json
    } else {
        InputFormat::Unknown
    }
}

/// Whether the byte prefix starts a JSON stream (first token must scan).
///
/// Inspects up to 512 bytes, like Go's `IsJSONData` peek.
#[must_use]
pub fn is_json_data(data: &[u8]) -> bool {
    let end = data.len().min(512);
    first_token_ok(&data[..end])
}

/// Validates one leading JSON token (delimiter, string, number, literal).
fn first_token_ok(buf: &[u8]) -> bool {
    let mut i = 0;
    while i < buf.len() && matches!(buf[i], b' ' | b'\t' | b'\r' | b'\n') {
        i += 1;
    }
    if i >= buf.len() {
        return false;
    }
    match buf[i] {
        b'{' | b'[' | b'}' | b']' | b',' | b':' => true,
        b'"' => {
            i += 1;
            while i < buf.len() {
                match buf[i] {
                    b'"' => return true,
                    b'\\' => {
                        i += 1;
                        if i >= buf.len() {
                            return false;
                        }
                        match buf[i] {
                            b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => {}
                            b'u' => {
                                if i + 4 >= buf.len()
                                    || !buf[i + 1..i + 5].iter().all(u8::is_ascii_hexdigit)
                                {
                                    return false;
                                }
                                i += 4;
                            }
                            _ => return false,
                        }
                    }
                    0x00..=0x1F => return false,
                    _ => {}
                }
                i += 1;
            }
            false
        }
        b't' => scan_literal(buf, i, "true"),
        b'f' => scan_literal(buf, i, "false"),
        b'n' => scan_literal(buf, i, "null"),
        b'-' | b'0'..=b'9' => scan_number(buf, i),
        _ => false,
    }
}

/// Matches a literal plus a value boundary (or truncated-then-EOF is false).
fn scan_literal(buf: &[u8], start: usize, word: &str) -> bool {
    let bytes = word.as_bytes();
    if buf.len() < start + bytes.len() {
        return false;
    }
    if &buf[start..start + bytes.len()] != bytes {
        return false;
    }
    boundary_or_end(buf, start + bytes.len())
}

/// Matches a JSON number; a truncated-but-valid prefix counts as true.
fn scan_number(buf: &[u8], mut i: usize) -> bool {
    if buf[i] == b'-' {
        i += 1;
        if i >= buf.len() {
            return false;
        }
    }
    if buf[i] == b'0' {
        i += 1;
    } else if buf[i].is_ascii_digit() {
        while i < buf.len() && buf[i].is_ascii_digit() {
            i += 1;
        }
    } else {
        return false;
    }
    if i < buf.len() && buf[i] == b'.' {
        i += 1;
        let frac = i;
        while i < buf.len() && buf[i].is_ascii_digit() {
            i += 1;
        }
        if i == frac {
            return false;
        }
    }
    if i < buf.len() && matches!(buf[i], b'e' | b'E') {
        i += 1;
        if i < buf.len() && matches!(buf[i], b'+' | b'-') {
            i += 1;
        }
        let exp = i;
        while i < buf.len() && buf[i].is_ascii_digit() {
            i += 1;
        }
        if i == exp {
            return false;
        }
    }
    boundary_or_end(buf, i)
}

/// Whether the position ends the buffer or continues with a boundary byte.
fn boundary_or_end(buf: &[u8], i: usize) -> bool {
    i >= buf.len()
        || matches!(
            buf[i],
            b' ' | b'\t' | b'\r' | b'\n' | b',' | b']' | b'}' | b':'
        )
}
