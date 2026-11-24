//! Parser tests: facets and facet filters (port of Go's `parser_test.go`, part 5 of 8).
//!
//! Split from `parser_tests.rs` to honor the 1000-line file rule; shared
//! helpers live in [`common`].

#[path = "common/mod.rs"]
mod common;

use common::{child_attrs, parse_err, parse_str};

#[test]
fn parse_facets_error_1() {
    let err = parse_err(
        r"
	query {
		me(func: uid(0x1)) {
			friends @facets {
				name @facets(facet1,, facet2)
			}
			hometown
			age
		}
	}
",
    );
    assert!(err.contains("Consecutive commas not allowed."), "{err}");
}

#[test]
fn parse_facets_var_error() {
    let err = parse_err(
        r"
	query {
		me(func: uid(0x1)) {
			friends @facets {
				name @facets(facet1, b as)
			}
			hometown
			age
		}
	}
",
    );
    assert!(err.contains("Expected name in facet list"), "{err}");
}

#[test]
fn parse_facets_error_2() {
    let err = parse_err(
        r"
	query {
		me(func: uid(0x1)) {
			friends @facets {
				name @facets(facet1 facet2)
			}
			hometown
			age
		}
	}
",
    );
    assert!(err.contains("Expected ( after func name [facet1]"), "{err}");
}

#[test]
fn parse_facets_order_error_1() {
    let err = parse_err(
        r"
	query {
		me(func: uid(0x1)) {
			friends @facets(orderdesc: orderdesc: closeness) {
				name
			}
		}
	}
",
    );
    assert!(
        err.contains("Expected ( after func name [orderdesc]"),
        "{err}"
    );
}

#[test]
fn parse_facets_order_error_2() {
    let err = parse_err(
        r"
	query {
		me(func: uid(0x1)) {
			friends @facets(a as b as closeness) {
				name
			}
		}
	}
",
    );
    assert!(err.contains("Expected ( after func name [a]"), "{err}");
}

#[test]
fn parse_facets_order_with_alias() {
    let res = parse_str(
        r"
	query {
		me(func: uid(0x1)) {
			friends @facets(orderdesc: closeness, b as some, order: abc, key, key1: val, abcd) {
				val(b)
			}
		}
	}
",
    );
    let facets = res.query[0].children[0].facets.as_ref().expect("facets");
    assert_eq!(facets.param.len(), 6);
    assert_eq!(facets.param[0].alias, "order");
    assert_eq!(facets.param[0].key, "abc");
    assert_eq!(facets.param[1].key, "abcd");
    assert_eq!(facets.param[5].key, "val");
    assert_eq!(facets.param[5].alias, "key1");
}

#[test]
fn parse_facets_duplicate_var_error() {
    let err = parse_err(
        r"
	query {
		me(func: uid(0x1)) {
			friends @facets(a as closeness, b as closeness) {
				name
			}
		}
	}
",
    );
    assert!(err.contains("Duplicate variable mappings"), "{err}");
}

#[test]
fn parse_facets_order_var() {
    parse_str(
        r"
	query {
		me1(func: uid(0x1)) {
			friends @facets(orderdesc: a as b) {
				name
			}
		}
		me2(func: uid(a)) { }
	}
",
    );
}

#[test]
fn parse_facets_order_var_2() {
    let err = parse_err(
        r"
	query {
		me(func: uid(0x1)) {
			friends @facets(a as orderdesc: b) {
				name
			}
		}
		me(func: uid(a)) {

		}
	}
",
    );
    assert!(err.contains("Expected ( after func name [a]"), "{err}");
}

#[test]
fn parse_facets() {
    let res = parse_str(
        r"
	query {
		me(func: uid(0x1)) {
			friends @facets(orderdesc: closeness) {
				name
			}
		}
	}
",
    );
    assert_eq!(child_attrs(&res.query[0]), ["friends"]);
    assert!(res.query[0].children[0].facets.is_some());
    assert_eq!(child_attrs(&res.query[0].children[0]), ["name"]);
    assert_eq!(res.query[0].children[0].facets_order[0].key, "closeness");
    assert!(res.query[0].children[0].facets_order[0].desc);
}

