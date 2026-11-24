//! Parser tests: filters, `similar_to`, geo functions, count, comments, and IRIs (port of Go's `parser_test.go`, part 3 of 8).
//!
//! Split from `parser_tests.rs` to honor the 1000-line file rule; shared
//! helpers live in [`common`].

#[path = "common/mod.rs"]
mod common;

use std::collections::HashMap;

use common::{child_attrs, parse_err, parse_str};
use datastreamdb_dql::parse;

#[test]
fn parse_filter_root() {
    let res = parse_str(
        "\n\tquery {\n\t\tme(func:anyofterms(abc, \"Abc\")) @filter(allofterms(name, \"alice\")) {\n\t\t\tfriends @filter() {\n\t\t\t\tname @filter(namefilter(name, \"a\"))\n\t\t\t}\n\t\t\tgender @filter(eq(g, \"a\")),age @filter(neq(a, \"b\"))\n\t\t\thometown\n\t\t}\n\t}\n",
    );
    assert!(res.query[0].filter.is_some());
    assert_eq!(
        res.query[0].filter.as_ref().expect("filter").debug_string(),
        "(allofterms name \"alice\")"
    );
    assert_eq!(
        child_attrs(&res.query[0]),
        ["friends", "gender", "age", "hometown"]
    );
    assert_eq!(child_attrs(&res.query[0].children[0]), ["name"]);
    assert!(res.query[0].children[0].filter.is_none());
    assert_eq!(
        res.query[0].children[1]
            .filter
            .as_ref()
            .expect("filter")
            .debug_string(),
        "(eq g \"a\")"
    );
    assert_eq!(
        res.query[0].children[2]
            .filter
            .as_ref()
            .expect("filter")
            .debug_string(),
        "(neq a \"b\")"
    );
    assert_eq!(
        res.query[0].children[0].children[0]
            .filter
            .as_ref()
            .expect("filter")
            .debug_string(),
        "(namefilter name \"a\")"
    );
}

#[test]
fn parse_func_nested() {
    let res = parse_str(
        "\n\tquery {\n\t\tme(func: gt(count(friend), 10)) {\n\t\t\tfriends @filter() {\n\t\t\t\tname\n\t\t\t}\n\t\t\thometown\n\t\t}\n\t}\n",
    );
    let func = res.query[0].func.as_ref().expect("func");
    assert_eq!(func.name, "gt");
    assert_eq!(func.args[0].value, "10");
    assert!(func.is_count);
}

#[test]
fn parse_func_nested_2() {
    let res = parse_str(
        "\n\tquery {\n\t\tvar(func:uid(1)) {\n\t\t\ta as name\n\t\t}\n\t\tme(func: eq(name, val(a))) {\n\t\t\tfriends @filter() {\n\t\t\t\tname\n\t\t\t}\n\t\t\thometown\n\t\t}\n\t}\n",
    );
    let func = res.query[1].func.as_ref().expect("func");
    assert_eq!(func.name, "eq");
    assert_eq!(func.args[0].value, "a");
    assert!(func.args[0].is_value_var);
    assert!(!func.is_count);
}

#[test]
fn parse_filter_root_2() {
    let res = parse_str(
        "\n\tquery {\n\t\tme(func:anyofterms(abc, \"Abc\")) @filter(gt(count(friends), 10)) {\n\t\t\tfriends @filter() {\n\t\t\t\tname\n\t\t\t}\n\t\t\thometown\n\t\t}\n\t}\n",
    );
    assert_eq!(
        res.query[0].filter.as_ref().expect("filter").debug_string(),
        "(gt count(friends) \"10\")"
    );
    assert_eq!(child_attrs(&res.query[0]), ["friends", "hometown"]);
    assert_eq!(child_attrs(&res.query[0].children[0]), ["name"]);
    assert!(res.query[0].children[0].filter.is_none());
}

#[test]
fn parse_filter_root_error_2() {
    let err = parse_err(
        "\n\tquery {\n\t\tme(func:anyofterms(abc, \"Abc\")) @filter(gt(count(friends), sum(friends))) {\n\t\t\tfriends @filter() {\n\t\t\t\tname\n\t\t\t}\n\t\t\thometown\n\t\t}\n\t}\n",
    );
    assert!(
        err.contains("Multiple functions as arguments not allowed"),
        "{err}"
    );
}

