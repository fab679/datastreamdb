//! Math ports of Go's `math_test.go`: binary/unary/boolean/ternary tables,
//! big-float tables, vector smoke, and error messages.

#![allow(
    clippy::unreadable_literal,
    reason = "Go-ported test constants stay verbatim"
)]
#![allow(
    clippy::approx_constant,
    reason = "Go-ported expected values must stay literal"
)]

use bigdecimal::BigDecimal;
use datastreamdb_query::math::eval_math_tree;
use datastreamdb_query::subgraph::MathNode;
use datastreamdb_types::{ShardedMap, TypeId, Val, Value};
use std::str::FromStr;

fn int_val(num: i64) -> Val {
    Val {
        tid: TypeId::Int,
        value: Value::Int(num),
    }
}

fn float_val(num: f64) -> Val {
    Val {
        tid: TypeId::Float,
        value: Value::Float(num),
    }
}

fn big_val(text: &str) -> Val {
    Val {
        tid: TypeId::BigFloat,
        value: Value::BigFloat(BigDecimal::from_str(text).expect("decimal")),
    }
}

fn vec_val(values: &[f32]) -> Val {
    Val {
        tid: TypeId::VFloat,
        value: Value::VFloat(values.to_vec()),
    }
}

fn const_node(val: Val) -> MathNode {
    MathNode {
        constant: val,
        ..MathNode::default()
    }
}

fn map_node(uid: u64, val: Val) -> MathNode {
    let mut map = ShardedMap::new();
    map.set(uid, val);
    MathNode {
        var: "v".to_owned(),
        val: map,
        ..MathNode::default()
    }
}

fn tree(op: &str, children: Vec<MathNode>) -> MathNode {
    MathNode {
        op: op.to_owned(),
        child: children,
        ..MathNode::default()
    }
}

/// Asserts value equality (floats/bigfloats within libm tolerance).
fn assert_val_eq(got: &Val, want: &Val) {
    assert_eq!(got.tid, want.tid, "type: {got:?} vs {want:?}");
    match (&got.value, &want.value) {
        (Value::Float(a), Value::Float(b)) => {
            assert!((a - b).abs() < 1e-9, "float: {a} vs {b}");
        }
        (Value::BigFloat(a), Value::BigFloat(b)) => {
            let diff: BigDecimal = a - b;
            let zero = BigDecimal::from(0);
            let abs = if diff < zero { -diff } else { diff };
            assert!(
                abs < BigDecimal::from_str("0.000000001").expect("eps"),
                "bigfloat: {a} vs {b}"
            );
        }
        (Value::VFloat(a), Value::VFloat(b)) => assert_eq!(a, b),
        _ => assert_eq!(got.value, want.value),
    }
}

fn eval_const(op: &str, left: Val, right: Val) -> Val {
    let mut node = tree(op, vec![const_node(left), const_node(right)]);
    eval_math_tree(&mut node).expect("eval");
    node.constant.clone()
}

fn eval_const_err(op: &str, left: Val, right: Val) -> String {
    let mut node = tree(op, vec![const_node(left), const_node(right)]);
    eval_math_tree(&mut node)
        .expect_err("must fail")
        .to_string()
}

