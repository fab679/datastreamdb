//! Parser tests: mutations, GraphQL variables, regexp, and order (port of Go's `parser_test.go`, part 6 of 8).
//!
//! Split from `parser_tests.rs` to honor the 1000-line file rule; shared
//! helpers live in [`common`].

#[path = "common/mod.rs"]
mod common;

use std::collections::HashMap;

use common::{VarArrayCase, parse_err, parse_nquads, parse_str};
use datastreamdb_dql::{Request, parse};

#[test]
fn parse_regexp_1() {
    let res = parse_str(
        r"
	{
	  me(func: uid(0x1)) {
	    name
		friend @filter(regexp(name@en, /case INSENSITIVE regexp with \/ escaped value/i)) {
	      name@en
	    }
	  }
    }
",
    );
    assert_eq!(res.query.len(), 1);
    let func = res.query[0].children[1]
        .filter
        .as_ref()
        .expect("filter")
        .func
        .as_ref()
        .expect("func");
    assert_eq!(
        func.args[0].value,
        "case INSENSITIVE regexp with / escaped value"
    );
    assert_eq!(func.args[1].value, "i");
}

#[test]
fn parse_regexp_2() {
    let res = parse_str(
        r#"
	{
	  me(func:regexp(name@en, /another\/compilicated ("") regexp('')/)) {
	    name
	  }
    }
"#,
    );
    assert_eq!(res.query.len(), 1);
    let func = res.query[0].func.as_ref().expect("func");
    assert_eq!(func.args[0].value, "another/compilicated (\"\") regexp('')");
    assert_eq!(func.args[1].value, "");
}

#[test]
fn parse_regexp_3() {
    let res = parse_str(
        r#"
	{
	  me(func:allofterms(name, "barack")) @filter(regexp(secret, /whitehouse[0-9]{1,4}/fLaGs)) {
	    name
	  }
    }
"#,
    );
    assert_eq!(res.query.len(), 1);
    let func = res.query[0]
        .filter
        .as_ref()
        .expect("filter")
        .func
        .as_ref()
        .expect("func");
    assert_eq!(func.args[0].value, "whitehouse[0-9]{1,4}");
    assert_eq!(func.args[1].value, "fLaGs");
}

