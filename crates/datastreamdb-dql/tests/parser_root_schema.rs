//! Parser tests: root clauses, schema blocks, mutations, and fragments (port of Go's `parser_test.go`, part 2 of 8).
//!
//! Split from `parser_tests.rs` to honor the 1000-line file rule; shared
//! helpers live in [`common`].

#[path = "common/mod.rs"]
mod common;

use common::{child_attrs, parse_err, parse_str};

#[test]
fn parse_query_with_var_in_ineq() {
    let res = parse_str(
        "\n\t{\n\t\tvar(func: uid(0x0a)) {\n\t\t\tfr as friends {\n\t\t\t\ta as age\n\t\t\t}\n\t\t}\n\n\t\tme(func: uid(fr)) @filter(gt(val(a), 10)) {\n\t\t name\n\t\t}\n\t}\n",
    );
    assert_eq!(res.query.len(), 2);
    assert_eq!(res.query[0].children[0].var, "fr");
    assert_eq!(res.query[1].needs_var[0].name, "fr");
    assert_eq!(res.query[1].needs_var[0].typ, datastreamdb_dql::UID_VAR);
    let func = res.query[1]
        .filter
        .as_ref()
        .expect("filter")
        .func
        .as_ref()
        .expect("func");
    assert_eq!(func.needs_var[0].typ, datastreamdb_dql::VALUE_VAR);
    assert_eq!(func.args.len(), 1);
    assert_eq!(func.attr, "a");
    assert!(func.is_value_var);
    assert_eq!(func.args[0].value, "10");
    assert!(!func.args[0].is_value_var);
    assert_eq!(func.name, "gt");
}

#[test]
fn parse_query_with_var_1() {
    let res = parse_str(
        "\n\t{\n\t\tvar(func: uid(0x0a)) {\n\t\t\tL AS friends\n\t\t}\n\n\t\tme(func: uid(L)) {\n\t\t name\n\t\t}\n\t}\n",
    );
    assert_eq!(res.query.len(), 2);
    assert_eq!(res.query[0].children[0].var, "L");
    assert_eq!(res.query[1].needs_var[0].name, "L");
    assert_eq!(res.query[1].needs_var[0].typ, datastreamdb_dql::UID_VAR);
}

#[test]
fn parse_query_with_multiple_var() {
    let res = parse_str(
        "\n\t{\n\t\tvar(func: uid(0x0a)) {\n\t\t\tL AS friends {\n\t\t\t\tB AS relatives\n\t\t\t}\n\t\t}\n\n\t\tme(func: uid(L)) {\n\t\t name\n\t\t}\n\n\t\trelatives(func: uid(B)) {\n\t\t\tname\n\t\t}\n\t}\n",
    );
    assert_eq!(res.query.len(), 3);
    assert_eq!(res.query[0].children[0].var, "L");
    assert_eq!(res.query[0].children[0].children[0].var, "B");
    assert_eq!(res.query[1].needs_var[0].name, "L");
    assert_eq!(res.query[2].needs_var[0].name, "B");
    assert_eq!(res.query[1].needs_var[0].typ, datastreamdb_dql::UID_VAR);
    assert_eq!(res.query[2].needs_var[0].typ, datastreamdb_dql::UID_VAR);
    assert_eq!(res.query_vars[0].defines, ["L", "B"]);
    assert_eq!(res.query_vars[1].needs, ["L"]);
    assert_eq!(res.query_vars[2].needs, ["B"]);
}

#[test]
fn parse_shortest_path() {
    let res = parse_str(
        "\n\t{\n\t\tshortest(from:0x0a, to:0x0b, numpaths: 3, minweight: 3, maxweight: 6, maxfrontiersize: 1) {\n\t\t\tfriends\n\t\t\tname\n\t\t}\n\t}\n",
    );
    assert_eq!(res.query.len(), 1);
    let args = &res.query[0].shortest_path_args;
    assert_eq!(args.from.as_ref().expect("from").uid[0], 0xa);
    assert_eq!(args.to.as_ref().expect("to").uid[0], 0xb);
    assert_eq!(res.query[0].args["numpaths"], "3");
    assert_eq!(res.query[0].args["minweight"], "3");
    assert_eq!(res.query[0].args["maxweight"], "6");
    assert_eq!(res.query[0].args["maxfrontiersize"], "1");
}