#[test]
fn parse_orderby_multiple_facets() {
    let res = parse_str(
        r"
	query {
		me(func: uid(0x1)) {
			friends @facets(orderdesc: closeness, orderasc: since) {
				name
			}
		}
	}
",
    );
    assert_eq!(child_attrs(&res.query[0]), ["friends"]);
    assert!(res.query[0].children[0].facets.is_some());
    assert_eq!(child_attrs(&res.query[0].children[0]), ["name"]);
    assert_eq!(res.query[0].children[0].facets_order.len(), 2);
    assert_eq!(res.query[0].children[0].facets_order[0].key, "closeness");
    assert!(res.query[0].children[0].facets_order[0].desc);
    assert_eq!(res.query[0].children[0].facets_order[1].key, "since");
    assert!(!res.query[0].children[0].facets_order[1].desc);
}

#[test]
fn parse_orderby_multiple_facets_with_alias() {
    let res = parse_str(
        r"
	query {
		me(func: uid(0x1)) {
			friends @facets(orderdesc: closeness, orderasc: since, score, location:from) {
				name
			}
		}
	}
",
    );
    assert_eq!(child_attrs(&res.query[0]), ["friends"]);
    assert!(res.query[0].children[0].facets.is_some());
    assert_eq!(child_attrs(&res.query[0].children[0]), ["name"]);
    assert_eq!(res.query[0].children[0].facets_order.len(), 2);
    assert_eq!(res.query[0].children[0].facets_order[0].key, "closeness");
    assert!(res.query[0].children[0].facets_order[0].desc);
    assert_eq!(res.query[0].children[0].facets_order[1].key, "since");
    assert!(!res.query[0].children[0].facets_order[1].desc);
    assert_eq!(
        res.query[0].children[0]
            .facets
            .as_ref()
            .expect("facets")
            .param
            .len(),
        4
    );
    assert!(res.query[0].children[0].facets_filter.is_none());
    assert!(res.query[0].children[0].facet_var.is_empty());
    let from = res.query[0].children[0]
        .facets
        .as_ref()
        .expect("facets")
        .param
        .iter()
        .find(|p| p.key == "from")
        .expect("from");
    assert_eq!(from.alias, "location");
}

#[test]
fn parse_orderby_same_facets_multiple_times() {
    let err = parse_err(
        r"
	query {
		me(func: uid(0x1)) {
			friends @facets(orderdesc: closeness, orderasc: closeness) {
				name
			}
		}
	}
",
    );
    assert!(
        err.contains("Sorting by facet: [closeness] can only be done once"),
        "{err}"
    );
}

#[test]
fn parse_orderby_facet() {
    let res = parse_str(
        r"
	query {
		me(func: uid(0x1)) {
			friends @facets {
				name @facets(facet1)
			}
			hometown
			age
		}
	}
",
    );
    assert_eq!(child_attrs(&res.query[0]), ["friends", "hometown", "age"]);
    let friends = &res.query[0].children[0];
    assert!(friends.facets.as_ref().expect("facets").all_keys);
    assert_eq!(child_attrs(friends), ["name"]);
    let name = &friends.children[0];
    let facets = name.facets.as_ref().expect("facets");
    assert!(!facets.all_keys);
    assert_eq!(facets.param[0].key, "facet1");
}

#[test]
fn parse_facets_multiple() {
    let res = parse_str(
        r"
	query {
		me(func: uid(0x1)) {
			friends @facets {
				name @facets(key1, key2, key3)
			}
			hometown
			age
		}
	}
",
    );
    assert_eq!(child_attrs(&res.query[0]), ["friends", "hometown", "age"]);
    let friends = &res.query[0].children[0];
    assert!(friends.facets.as_ref().expect("facets").all_keys);
    assert_eq!(child_attrs(friends), ["name"]);
    let name = &friends.children[0];
    let facets = name.facets.as_ref().expect("facets");
    assert!(!facets.all_keys);
    assert_eq!(facets.param.len(), 3);
}

