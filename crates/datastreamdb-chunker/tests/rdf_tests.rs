//! Port of Go's `TestLex` (`rdf_parser_test.go`): the full N-Quad table.

use datastreamdb_chunker::{ChunkError, NQuad, parse_rdf, parse_rdfs};
use datastreamdb_lex::Lexer;
use datastreamdb_tok::facets::{Facet, FacetValType};
use datastreamdb_types::ApiValue;

fn edge(subject: &str, predicate: &str, object_id: &str) -> NQuad {
    NQuad {
        subject: subject.into(),
        predicate: predicate.into(),
        object_id: object_id.into(),
        ..NQuad::default()
    }
}

fn lit(subject: &str, predicate: &str, value: ApiValue) -> NQuad {
    NQuad {
        subject: subject.into(),
        predicate: predicate.into(),
        object_value: Some(value),
        ..NQuad::default()
    }
}

fn facet(key: &str, value: Vec<u8>, val_type: FacetValType, tokens: &[&str]) -> Facet {
    Facet {
        key: key.into(),
        value,
        val_type,
        tokens: tokens.iter().map(|t| (*t).to_owned()).collect(),
        alias: String::new(),
    }
}

fn int_facet(key: &str, v: i64) -> Facet {
    facet(key, v.to_le_bytes().to_vec(), FacetValType::Int, &[])
}

fn float_facet(key: &str, v: f64) -> Facet {
    facet(key, v.to_le_bytes().to_vec(), FacetValType::Float, &[])
}

fn bool_facet(key: &str, v: bool) -> Facet {
    facet(key, vec![u8::from(v)], FacetValType::Bool, &[])
}

fn datetime_facet(key: &str, rfc3339: &str) -> Facet {
    // Independent time math (chrono) + the 12-byte layout pinned by
    // datastreamdb-types; Go pins its own 15-byte encoding for the same instants.
    let dt = chrono::DateTime::parse_from_rfc3339(rfc3339).expect("fixture time");
    let bytes = datastreamdb_types::datetime_to_binary(&dt);
    facet(key, bytes, FacetValType::Datetime, &[])
}

enum Expect {
    Quad(NQuad),
    Err,
    Empty,
}

fn case(input: &'static str, nq: NQuad) -> (&'static str, Expect) {
    (input, Expect::Quad(nq))
}

fn err(input: &'static str) -> (&'static str, Expect) {
    (input, Expect::Err)
}

fn empty(input: &'static str) -> (&'static str, Expect) {
    (input, Expect::Empty)
}