#[test]
fn parse_shortest_path_with_uid_vars() {
    let res = parse_str(
        "{\n\t\ta as var(func: uid(0x01))\n\t\tb as var(func: uid(0x02))\n\n\t\tshortest(from: uid(a), to: uid(b)) {\n\t\t\tpassword\n\t\t\tfriend\n\t\t}\n\n\t}",
    );
    let q = &res.query[2];
    let from = q.shortest_path_args.from.as_ref().expect("from");
    assert_eq!(from.needs_var.len(), 1);
    assert_eq!(from.needs_var[0].name, "a");
    assert_eq!(from.name, "uid");
    let to = q.shortest_path_args.to.as_ref().expect("to");
    assert_eq!(to.needs_var.len(), 1);
}

#[test]
fn parse_shortest_path_invalid_fn_error() {
    parse_err(
        "{\n\t\tshortest(from: eq(a), to: uid(b)) {\n\t\t\tpassword\n\t\t\tfriend\n\t\t}\n\n\t}",
    );
}

#[test]
fn parse_multiple_queries() {
    let res = parse_str(
        "\n\t{\n\t\tyou(func: uid(0x0a)) {\n\t\t\tname\n\t\t}\n\n\t\tme(func: uid(0x0b)) {\n\t\t friends\n\t\t}\n\t}\n",
    );
    assert_eq!(res.query.len(), 2);
}

#[test]
fn parse_root_args_1() {
    let res = parse_str(
        "\n\tquery {\n\t\tme(func: uid(0x0a), first: -4, offset: +1) {\n\t\t\tfriends {\n\t\t\t\tname\n\t\t\t}\n\t\t\tgender,age\n\t\t\thometown\n\t\t}\n\t}\n",
    );
    assert_eq!(res.query.len(), 1);
    assert_eq!(res.query[0].args.len(), 2);
    assert_eq!(res.query[0].args["first"], "-4");
    assert_eq!(res.query[0].args["offset"], "+1");
    assert_eq!(
        child_attrs(&res.query[0]),
        ["friends", "gender", "age", "hometown"]
    );
    assert_eq!(child_attrs(&res.query[0].children[0]), ["name"]);
}

#[test]
fn parse_root_args_2() {
    let res = parse_str(
        "\n\tquery {\n\t\tme(func: uid(0x0a), first: 1, offset:0) {\n\t\t\tfriends {\n\t\t\t\tname\n\t\t\t}\n\t\t\tgender,age\n\t\t\thometown\n\t\t}\n\t}\n",
    );
    assert_eq!(res.query.len(), 1);
    assert_eq!(res.query[0].args.len(), 2);
    assert_eq!(res.query[0].args["first"], "1");
    assert_eq!(res.query[0].args["offset"], "0");
    assert_eq!(
        child_attrs(&res.query[0]),
        ["friends", "gender", "age", "hometown"]
    );
    assert_eq!(child_attrs(&res.query[0].children[0]), ["name"]);
}

#[test]
fn parse_basic() {
    let res = parse_str(
        "\n\tquery {\n\t\tme(func: uid(0x0a)) {\n\t\t\tfriends {\n\t\t\t\tname\n\t\t\t}\n\t\t\tgender,age\n\t\t\thometown\n\t\t}\n\t}\n",
    );
    assert_eq!(res.query.len(), 1);
    assert_eq!(
        child_attrs(&res.query[0]),
        ["friends", "gender", "age", "hometown"]
    );
    assert_eq!(child_attrs(&res.query[0].children[0]), ["name"]);
}

#[test]
fn parse_error_invalid_op() {
    let err = parse_err(
        "\n\t\tme(func: uid(0x0a)) {\n\t\t\tfriends {\n\t\t\t\tname\n\t\t\t}\n\t\t\tgender,age\n\t\t\thometown\n\t\t}\n\t}\n",
    );
    assert!(err.contains("Invalid operation type: me"), "{err}");
}

#[test]
fn parse_xid() {
    let res =
        parse_str("\n\tquery {\n\t\tuser(func: uid( 0x11)) {\n\t\t\ttype.object.name\n\t\t}\n\t}");
    assert_eq!(child_attrs(&res.query[0]), ["type.object.name"]);
}

#[test]
fn parse_id_list() {
    let res =
        parse_str("\n\tquery {\n\t\tuser(func: uid(0x1)) {\n\t\t\ttype.object.name\n\t\t}\n\t}");
    assert_eq!(child_attrs(&res.query[0]), ["type.object.name"]);
}

