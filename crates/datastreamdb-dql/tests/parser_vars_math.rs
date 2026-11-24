//! Parser tests: query variables, math expressions, and aggregations (port of Go's `parser_test.go`, part 1 of 8).
//!
//! Split from `parser_tests.rs` to honor the 1000-line file rule; shared
//! helpers live in [`common`].

#[path = "common/mod.rs"]
mod common;

use std::collections::HashMap;

use common::{math_debug, parse_err, parse_str};
use datastreamdb_dql::{Request, parse};

#[test]
fn parse_math_subs() {
    // Port of TestParseMathSubs.
    let q =
        "query test($a: int) {\nq(func: uid(0x1)) {\n\tx as count(uid)\n\tp : math(x + $a)\n}\n}";
    let mut variables = HashMap::new();
    variables.insert("$a".to_owned(), "3".to_owned());
    let gq = parse(Request {
        text: q.to_owned(),
        variables,
    })
    .expect("parse");
    let constant = &gq.query[0].children[1]
        .math_exp
        .as_ref()
        .expect("math")
        .child[1]
        .constant;
    assert_eq!(constant.tid, datastreamdb_types::TypeId::Int);
    assert_eq!(constant.value, datastreamdb_types::Value::Int(3));

    let mut variables = HashMap::new();
    variables.insert("$a".to_owned(), "3.3".to_owned());
    let err = parse(Request {
        text: q.to_owned(),
        variables,
    })
    .expect_err("must fail")
    .to_string();
    assert!(err.contains("Expected an int but got 3.3"), "{err}");
}

#[test]
fn parse_count_val_error() {
    let err = parse_err(
        "\n{\n  me(func: uid(1)) {\n    Upvote {\n       u as Author\n    }\n    count(val(u))\n  }\n}\n",
    );
    assert!(err.contains("Count of a variable is not allowed"), "{err}");
}

#[test]
fn parse_query_named_query() {
    parse_str("\nquery works() {\n  q(func: has(name)) {\n    name\n  }\n}\n");
}

#[test]
fn parse_query_name_query_without_brackets() {
    parse_str("\nquery works {\n  q(func: has(name)) {\n    name\n  }\n}\n");
}

#[test]
fn parse_var_error() {
    let err = parse_err(
        "\n\t{\n\t\tvar(func: uid(0x0a)) {\n\t\t\ta as friends\n\t\t}\n\n\t\tme(func: uid(a)) {\n\t\t\tuid(a)\n\t\t}\n\t}\n",
    );
    assert!(err.contains("Cannot do uid() of a variable"), "{err}");
}

#[test]
fn duplicate_query_aliases_error() {
    let err = parse_err(
        "\n{\n  find_michael(func: eq(name@., \"Michael\")) {\n    uid\n    name@.\n    age\n  }\n    find_michael(func: eq(name@., \"Amit\")) {\n      uid\n      name@.\n    }\n}",
    );
    assert!(!err.is_empty());
    let err = parse_err(
        "\n{\n  find_michael(func: eq(name@., \"Michael\")) {\n    uid\n    name@.\n    age\n  }\n}\nquery {find_michael(func: eq(name@., \"Amit\")) {\n      uid\n      name@.\n    }\n}\n",
    );
    assert!(!err.is_empty());
    parse_str(
        "\n{\n path as shortest(from: 0x1, to: 0x4) {\n  friend\n }\n path2 as shortest(from: 0x2, to: 0x3) {\n    friend\n }\n pathQuery1(func: uid(path)) {\n   name\n }\n pathQuery2(func: uid(path2)) {\n   name\n }\n\n}",
    );
}

#[test]
fn parse_query_list_pred_1() {
    parse_str(
        "\n\t{\n\t\tvar(func: uid( 0x0a)) {\n\t\t\tfriends {\n\t\t\t\texpand(_all_)\n\t\t\t}\n\t\t}\n\t}\n",
    );
}

#[test]
fn parse_query_expand_forward() {
    let err = parse_err(
        "\n\t{\n\t\tvar(func: uid( 0x0a)) {\n\t\t\tfriends {\n\t\t\t\texpand(_forward_)\n\t\t\t}\n\t\t}\n\t}\n",
    );
    assert!(
        err.contains("Argument _forward_ has been deprecated"),
        "{err}"
    );
}

#[test]
fn parse_query_expand_reverse() {
    let err = parse_err(
        "\n\t{\n\t\tvar(func: uid( 0x0a)) {\n\t\t\tfriends {\n\t\t\t\texpand(_reverse_)\n\t\t\t}\n\t\t}\n\t}\n",
    );
    assert!(
        err.contains("Argument _reverse_ has been deprecated"),
        "{err}"
    );
}

