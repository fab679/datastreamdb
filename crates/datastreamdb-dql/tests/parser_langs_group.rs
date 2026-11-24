//! Parser tests: language tags, normalize, and group-by (port of Go's `parser_test.go`, part 4 of 8).
//!
//! Split from `parser_tests.rs` to honor the 1000-line file rule; shared
//! helpers live in [`common`].

#[path = "common/mod.rs"]
mod common;

use common::{parse_err, parse_str};

#[test]
fn langs() {
    let res = parse_str(
        r"
	query {
		me(func: uid(1)) {
			name@en,name@en:ru:hu
		}
	}
	",
    );
    assert_eq!(res.query[0].children.len(), 2);
    assert_eq!(res.query[0].children[0].attr, "name");
    assert_eq!(res.query[0].children[0].langs, ["en"]);
    assert_eq!(res.query[0].children[1].attr, "name");
    assert_eq!(res.query[0].children[1].langs, ["en", "ru", "hu"]);
}

#[test]
fn all_langs() {
    let res = parse_str(
        r"
	query {
		me(func: uid(1)) {
			name@*
		}
	}
	",
    );
    assert_eq!(res.query[0].children.len(), 1);
    assert_eq!(res.query[0].children[0].attr, "name");
    assert_eq!(res.query[0].children[0].langs, ["*"]);
}

#[test]
fn langs_invalid_1() {
    let err = parse_err(
        r"
	query {
		me(func: uid(1)) {
			name@en@ru
		}
	}
	",
    );
    assert!(
        err.contains("Expected directive or language list, got @ru"),
        "{err}"
    );
}

#[test]
fn langs_invalid_2() {
    let err = parse_err(
        r"
	query {
		me(func: uid(1)) {
			@en:ru
		}
	}
	",
    );
    assert!(err.contains("Invalid use of directive."), "{err}");
}

#[test]
fn langs_invalid_3() {
    let err = parse_err(
        r"
	query {
		me(func: uid(1)) {
			name@en:ru, @en:ru
		}
	}
	",
    );
    assert!(
        err.contains("Expected directive or language list, got @en"),
        "{err}"
    );
}

#[test]
fn langs_invalid_4() {
    let err = parse_err(
        r"
	query {
		me(func: uid(1)) {
			name@
		}
	}
	",
    );
    assert!(
        err.contains("Unrecognized character in lexDirective: U+000A"),
        "{err}"
    );
}

#[test]
fn langs_invalid_5() {
    let err = parse_err(
        r"
	query {
		me(func: uid(1)) {
			name@<something.wrong>
		}
	}
	",
    );
    assert!(
        err.contains("Unrecognized character in lexDirective: U+003C '<'"),
        "{err}"
    );
}

#[test]
fn langs_invalid_6() {
    let err = parse_err(
        r"
		{
			me(func: uid(0x1004)) {
				name@hi:cn:...
			}
		}
	",
    );
    assert!(
        err.contains("Expected only one dot(.) while parsing language list."),
        "{err}"
    );
}

#[test]
fn langs_invalid_7() {
    let err = parse_err(
        r"
		{
			me(func: uid(0x1004)) {
				name@...
			}
		}
	",
    );
    assert!(
        err.contains("Expected only one dot(.) while parsing language list."),
        "{err}"
    );
}

#[test]
fn langs_invalid_8() {
    let err = parse_err(
        r"
	query {
		me(func: uid(1)) {
			name@*:en
		}
	}
	",
    );
    assert!(
        err.contains("If * is used, no other languages are allowed in the language list"),
        "{err}"
    );
}

#[test]
fn langs_invalid_9() {
    let err = parse_err(
        r#"
	query {
		me(func: eqs(name@*, "Amir")) {
			name@en
		}
	}
	"#,
    );
    assert!(
        err.contains("The * symbol cannot be used as a valid language inside functions"),
        "{err}"
    );
}

#[test]
fn langs_invalid_10() {
    let err = parse_err(
        r"
	query {
		me(func: uid(1)) {
			name@.:*
		}
	}
	",
    );
    assert!(
        err.contains("If * is used, no other languages are allowed in the language list"),
        "{err}"
    );
}

#[test]
fn langs_filter() {
    let res = parse_str(
        r#"
	query {
		me(func: uid(0x0a)) {
			friends @filter(alloftext(descr@en, "something")) {
				name
			}
			gender,age
			hometown
		}
	}
"#,
    );
    let func = res.query[0].children[0]
        .filter
        .as_ref()
        .expect("filter")
        .func
        .as_ref()
        .expect("func");
    assert_eq!(func.attr, "descr");
    assert_eq!(func.lang, "en");
}