#[allow(clippy::too_many_lines, reason = "ported Go test table")]
fn table() -> Vec<(&'static str, Expect)> {
    vec![
        case(
            "<some_subject_id> <predicate> <object_id> .",
            edge("some_subject_id", "predicate", "object_id"),
        ),
        case(
            "<some_subject_id>\t<predicate>\t<object_id>\t.",
            edge("some_subject_id", "predicate", "object_id"),
        ),
        case(
            "_:alice <predicate> <object_id> .",
            edge("_:alice", "predicate", "object_id"),
        ),
        case(
            "<0x01> <predicate> <object_id> .",
            edge("0x01", "predicate", "object_id"),
        ),
        case(
            "<some_subject_id> <predicate> <0x01> .",
            edge("some_subject_id", "predicate", "0x01"),
        ),
        case(
            "<0x01> <predicate> <0x02> .",
            edge("0x01", "predicate", "0x02"),
        ),
        case(
            "_:alice <follows> _:bob0 .",
            edge("_:alice", "follows", "_:bob0"),
        ),
        case(
            "_:alice <name> \"Alice In Wonderland\" .",
            lit(
                "_:alice",
                "name",
                ApiValue::Default("Alice In Wonderland".into()),
            ),
        ),
        case(
            "_:alice <name> \"Alice In Wonderland\"@en-0 .",
            NQuad {
                lang: "en-0".into(),
                ..lit(
                    "_:alice",
                    "name",
                    ApiValue::Default("Alice In Wonderland".into()),
                )
            },
        ),
        case(
            "_:alice <name@en> \"Alice In Wonderland\" .",
            NQuad {
                lang: "en".into(),
                ..lit(
                    "_:alice",
                    "name",
                    ApiValue::Default("Alice In Wonderland".into()),
                )
            },
        ),
        case(
            "_:alice <name@en> * .",
            NQuad {
                lang: "en".into(),
                ..lit("_:alice", "name", ApiValue::Default("_STAR_ALL".into()))
            },
        ),
        case(
            "_:alice <name> \"Alice In Wonderland\"^^<xs:string> .",
            lit(
                "_:alice",
                "name",
                ApiValue::Str("Alice In Wonderland".into()),
            ),
        ),
        case(
            "_:alice <age> \"013\"^^<xs:int> .",
            lit("_:alice", "age", ApiValue::Int(13)),
        ),
        case(
            "_:alice <age> \"013\"^^<xs:integer> .",
            lit("_:alice", "age", ApiValue::Int(13)),
        ),
        case(
            "_:alice <secret> \"password1\"^^<xs:password> .",
            lit("_:alice", "secret", ApiValue::Password("password1".into())),
        ),
        case(
            "<http://www.w3.org/2001/sw/RDFCore/nedges/> <http://purl.org/dc/terms/title> \"N-Edges\"@en-US .",
            NQuad {
                lang: "en-US".into(),
                ..lit(
                    "http://www.w3.org/2001/sw/RDFCore/nedges/",
                    "http://purl.org/dc/terms/title",
                    ApiValue::Default("N-Edges".into()),
                )
            },
        ),
        case(
            "_:art <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://xmlns.com/foaf/0.1/Person> .",
            edge(
                "_:art",
                "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
                "http://xmlns.com/foaf/0.1/Person",
            ),
        ),
        err("_:alice ."),
        err("_:alice knows ."),
        case(
            "<_:alice> <knows> <something> .",
            edge("_:alice", "knows", "something"),
        ),
        case(
            "_:alice <knows> <_:something> .",
            edge("_:alice", "knows", "_:something"),
        ),
        case(
            "<alice> <knows> * .",
            lit("alice", "knows", ApiValue::Default("_STAR_ALL".into())),
        ),
        case(
            "<alice> * * .",
            lit("alice", "_STAR_ALL", ApiValue::Default("_STAR_ALL".into())),
        ),
        err("<alice> <knows> ."),
        err(" 0x01 <knows> <something> ."),
        err("<alice> <knows>  0x01 ."),
        err("_:alice \"knows\" stuff ."),
        err("_:alice <knows> stuff ."),
        err("_:alice <knows> <stuff>"),
        err("\"_:alice\" <knows> <stuff> ."),
        err("_:alice <knows> \"stuff ."),
        err("_:alice <knows> \"stuff\"@-en ."),
        err("_:alice <knows> \"stuff\"^<string> ."),
        err("_:alice <knows> \"stuff\"^^xs:string ."),
        err("_:alice <age> \"thirteen\"^^<xs:int> ."),
        err("<alice> <knows> <*> ."),
        err("<*> <knows> \"stuff\" ."),
        err("<alice> <*> \"stuff\" ."),
        err("<alice> < * > \"stuff\" ."),
        err("<alice> <* *> \"stuff\" ."),
        err("_:alice <knows> \"stuff\"^^< * > ."),
        case(
            "_:alice <knows> \"\" .",
            lit("_:alice", "knows", ApiValue::Default(String::new())),
        ),
        case(
            "_:alice <knows> \"\"^^<xs:string> .",
            lit("_:alice", "knows", ApiValue::Str(String::new())),
        ),
        err("_:alice <knows> \"\"^^<xs:int> ."),
        case(
            "<alice> <knows> \"*\" .",
            lit("alice", "knows", ApiValue::Default("*".into())),
        ),
        case(
            "_:alice <knows> \"stuff\"^^<xs:string> <0xf2> .",
            NQuad {
                namespace: 0xf2,
                ..lit("_:alice", "knows", ApiValue::Str("stuff".into()))
            },
        ),
        case(
            "_:alice <knows> \"stuff\"^^<xs:string> <10> . # comment",
            NQuad {
                namespace: 10,
                ..lit("_:alice", "knows", ApiValue::Str("stuff".into()))
            },
        ),
        err("_:alice <knows> \"stuff\"^^<xs:string> \"0xf2\" ."),
        err("_:alice <knows> \"stuff\"^^<xs:string> 0x01 ."),
        err("_:alice <knows> \"stuff\"^^<xs:string> <quad> <pentagon> ."),
        err("_:alice <knows> \"stuff\"^^<xs:string> quad ."),
        err("_:alice <knows> \"stuff\"^^<xs:string> <*> ."),
        err("_:alice <knows> <bob> . <bob>"),
        case(
            "_:alice <likes> \"mov\\\"enpick\" .",
            lit("_:alice", "likes", ApiValue::Default("mov\"enpick".into())),
        ),
        case(
            "<\\u0021> <\\U123abcdE> <\\u0024> .",
            edge("\\u0021", "\\U123abcdE", "\\u0024"),
        ),
        err("<\\u0021> <\\U123abcdg> <\\u0024> ."),
        err("<messi with space> <friend> <ronaldo> ."),
        err("<with<> <with> <with> ."),
        err("<wi>th> <with> <with> ."),
        err("<\"with> <with> <with> ."),
        err("<{with> <with> <with> ."),
        err("<wi{th> <with> <with> ."),
        err("<with|> <with> <with> ."),
        err("<wit^h> <with> <with> ."),
        err("<w`ith> <with> <with> ."),
        err("<wi\\th> <with> <with> ."),
        err("_:|alice <abc> <abc> ."),
        err("_:al×ice <abc> <abc> ."),
        err("_:gabe <name> \"Gabe' ."),
        err("_:gabe <name> \"Gabe'^^<xs:yo> ."),
        case("_:0 <name> <good> .", edge("_:0", "name", "good")),
        case("_:0a.b <name> <good> .", edge("_:0a.b", "name", "good")),
        err("_:0a. <name> <bad> ."),
        err("<alice> <lives> \"wonder \\a land\" ."),
        case(
            "<alice> <lives> \"\\u0045 wonderland\" .",
            lit("alice", "lives", ApiValue::Default("E wonderland".into())),
        ),
        case(
            "<alice> <lives> \"wonderland\" (friend=\"hatter\").",
            NQuad {
                facets: vec![facet(
                    "friend",
                    b"hatter".to_vec(),
                    FacetValType::String,
                    &["\u{1}hatter"],
                )],
                ..lit("alice", "lives", ApiValue::Default("wonderland".into()))
            },
        ),
        case(
            "<alice> <lives> \"wonderland\" (friend=\"hatter \\u0045\") .",
            NQuad {
                facets: vec![facet(
                    "friend",
                    b"hatter E".to_vec(),
                    FacetValType::String,
                    &["\u{1}e", "\u{1}hatter"],
                )],
                ..lit("alice", "lives", ApiValue::Default("wonderland".into()))
            },
        ),
        err("<alice> <lives> \"\\u004 wonderland\" ."),
        case(
            "<alice> <lives> \"\x02 wonderland\" .",
            lit(
                "alice",
                "lives",
                ApiValue::Default("\x02 wonderland".into()),
            ),
        ),
        err("<alice> <lives> \"\\x2 wonderland\" ."),
        err("<alice> <lives> \"wonderful land\"@a- ."),
        case(
            "<alice> <lives> \"\\v\\t\\b\\n\\r\\f\\\"\\\\\"@a-b .",
            NQuad {
                lang: "a-b".into(),
                ..lit(
                    "alice",
                    "lives",
                    ApiValue::Default("\u{0b}\t\u{08}\n\r\u{0c}\"\\".into()),
                )
            },
        ),
        err("<alice> <lives> \"\\'\" ."),
        err("<alice> <lives> \"\\a\" ."),
        empty("# nothing happened"),
        err("<some_subject_id> # <predicate> <object_id> ."),
        err("<some_subject_id> <predicate> <object_id> # ."),
        err("check me as error"),
        empty("   "),
        case(
            "_:alice <knows> \"stuff\" <0x10> (key1=\"val1\",key2=13) .",
            NQuad {
                namespace: 0x10,
                facets: vec![
                    facet(
                        "key1",
                        b"val1".to_vec(),
                        FacetValType::String,
                        &["\u{1}val1"],
                    ),
                    int_facet("key2", 13),
                ],
                ..lit("_:alice", "knows", ApiValue::Default("stuff".into()))
            },
        ),
        case(
            "_:alice <knows> \"stuff\" <0x12> (key1=,key2=13) .",
            NQuad {
                namespace: 0x12,
                facets: vec![
                    facet("key1", Vec::new(), FacetValType::String, &["\u{1}"]),
                    int_facet("key2", 13),
                ],
                ..lit("_:alice", "knows", ApiValue::Default("stuff".into()))
            },
        ),
        case(
            "_:alice <knows> \"stuff\" (key1=,key2=13) .",
            NQuad {
                facets: vec![
                    facet("key1", Vec::new(), FacetValType::String, &["\u{1}"]),
                    int_facet("key2", 13),
                ],
                ..lit("_:alice", "knows", ApiValue::Default("stuff".into()))
            },
        ),
        case(
            "_:alice <knows> \"stuff\" ( key1 = 12 , key2=\"value2\", key3=, key4 =\"val4\" ) .",
            NQuad {
                facets: vec![
                    int_facet("key1", 12),
                    facet(
                        "key2",
                        b"value2".to_vec(),
                        FacetValType::String,
                        &["\u{1}value2"],
                    ),
                    facet("key3", Vec::new(), FacetValType::String, &["\u{1}"]),
                    facet(
                        "key4",
                        b"val4".to_vec(),
                        FacetValType::String,
                        &["\u{1}val4"],
                    ),
                ],
                ..lit("_:alice", "knows", ApiValue::Default("stuff".into()))
            },
        ),
        case(
            "_:alice <knows> \"stuff\" (key1=12,key2=\"value2\",key3=1.2,key4=2006-01-02T15:04:05,key5=true,key6=false) .",
            NQuad {
                facets: vec![
                    int_facet("key1", 12),
                    facet(
                        "key2",
                        b"value2".to_vec(),
                        FacetValType::String,
                        &["\u{1}value2"],
                    ),
                    float_facet("key3", 1.2),
                    datetime_facet("key4", "2006-01-02T15:04:05Z"),
                    bool_facet("key5", true),
                    bool_facet("key6", false),
                ],
                ..lit("_:alice", "knows", ApiValue::Default("stuff".into()))
            },
        ),
        case(
            "_:alice <knows> \"stuff\" (key1=2002-10-02T15:00:00.05Z, key2=2006-01-02T15:04:05, key3=2006-01-02T00:00:00Z) .",
            NQuad {
                facets: vec![
                    datetime_facet("key1", "2002-10-02T15:00:00.05Z"),
                    datetime_facet("key2", "2006-01-02T15:04:05Z"),
                    datetime_facet("key3", "2006-01-02T00:00:00Z"),
                ],
                ..lit("_:alice", "knows", ApiValue::Default("stuff".into()))
            },
        ),
        case(
            "_:alice <knows> \"stuff\" (k=0x0D) .",
            NQuad {
                facets: vec![int_facet("k", 13)],
                ..lit("_:alice", "knows", ApiValue::Default("stuff".into()))
            },
        ),
        case(
            "_:alice <knows> \"stuff\" (k=111111111111111111888888.23) .",
            NQuad {
                facets: vec![facet(
                    "k",
                    vec![0xA0, 0xA8, 0x4F, 0x6C, 0x58, 0x87, 0xB7, 0x44],
                    FacetValType::Float,
                    &[],
                )],
                ..lit("_:alice", "knows", ApiValue::Default("stuff".into()))
            },
        ),
        case(
            "_:alice <knows> \"stuff\" (key1=\"\\\"hello world\\\"\",key2=\"LineA\\nLineB\") .",
            NQuad {
                facets: vec![
                    facet(
                        "key1",
                        b"\"hello world\"".to_vec(),
                        FacetValType::String,
                        &["\u{1}hello", "\u{1}world"],
                    ),
                    facet(
                        "key2",
                        b"LineA\nLineB".to_vec(),
                        FacetValType::String,
                        &["\u{1}linea", "\u{1}lineb"],
                    ),
                ],
                ..lit("_:alice", "knows", ApiValue::Default("stuff".into()))
            },
        ),
        err("_:alice <knows> \"stuff\" (key1=\"val1\",key2) ."),
        err("_:alice <knows> \"stuff\" (key1=\"val1\",=) ."),
        err("_:alice <knows> \"stuff\" (key1=\"val1\",=\"val1\") ."),
        err("_:alice <knows> \"stuff\" (key1=\"val1\",key1 \"val1\") ."),
        err("_:alice <knows> \"stuff\" (key1=\"val1\",key1= \"val1\" ."),
        err("_:alice <knows> \"stuff\" (key1=\"val1\",key1= ."),
        err("_:alice <knows> \"stuff\" (key1=\"val1\",key1="),
        err("_:alice <knows> \"stuff\" (k==)"),
        err("_:alice <knows> \"stuff\" (k=,) ."),
        err("_:alice <knows> \"stuff\" (k=111111111111111111888888) ."),
        err("_:alice <knows> \"stuff\" (k=0x1787586C4FA8A0284FF8) ."),
        err("<alice> <password> \"guess123\"^^<pwd:password> ."),
        case(
            "* <pred> * .",
            lit("_STAR_ALL", "pred", ApiValue::Default("_STAR_ALL".into())),
        ),
        err("* <pred> \"random\"^^<int> ."),
        err(
            "_:company <name> \"TurfBytes\" . _:company <owner> _:owner . _:owner <name> \"Jason\" .",
        ),
        case(
            "<alice> <lives> \"A\tB\" .",
            lit("alice", "lives", ApiValue::Default("A\tB".into())),
        ),
        err("<alice> <age> \"NaN\"^^<xs:double> ."),
        err("<alice> <age> \"13\"^^<xs:double> (salary=NaN) ."),
        case(
            "uid(v) <lives> \"\x02 wonderland\" .",
            lit(
                "uid(v)",
                "lives",
                ApiValue::Default("\x02 wonderland".into()),
            ),
        ),
        case(
            "uid  (  v  ) <lives> \"vrinadavan\" .",
            lit("uid(v)", "lives", ApiValue::Default("vrinadavan".into())),
        ),
        case(
            "uid  (  val  ) <lives> \"vrinadavan\" .",
            lit("uid(val)", "lives", ApiValue::Default("vrinadavan".into())),
        ),
        case(
            "uid  (  val  ) <lives> uid(g) .",
            edge("uid(val)", "lives", "uid(g)"),
        ),
        case(
            "uid  (  val  ) <lives> uid ( g )  .",
            edge("uid(val)", "lives", "uid(g)"),
        ),
        err("uid  (  val   <lives> uid ( g )  ."),
        err("uid   val )   <lives> uid ( g )  ."),
        err("ui(uid)   <lives> uid ( g )  ."),
        err("uid())   <lives> uid ( g )  ."),
        err("uid()   <lives> uid ( g )  ."),
        err("uid(a)   <lives> uid (  )  ."),
        err("uid(a)   lives> uid (  )  ."),
    ]
}

#[test]
fn lex_table() {
    let mut lexer = Lexer::default();
    for (input, expect) in table() {
        let result = parse_rdf(input, &mut lexer);
        match expect {
            Expect::Empty => {
                assert_eq!(result, Err(ChunkError::Empty), "ignorable input: {input:?}");
            }
            Expect::Err => assert!(
                matches!(result, Err(ChunkError::Message(_))),
                "expected error for {input:?}, got {result:?}"
            ),
            Expect::Quad(expected) => {
                assert_eq!(result.as_ref(), Ok(&expected), "mismatch for {input:?}");
            }
        }
    }
}

#[test]
fn parse_rdfs_skips_blanks_and_hints_lists() {
    let data = b"<a> <p> <b> .\n# comment\n\n<a> <p> <c> .\n";
    let (nqs, meta) = parse_rdfs(data).expect("parse");
    assert_eq!(nqs.len(), 2);
    assert_eq!(meta.hint("p"), Some(datastreamdb_proto::HintType::List));
    assert_eq!(meta.hint("q"), None);
}

#[test]
fn parse_rdfs_reports_first_error() {
    let data = b"<a> <p> <b> .\n_:alice knows .\n";
    assert!(parse_rdfs(data).is_err());
}