#[test]
fn parse_query_expand_type() {
    parse_str(
        "\n\t{\n\t\tvar(func: uid( 0x0a)) {\n\t\t\tfriends {\n\t\t\t\texpand(Person)\n\t\t\t}\n\t\t}\n\t}\n",
    );
}

#[test]
fn parse_query_expand_multiple_types() {
    parse_str(
        "\n\t{\n\t\tvar(func: uid( 0x0a)) {\n\t\t\tfriends {\n\t\t\t\texpand(Person, Relative)\n\t\t\t}\n\t\t}\n\t}\n",
    );
}

#[test]
fn parse_query_alias_list_pred() {
    let res = parse_str("\n\t{\n\t\tme(func: uid(0x0a)) {\n\t\t\tpred: some_pred\n\t\t}\n\t}\n");
    assert_eq!(res.query[0].children[0].alias, "pred");
    assert_eq!(res.query[0].children[0].attr, "some_pred");
}

#[test]
fn parse_query_count_list_pred() {
    let res = parse_str("\n\t{\n\t\tme(func: uid(0x0a)) {\n\t\t\tcount(some_pred)\n\t\t}\n\t}\n");
    assert!(res.query[0].children[0].is_count);
    assert_eq!(res.query[0].children[0].attr, "some_pred");
}

#[test]
fn parse_query_list_pred_2() {
    parse_str(
        "\n\t{\n\t\tvar(func: uid(0x0a)) {\n\t\t\tf as friends\n\t\t}\n\n\t\tvar(func: uid(f)) {\n\t\t\tl as some_pred\n\t\t}\n\n\t\tvar(func: uid( 0x0a)) {\n\t\t\tfriends {\n\t\t\t\texpand(val(l))\n\t\t\t}\n\t\t}\n\t}\n",
    );
}

#[test]
fn parse_query_list_pred_multi_var_error() {
    let err = parse_err(
        "\n\t{\n\t\tvar(func: uid(0x0a)) {\n\t\t\tf as friends\n\t\t}\n\n\t\tvar(func: uid(f)) {\n\t\t\tl as some_pred\n\t\t\tfriend {\n\t\t\t\tg as some_pred\n\t\t\t}\n\t\t}\n\n\t\tvar(func: uid( 0x0a)) {\n\t\t\tfriends {\n\t\t\t\texpand(val(l, g))\n\t\t\t}\n\t\t}\n\t}\n",
    );
    assert!(err.contains("Exactly one variable expected"), "{err}");
}

#[test]
fn parse_query_with_no_var_val_error() {
    parse_str(
        "\n\t{\n\t\tme(func: uid(), orderasc: val(n)) {\n\t\t\tname\n\t\t}\n\n\t\tvar(func: uid(0x0a)) {\n\t\t\tfriends {\n\t\t\t\tn AS name\n\t\t\t}\n\t\t}\n\t}\n",
    );
}

#[test]
fn parse_query_agg_child() {
    let err = parse_err(
        "\n\t{\n\t\tvar(func: uid(0x0a)) {\n\t\t\tmin(friends) {\n\t\t\t\tname\n\t\t\t}\n\t\t}\n\t}\n",
    );
    assert!(
        err.contains("Only variables allowed in aggregate functions"),
        "{err}"
    );
}

#[test]
fn parse_query_with_xid_error() {
    let err = parse_err(
        "\n{\n      me(func: uid(aliceInWonderland)) {\n        type\n        writtenIn\n        name\n        character {\n                name\n        }\n        author {\n                name\n                born\n                died\n        }\n      }\n    }",
    );
    assert!(
        err.contains("Some variables are used but not defined"),
        "{err}"
    );
    assert!(err.contains("Used:[aliceInWonderland]"), "{err}");
}

#[test]
fn parse_query_with_multi_var_val_error() {
    let err = parse_err(
        "\n\t{\n\t\tme(func: uid(L), orderasc: val(n, d)) {\n\t\t\tname\n\t\t}\n\n\t\tvar(func: uid(0x0a)) {\n\t\t\tL AS friends {\n\t\t\t\tn AS name\n\t\t\t\td as age\n\t\t\t}\n\t\t}\n\t}\n",
    );
    assert!(
        err.contains("Expected only one variable but got: 2"),
        "{err}"
    );
}

