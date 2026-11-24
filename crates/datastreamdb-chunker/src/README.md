# datastreamdb-chunker/src

Implementation of `datastreamdb-chunker`. Public API lives in `lib.rs`; one module
per Go source file, named after that file.

- `lib.rs` - re-exports, `ChunkError`, `split_lang`, `is_go_space`.
- `model.rs` - `NQuad` (mirrors `api.NQuad`).
- `rdf_state.rs` - RDF lexer states (port of `rdf_state.go`).
- `rdf_parser.rs` - line parser (port of `rdf_parser.go`).
- `json_parser.rs` - `NQuadBuffer`, JSON parser (port of `json_parser.go`).
- `chunk.rs` - `Chunker`, `Cursor`, formats (port of `chunk.go`).
