//! Parser tests: aggregations at root, misc root functions, and fuzz smoke (port of Go's `parser_test.go`, part 8 of 8).
//!
//! Split from `parser_tests.rs` to honor the 1000-line file rule; shared
//! helpers live in [`common`].

#[path = "common/mod.rs"]
mod common;

use std::collections::HashMap;

use common::{parse_err, parse_str};
use datastreamdb_dql::{Request, parse};

#[test]
fn parse_regexp_5() {
    let err = parse_err(
        r"
	{
	  me(func:regexp(name@en, /pattern/flag123)) {
	    name
	  }
    }
",
    );
    assert!(
        err.contains("Expected comma or language but got: 123"),
        "{err}"
    );
}

#[test]
fn parse_regexp_6() {
    let err = parse_err(
        r"
	{
	  me(func:regexp(name@en, /pattern\/)) {
	    name
	  }
    }
",
    );
    assert!(err.contains("Unclosed regexp"), "{err}");
}

#[test]
fn count_at_root() {
    parse_str("{\n\t\tme(func: uid( 1)) {\n\t\t\tcount(uid)\n\t\t\tcount(enemy)\n\t\t}\n\t}");
}

#[test]
fn count_at_root_err() {
    let err = parse_err(
        "{\n\t\tme(func: uid( 1)) {\n\t\t\tcount(enemy) {\n\t\t\t\tname\n\t\t\t}\n\t\t}\n\t}",
    );
    assert!(
        err.contains("Cannot have children attributes when asking for count"),
        "{err}"
    );
}

#[test]
fn count_at_root_err_2() {
    let err = parse_err("{\n\t\tme(func: uid( 1)) {\n\t\t\tcount()\n\t\t}\n\t}");
    assert!(
        err.contains("Cannot use count(), please use count(uid)"),
        "{err}"
    );
}

#[test]
fn has_func_at_root() {
    parse_str("{\n\t\tme(func: has(name@en)) {\n\t\t\tname\n\t\t}\n\t}");
}

#[test]
fn has_filter_at_root() {
    parse_str(
        "{\n\t\tme(func: allofterms(name, \"Steven Tom\")) @filter(has(director.film)) {\n\t\t\tname\n\t\t}\n\t}",
    );
}

#[test]
fn has_filter_at_child() {
    parse_str(
        "{\n\t\tme(func: anyofterms(name, \"Steven Tom\")) {\n\t\t\tname\n\t\t\tdirector.film @filter(has(genre)) {\n\t\t\t}\n\t\t}\n\t}",
    );
}

#[test]
fn dots_eof() {
    let err = parse_err("{\n\t\tme(func: uid( 0x1)) {\n\t\t\tname\n\t\t\t..");
    assert!(err.contains("Unclosed action"), "{err}");
}

#[test]
fn math_without_var_alias() {
    let err = parse_err(
        "{\n\t\t\tf(func: anyofterms(name, \"Rick Michonne Andrea\")) {\n\t\t\t\tageVar as age\n\t\t\t\tmath(ageVar *2)\n\t\t\t}\n\t\t}",
    );
    assert!(
        err.contains("Function math should be used with a variable or have an alias"),
        "{err}"
    );
}

#[test]
fn math_div_0() {
    // Port of TestMathDiv0.
    let cases = [
        ("{f(func: uid(1)){x:math(1+1)}}", false),
        ("{f(func: uid(1)){x:math(1/0)}}", true),
        ("{f(func: uid(1)){x:math(1/-0)}}", true),
        ("{f(func: uid(1)){x:math(1/ln(1))}}", true),
        ("{f(func: uid(1)){x:math(1/sqrt(0))}}", true),
        ("{f(func: uid(1)){x:math(1/floor(0))}}", true),
        ("{f(func: uid(1)){x:math(1/floor(0.5))}}", true),
        ("{f(func: uid(1)){x:math(1/floor(1.01))}}", false),
        ("{f(func: uid(1)){x:math(1/ceil(0))}}", true),
        ("{f(func: uid(1)){x:math(1%0}}", true),
        ("{f(func: uid(1)){x:math(1%floor(0)}}", true),
        ("{f(func: uid(1)){x:math(1 + 0)}}", false),
    ];
    for (input, has_error) in cases {
        let result = parse(Request {
            text: input.to_owned(),
            variables: HashMap::new(),
        });
        assert_eq!(result.is_err(), has_error, "input {input}");
    }
}

#[test]
fn multiple_equal() {
    let res = parse_str(
        "{\n\t\tme(func: eq(name,[\"Steven Spielberg\", \"Tom Hanks\"])) {\n\t\t\tname\n\t\t}\n\t}",
    );
    let func = res.query[0].func.as_ref().expect("func");
    assert_eq!(func.args.len(), 2);
    assert_eq!(func.args[1].value, "Tom Hanks");
}