#[test]
fn parse_facets_alias() {
    let res = parse_str(
        r"
	query {
		me(func: uid(0x1)) {
			friends @facets {
				name @facets(a1: key1, a2: key2, a3: key3)
			}
		}
	}
",
    );
    let friends = &res.query[0].children[0];
    assert!(friends.facets.as_ref().expect("facets").all_keys);
    assert_eq!(child_attrs(friends), ["name"]);
    let node = friends.children[0].facets.as_ref().expect("facets");
    assert!(!node.all_keys);
    assert_eq!(node.param.len(), 3);
    assert_eq!(node.param[0].alias, "a1");
    assert_eq!(node.param[0].key, "key1");
    assert_eq!(node.param[2].alias, "a3");
    assert_eq!(node.param[2].key, "key3");
}

#[test]
fn parse_facets_multiple_var() {
    let res = parse_str(
        r"
	query {
		me(func: uid(0x1)) {
			friends @facets {
				name @facets(a as key1, key2, b as key3)
			}
			hometown
			age
		}
		h(func: uid(a, b)) {
			uid
		}
	}
",
    );
    assert_eq!(child_attrs(&res.query[0]), ["friends", "hometown", "age"]);
    let friends = &res.query[0].children[0];
    assert!(friends.facets.as_ref().expect("facets").all_keys);
    assert_eq!(child_attrs(friends), ["name"]);
    let name = &friends.children[0];
    let facets = name.facets.as_ref().expect("facets");
    assert!(!facets.all_keys);
    assert_eq!(facets.param.len(), 3);
    // Go map reads of missing keys yield ""; mirror with unwrap_or.
    assert_eq!(name.facet_var.get("key1").map_or("", String::as_str), "a");
    assert_eq!(name.facet_var.get("key2").map_or("", String::as_str), "");
    assert_eq!(name.facet_var.get("key3").map_or("", String::as_str), "b");
}

#[test]
fn parse_facets_multiple_repeat() {
    let res = parse_str(
        r"
	query {
		me(func: uid(0x1)) {
			friends @facets {
				name @facets(key1, key2, key3, key1)
			}
			hometown
			age
		}
	}
",
    );
    assert_eq!(child_attrs(&res.query[0]), ["friends", "hometown", "age"]);
    let friends = &res.query[0].children[0];
    assert!(friends.facets.as_ref().expect("facets").all_keys);
    assert_eq!(child_attrs(friends), ["name"]);
    let name = &friends.children[0];
    let facets = name.facets.as_ref().expect("facets");
    assert!(!facets.all_keys);
    assert_eq!(facets.param.len(), 3);
}

#[test]
fn parse_facets_empty() {
    let res = parse_str(
        r"
	query {
		me(func: uid(0x1)) {
			friends @facets() {
			}
			hometown
			age
		}
	}
",
    );
    assert_eq!(child_attrs(&res.query[0]), ["friends", "hometown", "age"]);
    let facets = res.query[0].children[0].facets.as_ref().expect("facets");
    assert!(!facets.all_keys);
    assert!(facets.param.is_empty());
}

#[test]
fn parse_facets_fail_1() {
    let err = parse_err(
        r"
	query {
		me(func: uid(0x1)) {
			friends @facets(key1,, key2) {
			}
			hometown
			age
		}
	}
",
    );
    assert!(err.contains("Consecutive commas not allowed."), "{err}");
}

#[test]
fn parse_repeat_args_error_1() {
    let err = parse_err(
        r#"
	{
  		me(func: anyoftext(Text, "biology"), func: anyoftext(Text, "science")) {
			Text
  		}
	}
	"#,
    );
    assert!(err.contains("Only one function allowed at root"), "{err}");
}

#[test]
fn parse_repeat_args_error_2() {
    let err = parse_err(
        r#"
	{
  		me(func: anyoftext(Text, "science")) {
			Text(first: 1, first: 4)
  		}
	}
	"#,
    );
    assert!(err.contains("Got repeated key \"first\""), "{err}");
}

#[test]
fn facets_filter_simple() {
    let res = parse_str(
        r"
	       {
		me(func: uid(0x1)) {
			name
			friend @facets(eq(close, true)) {
				name
				gender
			}
		}
	}
",
    );
    assert_eq!(child_attrs(&res.query[0]), ["name", "friend"]);
    assert!(res.query[0].children[1].facets.is_none());
    assert!(res.query[0].children[1].facets_filter.is_some());
    assert_eq!(
        res.query[0].children[1]
            .facets_filter
            .as_ref()
            .expect("filter")
            .debug_string(),
        r#"(eq close "true")"#
    );
}