#[test]
fn process_binary_int_float() {
    let cases = [
        ("+", int_val(2), int_val(2), int_val(4)),
        ("+", float_val(2.0), int_val(2), float_val(4.0)),
        ("+", float_val(2.0), float_val(2.0), float_val(4.0)),
        (
            "+",
            int_val(48038396025285290),
            int_val(2),
            int_val(48038396025285292),
        ),
        ("-", int_val(100), int_val(1), int_val(99)),
        ("-", float_val(100.0), int_val(1), float_val(99.0)),
        ("-", float_val(100.0), float_val(1.0), float_val(99.0)),
        ("*", int_val(3), int_val(3), int_val(9)),
        ("*", float_val(3.0), int_val(3), float_val(9.0)),
        ("*", float_val(3.0), float_val(3.0), float_val(9.0)),
        ("/", int_val(12), int_val(4), int_val(3)),
        ("/", float_val(12.0), int_val(4), float_val(3.0)),
        ("/", float_val(12.0), float_val(4.0), float_val(3.0)),
        ("%", int_val(10), int_val(2), int_val(0)),
        ("%", float_val(10.0), int_val(2), float_val(0.0)),
        ("%", float_val(10.0), float_val(2.0), float_val(0.0)),
        ("max", int_val(1), int_val(100), int_val(100)),
        ("max", float_val(1.0), int_val(100), float_val(100.0)),
        ("max", float_val(1.0), float_val(100.0), float_val(100.0)),
        ("min", int_val(1), int_val(100), int_val(1)),
        ("min", float_val(1.0), int_val(100), float_val(1.0)),
        ("min", float_val(1.0), float_val(100.0), float_val(1.0)),
        ("logbase", int_val(16), int_val(2), float_val(4.0)),
        ("pow", int_val(2), int_val(3), float_val(8.0)),
    ];
    for (op, left, right, want) in cases {
        assert_val_eq(&eval_const(op, left, right), &want);
    }
}

#[test]
fn process_binary_vectors() {
    let cases = [
        (
            "+",
            vec_val(&[0.0005, 0.25001, 0.7500001]),
            vec_val(&[0.0005, 0.25001, 0.7500001]),
            vec_val(&[0.001, 0.50002, 1.5000002]),
        ),
        (
            "+",
            vec_val(&[0.5, 0.25, 0.75]),
            vec_val(&[0.5, 0.75, 0.25]),
            vec_val(&[1.0, 1.0, 1.0]),
        ),
        (
            "-",
            vec_val(&[1.0, 1.0, 1.0]),
            vec_val(&[0.5, 0.25, 0.75]),
            vec_val(&[0.5, 0.75, 0.25]),
        ),
        (
            "*",
            float_val(2.0),
            vec_val(&[0.25, 0.5, 0.75]),
            vec_val(&[0.5, 1.0, 1.5]),
        ),
        (
            "*",
            int_val(2),
            vec_val(&[0.25, 0.5, 0.75]),
            vec_val(&[0.5, 1.0, 1.5]),
        ),
        (
            "*",
            vec_val(&[0.25, 0.5, 0.75]),
            int_val(2),
            vec_val(&[0.5, 1.0, 1.5]),
        ),
        (
            "/",
            vec_val(&[0.5, 1.0, 1.5]),
            float_val(2.0),
            vec_val(&[0.25, 0.5, 0.75]),
        ),
        (
            "dot",
            vec_val(&[1.0, 2.0, 3.0]),
            vec_val(&[1.0, 2.0, 3.0]),
            float_val(14.0),
        ),
    ];
    for (op, left, right, want) in cases {
        assert_val_eq(&eval_const(op, left, right), &want);
    }
}