#[test]
fn parse_query_with_var_val_agg_err() {
    let err = parse_err(
        "\n\t{\n\t\tme(func: uid(L), orderasc: val(c)) {\n\t\t\tname\n\t\t}\n\n\t\tvar(func: uid(0x0a)) {\n\t\t\tL as friends {\n\t\t\t\ta as age\n\t\t\t\tc as sumvar()\n\t\t\t}\n\t\t}\n\t}\n",
    );
    assert!(err.contains("Expected argument but got ')'"), "{err}");
}

#[test]
fn parse_query_with_var_val_agg_error_1() {
    let err = parse_err(
        "\n\t{\n\t\tme(func: uid(L), orderasc: val(d)) {\n\t\t\tname\n\t\t}\n\n\t\tvar(func: uid(0x0a)) {\n\t\t\tL as friends {\n\t\t\t\ta as age\n\t\t\t\tb as count(friends)\n\t\t\t\tc as count(relatives)\n\t\t\t\td as math(a + b*c + exp())\n\t\t\t}\n\t\t}\n\t}\n",
    );
    assert!(err.contains("Empty () not allowed in math block"), "{err}");
}

#[test]
fn parse_query_with_var_val_agg_math_log_error() {
    // Port of TestParseQueryWithVarValAgg_Error2 (underscore variant).
    let err = parse_err(
        "\n\t{\n\t\tme(func: uid(L), orderasc: val(d)) {\n\t\t\tname\n\t\t}\n\n\t\tvar(func: uid(0x0a)) {\n\t\t\tL as friends {\n\t\t\t\ta as age\n\t\t\t\tb as count(friends)\n\t\t\t\tc as count(relatives)\n\t\t\t\td as math(a + b*c+ log())\n\t\t\t}\n\t\t}\n\t}\n",
    );
    assert!(err.contains("Unknown math function: log"), "{err}");
}

#[test]
fn parse_query_with_var_val_agg_error_3() {
    let err = parse_err(
        "\n\t{\n\t\tme(func: uid(L), orderasc: val(d)) {\n\t\t\tname\n\t\t\tval(f)\n\t\t}\n\n\t\tvar(func: uid(0x0a)) {\n\t\t\tL as friends {\n\t\t\t\ta as age\n\t\t\t\tb as count(friends)\n\t\t\t\tc as count(relatives)\n\t\t\t\td as math(a + b*c)\n\t\t\t\tf as math()\n\t\t\t}\n\t\t}\n\t}\n",
    );
    assert!(err.contains("Empty () not allowed in math block"), "{err}");
}
#[test]
fn parse_query_with_var_val_agg_nested() {
    let debug = math_debug(
        "\n\t{\n\t\tme(func: uid(L), orderasc: val(d)) {\n\t\t\tname\n\t\t}\n\n\t\tvar(func: uid(0x0a)) {\n\t\t\tL as friends {\n\t\t\t\ta as age\n\t\t\t\tb as count(friends)\n\t\t\t\tc as count(relatives)\n\t\t\t\td as math(a + b*c)\n\t\t\t}\n\t\t}\n\t}\n",
        1,
        0,
        3,
    );
    assert_eq!(debug, "(+ a (* b c))");
}

#[test]
fn parse_query_with_var_val_agg_nested_2() {
    let query = "\n\t{\n\t\tme(func: uid(L), orderasc: val(d)) {\n\t\t\tname\n\t\t\tval(q)\n\t\t}\n\n\t\tvar(func: uid(0x0a)) {\n\t\t\tL as friends {\n\t\t\t\ta as age\n\t\t\t\tb as count(friends)\n\t\t\t\tc as count(relatives)\n\t\t\t\td as math(exp(a + b + 1.0) - ln(c))\n\t\t\t\tq as math(c*-1.0+-b+(-b*c))\n\t\t\t}\n\t\t}\n\t}\n";
    assert_eq!(
        math_debug(query, 1, 0, 3),
        "(- (exp (+ (+ a b) 1E+00)) (ln c))"
    );
    assert_eq!(
        math_debug(query, 1, 0, 4),
        "(+ (+ (* c (u- 1E+00)) (u- b)) (* (u- b) c))"
    );
}