#[test]
fn parse_filter_simplest() {
    let res = parse_str(
        "\n\tquery {\n\t\tme(func: uid(0x0a)) {\n\t\t\tfriends @filter() {\n\t\t\t\tname @filter(namefilter(name, \"a\"))\n\t\t\t}\n\t\t\tgender @filter(eq(g, \"a\")),age @filter(neq(a, \"b\"))\n\t\t\thometown\n\t\t}\n\t}\n",
    );
    assert_eq!(
        child_attrs(&res.query[0]),
        ["friends", "gender", "age", "hometown"]
    );
    assert_eq!(child_attrs(&res.query[0].children[0]), ["name"]);
    assert!(res.query[0].children[0].filter.is_none());
    assert_eq!(
        res.query[0].children[1]
            .filter
            .as_ref()
            .expect("filter")
            .debug_string(),
        "(eq g \"a\")"
    );
    assert_eq!(
        res.query[0].children[2]
            .filter
            .as_ref()
            .expect("filter")
            .debug_string(),
        "(neq a \"b\")"
    );
    assert_eq!(
        res.query[0].children[0].children[0]
            .filter
            .as_ref()
            .expect("filter")
            .debug_string(),
        "(namefilter name \"a\")"
    );
}

#[test]
fn parse_filter_op() {
    let res = parse_str(
        "\n\tquery {\n\t\tme(func: uid(0x0a)) {\n\t\t\tfriends @filter(a(aa, \"aaa\") or b(bb, \"bbb\")\n\t\t\tand c(cc, \"ccc\")) {\n\t\t\t\tname\n\t\t\t}\n\t\t\tgender,age\n\t\t\thometown\n\t\t}\n\t}\n",
    );
    assert_eq!(
        child_attrs(&res.query[0]),
        ["friends", "gender", "age", "hometown"]
    );
    assert_eq!(child_attrs(&res.query[0].children[0]), ["name"]);
    assert_eq!(
        res.query[0].children[0]
            .filter
            .as_ref()
            .expect("filter")
            .debug_string(),
        r#"(OR (a aa "aaa") (AND (b bb "bbb") (c cc "ccc")))"#,
    );
}

#[test]
fn parse_filter_op_error_1() {
    let err = parse_err(
        "\n\tquery {\n\t\tme(func: uid(0x0a)) {\n\t\t\tfriends @filter(a(aa \"aaa\") or b(b \"bbb\")) {\n\t\t\t\tname\n\t\t\t}\n\t\t\tgender,age\n\t\t\thometown\n\t\t}\n\t}\n",
    );
    assert!(
        err.contains("Expected comma or language but got: \"aaa\""),
        "{err}"
    );
}

#[test]
fn parse_filter_op_no_error_2() {
    parse_str(
        "\n\tquery {\n\t\tme(func: uid(0x0a)) {\n\t\t\tfriends @filter(a(aa, \"aaa\") or b(b, \"bbb\")) {\n\t\t\t\tname\n\t\t\t}\n\t\t\tgender,age\n\t\t\thometown\n\t\t}\n\t}\n",
    );
}

#[test]
fn parse_filter_op_error_3() {
    let err = parse_err(
        "\n\t\tquery {\n\t\tme(func: uid(0x0a)) {\n\t\t\tfriends @filter(a(aa, \"aaa\") or b(b, \"bbb\") and) {\n\t\t\t\tname\n\t\t\t}\n\t\t\tgender,age\n\t\t\thometown\n\t\t}\n\t}\n",
    );
    assert!(err.contains("Invalid filter statement"), "{err}");
}

#[test]
fn parse_filter_op_not_1() {
    let res = parse_str(
        "\n\tquery {\n\t\tme(func: uid(0x0a)) {\n\t\t\tfriends @filter(not a(aa, \"aaa\")) {\n\t\t\t\tname\n\t\t\t}\n\t\t\tgender,age\n\t\t\thometown\n\t\t}\n\t}\n",
    );
    assert_eq!(
        child_attrs(&res.query[0]),
        ["friends", "gender", "age", "hometown"]
    );
    assert_eq!(child_attrs(&res.query[0].children[0]), ["name"]);
    assert_eq!(
        res.query[0].children[0]
            .filter
            .as_ref()
            .expect("filter")
            .debug_string(),
        "(NOT (a aa \"aaa\"))"
    );
}

