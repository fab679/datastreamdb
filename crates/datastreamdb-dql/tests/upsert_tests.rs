//! Port of Go's `upsert_test.go` and `dql_test.go`.

use datastreamdb_dql::parse_dql;

fn dql_ok(query: &str) {
    parse_dql(query).expect("parse_dql");
}

fn dql_err(query: &str) -> String {
    parse_dql(query).expect_err("must fail").to_string()
}

#[test]
fn invalid_block_err() {
    dql_ok(
        r"
query {
  me(func: eq(age, 34)) {
    uid
    friend {
      uid
      age
    }
  }
}",
    );
}

#[test]
fn extra_right_curl_err() {
    let err = dql_err(
        r"
upsert {
  query {
    me(func: eq(age, 34)) {
      uid
      friend {
        uid
        age
      }
    }
  }
}
}
",
    );
    assert!(err.contains("Too many right curl"), "{err}");
}

#[test]
fn no_mutation_err() {
    let err = dql_err(
        r"
upsert {
  query {
    me(func: eq(age, 34)) {
      uid
      friend {
        uid age
      }
    }
  }
}
",
    );
    assert!(err.contains("Empty mutation block"), "{err}");
}

#[test]
fn multiple_query_err() {
    let err = dql_err(
        r#"
upsert {
  query {
    me(func: eq(age, 34)) {
      uid
      friend {
        uid
        age
      }
    }
  }

  query {
    me2(func: eq(age, 34)) {
      uid
      friend {
        uid
        age
      }
    }
  }

  mutation {
    set {
      "_:user1" <age> "45" .
    }
  }
}
"#,
    );
    assert!(
        err.contains("Multiple query ops inside upsert block"),
        "{err}"
    );
}

#[test]
fn empty_upsert_err() {
    let err = dql_err("upsert {}");
    assert!(err.contains("Empty mutation block"), "{err}");
}

#[test]
fn no_right_curl_err() {
    let err = dql_err("upsert {");
    assert!(err.contains("Unclosed upsert block"), "{err}");
}