#[test]
fn parse_query_with_var_val_agg_nested_4() {
    let debug = math_debug(
        "\n\t{\n\t\tme(func: uid(L), orderasc: val(d) ) {\n\t\t\tname\n\t\t}\n\n\t\tvar(func: uid(0x0a)) {\n\t\t\tL as friends {\n\t\t\t\ta as age\n\t\t\t\tb as count(friends)\n\t\t\t\tc as count(relatives)\n\t\t\t\td as math(exp(a + b + 1.0) - max(c,ln(c)) + sqrt(a%b))\n\t\t\t}\n\t\t}\n\t}\n",
        1,
        0,
        3,
    );
    assert_eq!(
        debug,
        "(+ (- (exp (+ (+ a b) 1E+00)) (max c (ln c))) (sqrt (% a b)))"
    );
}

#[test]
fn parse_query_with_var_val_agg_nested_5() {
    let debug = math_debug(
        "\n\t{\n\t\tme(func: uid(L), orderasc: val(d) ) {\n\t\t\tname\n\t\t}\n\n\t\tvar(func: uid(0x0a)) {\n\t\t\tL as friends {\n\t\t\t\ta as age\n\t\t\t\tb as count(friends)\n\t\t\t\tc as count(relatives)\n\t\t\t\td as math(a * b / c)\n\t\t\t}\n\t\t}\n\t}\n",
        1,
        0,
        3,
    );
    assert_eq!(debug, "(/ (* a b) c)");
}

#[test]
fn parse_query_with_var_val_agg_log_sqrt() {
    let query = "\n\t{\n\t\tme(func: uid(L), orderasc: val(d) ) {\n\t\t\tname\n\t\t\tval(e)\n\t\t}\n\n\t\tvar(func: uid(0x0a)) {\n\t\t\tL as friends {\n\t\t\t\ta as age\n\t\t\t\td as math(ln(sqrt(a)))\n\t\t\t\te as math(sqrt(ln(a)))\n\t\t\t}\n\t\t}\n\t}\n";
    assert_eq!(math_debug(query, 1, 0, 1), "(ln (sqrt a))");
    assert_eq!(math_debug(query, 1, 0, 2), "(sqrt (ln a))");
}

#[test]
fn parse_query_with_var_val_dot_product() {
    let debug = math_debug(
        "\n\t{\n\t\tme(func: uid(L), orderasc: val(d) ) {\n\t\t\tname\n\t\t}\n\n\t\tvar(func: uid(0x0a)) {\n\t\t\tL as friends {\n\t\t\t\ta as vfloat\n\t\t\t\tb as vfloat\n\t\t\t\tc as count(relatives)\n\t\t\t\td as math(a dot b * c)\n\t\t\t}\n\t\t}\n\t}\n",
        1,
        0,
        3,
    );
    assert_eq!(debug, "(dot a (* b c))");
}

#[test]
fn parse_query_with_var_val_dot_product_2() {
    let debug = math_debug(
        "\n\t{\n\t\tme(func: uid(L), orderasc: val(d) ) {\n\t\t\tname\n\t\t}\n\n\t\tvar(func: uid(0x0a)) {\n\t\t\tL as friends {\n\t\t\t\ta as vfloat\n\t\t\t\tb as vfloat\n\t\t\t\tc as count(relatives)\n\t\t\t\td as math(a dot b / c)\n\t\t\t}\n\t\t}\n\t}\n",
        1,
        0,
        3,
    );
    assert_eq!(debug, "(dot a (/ b c))");
}

#[test]
fn parse_query_with_var_val_agg_nested_conditional() {
    let query = "\n\t{\n\t\tme(func: uid(L), orderasc: val(d) ) {\n\t\t\tname\n\t\t\tval(f)\n\t\t}\n\n\t\tvar(func: uid(0x0a)) {\n\t\t\tL as friends {\n\t\t\t\ta as age\n\t\t\t\tb as count(friends)\n\t\t\t\tc as count(relatives)\n\t\t\t\td as math(cond(a <= 10.0, exp(a + b + 1.0), ln(c)) + 10*a)\n\t\t\t\te as math(cond(a!=10.0, exp(a + b + 1.0), ln(d)))\n\t\t\t\tf as math(cond(a==10.0, exp(a + b + 1.0), ln(e)))\n\t\t\t}\n\t\t}\n\t}\n";
    assert_eq!(
        math_debug(query, 1, 0, 3),
        "(+ (cond (<= a 1E+01) (exp (+ (+ a b) 1E+00)) (ln c)) (* 10 a))"
    );
    assert_eq!(
        math_debug(query, 1, 0, 4),
        "(cond (!= a 1E+01) (exp (+ (+ a b) 1E+00)) (ln d))"
    );
    assert_eq!(
        math_debug(query, 1, 0, 5),
        "(cond (== a 1E+01) (exp (+ (+ a b) 1E+00)) (ln e))"
    );
}

