# datastreamdb-lex

Low-level lexing primitives (IRIs, RDF atoms).

- Go source: `dgraph-main/lex` (reference clone at repo root; read-only).
- Status: ported (Milestone 2). See [TODO.md](TODO.md) and [docs/PROJECT_MAP.md](../../docs/PROJECT_MAP.md).
- Embedding: this crate must stay usable from `datastreamdb-core` inside one process with many isolated instances.

## Layout

- `src/` - implementation (see [src/README.md](src/README.md)).

## API map (Go -> Rust)

| Go (`lex`) | Rust (`datastreamdb_lex`) |
|---|---|
| `Lexer`, `Item`, `ItemIterator` | same names |
| `ItemType`, `ItemEOF`, `ItemError` | `ItemType`, `ITEM_EOF`, `ITEM_ERROR` |
| `EOF` (`rune(-1)`) | `EOF: Option<char> = None`; `next`/`peek` return `Option<char>` |
| `StateFn func(*Lexer) StateFn` | `StateFn(fn(&mut Lexer) -> Option<StateFn>)` newtype |
| `CheckRune`, `CheckRuneRec` | `fn(char) -> bool`, `fn(char, &mut Lexer) -> bool` |
| `IRIRef`, `HasUChars`, `HasXChars` | `iri_ref`, `has_uchars`, `has_xchars` |
| `ItemIterator.Next` | `advance` (avoids `Iterator::next` clash) |
| `IsEndOfLine(rune)` | `is_end_of_line(Option<char>)` |

## Recorded deltas

- Positions are `usize` byte offsets; line/column are `i32` (`-1` marks
  synthetic out-of-range items, as in Go).
- CRLF advances the line twice, exactly as the Go `moveStartToPos`.
- `Lexer::width` is kept for shape parity but never written, as in Go.
- Rust `&str` is guaranteed UTF-8, so there is no `RuneError` path.
- `lex_quoted_string` reports a trailing backslash at EOF as "Unexpected end
  of input." (Go formats `%c` of `EOF`, which is unspecified garbage).