#[test]
fn parse_id_list_1() {
    let res = parse_str(
        "\n\tquery {\n\t\tuser(func: uid(0x1, 0x34)) {\n\t\t\ttype.object.name\n\t\t}\n\t}",
    );
    assert_eq!(child_attrs(&res.query[0]), ["type.object.name"]);
    assert_eq!(res.query[0].uid, [0x1, 0x34]);
    assert_eq!(res.query[0].uid.len(), 2);
}

#[test]
fn parse_id_list_error() {
    let err = parse_err(
        "\n\tquery {\n\t\tuser(func: uid( [0x1, 0x1, abc, ade, 0x34))] {\n\t\t\ttype.object.name\n\t\t}\n\t}",
    );
    assert!(
        err.contains("Unrecognized character in lexText: U+005D ']'"),
        "{err}"
    );
}

#[test]
fn parse_id_list_error_2() {
    let err = parse_err(
        "\n\tquery {\n\t\tuser(func: uid( [0x1, 0x1, 2, 3, 0x34])) {\n\t\t\ttype.object.name\n\t\t}\n\t}",
    );
    assert!(
        err.contains("Unexpected character [ while parsing request."),
        "{err}"
    );
}

#[test]
fn parse_first() {
    let res = parse_str(
        "\n\tquery {\n\t\tuser(func: uid( 0x1)) {\n\t\t\ttype.object.name\n\t\t\tfriends (first: 10) {\n\t\t\t}\n\t\t}\n\t}",
    );
    assert_eq!(child_attrs(&res.query[0]), ["type.object.name", "friends"]);
    assert_eq!(res.query[0].children[1].args["first"], "10");
}

#[test]
fn parse_first_error() {
    let err = parse_err(
        "\n\tquery {\n\t\tuser(func: uid( 0x1)) {\n\t\t\ttype.object.name\n\t\t\tfriends (first: ) {\n\t\t\t}\n\t\t}\n\t}",
    );
    assert!(err.contains("Expecting argument value"), "{err}");
    assert!(err.contains("\")\""), "{err}");
}

#[test]
fn parse_after() {
    let res = parse_str(
        "\n\tquery {\n\t\tuser(func: uid( 0x1)) {\n\t\t\ttype.object.name\n\t\t\tfriends (first: 10, after: 3) {\n\t\t\t}\n\t\t}\n\t}",
    );
    assert_eq!(child_attrs(&res.query[0]), ["type.object.name", "friends"]);
    assert_eq!(res.query[0].children[1].args["first"], "10");
    assert_eq!(res.query[0].children[1].args["after"], "3");
}

#[test]
fn parse_offset() {
    let res = parse_str(
        "\n\tquery {\n\t\tuser(func: uid( 0x1)) {\n\t\t\ttype.object.name\n\t\t\tfriends (first: 10, offset: 3) {\n\t\t\t}\n\t\t}\n\t}",
    );
    assert_eq!(child_attrs(&res.query[0]), ["type.object.name", "friends"]);
    assert_eq!(res.query[0].children[1].args["first"], "10");
    assert_eq!(res.query[0].children[1].args["offset"], "3");
}

#[test]
fn parse_offset_error() {
    let err = parse_err(
        "\n\tquery {\n\t\tuser(func: uid( 0x1)) {\n\t\t\ttype.object.name\n\t\t\tfriends (first: 10, offset: ) {\n\t\t\t}\n\t\t}\n\t}",
    );
    assert!(err.contains("Expecting argument value"), "{err}");
    assert!(err.contains("\")\""), "{err}");
}

#[test]
fn parse_error_2() {
    let err = parse_err("\n\t\tquery {\n\t\t\tme {\n\t\t\t\tname\n\t\t\t}\n\t\t}\n\t");
    assert!(err.contains("Expected Left round brackets"), "{err}");
    assert!(err.contains("\"{\""), "{err}");
}

#[test]
fn parse_pass_1() {
    let err = parse_err(
        "\n\t\t{\n\t\t\tme(func: uid(0x0a)) {\n\t\t\t\tname,\n\t\t\t\tfriends(xid:what) {  # xid would be ignored.\n\t\t\t\t}\n\t\t\t}\n\t\t}\n\t",
    );
    assert!(err.contains("Got invalid keyword: xid"), "{err}");
}