#[test]
fn facets_filter_all() {
    let res = parse_str(
        r"
	       {
		me(func: uid(0x1)) {
			name
			friend @facets(eq(close, true) or eq(family, true)) @facets(close, family, since) {
				name @facets
				gender
			}
		}
	}
",
    );
    assert_eq!(child_attrs(&res.query[0]), ["name", "friend"]);
    let facets = res.query[0].children[1].facets.as_ref().expect("facets");
    assert_eq!(facets.param[0].key, "close");
    assert_eq!(facets.param[1].key, "family");
    assert_eq!(facets.param[2].key, "since");
    assert!(res.query[0].children[1].facets_filter.is_some());
    assert_eq!(
        res.query[0].children[1]
            .facets_filter
            .as_ref()
            .expect("filter")
            .debug_string(),
        r#"(OR (eq close "true") (eq family "true"))"#
    );
    assert_eq!(child_attrs(&res.query[0].children[1]), ["name", "gender"]);
    let name_child = &res.query[0].children[1].children[0];
    assert!(name_child.facets.is_some());
    assert!(name_child.facets_filter.is_none());
    let gender_child = &res.query[0].children[1].children[1];
    assert!(gender_child.facets.is_none());
    assert!(gender_child.facets_filter.is_none());
}

#[test]
fn facets_filter_fail() {
    let err = parse_err(
        r"
	       {
		me(func: uid(0x1)) {
			name
			friend @facets @facets(close, since) {
				name
				gender
			}
		}
	}
",
    );
    assert!(err.contains("Only one facets allowed"), "{err}");
}

#[test]
fn facets_filter_fail_2() {
    let err = parse_err(
        r"
	       {
		me(func: uid(0x1)) {
			name
			friend @facets(eq(close, true)) @facets(eq(family, true)) {
				name
				gender
			}
		}
	}
",
    );
    assert!(err.contains("Only one facets filter allowed"), "{err}");
}

#[test]
fn facets_filter_fail_3() {
    let err = parse_err(
        r"
	{
		K as var(func: uid(0x0a)) {
			L AS friends
		}
		me(func: uid(K)) {
			friend @facets(uid(L)) {
				name
			}
		}
	}
",
    );
    assert!(
        err.contains("variables are not allowed in facets filter"),
        "{err}"
    );
}

#[test]
fn facets_filter_fail_root() {
    let err = parse_err(
        r"
	{
		me(func: uid(0x1)) @facets(eq(some-facet, true)) {
			friend	{
				name
			}
		}
	}
",
    );
    assert!(err.contains("Unknown directive [facets]"), "{err}");
}

#[test]
fn facets_filter_at_value() {
    let res = parse_str(
        r"
	{
		me(func: uid(0x1)) {
			friend	{
				name @facets(eq(some.facet, true))
			}
		}
	}
",
    );
    assert_eq!(child_attrs(&res.query[0].children[0]), ["name"]);
    let name_child = &res.query[0].children[0].children[0];
    assert!(name_child.facets_filter.is_some());
    assert_eq!(
        name_child
            .facets_filter
            .as_ref()
            .expect("filter")
            .debug_string(),
        r#"(eq some.facet "true")"#
    );
}

#[test]
fn parse_query_with_attr_lang() {
    let res = parse_str(
        r"
	{
		me(func: uid(0x1)) {
			name
			friend(first:5, orderasc: name@en) {
				name@en
			}
		}
	}
",
    );
    assert_eq!(res.query.len(), 1);
    assert_eq!(res.query[0].children[1].order[0].attr, "name");
    assert_eq!(res.query[0].children[1].order[0].langs, ["en"]);
}

#[test]
fn parse_query_with_attr_lang_2() {
    let res = parse_str(
        r"
	{
	  me(func:regexp(name, /^[a-zA-z]*[^Kk ]?[Nn]ight/), orderasc: name@en, first:5) {
		name@en
		name@de
		name@it
	  }
	}
",
    );
    assert_eq!(res.query.len(), 1);
    assert_eq!(res.query[0].order[0].attr, "name");
    assert_eq!(res.query[0].order[0].langs, ["en"]);
}
