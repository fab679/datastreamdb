//! Port of the pure-parsing JSON tests (`json_parser_test.go`).
//!
//! Go's file is `//go:build integration`: most tests also `verify()` against
//! a live cluster, which has no Rust equivalent yet (see `TODO.md`). The
//! parsing halves (counts, shapes, exact values, errors) are all ported here.

use std::collections::HashMap;

use datastreamdb_chunker::{ChunkError, NQuad, NQuadBuffer, Op};
use datastreamdb_tok::facets::{Facet, FacetValType};
use datastreamdb_types::{ApiValue, Val, Value};

fn parse(json: &[u8], op: Op) -> Result<Vec<NQuad>, ChunkError> {
    let mut buf = NQuadBuffer::new(1000);
    buf.parse_json(json, op)?;
    Ok(buf.pending().to_vec())
}

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

fn facets_by_key(facets: &[Facet]) -> HashMap<&str, &Facet> {
    facets.iter().map(|f| (f.key.as_str(), f)).collect()
}

/// Port of Go's `checkFacets`: the `pred` edge carries exactly these facets.
fn check_facets(nqs: &[NQuad], pred: &str, expected: &[Facet]) {
    for nq in nqs.iter().filter(|n| n.predicate == pred) {
        assert_eq!(nq.facets.len(), expected.len(), "facet count for {pred}");
        let actual = facets_by_key(&nq.facets);
        for facet in expected {
            let got = actual.get(facet.key.as_str()).expect("facet key present");
            assert_eq!(*got, facet, "facet {}", facet.key);
        }
    }
}

/// Port of Go's `checkCount`: the first `pred` edge has `count` facets.
fn check_count(nqs: &[NQuad], pred: &str, count: usize) {
    for nq in nqs {
        if nq.predicate == pred {
            assert_eq!(nq.facets.len(), count);
            return;
        }
    }
    panic!("no nquad with predicate {pred}");
}

#[test]
fn json_number_parsing() {
    // Port of TestJsonNumberParsing.
    let cases: &[(&str, Option<ApiValue>)] = &[
        (
            "{\"uid\": \"1\", \"key\": 9223372036854775299}",
            Some(ApiValue::Int(9_223_372_036_854_775_299)),
        ),
        (
            "{\"uid\": \"1\", \"key\": 9223372036854775299.0}",
            Some(ApiValue::Double(9_223_372_036_854_775_299.0)),
        ),
        ("{\"uid\": \"1\", \"key\": 27670116110564327426}", None),
        (
            "{\"uid\": \"1\", \"key\": \"23452786\"}",
            Some(ApiValue::Str("23452786".into())),
        ),
        (
            "{\"uid\": \"1\", \"key\": \"23452786.2378\"}",
            Some(ApiValue::Str("23452786.2378".into())),
        ),
        (
            "{\"uid\": \"1\", \"key\": -1e10}",
            Some(ApiValue::Double(-1e10)),
        ),
        (
            "{\"uid\": \"1\", \"key\": 0E-0}",
            Some(ApiValue::Double(0.0)),
        ),
    ];
    for (input, out) in cases {
        let result = parse(input.as_bytes(), Op::Set);
        match out {
            Some(value) => {
                let nqs = result.expect(input);
                assert_eq!(nqs[0], lit("1", "key", value.clone()), "{input}");
            }
            None => assert!(result.is_err(), "expected error for {input}"),
        }
    }
}

#[test]
fn uid_out_of_range_errors() {
    let json = "{\"uid\":\"0xa14222b693e4ba34123\",\"name\":\"Name\"}";
    assert!(parse(json.as_bytes(), Op::Set).is_err());
}

#[test]
fn negative_uid_errors() {
    let json = "{\"uid\":\"-100\",\"name\":\"Name\"}";
    assert!(parse(json.as_bytes(), Op::Set).is_err());
}

#[test]
fn array_of_maps() {
    let json = r#"[
        {"uid": "uid(Project10)", "Ticket.row": {"uid": "uid(x)"}},
        {"Project.columns": [{"uid": "uid(x)"}], "uid": "uid(Project3)"},
        {"Ticket.onColumn": {"uid": "uid(x)"}, "uid": "uid(Ticket4)"}
    ]"#;
    let nqs = parse(json.as_bytes(), Op::Set).expect("parse");
    assert_eq!(nqs.len(), 3);
}