#[test]
fn parse_alias_count() {
    let res = parse_str(
        "\n\t\t{\n\t\t\tme(func: uid(0x0a)) {\n\t\t\t\tname,\n\t\t\t\tbestFriend: friends(first: 10) {\n\t\t\t\t\tnameCount: count(name)\n\t\t\t\t}\n\t\t\t}\n\t\t}\n\t",
    );
    assert_eq!(child_attrs(&res.query[0]), ["name", "friends"]);
    assert_eq!(res.query[0].children[1].alias, "bestFriend");
    assert_eq!(child_attrs(&res.query[0].children[1]), ["name"]);
    assert_eq!(res.query[0].children[1].children[0].alias, "nameCount");
}

#[test]
fn parse_alias_var() {
    let res = parse_str(
        "\n\t\t{\n\t\t\tme(func: uid(0x0a)) {\n\t\t\t\tname,\n\t\t\t\tf as bestFriend: friends(first: 10) {\n\t\t\t\t\tc as count(friend)\n\t\t\t\t}\n\t\t\t}\n\n\t\t\tfriend(func: uid(f)) {\n\t\t\t\tname\n\t\t\t\tfcount: val(c)\n\t\t\t}\n\t\t}\n\t ",
    );
    assert_eq!(child_attrs(&res.query[0]), ["name", "friends"]);
    assert_eq!(res.query[0].children[1].alias, "bestFriend");
    assert_eq!(res.query[1].children[1].alias, "fcount");
}

#[test]
fn parse_alias_max() {
    let res = parse_str(
        "\n\t\t{\n\t\t\tme(func: uid(0x0a)) {\n\t\t\t\tname,\n\t\t\t\tbestFriend: friends(first: 10) {\n\t\t\t\t\tx as count(friends)\n\t\t\t\t}\n\t\t\t\tmaxfriendcount: max(val(x))\n\t\t\t}\n\t\t}\n\t ",
    );
    assert_eq!(res.query[0].children[1].alias, "bestFriend");
    assert_eq!(res.query[0].children[2].alias, "maxfriendcount");
}

#[test]
fn parse_alias() {
    let res = parse_str(
        "\n\t\t{\n\t\t\tme(func: uid(0x0a)) {\n\t\t\t\tname,\n\t\t\t\tbestFriend: friends(first: 10) {\n\t\t\t\t\tname\n\t\t\t\t}\n\t\t\t}\n\t\t}\n\t ",
    );
    assert_eq!(child_attrs(&res.query[0]), ["name", "friends"]);
    assert_eq!(res.query[0].children[1].alias, "bestFriend");
    assert_eq!(child_attrs(&res.query[0].children[1]), ["name"]);
}

#[test]
fn parse_alias_1() {
    let res = parse_str(
        "\n\t\t{\n\t\t\tme(func: uid(0x0a)) {\n\t\t\t\tname: type.object.name.en\n\t\t\t\tbestFriend: friends(first: 10) {\n\t\t\t\t\tname: type.object.name.hi\n\t\t\t\t}\n\t\t\t}\n\t\t}\n\t ",
    );
    assert_eq!(
        child_attrs(&res.query[0]),
        ["type.object.name.en", "friends"]
    );
    assert_eq!(res.query[0].children[1].alias, "bestFriend");
    assert_eq!(res.query[0].children[1].children[0].alias, "name");
    assert_eq!(
        child_attrs(&res.query[0].children[1]),
        ["type.object.name.hi"]
    );
}

#[test]
fn parse_bad_alias() {
    let err = parse_err(
        "\n\t\t{\n\t\t\tme(func: uid(0x0a)) {\n\t\t\t\tname: type.object.name.en: after_colon\n\t\t\t\tbestFriend: friends(first: 10) {\n\t\t\t\t\tname: type.object.name.hi\n\t\t\t\t}\n\t\t\t}\n\t\t}\n\t ",
    );
    assert!(
        err.contains("Invalid colon after alias declaration"),
        "{err}"
    );
}

#[test]
fn parse_block() {
    let res = parse_str(
        "\n\t\t{\n\t\t\troot(func: uid( 0x0a)) {\n\t\t\t\ttype.object.name.es.419\n\t\t\t}\n\t\t}\n\t ",
    );
    assert_eq!(child_attrs(&res.query[0]), ["type.object.name.es.419"]);
}