#[test]
fn parse_filter_op_not_2() {
    let res = parse_str(
        "\n\tquery {\n\t\tme(func: uid(0x0a)) {\n\t\t\tfriends @filter(not(a(aa, \"aaa\") or (b(bb, \"bbb\"))) and c(cc, \"ccc\")) {\n\t\t\t\tname\n\t\t\t}\n\t\t\tgender,age\n\t\t\thometown\n\t\t}\n\t}\n",
    );
    assert_eq!(
        child_attrs(&res.query[0]),
        ["friends", "gender", "age", "hometown"]
    );
    assert_eq!(child_attrs(&res.query[0].children[0]), ["name"]);
    assert_eq!(
        res.query[0].children[0]
            .filter
            .as_ref()
            .expect("filter")
            .debug_string(),
        "(AND (NOT (OR (a aa \"aaa\") (b bb \"bbb\"))) (c cc \"ccc\"))"
    );
}

#[test]
fn parse_filter_op_2() {
    let res = parse_str(
        "\n\tquery {\n\t\tme(func: uid(0x0a)) {\n\t\t\tfriends @filter((a(aa, \"aaa\") Or b(bb, \"bbb\"))\n\t\t\t and c(cc, \"ccc\")) {\n\t\t\t\tname\n\t\t\t}\n\t\t\tgender,age\n\t\t\thometown\n\t\t}\n\t}\n",
    );
    assert_eq!(
        child_attrs(&res.query[0]),
        ["friends", "gender", "age", "hometown"]
    );
    assert_eq!(child_attrs(&res.query[0].children[0]), ["name"]);
    assert_eq!(
        res.query[0].children[0]
            .filter
            .as_ref()
            .expect("filter")
            .debug_string(),
        "(AND (OR (a aa \"aaa\") (b bb \"bbb\")) (c cc \"ccc\"))"
    );
}

#[test]
fn parse_filter_brac() {
    let res = parse_str(
        "\n\tquery {\n\t\tme(func: uid(0x0a)) {\n\t\t\tfriends @filter(  a(name, \"hello\") or b(name, \"world\", \"is\") and (c(aa, \"aaa\") or (d(dd, \"haha\") or e(ee, \"aaa\"))) and f(ff, \"aaa\")){\n\t\t\t\tname\n\t\t\t}\n\t\t\tgender,age\n\t\t\thometown\n\t\t}\n\t}\n",
    );
    assert_eq!(
        child_attrs(&res.query[0]),
        ["friends", "gender", "age", "hometown"]
    );
    assert_eq!(child_attrs(&res.query[0].children[0]), ["name"]);
    assert_eq!(
        res.query[0].children[0]
            .filter
            .as_ref()
            .expect("filter")
            .debug_string(),
        "(OR (a name \"hello\") (AND (AND (b name \"world\" \"is\") (OR (c aa \"aaa\") (OR (d dd \"haha\") (e ee \"aaa\")))) (f ff \"aaa\")))"
    );
}

#[test]
fn parse_filter_unbalanced_brac() {
    let err = parse_err(
        "\n\tquery {\n\t\tme(func: uid(0x0a)) {\n\t\t\tfriends @filter(  () {\n\t\t\t\tname\n\t\t\t}\n\t\t\tgender,age\n\t\t\thometown\n\t\t}\n\t}\n",
    );
    assert!(err.contains("Unclosed Brackets"), "{err}");
}

#[test]
fn parse_similar_to_named_params() {
    let res = parse_str(
        "{\n\t\tq(func: similar_to(voptions, 4, \"[0,0]\", distance_threshold: 1.5, ef: 12)) {\n\t\t\tuid\n\t\t}\n\t}",
    );
    assert_eq!(res.query.len(), 1);
    let func = res.query[0].func.as_ref().expect("func");
    assert_eq!(func.name, "similar_to");
    assert_eq!(func.attr, "voptions");
    assert_eq!(func.args[0].value, "4");
    assert_eq!(func.args[1].value, "[0,0]");
    assert_eq!(func.args.len(), 6);
    assert_eq!(func.args[2].value, "distance_threshold");
    assert_eq!(func.args[3].value, "1.5");
    assert_eq!(func.args[4].value, "ef");
    assert_eq!(func.args[5].value, "12");
}