#[test]
fn parse_query_with_var_val_agg_nested_3() {
    let debug = math_debug(
        "\n\t{\n\t\tme(func: uid(L), orderasc: val(d) ) {\n\t\t\tname\n\t}\n\n\t\tvar(func: uid(0x0a)) {\n\t\t\tL as friends {\n\t\t\t\ta as age\n\t\t\t\tb as count(friends)\n\t\t\t\tc as count(relatives)\n\t\t\t\td as math(a + b * c / a + exp(a + b + 1.0) - ln(c))\n\t\t\t}\n\t\t}\n\t}\n",
        1,
        0,
        3,
    );
    assert_eq!(
        debug,
        "(+ (+ a (/ (* b c) a)) (- (exp (+ (+ a b) 1E+00)) (ln c)))"
    );
}

#[test]
fn parse_query_with_var_val_agg_nested_error_1() {
    let err = parse_err(
        "\n\t{\n\t\tme(func: uid(L), orderasc: val(d)) {\n\t\t\tname\n\t\t}\n\n\t\tvar(func: uid(0x0a)) {\n\t\t\tL as friends {\n\t\t\t\ta as age\n\t\t\t\td as math(a + *)\n\t\t\t}\n\t\t}\n\t}\n",
    );
    assert!(err.contains("Expected 2 operands"), "{err}");
}

#[test]
fn parse_query_with_var_val_agg_nested_error_2() {
    let err = parse_err(
        "\n\t{\n\t\tme(func: uid(L), orderasc: val(d)) {\n\t\t\tname\n\t\t}\n\n\t\tvar(func: uid(0x0a)) {\n\t\t\tL as friends {\n\t\t\t\ta as age\n\t\t\t\tb as count(friends)\n\t\t\t\tc as count(relatives)\n\t\t\t\td as math(a +b*c -)\n\t\t\t}\n\t\t}\n\t}\n",
    );
    assert!(err.contains("Expected 2 operands"), "{err}");
}

#[test]
fn parse_query_with_level_agg() {
    let res = parse_str(
        "\n\t{\n\t\tvar(func: uid(0x0a)) {\n\t\t\tfriends {\n\t\t\t\ta as count(age)\n\t\t\t}\n\t\t\ts as sum(val(a))\n\t\t}\n\n\t\tsumage(func: uid( 0x0a)) {\n\t\t\tval(s)\n\t\t}\n\t}\n",
    );
    assert_eq!(res.query.len(), 2);
    assert_eq!(res.query[0].children[0].children[0].var, "a");
    assert!(res.query[0].children[1].is_internal);
    assert_eq!(res.query[0].children[1].needs_var[0].name, "a");
    assert_eq!(
        res.query[0].children[1].needs_var[0].typ,
        datastreamdb_dql::VALUE_VAR
    );
    assert_eq!(res.query[0].children[1].var, "s");
}

#[test]
fn parse_query_with_var_val_agg_combination() {
    let res = parse_str(
        "\n\t{\n\t\tme(func: uid(L), orderasc: val(c) ) {\n\t\t\tname\n\t\t\tval(c)\n\t\t}\n\n\t\tvar(func: uid(0x0a)) {\n\t\t\tL as friends {\n\t\t\t\tx as age\n\t\t\t}\n\t\t\ta as min(val(x))\n\t\t\tb as max(val(x))\n\t\t\tc as math(a + b)\n\t\t}\n\t}\n",
    );
    assert_eq!(res.query.len(), 2);
    assert_eq!(res.query[0].needs_var[0].name, "L");
    assert_eq!(res.query[0].needs_var[0].typ, datastreamdb_dql::UID_VAR);
    assert_eq!(res.query[0].needs_var[1].name, "c");
    assert_eq!(res.query[0].needs_var[1].typ, datastreamdb_dql::VALUE_VAR);
    assert_eq!(res.query[0].order[0].attr, "c");
    assert_eq!(res.query[0].children[0].attr, "name");
    assert_eq!(res.query[0].children[1].attr, "val");
    assert_eq!(res.query[0].children[1].needs_var.len(), 1);
    assert_eq!(res.query[0].children[1].needs_var[0].name, "c");
    assert_eq!(res.query[1].children[0].var, "L");
    assert_eq!(res.query[1].children[1].var, "a");
    assert_eq!(res.query[1].children[2].var, "b");
    assert_eq!(res.query[1].children[3].var, "c");
    let math = res.query[1].children[3].math_exp.as_ref().expect("math");
    assert_eq!(math.op, "+");
    assert_eq!(math.child[0].var, "a");
    assert_eq!(math.child[1].var, "b");
}