#[test]
fn delete_edges() {
    let json = r#"[{"uid": "0x1","name":null,"mobile":null,"car":null}]"#;
    let nqs = parse(json.as_bytes(), Op::Delete).expect("parse");
    assert_eq!(nqs.len(), 3);
}

#[test]
fn facets_all_types() {
    // Port of TestNquadsFromJsonFacets1 (string, bool, int, float, datetime).
    let operation = "READ WRITE";
    let val = Val {
        tid: datastreamdb_types::TypeId::String,
        value: Value::String(operation.into()),
    };
    let operation_tokens: Vec<String> =
        datastreamdb_tok::tok::get_term_tokens(std::slice::from_ref(&val))
            .expect("tokens")
            .into_iter()
            .map(|t| String::from_utf8_lossy(&t).into_owned())
            .collect();

    let time_str = "2006-01-02T15:04:05Z";
    let car_price = 30000.56f64;
    let json = format!(
        "[{{\"name\":\"Alice\",\"mobile\":\"040123456\",\"car\":\"MA0123\",\
         \"mobile|operation\": \"{operation}\", \"car|first\":true, \"car|age\": 3, \
         \"car|price\": {car_price}, \"car|since\": \"{time_str}\"}}]"
    );
    let nqs = parse(json.as_bytes(), Op::Set).expect("parse");
    assert_eq!(nqs.len(), 3);

    check_facets(
        &nqs,
        "mobile",
        &[Facet {
            key: "operation".into(),
            value: operation.as_bytes().to_vec(),
            val_type: FacetValType::String,
            tokens: operation_tokens,
            alias: String::new(),
        }],
    );
    // Go pins its 15-byte time encoding here; we pin our 12-byte layout for
    // the same instant (1136214245s, 0ns).
    check_facets(
        &nqs,
        "car",
        &[
            Facet {
                key: "first".into(),
                value: vec![1],
                val_type: FacetValType::Bool,
                tokens: Vec::new(),
                alias: String::new(),
            },
            Facet {
                key: "age".into(),
                value: 3i64.to_le_bytes().to_vec(),
                val_type: FacetValType::Int,
                tokens: Vec::new(),
                alias: String::new(),
            },
            Facet {
                key: "price".into(),
                value: car_price.to_le_bytes().to_vec(),
                val_type: FacetValType::Float,
                tokens: Vec::new(),
                alias: String::new(),
            },
            Facet {
                key: "since".into(),
                value: vec![0xe5, 0x40, 0xb9, 0x43, 0, 0, 0, 0, 0, 0, 0, 0],
                val_type: FacetValType::Datetime,
                tokens: Vec::new(),
                alias: String::new(),
            },
        ],
    );
}

#[test]
fn facets_on_child_edge() {
    // Port of TestNquadsFromJsonFacets2.
    let json = r#"[{"name":"Alice","friend":[{"name":"Dave","friend|close":"true"}]}]"#;
    let nqs = parse(json.as_bytes(), Op::Set).expect("parse");
    assert_eq!(nqs.len(), 3);
    check_count(&nqs, "friend", 1);
}

#[test]
fn facets_scalar_list_map() {
    // Port of TestNquadsFromJsonFacets3.
    let json = r#"[{"name":"Alice","friend": ["Joshua", "David", "Josh"],
        "friend|from": {"0": "school", "2": "college"},
        "friend|age": {"1": 20, "2": 21}}]"#;
    let nqs = parse(json.as_bytes(), Op::Set).expect("parse");
    assert_eq!(nqs.len(), 4);
    for nq in &nqs {
        let name = match nq.object_value.as_ref() {
            Some(ApiValue::Str(s)) => s.as_str(),
            _ => continue,
        };
        let expected = match name {
            "Alice" => 0,
            "Joshua" | "David" => 1,
            "Josh" => 2,
            _ => continue,
        };
        assert_eq!(nq.facets.len(), expected, "facets for {name}");
    }
}