#[test]
fn parse_similar_to_three_args() {
    let res =
        parse_str("{\n\t\tq(func: similar_to(voptions, 4, \"[0,0]\")) {\n\t\t\tuid\n\t\t}\n\t}");
    let func = res.query[0].func.as_ref().expect("func");
    assert_eq!(func.name, "similar_to");
    assert_eq!(func.args.len(), 2);
}

#[test]
fn parse_similar_to_rejects_object_literal_syntax() {
    let err = parse_err(
        "{\n\t\tq(func: similar_to(voptions, 4, \"[0,0]\", {ef: 12})) {\n\t\t\tuid\n\t\t}\n\t}",
    );
    assert!(
        err.contains("Unrecognized character inside a func: U+007B '{'"),
        "{err}"
    );
}

#[test]
fn parse_similar_to_with_query_variable() {
    let mut variables = HashMap::new();
    variables.insert("$eff".to_owned(), "64".to_owned());
    let res = parse(datastreamdb_dql::Request {
        text: "query test($eff: int) {\n\t\tq(func: similar_to(voptions, 4, \"[0,0]\", ef: $eff)) {\n\t\t\tuid\n\t\t}\n\t}".to_owned(),
        variables,
    })
    .expect("parse");
    let func = res.query[0].func.as_ref().expect("func");
    assert_eq!(func.name, "similar_to");
    assert_eq!(func.args.len(), 4);
    assert_eq!(func.args[2].value, "ef");
    assert_eq!(func.args[3].value, "64");
}

#[test]
fn parse_similar_to_rejects_legacy_string_options_syntax() {
    let err = parse_err(
        "{\n\t\tq(func: similar_to(voptions, 4, \"[0,0]\", \"ef=64,distance_threshold=0.45\")) {\n\t\t\tuid\n\t\t}\n\t}",
    );
    assert!(
        err.contains("Expected named parameter in similar_to options"),
        "{err}"
    );
}

#[test]
fn parse_similar_to_unknown_option() {
    let err = parse_err(
        "{\n\t\tq(func: similar_to(voptions, 4, \"[0,0]\", foo: 5)) {\n\t\t\tuid\n\t\t}\n\t}",
    );
    assert!(err.contains("Unknown option"), "{err}");
    assert!(err.contains("foo"), "{err}");
}

#[test]
fn parse_similar_to_duplicate_option() {
    let err = parse_err(
        "{\n\t\tq(func: similar_to(voptions, 4, \"[0,0]\", ef: 10, ef: 20)) {\n\t\t\tuid\n\t\t}\n\t}",
    );
    assert!(err.contains("Duplicate key"), "{err}");
    assert!(err.contains("ef"), "{err}");
}

#[test]
fn parse_non_similar_to_with_brace() {
    let err = parse_err("{\n\t\tq(func: eq(name, {value: \"test\"})) {\n\t\t\tuid\n\t\t}\n\t}");
    assert!(
        err.contains("Unrecognized character inside a func: U+007B '{'"),
        "{err}"
    );
}

#[test]
fn parse_filter_geo_1() {
    let res = parse_str(
        "\n\tquery {\n\t\tme(func: uid(0x0a)) {\n\t\t\tfriends @filter(near(loc, [-1.12 , 2.0123 ], 100.123 )) {\n\t\t\t\tname\n\t\t\t}\n\t\t\tgender,age\n\t\t\thometown\n\t\t}\n\t}\n",
    );
    let func = res.query[0].children[0]
        .filter
        .as_ref()
        .expect("filter")
        .func
        .as_ref()
        .expect("func");
    assert_eq!(func.args[0].value, "[-1.12,2.0123]");
    assert_eq!(func.args[1].value, "100.123");
    assert!(!func.args[0].is_value_var);
    assert!(!func.args[1].is_value_var);
}

#[test]
fn parse_filter_geo_2() {
    let res = parse_str(
        "\n\tquery {\n\t\tme(func: uid(0x0a)) {\n\t\t\tfriends @filter(within(loc, [[11.2 , -2.234 ], [ -31.23, 4.3214] , [5.312, 6.53]] )) {\n\t\t\t\tname\n\t\t\t}\n\t\t\tgender,age\n\t\t\thometown\n\t\t}\n\t}\n",
    );
    let func = res.query[0].children[0]
        .filter
        .as_ref()
        .expect("filter")
        .func
        .as_ref()
        .expect("func");
    assert_eq!(
        func.args[0].value,
        "[[11.2,-2.234],[-31.23,4.3214],[5.312,6.53]]"
    );
}

