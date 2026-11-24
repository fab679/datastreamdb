# datastreamdb-chunker

Bulk ingest: RDF N-Quads and JSON chunkers.

- Go source: `dgraph-main/chunker` (reference clone at repo root; read-only).
- Status: ported (Milestone 2). See [TODO.md](TODO.md) and [docs/PROJECT_MAP.md](../../docs/PROJECT_MAP.md).
- Embedding: this crate must stay usable from `datastreamdb-core` inside one process with many isolated instances.

## Layout

- `src/` - implementation (see [src/README.md](src/README.md)).
- `tests/` - ported Go test tables (RDF, JSON, chunker).
- `testdata/` - `data.rdf` fixture copied from Go.
- `benches/` - Criterion JSON parse bench.

## API map (Go -> Rust)

| Go (`chunker`) | Rust (`datastreamdb_chunker`) |
|---|---|
| `api.NQuad` | `NQuad` (with `ApiValue` + tok `Facet`) |
| `ParseRDF`, `ParseRDFs` | `parse_rdf`, `parse_rdfs` |
| `NQuadBuffer`, `NewNQuadBuffer` | `NQuadBuffer::new` |
| `SetNquads`, `DeleteNquads` | `Op::Set`, `Op::Delete` |
| `ParseJSON`, `FastParseJSON` | `parse_json` (one parser, reference semantics) |
| `Chunker`, `NewChunker` | `Chunker`, `new_chunker` |
| `bufio.Reader` + `io.EOF` | `Cursor` + `Chunk.eof` |
| `ErrEmpty`, `errEmptyUID` | `ChunkError::Empty`, `ChunkError::EmptyUid` |
| `DataFormat`, `IsJSONData` | `data_format`, `is_json_data` |

## Recorded deltas

- One serde-based JSON parser replaces both Go paths; numbers keep verbatim
  literals (via `RawValue`), reproducing `json.Number` semantics exactly.
- `NQuadBuffer` batches drain via `take_batches`, not a channel.
- JSON objects iterate sorted (deterministic); Go randomizes map order.
- `sane` uses `char::is_alphabetic`/`is_numeric`, not Go's exact tables.
- `FileReader`/`StreamReader` (gzip + encryption) wait for the loaders (M5).
- `NQuad` is a plain mirror; wire `api` prost types land with serving (M4).
