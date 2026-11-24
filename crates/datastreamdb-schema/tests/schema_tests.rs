//! Port of Go's `parse_test.go` (schema parsing + state).

use datastreamdb_proto::{
    Directive, OptionPair, SchemaUpdate, TypeUpdate, ValType, VectorIndexSpec,
};
use datastreamdb_schema::{SchemaState, parse};
use datastreamdb_types::TypeId;

fn root(attr: &str) -> String {
    datastreamdb_x::attr_in_root_namespace(attr)
}

fn check_schema(state: &SchemaState, expected: &[(&str, SchemaUpdate)]) {
    assert_eq!(state.predicates().len(), expected.len());
    for (name, update) in expected {
        let got = state
            .get(name, false)
            .unwrap_or_else(|| panic!("missing {name}"));
        assert_eq!(&got, update, "schema for {name}");
    }
}

const SCHEMA_VAL: &str = "
age:int .

name: string .
 address: string .
<http://scalar.com/helloworld/> : string .
amount: bigfloat .
coordinates: float32vector .
indexvector: float32vector @index(hnsw(metric:\"euclidean\")) .
";

#[test]
fn schema_parses_and_installs() {
    // Port of TestSchema.
    let state = SchemaState::new();
    state.parse_bytes(SCHEMA_VAL.as_bytes()).expect("parse");
    let hnsw = VectorIndexSpec {
        name: "hnsw".into(),
        options: vec![OptionPair {
            key: "metric".into(),
            value: "euclidean".into(),
        }],
    };
    check_schema(
        &state,
        &[
            (
                "0-name",
                SchemaUpdate {
                    predicate: root("name"),
                    value_type: ValType::String as i32,
                    ..SchemaUpdate::default()
                },
            ),
            (
                "0-address",
                SchemaUpdate {
                    predicate: root("address"),
                    value_type: ValType::String as i32,
                    ..SchemaUpdate::default()
                },
            ),
            (
                "0-http://scalar.com/helloworld/",
                SchemaUpdate {
                    predicate: root("http://scalar.com/helloworld/"),
                    value_type: ValType::String as i32,
                    ..SchemaUpdate::default()
                },
            ),
            (
                "0-age",
                SchemaUpdate {
                    predicate: root("age"),
                    value_type: ValType::Int as i32,
                    ..SchemaUpdate::default()
                },
            ),
            (
                "0-amount",
                SchemaUpdate {
                    predicate: root("amount"),
                    value_type: ValType::Bigfloat as i32,
                    ..SchemaUpdate::default()
                },
            ),
            (
                "0-coordinates",
                SchemaUpdate {
                    predicate: root("coordinates"),
                    value_type: ValType::Vfloat as i32,
                    ..SchemaUpdate::default()
                },
            ),
            (
                "0-indexvector",
                SchemaUpdate {
                    predicate: root("indexvector"),
                    value_type: ValType::Vfloat as i32,
                    directive: Directive::Index as i32,
                    index_specs: vec![hnsw],
                    ..SchemaUpdate::default()
                },
            ),
        ],
    );

    assert_eq!(state.type_of(&root("age")), Ok(TypeId::Int));
    assert_eq!(state.type_of(&root("amount")), Ok(TypeId::BigFloat));
    assert_eq!(state.type_of(&root("coordinates")), Ok(TypeId::VFloat));
    assert!(state.type_of(&root("agea")).is_err());
}

#[test]
fn schema_errors() {
    // Ports of TestSchema1/2/3_Error.
    let state = SchemaState::new();
    assert!(
        state
            .parse_bytes(b"\nage:int .\n\nname: string .\naddress: string .\n\n)\n")
            .is_err()
    );
    assert!(state.parse_bytes(b"\nname: ( string\n").is_err());
    assert!(state.parse_bytes(b"\ntest test: int\n").is_err());
}

#[test]
fn schema_index_errors() {
    // Ports of TestSchemaIndex_Error1/2/3.
    let state = SchemaState::new();
    // Duplicate tokenizers.
    assert!(state
        .parse_bytes(
            b"\nname: string @index(exact, exact) .\naddress: string @index(term) .\nid: id @index(exact, term, exact) .\n"
        )
        .is_err());
    // Object types can't be indexed.
    assert!(state.parse_bytes(b"\nperson: uid @index .\n").is_err());
    assert!(state.parse_bytes(b"\nvalue: default @index .\n").is_err());
    assert!(state.parse_bytes(b"\npass: password @index .\n").is_err());
    // Missing comma.
    assert!(
        state
            .parse_bytes(b"\nname:string @index(exact term) .\n")
            .is_err()
    );
}

#[test]
fn schema_index_custom() {
    // Port of TestSchemaIndexCustom.
    let state = SchemaState::new();
    state
        .parse_bytes(
            b"\nage     : int @index(int) .\nname    : string @index(exact) @count .\naddress : string @index(term) .\nfriend  : [uid] @reverse @count .\n",
        )
        .expect("parse");
    check_schema(
        &state,
        &[
            (
                "0-name",
                SchemaUpdate {
                    predicate: root("name"),
                    value_type: ValType::String as i32,
                    directive: Directive::Index as i32,
                    tokenizer: vec!["exact".into()],
                    count: true,
                    ..SchemaUpdate::default()
                },
            ),
            (
                "0-address",
                SchemaUpdate {
                    predicate: root("address"),
                    value_type: ValType::String as i32,
                    directive: Directive::Index as i32,
                    tokenizer: vec!["term".into()],
                    ..SchemaUpdate::default()
                },
            ),
            (
                "0-age",
                SchemaUpdate {
                    predicate: root("age"),
                    value_type: ValType::Int as i32,
                    directive: Directive::Index as i32,
                    tokenizer: vec!["int".into()],
                    ..SchemaUpdate::default()
                },
            ),
            (
                "0-friend",
                SchemaUpdate {
                    predicate: root("friend"),
                    value_type: ValType::Uid as i32,
                    directive: Directive::Reverse as i32,
                    count: true,
                    list: true,
                    ..SchemaUpdate::default()
                },
            ),
        ],
    );
    assert!(state.is_indexed(&root("name"), false));
    assert!(!state.is_reversed(&root("name"), false));
    assert_eq!(state.tokenizers(&root("age"), false)[0].name(), "int");
}

#[test]
fn parse_errors() {
    // Ports of TestParse, TestParse3_Error, TestParse5/6/7/8_Error.
    assert!(parse("age:int @index . name:string").is_err());
    assert!(parse("age:uid @index .").is_err());
    assert!(parse("value:default @index .").is_err());
    assert!(parse("pass:password @index .").is_err());
    assert!(parse("name:string @index .").is_err());
    assert!(parse("dob:dateTime @index .").is_err());
}

#[test]
fn parse_empty() {
    // Port of TestParse2.
    let result = parse("").expect("parse");
    assert!(result.preds.is_empty());
}

#[test]
fn parse_bad_tokenizer_type() {
    // Port of TestParse4_Error.
    let err = parse("alive:bool @index(geo) .").expect_err("must fail");
    assert!(
        err.to_string()
            .contains("Tokenizer: geo isn't valid for predicate: alive of type: bool"),
        "{err}"
    );
}

#[test]
fn parse_fulltext_ok() {
    // Port of TestParse4_NoError.
    assert!(parse("name:string @index(fulltext) .").is_ok());
}

#[test]
fn parse_noconflict_on_uid_ok() {
    // Port of TestParse9_Error (which actually expects success).
    assert!(parse("age:uid @noconflict .").is_ok());
}

#[test]
fn parse_scalar_lists() {
    // Port of TestParseScalarList.
    let result = parse("\n\t\tjobs: [string] @index(term) .\n\t\toccupations: [string] .\n\t\tgraduation: [dateTime] .\n\t")
        .expect("parse");
    assert_eq!(result.preds.len(), 3);
    assert_eq!(
        result.preds[0],
        SchemaUpdate {
            predicate: root("jobs"),
            value_type: ValType::String as i32,
            directive: Directive::Index as i32,
            tokenizer: vec!["term".into()],
            list: true,
            ..SchemaUpdate::default()
        }
    );
    assert_eq!(
        result.preds[1],
        SchemaUpdate {
            predicate: root("occupations"),
            value_type: ValType::String as i32,
            list: true,
            ..SchemaUpdate::default()
        }
    );
    assert_eq!(
        result.preds[2],
        SchemaUpdate {
            predicate: root("graduation"),
            value_type: ValType::Datetime as i32,
            list: true,
            ..SchemaUpdate::default()
        }
    );
}

#[test]
fn parse_scalar_list_errors() {
    // Ports of TestParseScalarListError1/2/3.
    let err = parse("\n\t\tfriend: [string .\n\t").expect_err("unclosed");
    assert!(
        err.to_string()
            .contains("Unclosed [ while parsing schema for: friend"),
        "{err}"
    );
    let err = parse("\n\t\tfriend: string] .\n\t").expect_err("stray bracket");
    assert!(err.to_string().contains("Invalid ending"), "{err}");
    let err = parse("\n\t\tfriend: [bool] .\n\t").expect_err("bool list");
    assert!(
        err.to_string()
            .contains("Unsupported type for list: [bool]"),
        "{err}"
    );
}

#[test]
fn parse_uid_list_and_single() {
    // Ports of TestParseUidList / TestParseUidSingleValue.
    let result = parse("\n\t\tfriend: [uid] .\n\t").expect("parse");
    assert_eq!(result.preds.len(), 1);
    assert_eq!(
        result.preds[0],
        SchemaUpdate {
            predicate: root("friend"),
            value_type: ValType::Uid as i32,
            list: true,
            ..SchemaUpdate::default()
        }
    );
    let result = parse("\n\t\tfriend: uid .\n\t").expect("parse");
    assert_eq!(
        result.preds[0],
        SchemaUpdate {
            predicate: root("friend"),
            value_type: ValType::Uid as i32,
            ..SchemaUpdate::default()
        }
    );
}

#[test]
fn parse_underscore_and_upsert() {
    // Ports of TestParseUnderscore / TestParseUpsert.
    assert!(parse("_share_:string @index(term) .").is_ok());
    assert!(
        parse(
            "\n\t\tjobs : string @index(exact) @upsert .\n\t\tage  : int @index(int) @upsert .\n\t"
        )
        .is_ok()
    );
}

fn field(name: &str) -> SchemaUpdate {
    SchemaUpdate {
        predicate: root(name),
        ..SchemaUpdate::default()
    }
}

#[test]
fn parse_types() {
    // Ports of TestParseEmptyType / TestParseTypeEOF / TestParseSingleType /
    // TestParseCombinedSchemasAndTypes / TestParseMultipleTypes.
    let result = parse("\n\t\ttype Person {\n\n\t\t}\n\t").expect("parse");
    assert_eq!(result.types.len(), 1);
    assert_eq!(
        result.types[0],
        TypeUpdate {
            type_name: root("Person"),
            fields: vec![],
        }
    );

    let result = parse("\n\t\ttype Person {\n\t\t}").expect("parse");
    assert_eq!(
        result.types[0],
        TypeUpdate {
            type_name: root("Person"),
            fields: vec![],
        }
    );

    let result = parse("\n\t\ttype Person {\n\t\t\tname\n\t\t}\n\t").expect("parse");
    assert_eq!(
        result.types[0],
        TypeUpdate {
            type_name: root("Person"),
            fields: vec![field("name")],
        }
    );

    let result =
        parse("\n\t\ttype Person {\n\t\t\tname\n\t\t}\n        name: string .\n\t").expect("parse");
    assert_eq!(result.preds.len(), 1);
    assert_eq!(
        result.preds[0],
        SchemaUpdate {
            predicate: root("name"),
            value_type: ValType::String as i32,
            ..SchemaUpdate::default()
        }
    );
    assert_eq!(
        result.types[0],
        TypeUpdate {
            type_name: root("Person"),
            fields: vec![field("name")],
        }
    );

    let result =
        parse("\n\t\ttype Person {\n\t\t\tname\n\t\t}\n\t\ttype Animal {\n\t\t\tname\n\t\t}\n\t")
            .expect("parse");
    assert_eq!(result.types.len(), 2);
    assert_eq!(result.types[0].type_name, root("Person"));
    assert_eq!(result.types[1].type_name, root("Animal"));
}

#[test]
fn parse_type_duplicate_fields() {
    // Port of TestParseTypeDuplicateFields.
    let err = parse("\n\t\ttype Person {\n\t\t\tname\n\t\t\tname\n\t\t}\n\t").expect_err("dup");
    assert!(
        err.to_string().contains("Duplicate fields with name: name"),
        "{err}"
    );
}

#[test]
fn parse_old_type_formats() {
    // Ports of TestOldTypeFormat / TestOldAndNewTypeFormat.
    let result = parse("\n\t\ttype Person {\n\t\t\tname: [string!]!\n\t\t\taddress: string!\n\t\t\tchildren: [Person]\n\t\t}\n\t")
        .expect("parse");
    assert_eq!(
        result.types[0],
        TypeUpdate {
            type_name: root("Person"),
            fields: vec![field("name"), field("address"), field("children")],
        }
    );
    let result = parse("\n\t\ttype Person {\n\t\t\tname: [string!]!\n\t\t\taddress\n\t\t}\n\t")
        .expect("parse");
    assert_eq!(result.types[0].fields.len(), 2);
}

#[test]
fn parse_type_missing_newline() {
    // Port of TestParseTypeErrMissingNewLine.
    let err = parse("\n\t\ttype Person {\n\t\t}type Animal {}").expect_err("missing newline");
    assert!(
        err.to_string()
            .contains("Expected new line or EOF after type declaration"),
        "{err}"
    );
}

#[test]
fn parse_comments() {
    // Ports of TestParseComments / TestParseCommentsNoop /
    // TestParseCommentsErrMissingType / TestParseTypeComments.
    parse(
        "\n\t\t#\n\t\t# This is a test\n\t\t#\n\t\tuser: bool .\n\t\tuser.name: string @index(exact) . # unique\n\t\tuser.email: string @index(exact) . # unique\n\t\tuser.password: password .\n\t\tuser.code: string .\n\t\tnode: bool .\n\t\tnode.hashid: string @index(exact) .\n\t\tnode.owner: uid @reverse .\n\t\tnode.parent: uid .\n\t\tnode.xdata: string .\n\t\t#\n\t\t# End of test\n\t\t#\n\t",
    )
    .expect("comments");
    parse("\n# line one\n# line two\n\t").expect("noop comments");
    let err =
        parse("\n\t\t# commented out type\n\t\tnode: # bool .\n\t").expect_err("missing type");
    assert!(err.to_string().contains("Missing Type"), "{err}");
    parse("\n\t\t# User is a service user\n\t\ttype User {\n\t\t\t# TODO\n\t\t\tname # e.g., srfrog\n\t\t\t\t\t\t\t # expanded comment\n\t\t}\n\t\t# /User\n\t").expect("type comments");
}

#[test]
fn parse_with_namespace_blocks() {
    // Port of TestParseWithNamespace.
    let result = parse(
        "\n\t\t[10] jobs: [string] @index(term) .\n\t\t[0x123] occupations: [string] .\n\t\t[0xf2] graduation: [dateTime] .\n\t\t[0xf2] type Person {\n\t\t\tname: [string!]!\n\t\t\taddress: string!\n\t\t\tchildren: [Person]\n\t\t}\n\t",
    )
    .expect("parse");
    assert_eq!(result.preds.len(), 3);
    assert_eq!(
        result.preds[0],
        SchemaUpdate {
            predicate: datastreamdb_x::namespace_attr(10, "jobs"),
            value_type: ValType::String as i32,
            directive: Directive::Index as i32,
            tokenizer: vec!["term".into()],
            list: true,
            ..SchemaUpdate::default()
        }
    );
    assert_eq!(
        result.preds[1],
        SchemaUpdate {
            predicate: datastreamdb_x::namespace_attr(0x123, "occupations"),
            value_type: ValType::String as i32,
            list: true,
            ..SchemaUpdate::default()
        }
    );
    assert_eq!(
        result.preds[2],
        SchemaUpdate {
            predicate: datastreamdb_x::namespace_attr(0xf2, "graduation"),
            value_type: ValType::Datetime as i32,
            list: true,
            ..SchemaUpdate::default()
        }
    );
    assert_eq!(result.types.len(), 1);
    assert_eq!(
        result.types[0],
        TypeUpdate {
            type_name: datastreamdb_x::namespace_attr(0xf2, "Person"),
            fields: vec![
                SchemaUpdate {
                    predicate: datastreamdb_x::namespace_attr(0xf2, "name"),
                    ..SchemaUpdate::default()
                },
                SchemaUpdate {
                    predicate: datastreamdb_x::namespace_attr(0xf2, "address"),
                    ..SchemaUpdate::default()
                },
                SchemaUpdate {
                    predicate: datastreamdb_x::namespace_attr(0xf2, "children"),
                    ..SchemaUpdate::default()
                },
            ],
        }
    );
}

#[test]
fn state_predicate_helpers() {
    // Exercises state helpers Go covers indirectly (worker paths).
    let state = SchemaState::new();
    state
        .parse_bytes(b"friend: [uid] @reverse @count .\nname: string @index(exact) .\n")
        .expect("parse");
    assert!(state.is_reversed(&root("friend"), false));
    assert!(state.has_count(&root("friend"), false));
    assert!(state.is_list(&root("friend")));
    assert!(!state.is_list(&root("name")));
    assert!(!state.has_upsert(&root("name")));
    assert!(!state.has_lang(&root("name")));
    assert!(!state.has_no_conflict(&root("name")));
    assert!(!state.has_no_conflict(&root("missing")));
    assert_eq!(state.tokenizer_names(&root("name"), false), ["exact"]);
    assert!(state.vector_indexes(&root("name"), false).is_empty());
    assert_eq!(state.predicates_to_delete(&root("name")), [root("name")]);
    assert!(!state.indexing_in_progress());
    assert!(state.get_type(&root("Person")).is_none());

    // Write-context views prefer the mutation schema.
    let write_update = SchemaUpdate {
        predicate: root("name"),
        value_type: ValType::String as i32,
        directive: Directive::Index as i32,
        tokenizer: vec!["term".into()],
        ..SchemaUpdate::default()
    };
    state.set_mut_schema(&root("name"), write_update);
    assert!(state.indexing_in_progress());
    assert_eq!(state.indexing_predicates(), [root("name")]);
    assert_eq!(state.tokenizer_names(&root("name"), true), ["term"]);
    assert_eq!(state.tokenizer_names(&root("name"), false), ["exact"]);
    state.delete_mut_schema(&root("name"));
    assert!(!state.indexing_in_progress());

    // Namespaces and per-namespace deletion.
    let typed = TypeUpdate {
        type_name: datastreamdb_x::namespace_attr(0xf2, "Person"),
        fields: vec![],
    };
    state.set_type(&datastreamdb_x::namespace_attr(0xf2, "Person"), typed);
    assert!(state.namespaces().contains(&0xf2));
    state.delete_preds_for_ns(0xf2);
    assert!(
        state
            .get_type(&datastreamdb_x::namespace_attr(0xf2, "Person"))
            .is_none()
    );
    assert!(state.get(&root("name"), false).is_some());
}

#[test]
fn initial_data_and_predef_checks() {
    use datastreamdb_schema::{
        check_and_modify_pre_def_predicate, complete_initial_schema, complete_initial_types,
        initial_schema, initial_types, is_pre_def_type_changed,
    };
    let schema = initial_schema(0, false);
    assert!(schema.iter().any(|s| s.predicate == "0-datastreamdb.type"));
    assert!(!schema.iter().any(|s| s.predicate == "0-datastreamdb.xid"));
    let full = complete_initial_schema(0);
    assert!(full.iter().any(|s| s.predicate == "0-datastreamdb.xid"));
    let types = initial_types(0, false);
    assert!(
        types
            .iter()
            .any(|t| t.type_name == "0-datastreamdb.graphql")
    );
    assert!(complete_initial_types(0).len() >= types.len());

    // Unchanged pre-defined update reports no change.
    let mut xid = full
        .iter()
        .find(|s| s.predicate == "0-datastreamdb.xid")
        .expect("xid")
        .clone();
    assert!(!check_and_modify_pre_def_predicate(&mut xid));
    // Other changes are reported.
    xid.count = true;
    assert!(check_and_modify_pre_def_predicate(&mut xid));
    // Non-pre-defined predicates are ignored.
    let mut custom = SchemaUpdate {
        predicate: root("name"),
        ..SchemaUpdate::default()
    };
    assert!(!check_and_modify_pre_def_predicate(&mut custom));

    let user = complete_initial_types(0)
        .into_iter()
        .find(|t| t.type_name == "0-datastreamdb.type.User")
        .expect("user");
    assert!(!is_pre_def_type_changed(&user));
    let mut changed = user.clone();
    changed.fields.push(SchemaUpdate {
        predicate: root("extra"),
        ..SchemaUpdate::default()
    });
    assert!(is_pre_def_type_changed(&changed));
    let custom_type = TypeUpdate {
        type_name: root("Person"),
        fields: vec![],
    };
    assert!(!is_pre_def_type_changed(&custom_type));
}
