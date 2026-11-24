//! Port of Go's `chunk_test.go` (stream chunking + format detection).
//!
//! `TestFileReader` has no port yet: `FileReader` (gzip + encryption) waits
//! for the loader milestones (see `TODO.md`).

use datastreamdb_chunker::{Chunker, Cursor, InputFormat, data_format, is_json_data, new_chunker};

fn chunk_str(chunker: &mut Chunker, input: &str) -> (Vec<String>, bool) {
    let mut cursor = Cursor::new(input.as_bytes());
    let mut chunks = Vec::new();
    loop {
        let chunk = chunker.chunk(&mut cursor).expect("chunk");
        chunks.push(String::from_utf8(chunk.data).expect("utf8"));
        if chunk.eof {
            return (chunks, true);
        }
    }
}

#[test]
fn json_load_start_errors() {
    // Port of TestJSONLoadStart.
    let cases = [
        ("[,]", "Illegal rune found \",\", expecting {"),
        ("[a]", "Illegal rune found \"a\", expecting {"),
        ("{}]", "JSON map is followed by an extraneous ]"),
        ("These are words.", "file is not JSON"),
        (
            "\u{1f}\u{8b}\u{08}\u{08}>\u{c7}\n\\\u{00}\u{03}empty\u{00}",
            "file is binary",
        ),
    ];
    for (json, desc) in cases {
        let mut chunker = new_chunker(InputFormat::Json, 1000);
        let mut cursor = Cursor::new(json.as_bytes());
        assert!(chunker.chunk(&mut cursor).is_err(), "{desc}");
    }
}

#[test]
fn chunk_json_map_and_array() {
    // Port of TestChunkJSONMapAndArray.
    let cases = [
        ("[]", vec!["[]"]),
        ("[{}]", vec!["[{}]"]),
        ("[{\"user\": \"alice\"}]", vec!["[{\"user\":\"alice\"}]"]),
        (
            "[{\"user\": \"alice\", \"age\": 26}]",
            vec!["[{\"user\":\"alice\",\"age\":26}]"],
        ),
        (
            "[{\"user\": \"alice\", \"age\": 26}, {\"name\": \"bob\"}]",
            vec!["[{\"user\":\"alice\",\"age\":26},{\"name\":\"bob\"}]"],
        ),
    ];
    for (json, expected) in cases {
        let mut chunker = new_chunker(InputFormat::Json, 1000);
        let (chunks, eof) = chunk_str(&mut chunker, json);
        assert!(eof, "stream ends: {json}");
        assert_eq!(chunks, expected, "chunks for {json}");
    }
}

#[test]
fn json_load_read_next_errors() {
    // Port of TestJSONLoadReadNext.
    let cases = [
        ("[,]", "no start of JSON map 1"),
        (
            "[ this is not really a json array ]",
            "no start of JSON map 2",
        ),
        ("[{]", "malformed map"),
        ("[{}", "malformed array"),
    ];
    for (json, desc) in cases {
        let mut chunker = new_chunker(InputFormat::Json, 1000);
        let mut cursor = Cursor::new(json.as_bytes());
        if let Ok(chunk) = chunker.chunk(&mut cursor) {
            assert!(
                chunker.parse(&chunk.data).is_err(),
                "parse must fail: {desc}"
            );
        }
    }
}

#[test]
fn json_load_success_first() {
    // Port of TestJSONLoadSuccessFirst.
    let cases = [
        ("[{}]", "[{}]", "empty map"),
        (
            "[{\"closingDelimeter\":\"}\"}]",
            "[{\"closingDelimeter\":\"}\"}]",
            "quoted closing brace",
        ),
        (
            "[{\"company\":\"datastreamdb\"}]",
            "[{\"company\":\"datastreamdb\"}]",
            "simple, compact map",
        ),
        (
            "[\n  {\n    \"company\" : \"datastreamdb\"\n  }\n]\n",
            "[{\"company\":\"datastreamdb\"}]",
            "simple, pretty map",
        ),
        (
            "[{\"professor\":\"Alastor \\\"Mad-Eye\\\" Moody\"}]",
            "[{\"professor\":\"Alastor \\\"Mad-Eye\\\" Moody\"}]",
            "escaped balanced quotes",
        ),
        (
            "[{\"something{\": \"}something\"}]",
            "[{\"something{\":\"}something\"}]",
            "escape quoted brackets",
        ),
        (
            "[{\"height\":\"6'0\\\"\"}]",
            "[{\"height\":\"6'0\\\"\"}]",
            "escaped unbalanced quote",
        ),
        (
            "[{\"house\":{\"Hermione\":\"Gryffindor\",\"Cedric\":\"Hufflepuff\",\"Luna\":\"Ravenclaw\",\"Draco\":\"Slytherin\",}}]",
            "[{\"house\":{\"Hermione\":\"Gryffindor\",\"Cedric\":\"Hufflepuff\",\"Luna\":\"Ravenclaw\",\"Draco\":\"Slytherin\",}}]",
            "nested braces",
        ),
    ];
    for (json, expected, desc) in cases {
        let mut chunker = new_chunker(InputFormat::Json, 1000);
        let mut cursor = Cursor::new(json.as_bytes());
        let chunk = chunker.chunk(&mut cursor).expect(desc);
        assert_eq!(
            String::from_utf8(chunk.data).expect("utf8"),
            expected,
            "{desc}"
        );
    }
}