#[test]
fn parse_regexp_4() {
    let err = parse_err(
        r"
	{
	  me(func:regexp(name@en, /pattern/123)) {
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
fn order_1() {
    let res = parse_str(
        r"
		{
			me(func: uid(1), orderdesc: name, orderasc: age) {
				name
			}
		}
	",
    );
    assert_eq!(res.query[0].order.len(), 2);
    assert_eq!(res.query[0].order[0].attr, "name");
    assert!(res.query[0].order[0].desc);
    assert_eq!(res.query[0].order[1].attr, "age");
    assert!(!res.query[0].order[1].desc);
}

#[test]
fn order_2() {
    let res = parse_str(
        r#"
		{
			me(func: uid(0x01)) {
				friend(orderasc: alias, orderdesc: name) @filter(lt(alias, "Pat")) {
					alias
				}
			}
		}
	"#,
    );
    let curp = &res.query[0].children[0];
    assert_eq!(curp.order.len(), 2);
    assert_eq!(curp.order[0].attr, "alias");
    assert!(!curp.order[0].desc);
    assert_eq!(curp.order[1].attr, "name");
    assert!(curp.order[1].desc);
}

#[test]
fn multiple_order_error() {
    let err = parse_err(
        r"
		{
			me(func: uid(0x01)) {
				friend(orderasc: alias, orderdesc: alias) {
					alias
				}
			}
		}
	",
    );
    assert!(
        err.contains("Sorting by an attribute: [alias] can only be done once"),
        "{err}"
    );
}

#[test]
fn multiple_order_error_2() {
    let err = parse_err(
        r"
		{
			me(func: uid(0x01),orderasc: alias, orderdesc: alias) {
				friend {
					alias
				}
			}
		}
	",
    );
    assert!(
        err.contains("Sorting by an attribute: [alias] can only be done once"),
        "{err}"
    );
}

#[test]
fn eq_arg_with_dollar() {
    let res = parse_str(
        r#"
	{
		ab(func: eq(name@en, "$pringfield (or, How)")) {
			uid
		}
	}
	"#,
    );
    assert_eq!(
        res.query[0].func.as_ref().expect("func").args[0].value,
        "$pringfield (or, How)"
    );
}

#[test]
fn lang_with_dash() {
    let res = parse_str("{\n\t\tq(func: uid(1)) {\n\t\t\ttext@en-us\n\t\t}\n\t}");
    assert_eq!(res.query[0].children[0].langs, ["en-us"]);
}

#[test]
fn order_by_var_and_pred() {
    let err = parse_err(
        "{\n\t\tq(func: uid(1), orderasc: name, orderdesc: val(n)) {\n\t\t}\n\n\t\tvar(func: uid(0x0a)) {\n\t\t\tfriends {\n\t\t\t\tn AS name\n\t\t\t}\n\t\t}\n\n\t}",
    );
    assert!(
        err.contains("Multiple sorting only allowed by predicates."),
        "{err}"
    );
    let err = parse_err(
        "{\n\t\tq(func: uid(1)) {\n\t\t}\n\n\t\tvar(func: uid(0x0a)) {\n\t\t\tfriends (orderasc: name, orderdesc: val(n)) {\n\t\t\t\tn AS name\n\t\t\t}\n\t\t}\n\n\t}",
    );
    assert!(
        err.contains("Multiple sorting only allowed by predicates."),
        "{err}"
    );
    parse_str(
        "{\n\t\tq(func: uid(1)) {\n\t\t}\n\n\t\tvar(func: uid(0x0a)) {\n\t\t\tfriends (orderasc: name, orderdesc: genre) {\n\t\t\t\tname\n\t\t\t}\n\t\t}\n\n\t}",
    );
}

#[test]
fn invalid_val_usage() {
    let err = parse_err(
        r"
		{
			me(func: uid(0x01)) {
				val(uid) {
					nope
				}
			}
		}
	",
    );
    assert!(err.contains("Query syntax invalid."), "{err}");
}

#[test]
fn order_with_multiple_lang_fail() {
    let err = parse_err(
        r"
	{
		me(func: uid(0x1), orderasc: name@en:fr, orderdesc: lastname@ci, orderasc: salary) {
			name
		}
	}
	",
    );
    assert!(
        err.contains("Sorting by an attribute: [name@en:fr] can only be done on one language"),
        "{err}"
    );
}

#[test]
fn order_with_lang() {
    let res = parse_str(
        r"
	{
		me(func: uid(0x1), orderasc: name@en, orderdesc: lastname@ci, orderasc: salary) {
			name
		}
	}
",
    );
    assert_eq!(res.query.len(), 1);
    let orders = &res.query[0].order;
    assert_eq!(orders[0].attr, "name");
    assert_eq!(orders[0].langs, ["en"]);
    assert_eq!(orders[1].attr, "lastname");
    assert_eq!(orders[1].langs, ["ci"]);
    assert_eq!(orders[2].attr, "salary");
    assert!(orders[2].langs.is_empty());
}

#[test]
fn parse_lang_tag_after_string_in_root() {
    let err = parse_err(
        r#"
		{
			q(func: anyofterms(name, "Hello"@en)) {
				uid
			}
		}
	"#,
    );
    assert!(
        err.contains("Invalid usage of '@' in function argument"),
        "{err}"
    );
}

#[test]
fn parse_lang_tag_after_string_in_filter() {
    let err = parse_err(
        r#"
		{
			q(func: uid(0x01)) @filter(eq(name, "Hello"@en)) {
				uid
			}
		}
	"#,
    );
    assert!(
        err.contains("Invalid usage of '@' in function argument"),
        "{err}"
    );
}
#[test]
fn parse_mutation() {
    let req = datastreamdb_dql::parse_mutation(
        r"
		{
			set {
				<name> <is> <something> .
				<hometown> <is> <san/francisco> .
			}
			delete {
				<name> <is> <something-else> .
			}
		}
	",
    )
    .expect("mutation");
    let mu = &req.mutations[0];
    let sets = parse_nquads(&mu.set_nquads);
    assert_eq!(sets[0].subject, "name");
    assert_eq!(sets[0].predicate, "is");
    assert_eq!(sets[0].object_id, "something");
    assert_eq!(sets[1].subject, "hometown");
    assert_eq!(sets[1].predicate, "is");
    assert_eq!(sets[1].object_id, "san/francisco");
    let dels = parse_nquads(&mu.del_nquads);
    assert_eq!(dels[0].subject, "name");
    assert_eq!(dels[0].predicate, "is");
    assert_eq!(dels[0].object_id, "something-else");
}

#[test]
fn parse_mutation_too_many_blocks() {
    let err = datastreamdb_dql::parse_mutation(
        "\n         {\n           set { _:a1 <reg> \"a1 content\" . }\n         }{\n\t\t   set { _:b2 <reg> \"b2 content\" . }\n\t\t }",
    )
    .expect_err("must fail")
    .to_string();
    assert!(
        err.contains("Unexpected { after the end of the block"),
        "{err}"
    );
    let err = datastreamdb_dql::parse_mutation("{set { _:a1 <reg> \"a1 content\" . }} something")
        .expect_err("must fail")
        .to_string();
    assert!(err.contains("Invalid operation type: something"), "{err}");
    datastreamdb_dql::parse_mutation(
        "\n          # comments are ok\n\t\t  {\n\t\t    set { _:a1 <reg> \"a1 content\" . } # comments are ok\n          } # comments are ok",
    )
    .expect("comments ok");
}

#[test]
fn parse_missing_graphql_var() {
    for q in [
        "{ q(func: eq(name, $a)) { name }}",
        "query { q(func: eq(name, $a)) { name }}",
        "query foo { q(func: eq(name, $a)) { name }}",
        "query foo () { q(func: eq(name, $a)) { name }}",
        "query foo ($b: string) { q(func: eq(name, $a)) { name }}",
        "query foo ($a: string) { q(func: eq(name, $b)) { name }}",
    ] {
        let mut variables = HashMap::new();
        variables.insert("$a".to_owned(), "alice".to_owned());
        parse(Request {
            text: q.to_owned(),
            variables,
        })
        .expect_err(q);
    }
}

#[test]
fn parse_graphql_var_pagination_root() {
    for q in [
        "query test($a: int = 2){ q(func: uid(0x1), first: $a) { name }}",
        "query test($a: int = 2){ q(func: uid(0x1), offset: $a) { name }}",
        "query test($a: int = 2){ q(func: uid(0x1), orderdesc: name, first: $a) { name }}",
        "query test($a: int = 2){ q(func: uid(0x1), orderdesc: name, offset: $a) { name }}",
        "query test($a: int = 2){ q(func: eq(name, \"abc\"), orderdesc: name, first: $a) { name }}",
        "query test($a: int = 2){ q(func: eq(name, \"abc\"), orderdesc: name, offset: $a) { name }}",
    ] {
        let mut variables = HashMap::new();
        variables.insert("$a".to_owned(), "3".to_owned());
        let res = parse(Request {
            text: q.to_owned(),
            variables,
        })
        .expect(q);
        let args = &res.query[0].args;
        assert!(
            args.get("first") == Some(&"3".to_owned())
                || args.get("offset") == Some(&"3".to_owned()),
            "{q}"
        );
    }
}

#[test]
fn parse_graphql_var_pagination_child() {
    for q in [
        "query test($a: int = 2){ q(func: uid(0x1)) { friend(first: $a) }}",
        "query test($a: int = 2){ q(func: uid(0x1)) { friend(offset: $a) }}",
        "query test($a: int = 2){ q(func: uid(0x1), orderdesc: name) { friend(first: $a) }}",
        "query test($a: int = 2){ q(func: uid(0x1), orderdesc: name) { friend(offset: $a) }}",
        "query test($a: int = 2){ q(func: eq(name, \"abc\"), orderdesc: name) { friend(first: $a) }}",
        "query test($a: int = 2){ q(func: eq(name, \"abc\"), orderdesc: name) { friend(offset: $a) }}",
    ] {
        let mut variables = HashMap::new();
        variables.insert("$a".to_owned(), "3".to_owned());
        let res = parse(Request {
            text: q.to_owned(),
            variables,
        })
        .expect(q);
        let args = &res.query[0].children[0].args;
        assert!(
            args.get("first") == Some(&"3".to_owned())
                || args.get("offset") == Some(&"3".to_owned()),
            "{q}"
        );
    }
}

#[test]
fn parse_graphql_var_pagination_root_multiple() {
    let mut variables = HashMap::new();
    variables.insert("$a".to_owned(), "3".to_owned());
    variables.insert("$b".to_owned(), "5".to_owned());
    variables.insert("$after".to_owned(), "0x123".to_owned());
    let res = parse(Request {
        text: "query test($a: int, $b: int, $after: string){\n\t\tq(func: uid(0x1), first: $a, offset: $b, after: $after, orderasc: name) {\n\t\t\tfriend\n\t\t}\n\t}".to_owned(),
        variables,
    })
    .expect("parse");
    let args = &res.query[0].args;
    assert_eq!(args["first"], "3");
    assert_eq!(args["offset"], "5");
    assert_eq!(args["after"], "0x123");
    assert_eq!(res.query[0].order[0].attr, "name");
}

#[test]
fn parse_graphql_var_array() {
    let cases: &[VarArrayCase] = &[
        (
            "query test($a: string){q(func: eq(name, [$a])) {name}}",
            &[("$a", "srfrog")],
            1,
        ),
        (
            "query test($a: string, $b: string){q(func: eq(name, [$a, $b])) {name}}",
            &[("$a", "srfrog"), ("$b", "horseman")],
            2,
        ),
        (
            "query test($a: string, $b: string, $c: string){q(func: eq(name, [$a, $b, $c])) {name}}",
            &[("$a", "srfrog"), ("$b", "horseman"), ("$c", "missbug")],
            3,
        ),
        (
            "query test($a: string){q(func: eq(name, [$a, \"mrtrout\"])) {name}}",
            &[("$a", "srfrog")],
            2,
        ),
        (
            "query test($a: string){q(func: eq(name, [\"mrtrout\", $a])) {name}}",
            &[("$a", "srfrog")],
            2,
        ),
    ];
    for (text, vars, count) in cases {
        let variables: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        let res = parse(Request {
            text: (*text).to_owned(),
            variables: variables.clone(),
        })
        .expect(text);
        assert_eq!(res.query.len(), 1);
        let func = res.query[0].func.as_ref().expect("func");
        assert_eq!(func.name, "eq");
        assert_eq!(func.args.len(), *count);
        for val in variables.values() {
            assert!(func.args.iter().any(|a| &a.value == val), "{text}");
        }
    }
}

#[test]
fn parse_graphql_var_array_uid_in() {
    let cases: &[VarArrayCase] = &[
        (
            "query test($a: string){q(func: uid_in(director.film, [$a])) {name}}",
            &[("$a", "0x4e472a")],
            1,
        ),
        (
            "query test($a: string, $b: string){q(func: uid_in(director.film, [$a, $b])) {name}}",
            &[("$a", "0x4e472a"), ("$b", "0x4e9545")],
            2,
        ),
        (
            "query test($a: string){q(func: uid_in(name, [$a, \"0x4e9545\"])) {name}}",
            &[("$a", "0x4e472a")],
            2,
        ),
        (
            "query test($a: string){q(func: uid_in(name, [\"0x4e9545\", $a])) {name}}",
            &[("$a", "0x4e472a")],
            2,
        ),
    ];
    for (text, vars, count) in cases {
        let variables: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        let res = parse(Request {
            text: (*text).to_owned(),
            variables: variables.clone(),
        })
        .expect(text);
        assert_eq!(res.query.len(), 1);
        let func = res.query[0].func.as_ref().expect("func");
        assert_eq!(func.name, "uid_in");
        assert_eq!(func.args.len(), *count);
        for val in variables.values() {
            assert!(func.args.iter().any(|a| &a.value == val), "{text}");
        }
    }
}

#[test]
fn uid_in_with_no_parse_errors() {
    parse_str(
        r"{
		schoolVar as q(func: uid(5000))
		me(func: uid(1, 23, 24 )) {
			friend @filter(uid_in(school, uid(schoolVar))) {
				name
			}
		}
	}",
    );
}

#[test]
fn uid_in_with_parse_errors() {
    let cases = [
        (
            "uid_in query with without argument",
            "{\n\t\t\t\tme(func: uid(1, 23, 24 )) {\n\t\t\t\t\tfriend @filter(uid_in(school, )) {\n\t\t\t\t\t\tname\n\t\t\t\t\t}\n\t\t\t\t}\n\t\t\t}",
            "Empty Argument",
        ),
        (
            "uid_in query with without argument (2)",
            "{\n\t\t\t\tme(func: uid(1, 23, 24 )) {\n\t\t\t\t\tfriend @filter(uid_in(school )) {\n\t\t\t\t\t\tname\n\t\t\t\t\t}\n\t\t\t\t}\n\t\t\t}",
            "uid_in function expects an argument, got none",
        ),
        (
            "query with nested uid without variable",
            "{\n\t\t\t\tme(func: uid(1, 23, 24 )) {\n\t\t\t\t\tfriend @filter(uid_in(school, uid(5000))) {\n\t\t\t\t\t\tname\n\t\t\t\t\t}\n\t\t\t\t}\n\t\t\t}",
            "Nested uid fn expects 1 uid variable, got 0",
        ),
        (
            "query with nested uid with variable and constant",
            "{\n\t\t\t\tuidVar as q(func: uid( 5000))\n\t\t\t\tme(func: uid(1, 23, 24 )) {\n\t\t\t\t\tfriend @filter(uid_in(school, uid(uidVar, 5001))) {\n\t\t\t\t\t\tname\n\t\t\t\t\t}\n\t\t\t\t}\n\t\t\t}",
            "Nested uid fn expects only uid variable, got UID",
        ),
        (
            "query with nested uid with two variables",
            "{\n\t\t\t\tuidVar1 as q(func: uid( 5000))\n\t\t\t\tuidVar2 as q(func: uid( 5000))\n\t\t\t\tme(func: uid(1, 23, 24 )) {\n\t\t\t\t\tfriend @filter(uid_in(school, uid(uidVar1, uidVar2))) {\n\t\t\t\t\t\tname\n\t\t\t\t\t}\n\t\t\t\t}\n\t\t\t}",
            "Nested uid fn expects 1 uid variable, got 2",
        ),
        (
            "query with nested uid with dql variable",
            "query queryWithDQL($schoolUID: string = \"5001\"){\n\t\t\t\tme(func: uid(1, 23, 24 )){\n\t\t\t\t\tfriend @filter(uid_in(school, uid( $schoolUID))) {\n\t\t\t\t\t\tname\n\t\t\t\t\t}\n\t\t\t\t}\n\t\t\t}",
            "Nested uid fn expects 1 uid variable, got 0",
        ),
    ];
    for (name, query, expected) in cases {
        let err = parse_err(query);
        assert!(err.contains(expected), "{name}: {err}");
    }
}

#[test]
fn parse_graphql_value_array() {
    let res = parse_str(
        r#"
	{
		q(func: eq(name, ["srfrog", "horseman"])) {
			name
		}
	}"#,
    );
    assert_eq!(res.query.len(), 1);
    let func = res.query[0].func.as_ref().expect("func");
    assert_eq!(func.name, "eq");
    assert_eq!(func.args.len(), 2);
    assert_eq!(func.args[0].value, "srfrog");
    assert_eq!(func.args[1].value, "horseman");
}

#[test]
fn parse_graphql_mixed_var_array() {
    let mut variables = HashMap::new();
    variables.insert("$a".to_owned(), "dos".to_owned());
    variables.insert("$b".to_owned(), "tres".to_owned());
    variables.insert("$c".to_owned(), "cinco".to_owned());
    let res = parse(Request {
        text: "query test($a: string, $b: string, $c: string){\n\t\tq(func: eq(name, [\"uno\", $a, $b, \"cuatro\", $c])) {\n\t\t\tname\n\t\t}\n\t}"
            .to_owned(),
        variables,
    })
    .expect("parse");
    assert_eq!(res.query.len(), 1);
    let func = res.query[0].func.as_ref().expect("func");
    assert_eq!(func.name, "eq");
    assert_eq!(func.args.len(), 5);
    assert_eq!(func.args[0].value, "uno");
    assert_eq!(func.args[1].value, "dos");
    assert_eq!(func.args[2].value, "tres");
    assert_eq!(func.args[3].value, "cuatro");
    assert_eq!(func.args[4].value, "cinco");
}