#[test]
fn parse_query_with_var_val_agg() {
    let res = parse_str(
        "\n\t{\n\t\tme(func: uid(L), orderasc: val(n) ) {\n\t\t\tname\n\t\t}\n\n\t\tvar(func: uid(0x0a)) {\n\t\t\tL AS friends {\n\t\t\t\tna as name\n\t\t\t}\n\t\t\tn as min(val(na))\n\t\t}\n\t}\n",
    );
    assert_eq!(res.query.len(), 2);
    assert_eq!(res.query[0].needs_var[0].name, "L");
    assert_eq!(res.query[0].needs_var[0].typ, datastreamdb_dql::UID_VAR);
    assert_eq!(res.query[0].needs_var[1].name, "n");
    assert_eq!(res.query[0].needs_var[1].typ, datastreamdb_dql::VALUE_VAR);
    assert_eq!(res.query[0].order[0].attr, "n");
    assert_eq!(res.query[0].children[0].attr, "name");
    assert_eq!(res.query[1].children[0].var, "L");
    assert_eq!(res.query[1].children[0].children[0].var, "na");
    assert_eq!(res.query[1].children[1].var, "n");
    assert_eq!(
        res.query[1].children[1].func.as_ref().expect("func").name,
        "min"
    );
}

#[test]
fn parse_query_with_var_val_agg_error() {
    let err = parse_err(
        "\n\t{\n\t\tme(func: uid(L), orderasc: uid(n)) {\n\t\t\tname\n\t\t}\n\n\t\tvar(func: uid(0x0a)) {\n\t\t\tL AS friends {\n\t\t\t\tna as name\n\t\t\t}\n\t\t\tn as min(val(na))\n\t\t}\n\t}\n",
    );
    assert!(
        err.contains("Expected val(). Got uid() with order."),
        "{err}"
    );
}

#[test]
fn parse_query_with_var_val_agg_error_2() {
    let err = parse_err(
        "\n\t{\n\t\tme(func: val(L), orderasc: val(n)) {\n\t\t\tname\n\t\t}\n\n\t\tvar(func: uid(0x0a)) {\n\t\t\tL AS friends {\n\t\t\t\tna as name\n\t\t\t}\n\t\t\tn as min(val(na))\n\t\t}\n\t}\n",
    );
    assert!(err.contains("Function name: val is not valid."), "{err}");
}

#[test]
fn parse_query_with_var_val_count() {
    let res = parse_str(
        "\n\t{\n\t\tme(func: uid(L), orderasc: val(n) ) {\n\t\t\tname\n\t\t}\n\n\t\tvar(func: uid(0x0a)) {\n\t\t\tL AS friends {\n\t\t\t\tn AS count(friend)\n\t\t\t}\n\t\t}\n\t}\n",
    );
    assert_eq!(res.query.len(), 2);
    assert_eq!(res.query[0].needs_var[0].name, "L");
    assert_eq!(res.query[0].needs_var[0].typ, datastreamdb_dql::UID_VAR);
    assert_eq!(res.query[0].needs_var[1].name, "n");
    assert_eq!(res.query[0].needs_var[1].typ, datastreamdb_dql::VALUE_VAR);
    assert_eq!(res.query[0].order[0].attr, "n");
    assert_eq!(res.query[0].children[0].attr, "name");
    assert_eq!(res.query[1].children[0].var, "L");
    assert!(res.query[1].children[0].children[0].is_count);
}

#[test]
fn parse_query_with_var_val() {
    let res = parse_str(
        "\n\t{\n\t\tme(func: uid(L), orderasc: val(n) ) {\n\t\t\tname\n\t\t}\n\n\t\tvar(func: uid(0x0a)) {\n\t\t\tL AS friends {\n\t\t\t\tn AS name\n\t\t\t}\n\t\t}\n\t}\n",
    );
    assert_eq!(res.query.len(), 2);
    assert_eq!(res.query[0].needs_var[0].name, "L");
    assert_eq!(res.query[0].needs_var[0].typ, datastreamdb_dql::UID_VAR);
    assert_eq!(res.query[0].needs_var[1].name, "n");
    assert_eq!(res.query[0].needs_var[1].typ, datastreamdb_dql::VALUE_VAR);
    assert_eq!(res.query[0].order[0].attr, "n");
    assert_eq!(res.query[0].children[0].attr, "name");
    assert_eq!(res.query[1].children[0].var, "L");
    assert_eq!(res.query[1].children[0].children[0].var, "n");
}