#[test]
fn facets_scalar_list_invalid() {
    // Port of TestNquadsFromJsonFacets4.
    let cases = [
        (
            "facets_should_be_map",
            true,
            r#"[{"name":"Alice","friend": ["Joshua", "David", "Josh"],"friend|age": 20}]"#,
        ),
        (
            "predicate_should_be_list",
            true,
            r#"[{"name":"Alice","friend": "Joshua","friend|age": {"0": 20}}]"#,
        ),
        (
            "only_scalar_values_in_facet_map",
            true,
            r#"[{"name":"Alice","friend": ["Joshua"],"friend|age": {"0": {"1": 20}}}]"#,
        ),
        (
            "invalid_key_in_facet_map",
            true,
            r#"[{"name":"Alice","friend": ["Joshua"],"friend|age": {"a": 20}}]"#,
        ),
        (
            "predicate_is_null",
            false,
            r#"[{"name":"Alice","friend": null,"friend|age": {"0": 20}}]"#,
        ),
        (
            "empty_scalar_list",
            false,
            r#"[{"name":"Alice","friend": [],"friend|age": {"0": 20}}]"#,
        ),
        (
            "facet_map_is_null",
            false,
            r#"[{"name":"Alice","friend": ["Joshua"],"friend|age": null}]"#,
        ),
        (
            "facet_values_should_not_be_list",
            true,
            r#"[{"name":"Alice","friend": ["Joshua", "David", "Josh"],"friend|age": ["20"]}]"#,
        ),
        (
            "facet_map_index_beyond_length",
            false,
            r#"[{"name":"Alice","friend": ["Joshua", "David", "Josh"],"friend|age": {"100": 30, "20": 28}}]"#,
        ),
    ];
    for (name, error_out, json) in cases {
        let result = parse(json.as_bytes(), Op::Set);
        assert_eq!(result.is_err(), error_out, "case {name}");
    }
}

#[test]
fn facets_nested_children() {
    // Port of TestNquadsFromJsonFacets5.
    let json = r#"[{"name": "Alice", "friend": [{"name": "Dave", "friend|close": true,
        "friend": [{"name": "Emily", "friend|close": true}]}]}]"#;
    let nqs = parse(json.as_bytes(), Op::Set).expect("parse");
    assert_eq!(nqs.len(), 5);
    check_count(&nqs, "friend", 1);
}

#[test]
fn delete_without_uid_errors() {
    // Port of TestNquadsFromJsonError1 (identity with EmptyUid).
    let json = r#"{"name":"Alice","school":{"name":"Wellington Public School"}}"#;
    assert_eq!(
        parse(json.as_bytes(), Op::Delete),
        Err(ChunkError::EmptyUid)
    );
}

#[test]
fn scalar_and_geo_lists() {
    // Port of TestNquadsFromJsonList.
    let json = "{\"address\":[\"Riley Street\",\"Redfern\"],\"phone_number\":[123,9876],\
        \"points\":[{\"type\":\"Point\", \"coordinates\":[1.1,2.0]},{\"type\":\"Point\", \"coordinates\":[2.0,1.1]}]}";
    let nqs = parse(json.as_bytes(), Op::Set).expect("parse");
    assert_eq!(nqs.len(), 6);
}