#[test]
fn parse_eq_arg() {
    let res = parse_str(
        r#"
	{
		me(func: uid(1, 20)) @filter(eq(name, ["And\"rea", "Bob"])) {
		 name
		}
	}
"#,
    );
    assert_eq!(
        res.query[0]
            .filter
            .as_ref()
            .expect("filter")
            .func
            .as_ref()
            .expect("func")
            .args
            .len(),
        2
    );
}

#[test]
fn parser_fuzz_no_panic() {
    // Port of TestParserFuzz: crash-regression corpus; result ignored, but a
    // panic fails the test (like Go's recover check).
    let cases = [
        ("test001", "{e(){@filter(q(/"),
        ("test002", "{e(){@filter(p(%"),
        ("test003", "{e(){@filter(0(/\0"),
        ("test004", "query #"),
        ("test005", "{e(){{@filter(q(/"),
        ("test006", "{e(func:0(0(0,/"),
        ("test007", "{e(func:uid()){@filter(0(/\\"),
        ("test008", "{e(){@filter(0(/\\0/"),
        ("test009", "{e(){@filter(p(/\\"),
        ("test010", "{m(func:0(0(0(0,/"),
        ("test011", "{e(){@filter(0(%0"),
        ("test012", "{e(func:uid()){@filter(p(%"),
        ("test013", "{e(orderasc:val(0)){{min(val(0)0("),
        ("test014", "query{e(func:uid(val(0)"),
        ("test015", "{e(){@filter(0(%000"),
        ("test016", "{e(){@filter(0(%"),
        ("test017", "{e(){{@filter(0(/"),
        ("test018", "{e(func:uid(0))@filter(p(/\\"),
        ("test019", "{e(func:uid()){@filter(p(/\\"),
        ("test020", "{e(){@filter(p(/\\00"),
        ("test021", "{e(func:uid(0)){@filter(0(%"),
        ("test022", "{e(){@filter(0(/"),
        ("test023", "{e(func:uid()){@filter(p(/"),
        ("test024", "{s(func:uid(val(0)"),
        ("test025", "{e()@filter(0(%"),
        ("test026", "{e(func:uid(0)){@filter(0(/\\"),
        ("test027", "{e(){@filter(0(%"),
        ("test028", "{e(){@filter(0(%00"),
        ("test029", "{e(func:uid(0)){@filter(p(/\\e/i)){e}}}"),
        ("test030", "{e(func:uid(0)){@filter(p(%//i))00"),
        ("test031", "{e()@filter(p(/\u{fffd}Is))}"),
        ("test032", "{e(){@filter(0(0,/"),
        ("test033", "{e(func:uid()){@filter(0(/"),
        ("test034", "{e()@filter(0(/"),
        ("test035", "{e(func:uid(0)){@filter(p(/\\"),
        ("test036", "{e(func:uid())@filter(0(%"),
        ("test037", "{e(func:uid(0)){@filter(0(%"),
        ("test038", "{e(func:uid(0))@filter(p(/\\0/"),
        ("test039", "{e(func:uid(0)){@filter(p(/\\e/i)){e}00"),
        ("test040", "{e(){@filter(0(-"),
        ("test041", "{e(func:uid(0)){@filter(0(%0"),
        ("test042", "{e()@filter(q(/"),
        ("test043", "{e(func:uid(0)){@filter(p(%"),
        ("test044", "{e()@filter(p(/"),
        ("test045", "{e(func:uid())@filter(0(/"),
        ("test046", "{e(func:uid(0)){@filter(p(/\\e/"),
        ("test047", "{e(func:uid()){@filter(0(%"),
        ("test048", "{e()@filter(0(0,/"),
        ("test049", "{e(){{@filter(0(0,/"),
        ("test050", "{e(func:uid(0)){@filter(p(/"),
        ("test051", "{e()@filter(0(-"),
        ("test052", "{e(func:uid(0)){@filter(0(/"),
        ("test053", "{e(func:uid())@filter(p(%"),
        ("test054", "{e(orderasc:val(0)){min(val(0)0("),
        ("test055", "{e(){@filter(p(/"),
        ("test056", "{e(func:uid())@filter(p(/"),
        ("test057", "a<><\\\u{fffd}"),
        ("test058", "L<\\\u{1d300}"),
        ("test059", "{d(after:<>0)}"),
        ("test060", "{e(orderasc:#"),
    ];
    for (_name, input) in cases {
        // Result ignored: a panic here fails the test (like Go's recover).
        let _ = parse(Request {
            text: input.to_owned(),
            variables: HashMap::new(),
        });
    }
}