#[test]
fn parse_schema() {
    let res = parse_str("\n\t\tschema (pred : name) {\n\t\t\tpred\n\t\t\ttype\n\t\t}\n\t ");
    let schema = res.schema.as_ref().expect("schema");
    assert_eq!(schema.predicates[0], "name");
    assert_eq!(schema.fields.len(), 2);
    assert_eq!(schema.fields[0], "pred");
    assert_eq!(schema.fields[1], "type");
}

#[test]
fn parse_schema_multi() {
    let res = parse_str("\n\t\tschema (pred : [name,hi]) {\n\t\t\tpred\n\t\t\ttype\n\t\t}\n\t ");
    let schema = res.schema.as_ref().expect("schema");
    assert_eq!(schema.predicates.len(), 2);
    assert_eq!(schema.predicates[0], "name");
    assert_eq!(schema.predicates[1], "hi");
    assert_eq!(schema.fields.len(), 2);
    assert_eq!(schema.fields[0], "pred");
    assert_eq!(schema.fields[1], "type");
}

#[test]
fn parse_schema_all() {
    let res = parse_str("\n\t\tschema {\n\t\t\tpred\n\t\t\ttype\n\t\t}\n\t ");
    let schema = res.schema.as_ref().expect("schema");
    assert!(schema.predicates.is_empty());
    assert_eq!(schema.fields.len(), 2);
    assert_eq!(schema.fields[0], "pred");
    assert_eq!(schema.fields[1], "type");
}

#[test]
fn parse_schema_with_comments() {
    let res = parse_str(
        "\n\t\tschema (pred : name) {\n\t\t\t#hi\n\t\t\tpred #bye\n\t\t\ttype\n\t\t}\n\t ",
    );
    let schema = res.schema.as_ref().expect("schema");
    assert_eq!(schema.predicates[0], "name");
    assert_eq!(schema.fields.len(), 2);
    assert_eq!(schema.fields[0], "pred");
    assert_eq!(schema.fields[1], "type");
}

#[test]
fn parse_schema_and_query() {
    let err = parse_err(
        "\n\t\tschema {\n\t\t\tpred\n\t\t\ttype\n\t\t}\n\t\tquery {\n\t\t\tme(func: uid( tomhanks)) {\n\t\t\t\tname\n\t\t\t\thometown\n\t\t\t}\n\t\t}\n\t ",
    );
    assert!(
        err.contains("Schema block is not allowed with query block"),
        "{err}"
    );
    let err = parse_err(
        "\n\t\tquery {\n\t\t\tme(func: uid( tomhanks)) {\n\t\t\t\tname\n\t\t\t\thometown\n\t\t\t}\n\t\t}\n\t\tschema {\n\t\t\tpred\n\t\t\ttype\n\t\t}\n\t ",
    );
    assert!(
        err.contains("Schema block is not allowed with query block"),
        "{err}"
    );
}

#[test]
fn parse_schema_type() {
    let res = parse_str("\n\t\tschema (type: Person) {\n\t\t}\n\t ");
    let schema = res.schema.as_ref().expect("schema");
    assert!(schema.predicates.is_empty());
    assert_eq!(schema.types.len(), 1);
    assert_eq!(schema.types[0], "Person");
    assert!(schema.fields.is_empty());
}

#[test]
fn parse_schema_type_multi() {
    let res = parse_str("\n\t\tschema (type: [Person, Animal]) {\n\t\t}\n\t ");
    let schema = res.schema.as_ref().expect("schema");
    assert!(schema.predicates.is_empty());
    assert_eq!(schema.types.len(), 2);
    assert_eq!(schema.types[0], "Person");
    assert_eq!(schema.types[1], "Animal");
    assert!(schema.fields.is_empty());
}

#[test]
fn parse_schema_special_chars() {
    let res = parse_str("\n\t\tschema (pred: [Person, <人物>]) {\n\t\t}\n\t ");
    let schema = res.schema.as_ref().expect("schema");
    assert_eq!(schema.predicates.len(), 2);
    assert!(schema.types.is_empty());
    assert_eq!(schema.predicates[0], "Person");
    assert_eq!(schema.predicates[1], "人物");
    assert!(schema.fields.is_empty());
}