#[test]
#[allow(clippy::too_many_lines, reason = "Go-ported error table")]
fn process_binary_errors() {
    let cases = [
        (
            "+",
            int_val(9223372036854775800),
            int_val(10),
            "Integer overflow",
        ),
        (
            "+",
            int_val(-10),
            int_val(-9223372036854775800),
            "Integer overflow",
        ),
        (
            "-",
            int_val(9223372036854775800),
            int_val(-10),
            "Integer overflow",
        ),
        (
            "-",
            int_val(-10),
            int_val(9223372036854775800),
            "Integer overflow",
        ),
        (
            "*",
            vec_val(&[0.5, 0.25, 0.75]),
            float_val(1.243e40),
            "Float32 overflow",
        ),
        (
            "/",
            vec_val(&[0.5, 0.25, 0.75]),
            float_val(1.243e40),
            "Float32 overflow",
        ),
        (
            "/",
            vec_val(&[0.5, 0.25, 0.75]),
            vec_val(&[0.5, 0.25, 0.75]),
            "invalid types 2, 2 for func /",
        ),
        (
            "dot",
            vec_val(&[0.5, 0.25, 0.75]),
            vec_val(&[0.5, 0.25, 0.75, 0.1]),
            "The length of vectors must match",
        ),
        (
            "/",
            vec_val(&[0.5, 0.25, 0.75]),
            int_val(0),
            "Division by zero",
        ),
        (
            "+",
            vec_val(&[0.5, 0.25, 0.75]),
            int_val(3),
            "Left and right arguments must match",
        ),
        (
            "*",
            int_val(9223372036854775),
            int_val(10000),
            "Integer overflow",
        ),
        (
            "*",
            int_val(-10000),
            int_val(9223372036854775),
            "Integer overflow",
        ),
        ("/", int_val(23), int_val(0), "Division by zero"),
        ("/", float_val(23.0), float_val(0.0), "Division by zero"),
        ("%", int_val(23), int_val(0), "Division by zero"),
        ("%", float_val(23.0), float_val(0.0), "Division by zero"),
        (
            "pow",
            int_val(-2),
            float_val(1.7),
            "Fractional power of negative number",
        ),
        ("logbase", int_val(-2), int_val(2), "Log of negative number"),
        ("logbase", int_val(2), int_val(-2), "Log of negative number"),
        (
            "logbase",
            float_val(-2.0),
            float_val(2.0),
            "Log of negative number",
        ),
        (
            "logbase",
            float_val(2.0),
            float_val(-2.0),
            "Log of negative number",
        ),
    ];
    for (op, left, right, want) in cases {
        assert_eq!(eval_const_err(op, left, right), want, "op {op}");
    }
}

#[test]
fn process_binary_bigfloat() {
    let cases = [
        ("+", big_val("2.15"), big_val("1.15"), big_val("3.3")),
        ("-", big_val("100"), big_val("1"), big_val("99")),
        ("*", big_val("3"), big_val("3"), big_val("9")),
        ("/", big_val("12"), big_val("4"), big_val("3")),
        ("max", big_val("1"), big_val("100"), big_val("100")),
        ("min", big_val("1"), big_val("100"), big_val("1")),
    ];
    for (op, left, right, want) in cases {
        assert_val_eq(&eval_const(op, left, right), &want);
    }
}

fn eval_unary(op: &str, arg: Val) -> Val {
    let mut node = tree(op, vec![const_node(arg)]);
    eval_math_tree(&mut node).expect("eval");
    node.constant.clone()
}

fn eval_unary_err(op: &str, arg: Val) -> String {
    let mut node = tree(op, vec![const_node(arg)]);
    eval_math_tree(&mut node)
        .expect_err("must fail")
        .to_string()
}

#[test]
fn process_unary() {
    assert_val_eq(&eval_unary("u-", int_val(2)), &int_val(-2));
    assert_val_eq(&eval_unary("u-", big_val("3.1")), &big_val("-3.1"));
    assert_val_eq(&eval_unary("ln", int_val(15)), &float_val(2.70805020110221));
    assert_val_eq(
        &eval_unary("exp", int_val(1)),
        &float_val(2.718281828459045),
    );
    assert_val_eq(&eval_unary("sqrt", float_val(9.0)), &float_val(3.0));
    assert_val_eq(&eval_unary("floor", float_val(2.5)), &float_val(2.0));
    assert_val_eq(
        &eval_unary("floor", big_val("1.7320508075688772")),
        &big_val("1"),
    );
    assert_val_eq(&eval_unary("ceil", float_val(2.5)), &float_val(3.0));
    assert_val_eq(
        &eval_unary("ceil", big_val("1.7320508075688772")),
        &big_val("2"),
    );
    assert_val_eq(
        &eval_unary("u-", vec_val(&[1.25, 2.5, 3.25])),
        &vec_val(&[-1.25, -2.5, -3.25]),
    );
    // BigFloat sqrt(3.1) against our decimal arithmetic.
    let got = eval_unary("sqrt", big_val("3.1"));
    assert_val_eq(&got, &big_val("1.760681686165901"));
}