#[test]
fn parse_query_with_var_multi_root() {
    let res = parse_str(
        "\n\t{\n\t\tme(func: uid( L, J, K)) {name}\n\t\tvar(func: uid(0x0a)) {L AS friends}\n\t\tvar(func: uid(0x0a)) {J AS friends}\n\t\tvar(func: uid(0x0a)) {K AS friends}\n\t}\n",
    );
    assert_eq!(res.query.len(), 4);
    assert_eq!(res.query[0].needs_var[0].name, "L");
    assert_eq!(res.query[0].needs_var[1].name, "J");
    assert_eq!(res.query[0].needs_var[2].name, "K");
    assert_eq!(res.query[0].needs_var[0].typ, datastreamdb_dql::UID_VAR);
    assert_eq!(res.query[0].needs_var[1].typ, datastreamdb_dql::UID_VAR);
    assert_eq!(res.query[0].needs_var[2].typ, datastreamdb_dql::UID_VAR);
    assert_eq!(res.query[1].children[0].var, "L");
    assert_eq!(res.query[2].children[0].var, "J");
    assert_eq!(res.query[3].children[0].var, "K");
}

#[test]
fn parse_query_with_var() {
    let res = parse_str(
        "\n\t{\n\t\tme(func: uid(L)) {name}\n\t\thim(func: uid(J)) {name}\n\t\tyou(func: uid(K)) {name}\n\t\tvar(func: uid(0x0a)) {L AS friends}\n\t\tvar(func: uid(0x0a)) {J AS friends}\n\t\tvar(func: uid(0x0a)) {K AS friends}\n\t}\n",
    );
    assert_eq!(res.query.len(), 6);
    assert_eq!(res.query[0].needs_var[0].name, "L");
    assert_eq!(res.query[1].needs_var[0].name, "J");
    assert_eq!(res.query[2].needs_var[0].name, "K");
    assert_eq!(res.query[0].needs_var[0].typ, datastreamdb_dql::UID_VAR);
    assert_eq!(res.query[1].needs_var[0].typ, datastreamdb_dql::UID_VAR);
    assert_eq!(res.query[2].needs_var[0].typ, datastreamdb_dql::UID_VAR);
    assert_eq!(res.query[3].children[0].var, "L");
    assert_eq!(res.query[4].children[0].var, "J");
    assert_eq!(res.query[5].children[0].var, "K");
}

#[test]
fn parse_query_with_var_error_1() {
    let err = parse_err(
        "\n\t{\n\t\thim(func: uid(J)) {name}\n\t\tyou(func: uid(K)) {name}\n\t\tvar(func: uid(0x0a)) {L AS friends}\n\t\tvar(func: uid(0x0a)) {J AS friends}\n\t\tvar(func: uid(0x0a)) {K AS friends}\n\t}\n",
    );
    assert!(
        err.contains("Some variables are defined but not used"),
        "{err}"
    );
}

#[test]
fn parse_query_with_var_error_2() {
    let err = parse_err(
        "\n\t{\n\t\tme(func: uid(L)) {name}\n\t\thim(func: uid(J)) {name}\n\t\tyou(func: uid(K)) {name}\n\t\tvar(func: uid(0x0a)) {L AS friends}\n\t\tvar(func: uid(0x0a)) {K AS friends}\n\t}\n",
    );
    assert!(
        err.contains("Some variables are used but not defined"),
        "{err}"
    );
}

#[test]
fn parse_query_filter_error_1a() {
    let err = parse_err(
        "\n\t{\n\t\tme(func: uid(1) @filter(anyof(name, \"alice\"))) {\n\t\t name\n\t\t}\n\t}\n",
    );
    assert!(err.contains("\"@\""), "{err}");
}

#[test]
fn parse_query_filter_error_1b() {
    let err = parse_err(
        "\n\t{\n\t\tme(func: uid(1)) @filter(anyofterms(name\"alice\")) {\n\t\t name\n\t\t}\n\t}\n",
    );
    assert!(
        err.contains("Expected comma or language but got: \"alice\""),
        "{err}"
    );
}

#[test]
fn parse_query_filter_error_2() {
    let err = parse_err(
        "\n\t{\n\t\tme(func: uid(1)) @filter(anyofterms(name \"alice\")) {\n\t\t name\n\t\t}\n\t}\n",
    );
    assert!(
        err.contains("Expected comma or language but got: \"alice\""),
        "{err}"
    );
}