#[test]
fn langs_filter_error_1() {
    let err = parse_err(
        r#"
	query {
		me(func: uid(0x0a)) {
			friends @filter(alloftext(descr@en@de, "something")) {
				name
			}
			gender,age
			hometown
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
fn langs_filter_error_2() {
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
	}
"#,
    );
    assert!(
        err.contains("Unrecognized character in lexDirective: U+002C ','"),
        "{err}"
    );
}

#[test]
fn langs_function() {
    let res = parse_str(
        r#"
	query {
		me(func:alloftext(descr@en, "something")) {
			friends {
				name
			}
			gender,age
			hometown
		}
	}
"#,
    );
    let func = res.query[0].func.as_ref().expect("func");
    assert_eq!(func.attr, "descr");
    assert_eq!(func.lang, "en");
}

#[test]
fn langs_function_multiple_langs() {
    let err = parse_err(
        r#"
	query {
		me(func:alloftext(descr@hi:en, "something")) {
			friends {
				name
			}
			gender,age
			hometown
		}
	}
"#,
    );
    assert!(err.contains("Expected arg after func [alloftext]"), "{err}");
    assert!(err.contains("\":\""), "{err}");
}

#[test]
fn parse_normalize() {
    let res = parse_str(
        r"
	query {
		me(func: uid( 0x3)) @normalize {
			friends {
				name
			}
			gender
			hometown
		}
}
",
    );
    assert!(res.query[0].normalize);
}

#[test]
fn parse_groupby_root() {
    let res = parse_str(
        r"
	query {
		me(func: uid(1, 2, 3)) @groupby(friends) {
				a as count(uid)
		}

		groups(func: uid(a)) {
			uid
			val(a)
		}
	}
",
    );
    assert_eq!(res.query[0].groupby_attrs.len(), 1);
    assert_eq!(res.query[0].groupby_attrs[0].attr, "friends");
    assert_eq!(res.query[0].children[0].var, "a");
}

#[test]
fn parse_groupby_with_count_var() {
    let res = parse_str(
        r"
	query {
		me(func: uid(0x1)) {
			friends @groupby(friends) {
				a as count(uid)
			}
			hometown
			age
		}

		groups(func: uid(a)) {
			uid
			val(a)
		}
	}
",
    );
    assert_eq!(res.query[0].children[0].groupby_attrs.len(), 1);
    assert_eq!(res.query[0].children[0].groupby_attrs[0].attr, "friends");
    assert_eq!(res.query[0].children[0].children[0].var, "a");
}

#[test]
fn parse_groupby_with_max_var() {
    let res = parse_str(
        r"
	query {
		me(func: uid(0x1)) {
			friends @groupby(friends) {
				a as max(first-name@en:ta)
			}
			hometown
			age
		}

		groups(func: uid(a)) {
			uid
			val(a)
		}
	}
",
    );
    assert_eq!(res.query[0].children[0].groupby_attrs.len(), 1);
    assert_eq!(res.query[0].children[0].groupby_attrs[0].attr, "friends");
    assert_eq!(res.query[0].children[0].children[0].attr, "first-name");
    assert_eq!(res.query[0].children[0].children[0].langs, ["en", "ta"]);
    assert_eq!(res.query[0].children[0].children[0].var, "a");
}

#[test]
fn parse_groupby() {
    let res = parse_str(
        r"
	query {
		me(func: uid(0x1)) {
			friends @groupby(name@en) {
				count(uid)
			}
			hometown
			age
		}
	}
",
    );
    assert_eq!(res.query[0].children[0].groupby_attrs.len(), 1);
    assert_eq!(res.query[0].children[0].groupby_attrs[0].attr, "name");
    assert_eq!(res.query[0].children[0].groupby_attrs[0].langs[0], "en");
}

#[test]
fn parse_groupby_with_alias() {
    let res = parse_str(
        r"
	query {
		me(func: uid(0x1)) {
			friends @groupby(name) {
				GroupCount: count(uid)
			}
			hometown
			age
		}
	}
",
    );
    assert_eq!(res.query[0].children[0].groupby_attrs.len(), 1);
    assert_eq!(res.query[0].children[0].groupby_attrs[0].attr, "name");
    assert_eq!(res.query[0].children[0].children[0].alias, "GroupCount");
}

#[test]
fn parse_groupby_with_alias_for_key() {
    let res = parse_str(
        r"
	query {
		me(func: uid(0x1)) {
			friends @groupby(Name: name, SchooL: school) {
				count(uid)
			}
			hometown
			age
		}
	}
",
    );
    assert_eq!(res.query[0].children[0].groupby_attrs.len(), 2);
    assert_eq!(res.query[0].children[0].groupby_attrs[0].alias, "Name");
    assert_eq!(res.query[0].children[0].groupby_attrs[1].alias, "SchooL");
}

#[test]
fn parse_groupby_with_alias_for_error() {
    let err = parse_err(
        r"
	query {
		me(func: uid(0x1)) {
			friends @groupby(first: 10, SchooL: school) {
				count(uid)
			}
			hometown
			age
		}
	}
",
    );
    assert!(
        err.contains("Can't use keyword first as alias in groupby"),
        "{err}"
    );
}

#[test]
fn parse_groupby_error() {
    let err = parse_err(
        r"
	query {
		me(func: uid(0x1)) {
			friends @groupby(name) {
				name
				count(uid)
			}
			hometown
			age
		}
	}
",
    );
    assert!(
        err.contains("Only aggregator/count functions allowed inside @groupby"),
        "{err}"
    );
}