#[test]
fn process_unary_errors() {
    assert_eq!(eval_unary_err("ln", int_val(-2)), "Log of negative number");
    assert_eq!(
        eval_unary_err("ln", float_val(-2.0)),
        "Log of negative number"
    );
    assert_eq!(eval_unary_err("u-", int_val(i64::MIN)), "Integer overflow");
    assert_eq!(
        eval_unary_err("sqrt", int_val(-2)),
        "Root of negative number"
    );
    assert_eq!(
        eval_unary_err("sqrt", float_val(-2.0)),
        "Root of negative number"
    );
}

fn eval_boolean(op: &str, left: Val, right: Val) -> Val {
    let mut node = tree(op, vec![map_node(0, left), const_node(right)]);
    eval_math_tree(&mut node).expect("eval");
    node.val.get(0).expect("key 0").clone()
}

#[test]
fn process_binary_boolean() {
    let cases = [
        ("<", true),
        (">", false),
        ("<=", true),
        (">=", false),
        ("==", false),
        ("!=", true),
    ];
    for (op, want) in cases {
        let got = eval_boolean(op, int_val(1), int_val(2));
        assert_eq!(got.tid, TypeId::Bool);
        assert_eq!(got.value, datastreamdb_types::Value::Bool(want), "op {op}");
    }
}

#[test]
fn bigfloat_maths_boolean() {
    let cases = [
        ("==", "2.123", "2.123", true),
        ("!=", "2.4623", "3.623", true),
        (">=", "2.123", "4.123", false),
        ("<=", "2.123", "3.992", true),
        (">", "2.45", "3.43", false),
        ("<", "2.1213", "2.1232", true),
    ];
    for (op, left, right, want) in cases {
        let got = eval_boolean(op, big_val(left), big_val(right));
        assert_eq!(got.tid, TypeId::Bool);
        assert_eq!(got.value, datastreamdb_types::Value::Bool(want), "op {op}");
    }
}

#[test]
fn process_ternary() {
    let mut node = tree(
        "cond",
        vec![
            map_node(
                0,
                Val {
                    tid: TypeId::Bool,
                    value: datastreamdb_types::Value::Bool(true),
                },
            ),
            const_node(int_val(1)),
            const_node(int_val(2)),
        ],
    );
    eval_math_tree(&mut node).expect("eval");
    assert_eq!(node.val.get(0).expect("key 0").value, int_val(1).value);

    let mut node = tree(
        "cond",
        vec![
            map_node(
                0,
                Val {
                    tid: TypeId::Bool,
                    value: datastreamdb_types::Value::Bool(false),
                },
            ),
            const_node(float_val(1.0)),
            const_node(float_val(2.0)),
        ],
    );
    eval_math_tree(&mut node).expect("eval");
    assert_eq!(node.val.get(0).expect("key 0").value, float_val(2.0).value);

    let mut node = tree(
        "cond",
        vec![
            map_node(
                0,
                Val {
                    tid: TypeId::Bool,
                    value: datastreamdb_types::Value::Bool(false),
                },
            ),
            const_node(big_val("1.456")),
            const_node(big_val("2.123")),
        ],
    );
    eval_math_tree(&mut node).expect("eval");
    assert_val_eq(node.val.get(0).expect("key 0"), &big_val("2.123"));
}

#[test]
fn vector_smoke() {
    // sqrt(dot(a - b, a - b)) with disjoint maps only checks for panics.
    let diff = |uid: u64| MathNode {
        op: "-".to_owned(),
        child: vec![
            map_node(0, vec_val(&[1.0, 2.0])),
            map_node(uid, vec_val(&[1.0, 2.0])),
        ],
        ..MathNode::default()
    };
    let mut node = tree(
        "sqrt",
        vec![MathNode {
            op: "dot".to_owned(),
            child: vec![diff(0), diff(123)],
            ..MathNode::default()
        }],
    );
    eval_math_tree(&mut node).expect("eval");
}