#[test]
fn parse_query_with_var_at_root_filter_id() {
    let res = parse_str(
        "\n\t{\n\t\tK as var(func: uid(0x0a)) {\n\t\t\tL AS friends\n\t\t}\n\t\tme(func: uid(K)) @filter(uid(L)) {\n\t\t name\n\t\t}\n\t}\n",
    );
    assert_eq!(res.query.len(), 2);
    assert_eq!(res.query[0].var, "K");
    assert_eq!(res.query[0].children[0].var, "L");
    let filter = res.query[1].filter.as_ref().expect("filter");
    let func = filter.func.as_ref().expect("func");
    assert_eq!(func.needs_var[0].name, "L");
    assert_eq!(func.needs_var[0].typ, datastreamdb_dql::UID_VAR);
    assert_eq!(res.query_vars[0].defines, ["K", "L"]);
}

#[test]
fn parse_query_with_var_at_root() {
    let res = parse_str(
        "\n\t{\n\t\tK AS var(func: uid(0x0a)) {\n\t\t\tfr as friends\n\t\t}\n\t\tme(func: uid(fr)) @filter(uid(K)) {\n\t\t name\t@filter(uid(fr))\n\t\t}\n\t}\n",
    );
    assert_eq!(res.query.len(), 2);
    assert_eq!(res.query[0].var, "K");
    assert_eq!(res.query[0].children[0].var, "fr");
    assert_eq!(res.query[1].needs_var[0].name, "fr");
    assert_eq!(res.query[1].needs_var[0].typ, datastreamdb_dql::UID_VAR);
    assert_eq!(res.query_vars[0].defines, ["K", "fr"]);
}

#[test]
fn parse_query_with_var_in_ineq_error() {
    let err = parse_err(
        "\n\t{\n\t\tvar(func: uid(0x0a)) {\n\t\t\tfr as friends {\n\t\t\t\ta as age\n\t\t\t}\n\t\t}\n\n\t\tme(func: uid(fr)) @filter(gt(val(a, b), 10)) {\n\t\t name\n\t\t}\n\t}\n",
    );
    assert!(
        err.contains("Multiple variables not allowed in a function"),
        "{err}"
    );
}

#[test]
fn len_function_with_multiple_variable_error() {
    let err = parse_err(
        "\n\t{\n\t\tvar(func: uid(0x0a)) {\n\t\t\tfr as friends {\n\t\t\t\ta as age\n\t\t\t}\n\t\t}\n\n\t\tme(func: uid(fr)) @filter(gt(len(a, b), 10)) {\n\t\t name\n\t\t}\n\t}\n",
    );
    assert!(
        err.contains("Multiple variables not allowed in len function"),
        "{err}"
    );
}

#[test]
fn len_function_inside_uid_error() {
    let err = parse_err(
        "\n\t{\n\t\tvar(func: uid(0x0a)) {\n\t\t\tfr as friends {\n\t\t\t\ta as age\n\t\t\t}\n\t\t}\n\n\t\tme(func: uid(fr)) @filter(uid(len(a), 10)) {\n\t\t\tname\n\t\t}\n\t}\n",
    );
    assert!(
        err.contains("len function only allowed inside inequality"),
        "{err}"
    );
}

#[test]
fn len_function_with_no_variable() {
    let err = parse_err(
        "\n\t{\n\t\tvar(func: uid(0x0a)) {\n\t\t\tfr as friends {\n\t\t\t\ta as age\n\t\t\t}\n\t\t}\n\n\t\tme(func: uid(fr)) @filter(len(), 10) {\n\t\t\tname\n\t\t}\n\t}\n",
    );
    assert!(err.contains("Got empty attr for function"), "{err}");
}

#[test]
fn len_as_second_argument_error() {
    parse_err(
        "\n\t{\n\t\tvar(func: uid(0x0a)) {\n\t\t\tfr as friends {\n\t\t\t\ta as age\n\t\t\t}\n\t\t}\n\n\t\tme(func: uid(fr)) @filter(10, len(fr)) {\n\t\t\tname\n\t\t}\n\t}\n",
    );
}

#[test]
fn count_with_len_function_error() {
    parse_err(
        "\n\t{\n\t\tvar(func: uid(0x0a)) {\n\t\t\tfr as friends {\n\t\t\t\ta as age\n\t\t\t}\n\t\t}\n\n\t\tme(func: uid(fr)) @filter(count(name), len(fr)) {\n\t\t\tname\n\t\t}\n\t}\n",
    );
}