#[test]
fn parse_filter_geo_3() {
    let err = parse_err(
        "\n\tquery {\n\t\tme(func: uid(0x0a)) {\n\t\t\tfriends @filter(near(loc, [[1 , 2 ], [[3, 4] , [5, 6]] )) {\n\t\t\t\tname\n\t\t\t}\n\t\t\tgender,age\n\t\t\thometown\n\t\t}\n\t}\n",
    );
    assert!(err.contains("Found invalid item"), "{err}");
}

#[test]
fn parse_filter_geo_4() {
    let err = parse_err(
        "\n\tquery {\n\t\tme(func: uid(0x0a)) {\n\t\t\tfriends @filter(near(loc, [[1 , 2 ], [3, 4] , [5, 6]]] )) {\n\t\t\t\tname\n\t\t\t}\n\t\t\tgender,age\n\t\t\thometown\n\t\t}\n\t}\n",
    );
    assert!(err.contains("Expected right round or comma"), "{err}");
    assert!(err.contains("\"]\""), "{err}");
}

#[test]
fn parse_filter_empty_argument() {
    let err = parse_err(
        "\n\tquery {\n\t\tme(func: uid(0x0a)) {\n\t\t\tfriends @filter(allofterms(name,,)) {\n\t\t\t\tname\n\t\t\t}\n\t\t\tgender,age\n\t\t\thometown\n\t\t}\n\t}\n",
    );
    assert!(err.contains("Consecutive commas not allowed"), "{err}");
}

#[test]
fn parse_filter_unknown_directive_error_1() {
    let err = parse_err(
        "\n\tquery {\n\t\tme(func: uid(0x0a)) {\n\t\t\tfriends @filtererr {\n\t\t\t\tname\n\t\t\t}\n\t\t\tgender,age\n\t\t\thometown\n\t\t}\n\t}",
    );
    assert!(
        err.contains("Cannot have children for attr: friends with lang tags:"),
        "{err}"
    );
}

#[test]
fn parse_filter_unknown_directive_error_2() {
    let err = parse_err(
        "\n\tquery {\n\t\tme(func: uid(0x0a)) {\n\t\t\tfriends @filtererr ()\n\t\t\tgender,age\n\t\t\thometown\n\t\t}\n\t}",
    );
    assert!(err.contains("Unknown directive [filtererr]"), "{err}");
}

#[test]
fn parse_generator_error_1() {
    let err = parse_err(
        "{\n\t\tme(allofterms(name, \"barack\")) {\n\t\t\tfriends {\n\t\t\t\tname\n\t\t\t}\n\t\t\tgender,age\n\t\t\thometown\n\t\t\tcount(friends)\n\t\t}\n\t}\n",
    );
    assert!(err.contains("Got invalid keyword: allofterms"), "{err}");
}

#[test]
fn parse_generator_error_2() {
    let err = parse_err(
        "{\n\t\tme(func: allofterms(name, \"barack\")) {\n\t\t\tfriends(all: 5) {\n\t\t\t\tname\n\t\t\t}\n\t\t\tgender,age\n\t\t\thometown\n\t\t\tcount(friends)\n\t\t}\n\t}\n",
    );
    assert!(err.contains("Got invalid keyword: all"), "{err}");
}

#[test]
fn parse_quoted_function_attribute_error() {
    let err = parse_err(
        r#"{
		me(func: allofterms("name", "barack")) {
			friends {
				name
			}
			gender,age
			hometown
			count(friends)
		}
	}
"#,
    );
    assert!(
        err.contains("Attribute in function must not be quoted"),
        "{err}"
    );
}

#[test]
fn parse_count_as_func_multiple() {
    let res = parse_str(
        r"{
		me(func: uid(1)) {
			count(friends), count(relatives)
			count(classmates)
			gender,age
			hometown
		}
	}
",
    );
    assert_eq!(res.query[0].children.len(), 6);
    assert!(res.query[0].children[0].is_count);
    assert_eq!(res.query[0].children[0].attr, "friends");
    assert!(res.query[0].children[1].is_count);
    assert_eq!(res.query[0].children[1].attr, "relatives");
    assert!(res.query[0].children[2].is_count);
    assert_eq!(res.query[0].children[2].attr, "classmates");
}