#[test]
fn json_load_success_all_single_chunk() {
    // Port of TestJSONLoadSuccessAll. (Go's per-chunk expectations are dead:
    // the whole document fits one chunk, so the loop only asserts EOF.)
    let doc =
        "[\n{}\n,\n{\"closingDelimeter\" : \"}\"},\n{\"company\" : \"datastreamdb\",\n\"age\": 3}]";
    let mut chunker = new_chunker(InputFormat::Json, 1000);
    let (chunks, eof) = chunk_str(&mut chunker, doc);
    assert!(eof);
    assert_eq!(chunks.len(), 1);
    assert_eq!(
        chunks[0],
        "[{},{\"closingDelimeter\":\"}\"},{\"company\":\"datastreamdb\",\"age\":3}]"
    );
}

#[test]
fn data_format_table() {
    // Port of TestDataFormat (pure function; no files needed).
    let cases = [
        ("test-1.json", InputFormat::Json),
        ("test-2.rdf", InputFormat::Rdf),
        ("test-3.txt", InputFormat::Unknown),
        ("test-4.json.gz", InputFormat::Json),
        ("test-5.rdf.gz", InputFormat::Rdf),
    ];
    for (name, expected) in cases {
        assert_eq!(data_format(name, ""), expected, "{name}");
    }
    assert_eq!(data_format("x.txt", "rdf"), InputFormat::Rdf);
    assert_eq!(data_format("x.txt", "json"), InputFormat::Json);
    assert_eq!(data_format("x.rdf", "json"), InputFormat::Rdf);
}

#[test]
fn rdf_chunker_roundtrips_fixture() {
    // Port of TestRDFChunkerChunk (direct bytes; FileReader waits for M5).
    let data = include_bytes!("../testdata/data.rdf");
    let mut chunker = new_chunker(InputFormat::Rdf, 1000);
    let mut cursor = Cursor::new(data);
    let mut out = Vec::new();
    loop {
        let chunk = chunker.chunk(&mut cursor).expect("chunk");
        out.extend_from_slice(&chunk.data);
        if chunk.eof {
            break;
        }
    }
    assert_eq!(out, data);
}

#[test]
fn rdf_chunker_parses_fixture() {
    let data = include_bytes!("../testdata/data.rdf");
    let mut chunker = new_chunker(InputFormat::Rdf, 1000);
    let mut cursor = Cursor::new(data);
    loop {
        let chunk = chunker.chunk(&mut cursor).expect("chunk");
        let done = chunk.eof;
        chunker.parse(&chunk.data).expect("parse");
        if done {
            break;
        }
    }
    // 96 email edges (last line lacks a trailing newline), all parsed.
    assert_eq!(chunker.nquads().pending().len(), 96);
}

#[test]
fn json_sniffing() {
    // `IsJSONData` has no Go unit test; these pin the port.
    assert!(is_json_data(b"{\"a\": 1}"));
    assert!(is_json_data(b"  [1, 2]"));
    assert!(is_json_data(b"\"str\""));
    assert!(is_json_data(b"123"));
    assert!(is_json_data(b"true"));
    assert!(is_json_data(b"null"));
    assert!(is_json_data(b"{\"a\":"));
    assert!(!is_json_data(b"These are words."));
    assert!(!is_json_data(b""));
    assert!(!is_json_data(b"   "));
    assert!(!is_json_data(b"\x1f\x8b\x08"));
    assert!(!is_json_data(b"\"unterminated"));
    assert!(!is_json_data(b"nul"));
    assert!(!is_json_data(b"01"));
}

#[test]
fn unknown_format_panics() {
    let result = std::panic::catch_unwind(|| new_chunker(InputFormat::Unknown, 1));
    assert!(result.is_err());
}