#[test]
fn incomplete_block_err() {
    let err = dql_err(
        r#"
upsert {
  mutation {
    set {
      "_:user1" <age> "45" .
    }
  }

  query {
    me(func: eq(age, "{
"#,
    );
    assert!(err.contains("Unexpected end of input"), "{err}");
}

#[test]
fn missing_query_err() {
    let err = dql_err(
        r#"
upsert {
  mutation {
    set {
      "_:user1" <age> "45" .
    }
  }
}
"#,
    );
    assert!(err.contains("Query op not found in upsert block"), "{err}");
}

#[test]
fn upsert_with_fragment() {
    dql_ok(
        r#"
upsert {
  query {
    me(func: eq(age, 34)) {
      ...fragmentA
      friend {
        ...fragmentA
        age
      }
    }
  }

  fragment fragmentA {
    uid
  }

  mutation {
    set {
      "_:user1" <age> "45" .
    }
  }
}
"#,
    );
}

#[test]
fn upsert_ex_1() {
    dql_ok(
        r#"
upsert {
  query {
    me(func: eq(age, "{")) {
      uid
      friend {
        uid
        age
      }
    }
  }

  mutation {
    set {
      "_:user1" <age> "45" .
    }
  }
}
"#,
    );
}

#[test]
fn upsert_with_spaces() {
    dql_ok(
        r#"
upsert

{
  query

  {
    me(func: eq(age, "{")) {
      uid
      friend {
        uid
        age
      }
    }
  }

  mutation

  {
    set
    {
      "_:user1" <age> "45" .

      # This is a comment
      "_:user1" <name> "{vishesh" .
    }}
}
"#,
    );
}

#[test]
fn upsert_with_blank_node() {
    dql_ok(
        r#"
upsert {
  mutation {
    set {
      "_:user1" <age> "45" .
    }
  }

  query {
    me(func: eq(age, 34)) {
      uid
      friend {
        uid
        age
      }
    }
  }
}
"#,
    );
}

#[test]
fn upsert_mutation_then_query() {
    dql_ok(
        r#"
upsert {
  query {
    me(func: eq(age, 34)) {
      uid
      friend {
        uid
        age
      }
    }
  }

  mutation {
    set {
      "_:user1" <age> "45" .
    }
  }
}
"#,
    );
}

#[test]
fn upsert_with_filter() {
    dql_ok(
        r#"
upsert {
  query {
    me(func: eq(age, 34)) @filter(ge(name, "user")) {
      uid
      friend {
        uid
        age
      }
    }
  }

  mutation {
    set {
      uid(a) <age> "45"
      uid(b) <age> "45" .
    }
  }
}"#,
    );
}

#[test]
fn conditional_upsert_with_newlines() {
    dql_ok(
        r#"
upsert {
  query {
    me(func: eq(age, 34)) @filter(ge(name, "user")) {
      m as uid
      friend {
        f as uid
        age
      }
    }
  }

  mutation @if(eq(len(m), 1)
               AND
               gt(len(f), 0)) {
    set {
      uid(m) <age> "45" .
      uid(f) <age> "45" .
    }
  }
}
"#,
    );
}

#[test]
fn conditional_upsert_func_tree() {
    dql_ok(
        r#"
upsert {
  query {
    me(func: eq(age, 34)) @filter(ge(name, "user")) {
      uid
      friend {
        uid
        age
      }
    }
  }

  mutation @if( ( eq(len(m), 1)
                  OR
                  lt(90, len(h)))
                AND
                gt(len(f), 0)) {
    set {
      uid(m) <age> "45" .
      uid(f) <age> "45" .
    }
  }
}
"#,
    );
}

#[test]
fn conditional_upsert_multiple_func_arg() {
    dql_ok(
        r#"
upsert {
  query {
    me(func: eq(age, 34)) @filter(ge(name, "user")) {
      uid
      friend {
        uid
        age
      }
    }
  }

  mutation @if( ( eq(len(m), len(t))
                  OR
                  lt(90, len(h)))
                AND
                gt(len(f), 0)) {
    set {
      uid(m) <age> "45" .
      uid(f) <age> "45" .
    }
  }
}
"#,
    );
}

#[test]
fn conditional_upsert_err_missing_right_round() {
    let err = dql_err(
        r#"
upsert {
  query {
    me(func: eq(age, 34)) @filter(ge(name, "user")) {
      uid
      friend {
        uid
        age
      }
    }
  }

  mutation @if(eq(len(m, 1)
               AND
               gt(len(f), 0)) {
    set {
      uid(m) <age> "45" .
      uid(f) <age> "45" .
    }
  }
}
"#,
    );
    assert!(err.contains("Matching brackets not found"), "{err}");
}

#[test]
fn conditional_upsert_err_unclosed() {
    let err = dql_err("upsert {\n  mutation @if(eq(len(m), 1) AND gt(len(f), 0))");
    assert!(err.contains("Unclosed mutation action"), "{err}");
}

#[test]
fn conditional_upsert_err_invalid_if() {
    let err = dql_err("upsert {\n  mutation @if");
    assert!(err.contains("Matching brackets not found"), "{err}");
}

#[test]
fn conditional_upsert_err_wrong_if() {
    let err = dql_err(
        r#"upsert {
  mutation @fi( ( eq(len(m), 1)
                  OR
                  lt(len(h), 90))
                AND
                gt(len(f), 0)) {
    set {
      uid(m) <age> "45" .
      uid(f) <age> "45" .
    }
  }

  query {
    me(func: eq(age, 34)) @filter(ge(name, "user")) {
      uid
      friend {
        uid
        age
      }
    }
  }
}
"#,
    );
    assert!(err.contains("Expected @if, found [@fi]"), "{err}");
}

#[test]
fn multiple_mutation() {
    let req = parse_dql(
        r#"
upsert {
  mutation @if(eq(len(m), 1)) {
    set {
      uid(m) <age> "45" .
    }
  }

  mutation @if(not(eq(len(m), 1))) {
    set {
      uid(f) <age> "45" .
    }
  }

  mutation {
    set {
      _:user <age> "45" .
    }
  }

  query {
    me(func: eq(age, 34)) @filter(ge(name, "user")) {
      uid
    }
  }
}"#,
    )
    .expect("parse");
    assert_eq!(req.mutations.len(), 3);
}

#[test]
fn multiple_mutation_different_order() {
    let req = parse_dql(
        r#"
upsert {
  mutation @if(eq(len(m), 1)) {
    set {
      uid(m) <age> "45" .
    }
  }

  query {
    me(func: eq(age, 34)) @filter(ge(name, "user")) {
      uid
    }
  }

  mutation @if(not(eq(len(m), 1))) {
    set {
      uid(f) <age> "45" .
    }
  }

  mutation {
    set {
      _:user <age> "45" .
    }
  }
}"#,
    )
    .expect("parse");
    assert_eq!(req.mutations.len(), 3);
}

#[test]
fn parse_dql_cases() {
    // Port of TestParseDQL.
    for query in [
        "{ set {_:a <name> \"Alice\" .}}",
        "schema{}",
        r#"upsert {
			query {
				ns(func: eq(datastreamdb.namespace.name, "ns1")) {
					q as var
					datastreamdb.namespace.id
				}
			}

			mutation {
				delete {
					uid(q) * * .
				}
			}
		}"#,
        r#"{
			q(func: eq(datastreamdb.namespace.name, "ns1")) {
				uid
				datastreamdb.type
				datastreamdb.namespace.id
			}
		}"#,
        r" query test($a: int) {
			q(func: uid(0x1)) {
				x as count(uid)
				p : math(x + $a)
			}
		}",
        r"{
			me(func: uid(1)) {
				Upvote {
					u as Author
				}
				count(val(u))
			}
		}",
        r"{
			user(func: uid(0x0a)) {
				...fragmenta,...fragmentb
				friends {
					name
				}
				...fragmentc
				hobbies
				...fragmentd
			}
			me(func: uid(0x01)) {
				...fragmenta
				...fragmentb
			}
		}
		fragment fragmenta {
			name
		}
		fragment fragmentb {
			id
		}
		fragment fragmentc {
			name
		}
		fragment fragmentd {
			id
		}",
        r"{
			set {
				<name> <is> <something> .
				<hometown> <is> <san/francisco> .
			}
			delete {
				<name> <is> <something-else> .
			}
		}",
        r"{
			<user-query>(func: uid(0x1)) {
				name
			}
		}",
    ] {
        parse_dql(query).expect(query);
    }
}
