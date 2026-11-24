//! Parser tests: type functions, expand, recurse, cascade, and value filters (port of Go's `parser_test.go`, part 7 of 8).
//!
//! Split from `parser_tests.rs` to honor the 1000-line file rule; shared
//! helpers live in [`common`].

#[path = "common/mod.rs"]
mod common;

use std::collections::HashMap;

use common::{parse_err, parse_str};
use datastreamdb_dql::{Request, parse};

#[test]
fn line_and_column_number_in_error_output() {
    let err = parse_err(
        r#"
	query {
		me(func: uid(0x0a)) {
			friends @filter(alloftext(descr@, "something")) {
				name
			}
			gender,age
			hometown
		}
	}"#,
    );
    assert!(
        err.contains("line 4 column 35: Unrecognized character in lexDirective: U+002C ','"),
        "{err}"
    );
}

#[test]
fn type_function() {
    let res = parse_str(
        r"
	query {
		me(func: type(Person)) {
			name
		}
	}",
    );
    assert_eq!(res.query.len(), 1);
    let func = res.query[0].func.as_ref().expect("func");
    assert_eq!(func.name, "type");
    assert_eq!(func.args.len(), 1);
    assert_eq!(func.args[0].value, "Person");
}

#[test]
fn type_function_error_1() {
    let err = parse_err(
        r"
	query {
		me(func: type(Person, School)) {
			name
		}
	}",
    );
    assert!(
        err.contains("type function only supports one argument"),
        "{err}"
    );
}

#[test]
fn type_in_filter() {
    let res = parse_str(
        r"
	query {
		me(func: uid(0x01)) @filter(type(Person)) {
			name
		}
	}",
    );
    assert_eq!(res.query.len(), 1);
    assert_eq!(res.query[0].func.as_ref().expect("func").name, "uid");
    assert_eq!(res.query[0].children.len(), 1);
    assert_eq!(res.query[0].children[0].attr, "name");
    let func = res.query[0]
        .filter
        .as_ref()
        .expect("filter")
        .func
        .as_ref()
        .expect("func");
    assert_eq!(func.name, "type");
    assert_eq!(func.args.len(), 1);
    assert_eq!(func.args[0].value, "Person");
}

#[test]
fn type_filter_in_predicate() {
    let res = parse_str(
        r"
	query {
		me(func: uid(0x01)) {
			friend @filter(type(Person)) {
				name
			}
		}
	}",
    );
    assert_eq!(res.query.len(), 1);
    assert_eq!(res.query[0].func.as_ref().expect("func").name, "uid");
    assert_eq!(res.query[0].children.len(), 1);
    assert_eq!(res.query[0].children[0].attr, "friend");
    let func = res.query[0].children[0]
        .filter
        .as_ref()
        .expect("filter")
        .func
        .as_ref()
        .expect("func");
    assert_eq!(func.name, "type");
    assert_eq!(func.args.len(), 1);
    assert_eq!(func.args[0].value, "Person");
    assert_eq!(res.query[0].children[0].children.len(), 1);
    assert_eq!(res.query[0].children[0].children[0].attr, "name");
}

#[test]
fn parse_expand_type() {
    let res = parse_str(
        r"
	{
		var(func: has(name)) {
			expand(Person,Animal) {
				uid
			}
		}
	}
",
    );
    assert_eq!(res.query.len(), 1);
    assert_eq!(res.query[0].children.len(), 1);
    assert_eq!(res.query[0].children[0].attr, "expand");
    assert_eq!(res.query[0].children[0].expand, "Person,Animal");
    assert_eq!(res.query[0].children[0].children.len(), 1);
    assert_eq!(res.query[0].children[0].children[0].attr, "uid");
}

#[test]
fn parse_var_after_count_qry() {
    parse_str(
        r#"
		{
			q(func: allofterms(name@en, "steven spielberg")) {
				director.film {
					u1 as count(uid)
					genre {
						u2 as math(1)
					}
			  	}
			}

			sum() {
				totalMovies: sum(val(u1))
				totalGenres: sum(val(u2))
			}
		}
	"#,
    );
}