#[test]
fn delete_edge_exact() {
    let nqs = parse(br#"{"uid":1000,"friend":[{"uid":1001}]}"#, Op::Delete).expect("parse");
    assert_eq!(nqs[0], edge("1000", "friend", "1001"));
}

#[test]
fn delete_star_exact() {
    let nqs = parse(br#"{"uid":1000,"name": null}"#, Op::Delete).expect("parse");
    assert_eq!(
        nqs[0],
        lit("1000", "name", ApiValue::Default("_STAR_ALL".into()))
    );
}

#[test]
fn val_in_upsert() {
    let nqs = parse(br#"{"uid":1000, "name": "val(name)"}"#, Op::Set).expect("parse");
    assert_eq!(nqs[0], edge("1000", "name", "val(name)"));
}

#[test]
fn delete_star_with_lang() {
    let nqs = parse(br#"{"uid":1000,"name@es": null}"#, Op::Delete).expect("parse");
    assert_eq!(
        nqs[0],
        NQuad {
            lang: "es".into(),
            ..lit("1000", "name", ApiValue::Default("_STAR_ALL".into()))
        }
    );
}

#[test]
fn set_nil_value_ignored() {
    let nqs = parse(br#"{"uid":1000,"name": null}"#, Op::Set).expect("parse");
    assert!(nqs.is_empty());
}

#[test]
fn empty_facet_stays_empty() {
    // Port of TestNquadsFromJsonEmptyFacet: nothing buffered, so flush emits
    // no batch (otherwise the node would be deleted).
    let json = br#"{"uid":1000,"doesnt|exist":null}"#;
    let mut buf = NQuadBuffer::new(-1);
    buf.parse_json(json, Op::Delete).expect("parse");
    buf.flush();
    assert!(buf.take_batches().is_empty());
}

#[test]
fn empty_string_value() {
    let nqs = parse(br#"[{"name":""}]"#, Op::Set).expect("parse");
    assert_eq!(nqs[0].object_value, Some(ApiValue::Str(String::new())));
}

#[test]
fn person_with_geo_counts() {
    // Parsing half of TestNquadsFromJson1 (5 edges; cluster verify skipped).
    let json = r#"{"uid":"1","namespace":"0x2","name":"Alice","age":26,"married":true,
        "now":"2026-11-18T10:00:00Z","address":{"type":"Point","coordinates":[1.1,2.0]}}"#;
    let nqs = parse(json.as_bytes(), Op::Set).expect("parse");
    assert_eq!(nqs.len(), 5);
}

#[test]
fn friends_nest_counts() {
    // Parsing half of TestNquadsFromJson2 (6 edges; cluster verify skipped).
    let json = r#"{"name":"Alice","friend":[
        {"name":"Charlie","married":false},{"uid":"1000","name":"Bob"}]}"#;
    let nqs = parse(json.as_bytes(), Op::Set).expect("parse");
    assert_eq!(nqs.len(), 6);
}

#[test]
fn assorted_documents_parse() {
    // Parsing halves of Json3/Json4/JsonMap/MultipleJsonObjects/EmptyUid/
    // BlankNodes (Go asserts NoError before the cluster verify).
    let docs = [
        r#"{"name":"Alice","school":{"name":"Wellington Public School"}}"#,
        r#"[{"name":"Alice","mobile":"040123456","car":"MA0123", "age": 21, "weight": 58.7}]"#,
        "{\"name\":\"Alice\",\n\"age\": 25,\n\"friends\": [{\n\"name\": \"Bob\"\n}]}",
        r#"[{"name": "A", "age": 25, "friends": [{"name": "A1", "friends": [{"name": "A11"}, {"name": "A12"}]}, {"name": "A2", "friends": [{"name": "A21"}, {"name": "A22"}]}]}, {"name": "B", "age": 26, "friends": [{"name": "B1", "friends": [{"name": "B11"}, {"name": "B12"}]}, {"name": "B2", "friends": [{"name": "B21"}, {"name": "B22"}]}]}]"#,
        "{\"uid\":\"\",\"name\":\"Alice\",\"following\":[{\"name\":\"Bob\"}],\"school\":[{\"uid\":\"\",\"name\":\"Crown Public School\"}]}",
        "{\"uid\":\"_:alice\",\"name\":\"Alice\",\"following\":[{\"name\":\"Bob\"}],\"school\":[{\"uid\":\"_:school\",\"name\":\"Crown Public School\"}]}",
    ];
    for (i, doc) in docs.iter().enumerate() {
        parse(doc.as_bytes(), Op::Set).expect(doc);
        assert!(i < docs.len());
    }
}

#[test]
fn empty_string_vector_pred() {
    // Port of TestNquadsJsonEmptyStringVectorPred (parse half + value check).
    let json = r#"[{"uid":"1","name":"","description":"","description_v":""}]"#;
    let nqs = parse(json.as_bytes(), Op::Set).expect("parse");
    assert_eq!(nqs.len(), 3);
    for nq in &nqs {
        assert_eq!(
            nq.object_value,
            Some(ApiValue::Str(String::new())),
            "empty strings stay strings: {}",
            nq.predicate
        );
    }
}

#[test]
fn empty_bracket_vector_pred() {
    // Port of TestNquadsJsonEmptySquareBracketVectorPred.
    let json = r#"{"name":"ipad","description":"","description_v":"[]"}"#;
    let nqs = parse(json.as_bytes(), Op::Set).expect("parse");
    assert_eq!(nqs.len(), 3);
    let vpred = nqs
        .iter()
        .find(|n| n.predicate == "description_v")
        .expect("vpred");
    assert_eq!(vpred.object_value, Some(ApiValue::Str("[]".into())));
}

#[test]
fn valid_vector_pred() {
    // Port of TestNquadsJsonValidVector (parse half + value check).
    let json = r#"{"name":"ipad","description":"","description_v":"[1.1, 2.2, 3.3]"}"#;
    let nqs = parse(json.as_bytes(), Op::Set).expect("parse");
    assert_eq!(nqs.len(), 3);
    let vpred = nqs
        .iter()
        .find(|n| n.predicate == "description_v")
        .expect("vpred");
    assert_eq!(
        vpred.object_value,
        Some(ApiValue::Vfloat(datastreamdb_types::float_array_as_bytes(
            &[1.1f32, 2.2, 3.3]
        )))
    );
}