#[test]
fn parse_schema_type_special_chars() {
    let res = parse_str("\n\t\tschema (type: [Person, <人物>]) {\n\t\t}\n\t ");
    let schema = res.schema.as_ref().expect("schema");
    assert!(schema.predicates.is_empty());
    assert_eq!(schema.types.len(), 2);
    assert_eq!(schema.types[0], "Person");
    assert_eq!(schema.types[1], "人物");
    assert!(schema.fields.is_empty());
}

#[test]
fn parse_schema_error() {
    let err = parse_err("\n\t\tschema () {\n\t\t\tpred\n\t\t\ttype\n\t\t}\n\t ");
    assert!(err.contains("Invalid schema block"), "{err}");
}

#[test]
fn parse_schema_error_multi() {
    let err = parse_err(
        "\n\t\tschema {\n\t\t\tpred\n\t\t\ttype\n\t\t}\n\t\tschema {\n\t\t\tpred\n\t\t\ttype\n\t\t}\n\t ",
    );
    assert!(err.contains("Only one schema block allowed"), "{err}");
}

#[test]
fn parse_mutation_error() {
    let err = datastreamdb_dql::parse_mutation(
        "\n\t\tmutation {\n\t\t\tset {\n\t\t\t\t<name> <is> <something> .\n\t\t\t\t<hometown> <is> <san/francisco> .\n\t\t\t}\n\t\t\tdelete {\n\t\t\t\t<name> <is> <something-else> .\n\t\t\t}\n\t\t}\n\t ",
    )
    .expect_err("must fail")
    .to_string();
    assert!(err.contains("Unexpected token: [mutation]"), "{err}");
}

#[test]
fn parse_mutation_error_2() {
    let err = datastreamdb_dql::parse_mutation(
        "\n\t\t\tset {\n\t\t\t\t<name> <is> <something> .\n\t\t\t\t<hometown> <is> <san/francisco> .\n\t\t\t}\n\t\t\tdelete {\n\t\t\t\t<name> <is> <something-else> .\n\t\t\t}\n\t ",
    )
    .expect_err("must fail")
    .to_string();
    assert!(err.contains("Invalid operation type: set"), "{err}");
}

#[test]
fn parse_mutation_and_query_with_comments() {
    parse_err(
        "\n\t# Mutation\n\t\tmutation {\n\t\t\t# Set block\n\t\t\tset {\n\t\t\t\t<name> <is> <something> .\n\t\t\t\t<hometown> <is> <san/francisco> .\n\t\t\t}\n\t\t\t# Delete block\n\t\t\tdelete {\n\t\t\t\t<name> <is> <something-else> .\n\t\t\t}\n\t\t}\n\t\t# Query starts here.\n\t\tquery {\n\t\t\tme(func: uid( 0x5)) { # now mention children\n\t\t\t\tname\t\t# Name\n\t\t\t\thometown # hometown of the person\n\t\t\t}\n\t\t}\n\t ",
    );
}

#[test]
fn parse_fragment_multi_query() {
    let res = parse_str(
        "\n\t{\n\t\tuser(func: uid(0x0a)) {\n\t\t\t...fragmenta,...fragmentb\n\t\t\tfriends {\n\t\t\t\tname\n\t\t\t}\n\t\t\t...fragmentc\n\t\t\thobbies\n\t\t\t...fragmentd\n\t\t}\n\n\t\tme(func: uid(0x01)) {\n\t\t\t...fragmenta\n\t\t\t...fragmentb\n\t\t}\n\t}\n\n\tfragment fragmenta {\n\t\tname\n\t}\n\n\tfragment fragmentb {\n\t\tid\n\t}\n\n\tfragment fragmentc {\n\t\tname\n\t}\n\n\tfragment fragmentd {\n\t\tid\n\t}\n",
    );
    assert_eq!(
        child_attrs(&res.query[0]),
        ["name", "id", "friends", "name", "hobbies", "id"]
    );
    assert_eq!(child_attrs(&res.query[1]), ["name", "id"]);
}