#[test]
fn recurse_with_args() {
    let mut variables = HashMap::new();
    variables.insert("$hello".to_owned(), "1".to_owned());
    let res = parse(Request {
        text:
            "{\n\t\tme(func: eq(name, \"sad\")) @recurse(depth: $hello , loop: true) {\n\t\t}\n\t}"
                .to_owned(),
        variables,
    })
    .expect("parse");
    assert_eq!(res.query[0].recurse_args.depth, 1);

    let mut variables = HashMap::new();
    variables.insert("$hello".to_owned(), "true".to_owned());
    let res = parse(Request {
        text: "{\n\t\tme(func: eq(name, \"sad\"))@recurse(depth: 1 , loop: $hello) {\n\t\t}\n\t}"
            .to_owned(),
        variables,
    })
    .expect("parse");
    assert!(res.query[0].recurse_args.allow_loop);

    let mut variables = HashMap::new();
    variables.insert("$hello".to_owned(), "1".to_owned());
    variables.insert("$hello1".to_owned(), "true".to_owned());
    let res = parse(Request {
        text:
            "{\n\t\tme(func: eq(name, \"sad\"))@recurse(depth: $hello, loop: $hello1) {\n\t\t}\n\t}"
                .to_owned(),
        variables,
    })
    .expect("parse");
    assert!(res.query[0].recurse_args.allow_loop);
    assert_eq!(res.query[0].recurse_args.depth, 1);

    let mut variables = HashMap::new();
    variables.insert("$_hello_hello".to_owned(), "1".to_owned());
    variables.insert("$hello1_heelo1".to_owned(), "true".to_owned());
    let res = parse(Request {
        text: "{\n\t\tme(func: eq(name, \"sad\"))@recurse(depth: $_hello_hello, loop: $hello1_heelo1) {\n\t\t}\n\t}".to_owned(),
        variables,
    })
    .expect("parse");
    assert!(res.query[0].recurse_args.allow_loop);
    assert_eq!(res.query[0].recurse_args.depth, 1);
}

#[test]
fn recurse_with_args_with_error() {
    let err = parse_err(
        "{\n\t\tme(func: eq(name, \"sad\"))@recurse(depth: $hello, loop: true) {\n\t\t}\n\t}",
    );
    assert!(err.contains("variable $hello not defined"), "{err}");
    let err = parse_err(
        "{\n\t\tme(func: eq(name, \"sad\"))@recurse(depth: 1, loop: $hello) {\n\t\t}\n\t}",
    );
    assert!(err.contains("variable $hello not defined"), "{err}");
    let mut variables = HashMap::new();
    variables.insert("$hello".to_owned(), "sd".to_owned());
    variables.insert("$hello1".to_owned(), "true".to_owned());
    let err = parse(Request {
        text:
            "{\n\t\tme(func: eq(name, \"sad\"))@recurse(depth: $hello, loop: $hello1) {\n\t\t}\n\t}"
                .to_owned(),
        variables,
    })
    .expect_err("must fail")
    .to_string();
    assert!(err.contains("should be type of integer"), "{err}");
    let mut variables = HashMap::new();
    variables.insert("$hello".to_owned(), "1".to_owned());
    variables.insert("$hello1".to_owned(), "tre".to_owned());
    let err = parse(Request {
        text:
            "{\n\t\tme(func: eq(name, \"sad\"))@recurse(depth: $hello, loop: $hello1) {\n\t\t}\n\t}"
                .to_owned(),
        variables,
    })
    .expect_err("must fail")
    .to_string();
    assert!(err.contains("should be type of boolean"), "{err}");
}

#[test]
fn recurse() {
    let res =
        parse_str("{\n\t\tme(func: eq(name, \"sad\"))@recurse(depth: 1, loop: true) {\n\t\t}\n\t}");
    assert_eq!(res.query[0].recurse_args.depth, 1);
    assert!(res.query[0].recurse_args.allow_loop);
}

#[test]
fn recurse_with_error() {
    let err = parse_err(
        "{\n\t\tme(func: eq(name, \"sad\"))@recurse(depth: hello, loop: true) {\n\t\t}\n\t}",
    );
    assert!(
        err.contains("Value inside depth should be type of integer"),
        "{err}"
    );
    let err =
        parse_err("{\n\t\tme(func: eq(name, \"sad\"))@recurse(depth: 1, loop: tre) {\n\t\t}\n\t}");
    assert!(
        err.contains("Value inside loop should be type of boolean"),
        "{err}"
    );
}

#[test]
fn parse_expand_filter() {
    let res = parse_str(
        r#"
		{
			q(func: eq(name, "Frodo")) {
				expand(_all_) @filter(type(Person)) {
					uid
				}
			}
		}"#,
    );
    assert_eq!(res.query.len(), 1);
    assert_eq!(res.query[0].children.len(), 1);
    let func = res.query[0].children[0]
        .filter
        .as_ref()
        .expect("filter")
        .func
        .as_ref()
        .expect("func");
    assert_eq!(func.name, "type");
    assert_eq!(func.args.len(), 1);
    assert_eq!(func.args[0].value, "Person");
}