#[test]
fn parse_eq_arg_2() {
    let res = parse_str(
        r#"
	{
		me(func: eq(age, [1, 20])) @filter(eq(name, ["And\"rea", "Bob"])) {
		 name
		}
	}
"#,
    );
    assert_eq!(
        res.query[0]
            .filter
            .as_ref()
            .expect("filter")
            .func
            .as_ref()
            .expect("func")
            .args
            .len(),
        2
    );
    assert_eq!(res.query[0].func.as_ref().expect("func").args.len(), 2);
}

#[test]
fn filter_error() {
    parse_err(
        r"
	{
		me(func: uid(1, 3 , 5, 7)) { @filter(uid(3, 7))
			name
		}
	}
	",
    );
}

#[test]
fn filter_error_2() {
    parse_err(
        r#"
	{
		me(func: uid(1, 3 , 5, 7)) {
			name @filter(eq(name, 	"abc")) @filter(eq(name2, 	"abc"))
		}
	}
	"#,
    );
}

#[test]
fn double_group_by_error() {
    parse_err(
        r"
	{
		me(func: uid(1, 3 , 5, 7)) {
			name @groupby(abc) @groupby(bcd)
		}
	}
	",
    );
}

#[test]
fn filter_error_3() {
    parse_err(
        r#"
	{
		me(func: uid(1, 3 , 5, 7)) {
			expand(_all_) @filter(eq(name, "abc"))
		}
	}
	"#,
    );
}

#[test]
fn filter_uid() {
    let res = parse_str(
        r"
	{
		me(func: uid(1, 3 , 5, 7)) @filter(uid(3, 7)) {
			name
		}
	}
	",
    );
    assert_eq!(res.query[0].uid, [1, 3, 5, 7]);
    assert_eq!(
        res.query[0]
            .filter
            .as_ref()
            .expect("filter")
            .func
            .as_ref()
            .expect("func")
            .uid,
        [3, 7]
    );
}

#[test]
fn id_err() {
    let err = parse_err(
        r"
	{
		me(id: [1, 3 , 5, 7]) @filter(uid(3, 7)) {
			name
		}
	}
	",
    );
    assert!(err.contains("Got invalid keyword: id"), "{err}");
}

#[test]
fn filter_var_err() {
    let err = parse_err(
        r#"
	{
		x as m(func: allofterms(name, "Pawan Rawal"))
	}
	{
		me(func: uid(1, 3 , 5, 7)) @filter(var(x)) {
			name
		}
	}
	"#,
    );
    assert!(err.contains("Unexpected var()"), "{err}");
}

#[test]
fn agg_root_1() {
    let res = parse_str(
        r#"
		{
			var(func: anyofterms(name, "Rick Michonne Andrea")) {
				a as age
			}

			me() {
				sum(val(a))
				avg(val(a))
			}
		}
	"#,
    );
    assert_eq!(res.query[1].alias, "me");
    assert!(res.query[1].is_empty);
}

#[test]
fn agg_root_error() {
    let err = parse_err(
        r#"
		{
			var(func: anyofterms(name, "Rick Michonne Andrea")) {
				a as age
			}

			me() {
				sum(val(a))
				avg(val(a))
				friend @filter(anyofterms(name, "Hey")) {
					name
				}
			}
		}
	"#,
    );
    assert!(
        err.contains("Only aggregation/math functions allowed inside empty blocks. Got: friend"),
        "{err}"
    );
}

#[test]
fn agg_root_error_2() {
    let err = parse_err(
        r#"
		{
			var(func: anyofterms(name, "Rick Michonne Andrea")) {
				a as age
			}

			me() {
				avg(val(a))
				name
			}
		}
	"#,
    );
    assert!(
        err.contains("Only aggregation/math functions allowed inside empty blocks. Got: name"),
        "{err}"
    );
}

#[test]
fn agg_root_error_3() {
    let err = parse_err(
        r#"
		{
			var(func: anyofterms(name, "Rick Michonne Andrea")) {
				a as age
			}

			me() {
				avg
			}
		}
	"#,
    );
    assert!(
        err.contains("Only aggregation/math functions allowed inside empty blocks. Got: avg"),
        "{err}"
    );
}

#[test]
fn empty_function() {
    let err = parse_err(
        r"
		{
		  director(func:allofterms()) {
		    name@en
		    director.film (orderdesc: initial_release_date) {
		      name@en
		      initial_release_date
		    }
		  }
		}",
    );
    assert!(
        err.contains("Got empty attr for function: [allofterms]"),
        "{err}"
    );
}