#[test]
fn parse_fragment_no_nesting() {
    let res = parse_str(
        "\n\tquery {\n\t\tuser(func: uid(0x0a)) {\n\t\t\t...fragmenta,...fragmentb\n\t\t\tfriends {\n\t\t\t\tname\n\t\t\t}\n\t\t\t...fragmentc\n\t\t\thobbies\n\t\t\t...fragmentd\n\t\t}\n\t}\n\n\tfragment fragmenta {\n\t\tname\n\t}\n\n\tfragment fragmentb {\n\t\tid\n\t}\n\n\tfragment fragmentc {\n\t\tname\n\t}\n\n\tfragment fragmentd {\n\t\tid\n\t}\n",
    );
    assert_eq!(
        child_attrs(&res.query[0]),
        ["name", "id", "friends", "name", "hobbies", "id"]
    );
}

#[test]
fn parse_fragment_nest_1() {
    let res = parse_str(
        "\n\tquery {\n\t\tuser(func: uid(0x0a)) {\n\t\t\t...fragmenta\n\t\t\tfriends {\n\t\t\t\tname\n\t\t\t}\n\t\t}\n\t}\n\n\tfragment fragmenta {\n\t\tid\n\t\t...fragmentb\n\t}\n\n\tfragment fragmentb {\n\t\thobbies\n\t}\n",
    );
    assert_eq!(child_attrs(&res.query[0]), ["id", "hobbies", "friends"]);
}

#[test]
fn parse_fragment_nest_2() {
    let res = parse_str(
        "\n\tquery {\n\t\tuser(func: uid(0x0a)) {\n\t\t\tfriends {\n\t\t\t\t...fragmenta\n\t\t\t}\n\t\t}\n\t}\n\tfragment fragmenta {\n\t\tname\n\t\t...fragmentb\n\t}\n\tfragment fragmentb {\n\t\tnickname\n\t}\n",
    );
    assert_eq!(child_attrs(&res.query[0]), ["friends"]);
    assert_eq!(child_attrs(&res.query[0].children[0]), ["name", "nickname"]);
}

#[test]
fn parse_fragment_cycle() {
    let err = parse_err(
        "\n\tquery {\n\t\tuser(func: uid(0x0a)) {\n\t\t\t...fragmenta\n\t\t}\n\t}\n\tfragment fragmenta {\n\t\tname\n\t\t...fragmentb\n\t}\n\tfragment fragmentb {\n\t\t...fragmentc\n\t}\n\tfragment fragmentc {\n\t\tid\n\t\t...fragmenta\n\t}\n",
    );
    assert!(err.contains("Cycle detected"), "{err}");
}

#[test]
fn parse_fragment_missing() {
    let err = parse_err(
        "\n\tquery {\n\t\tuser(func: uid(0x0a)) {\n\t\t\t...fragmenta\n\t\t}\n\t}\n\tfragment fragmentb {\n\t\t...fragmentc\n\t}\n\tfragment fragmentc {\n\t\tid\n\t\t...fragmenta\n\t}\n",
    );
    assert!(err.contains("Missing fragment: fragmenta"), "{err}");
}

#[test]
fn parse_string_var_in_filter() {
    let res = parse_str(
        "\n\t\tquery versions($version: string = \"v0.7.3/beta\")\n\t\t{\n\t\t\tversions(func:eq(type, \"version\"))\n\t\t\t{\n\t\t\t\tversions @filter(eq(version_number, $version))\n\t\t\t\t{\n\t\t\t\t\tversion_number\n\t\t\t\t}\n\t\t\t}\n\t\t}\n\t ",
    );
    let func = res.query[0].children[0]
        .filter
        .as_ref()
        .expect("filter")
        .func
        .as_ref()
        .expect("func");
    assert_eq!(func.args[0].value, "v0.7.3/beta");
}

#[test]
fn parse_var_in_facet() {
    let res = parse_str(
        "\nquery works($since: string = \"2018\") {\n  q(func: has(works_in)) @cascade {\n    name\n    works_in @facets @facets(gt(since, $since)) {\n      name\n    }\n  }\n}",
    );
    let func = res.query[0].children[1]
        .facets_filter
        .as_ref()
        .expect("facets filter")
        .func
        .as_ref()
        .expect("func");
    assert_eq!(func.args[0].value, "2018");
}

#[test]
fn parse_variables_error_1() {
    let err = parse_err(
        "\n\tquery testQuery($a: string, $b: int!){\n\t\t\troot(func: uid( 0x0a)) {\n\t\t\t\ttype.object.name.es-419\n\t\t\t}\n\t\t}\n\t ",
    );
    assert!(
        err.contains("Unrecognized character in lexText: U+0034 '4'"),
        "{err}"
    );
}