#[test]
fn parse_expand_filter_err() {
    let err = parse_err(
        r#"
		{
			q(func: eq(name, "Frodo")) {
				expand(_all_) @filter(has(Person)) {
					uid
				}
			}
		}"#,
    );
    assert!(
        err.contains("expand is only compatible with type filters"),
        "{err}"
    );
}

#[test]
fn filter_with_dollar() {
    let res = parse_str(
        r#"
	{
		q(func: eq(name, "Bob"), first:5) @filter(eq(description, "$yo")) {
		  name
		  description
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
            .args[0]
            .value,
        "$yo"
    );
}

#[test]
fn filter_with_dollar_error() {
    parse_err(
        r#"
	{
		q(func: eq(name, "Bob"), first:5) @filter(eq(description, $yo)) {
		  name
		  description
		}
	  }
	"#,
    );
}

#[test]
fn filter_with_eq_and_len_in_wrong_order_args() {
    let err = parse_err(
        r"
	{
		var(func: has(school), first: 3) {
			f as uid
		}

		me(func: uid(f)) @filter(eq(3, len(f))) {
			count(uid)
		}
	}",
    );
    assert!(
        err.contains("incorrect order, the first argument should be len function"),
        "{err}"
    );
}

#[test]
fn filter_with_var() {
    let res = parse_str(
        r#"query data($a: string = "datastreamdb")
	{
		data(func: eq(name, "Bob"), first:5) @filter(eq(description, $a)) {
			name
			description
		  }
	}"#,
    );
    assert_eq!(
        res.query[0]
            .filter
            .as_ref()
            .expect("filter")
            .func
            .as_ref()
            .expect("func")
            .args[0]
            .value,
        "datastreamdb"
    );
}

#[test]
fn filter_with_empty() {
    let res = parse_str(
        r#"{
		names(func: has(name)) @filter(eq(name, "")) {
		  count(uid)
		}
	  }"#,
    );
    assert_eq!(
        res.query[0]
            .filter
            .as_ref()
            .expect("filter")
            .func
            .as_ref()
            .expect("func")
            .args[0]
            .value,
        ""
    );
}

#[test]
fn cascade() {
    let res = parse_str(
        r"{
		names(func: has(name)) @cascade {
		  name
		}
	  }",
    );
    assert_eq!(res.query[0].cascade[0], "__all__");
}

#[test]
fn cascade_parameterized() {
    let res = parse_str(
        r"{
		names(func: has(name)) @cascade(name, age) {
		  name
		  age
		  dob
		}
	  }",
    );
    assert_eq!(res.query[0].cascade[0], "name");
    assert_eq!(res.query[0].cascade[1], "age");
}

#[test]
fn bad_cascade_parameterized() {
    for query in [
        "{\n\t\t\tnames(func: has(name)) @cascade( {\n\t\t\t  name\n\t\t\t  age\n\t\t\t  dob\n\t\t\t}\n\t\t  }",
        "{\n\t\t\tnames(func: has(name)) @cascade) {\n\t\t\t  name\n\t\t\t  age\n\t\t\t  dob\n\t\t\t}\n\t\t }",
        "{\n\t\t\tnames(func: has(name)) @cascade() {\n\t\t\t  name\n\t\t\t  age\n\t\t\t  dob\n\t\t\t}\n\t\t  }",
        "{\n\t\t\tnames(func: has(name)) @cascade(,) {\n\t\t\t  name\n\t\t\t  age\n\t\t\t  dob\n\t\t\t}\n\t\t  }",
        "{\n\t\t\tnames(func: has(name)) @cascade(name,) {\n\t\t\t  name\n\t\t\t  age\n\t\t\t  dob\n\t\t\t}\n\t\t  }",
        "{\n\t\t\tnames(func: has(name)) @cascade(,name) {\n\t\t\t  name\n\t\t\t  age\n\t\t\t  dob\n\t\t\t}\n\t\t  }",
    ] {
        parse_err(query);
    }
}

#[test]
fn empty_id() {
    let mut variables = HashMap::new();
    variables.insert("$a".to_owned(), "   ".to_owned());
    parse(Request {
        text: "query me($a: string) { q(func: uid($a)) { name }}".to_owned(),
        variables,
    })
    .expect_err("ID cannot be empty");
}