#[test]
fn parse_count_as_func_multiple_error() {
    let err = parse_err(
        r"{
		me(func: uid(1)) {
			count(friends, relatives
			classmates)
			gender,age
			hometown
		}
	}
",
    );
    assert!(
        err.contains("Multiple predicates not allowed in single count"),
        "{err}"
    );
}

#[test]
fn parse_count_as_func() {
    let res = parse_str(
        r"{
		me(func: uid(1)) {
			count(friends)
			gender,age
			hometown
		}
	}
",
    );
    assert!(res.query[0].children[0].is_count);
    assert_eq!(res.query[0].children.len(), 4);
}

#[test]
fn parse_count_error_1() {
    let err = parse_err(
        r"{
		me(func: uid(1)) {
			count(friends
			gender,age
			hometown
		}
	}
",
    );
    assert!(err.contains("Unclosed Brackets"), "{err}");
}

#[test]
fn parse_count_error_2() {
    let err = parse_err(
        r"{
		me(func: uid(1)) {
			count((friends)
			gender,age
			hometown
		}
	}
",
    );
    assert!(err.contains("Unclosed Brackets"), "{err}");
}

#[test]
fn parse_check_pwd() {
    let res = parse_str(
        r#"{
		me(func: uid(1)) {
			checkpwd(password, "123456")
			hometown
		}
	}
"#,
    );
    let func = res.query[0].children[0].func.as_ref().expect("func");
    assert_eq!(func.name, "checkpwd");
    assert_eq!(func.args[0].value, "123456");
    assert_eq!(res.query[0].children[0].attr, "password");
}

#[test]
fn parse_comments() {
    parse_str(
        r#"
	# Something
	{
		me(func:allofterms(name, "barack")) {
			friends {
				name
			} # Something
			gender,age
			hometown
		}
	}
"#,
    );
}

#[test]
fn parse_comments_1() {
    parse_str(
        r#"{
		#Something
		me(func:allofterms(name, "barack")) {
			friends {
				name  # Name of my friend
			}
			gender,age
			hometown
		}
	}
"#,
    );
}

#[test]
fn parse_generator() {
    parse_str(
        r#"{
		me(func:allofterms(name, "barack")) {
			friends {
				name
			}
			gender,age
			hometown
		}
	}
"#,
    );
}

#[test]
fn parse_iri_ref() {
    let res = parse_str(
        r#"{
		me(func: uid( 0x1)) {
			<http://verygood.com/what/about/you>
			friends @filter(allofterms(<http://verygood.com/what/about/you>,
				"good better bad")){
				name
			}
			gender,age
			hometown
		}
	}"#,
    );
    assert_eq!(res.query[0].children.len(), 5);
    assert_eq!(
        res.query[0].children[0].attr,
        "http://verygood.com/what/about/you"
    );
    assert_eq!(
        res.query[0].children[1]
            .filter
            .as_ref()
            .expect("filter")
            .debug_string(),
        r#"(allofterms http://verygood.com/what/about/you "good better bad")"#,
    );
}

#[test]
fn parse_iri_ref_2() {
    let res = parse_str(
        r#"{
		me(func:anyofterms(<http://helloworld.com/how/are/you>, "good better bad")) {
			<http://verygood.com/what/about/you>
			friends @filter(allofterms(<http://verygood.com/what/about/you>,
				"good better bad")){
				name
			}
		}
	}"#,
    );
    assert_eq!(res.query[0].children.len(), 2);
    assert_eq!(
        res.query[0].children[0].attr,
        "http://verygood.com/what/about/you"
    );
    assert_eq!(
        res.query[0].children[1]
            .filter
            .as_ref()
            .expect("filter")
            .debug_string(),
        r#"(allofterms http://verygood.com/what/about/you "good better bad")"#,
    );
    assert_eq!(
        res.query[0].func.as_ref().expect("func").attr,
        "http://helloworld.com/how/are/you"
    );
}

#[test]
fn parse_iri_ref_space() {
    let err = parse_err(
        r"{
		me(func: uid( <http://helloworld.com/how/are/ you>)) {
		}
	      }",
    );
    assert!(
        err.contains("Unexpected character ' ' while parsing IRI"),
        "{err}"
    );
}

#[test]
fn parse_iri_ref_invalid_char() {
    let err = parse_err(
        r"{
		me(func: uid( <http://helloworld.com/how/are/^you>)) {
		}
	      }",
    );
    assert!(
        err.contains("Unexpected character '^' while parsing IRI"),
        "{err}"
    );
}
