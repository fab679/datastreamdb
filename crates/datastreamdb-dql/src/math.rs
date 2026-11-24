//! Math expression trees. Port of `dql/math.go`.

#![warn(missing_docs)]

use std::collections::HashMap;

use datastreamdb_lex::ItemIterator;
use datastreamdb_types::strconv::{parse_go_float, parse_go_int};
use datastreamdb_types::{ShardedMap, TypeId, Val, Value};

use super::ast::math_op_precedence;
use super::state::{ITEM_COMMA, ITEM_DOLLAR, ITEM_LEFT_ROUND, ITEM_NAME, ITEM_RIGHT_ROUND};
use crate::DqlError;

/// Math operations in tree form for evaluation.
#[derive(Clone, Debug, PartialEq)]
pub struct MathTree {
    /// Operator or function name.
    pub op: String,
    /// Variable leaf.
    pub var: String,
    /// Constant leaf (always parsed as int or float).
    pub constant: Val,
    /// Evaluated values (filled by the executor, like Go's `Val`).
    pub val: Option<ShardedMap>,
    /// Child expressions.
    pub child: Vec<MathTree>,
}

impl Default for MathTree {
    fn default() -> Self {
        Self {
            op: String::new(),
            var: String::new(),
            constant: Val {
                tid: TypeId::Default,
                value: Value::Null,
            },
            val: None,
            child: Vec::new(),
        }
    }
}

fn is_unary(f: &str) -> bool {
    matches!(f, "exp" | "ln" | "u-" | "sqrt" | "floor" | "ceil" | "since")
}

fn is_binary_math(f: &str) -> bool {
    matches!(f, "*" | "+" | "-" | "/" | "%")
}

fn is_ternary(f: &str) -> bool {
    f == "cond"
}

#[allow(clippy::float_cmp, reason = "Go compares constants with ==")]
fn is_zero(f: &str, rval: &Val) -> bool {
    match &rval.value {
        Value::Float(g) => match f {
            "floor" => *g >= 0.0 && *g < 1.0,
            "/" | "%" | "ceil" | "sqrt" | "u-" => *g == 0.0,
            "ln" => *g == 1.0,
            _ => false,
        },
        Value::Int(g) => match f {
            "floor" | "/" | "%" | "ceil" | "sqrt" | "u-" => *g == 0,
            "ln" => *g == 1,
            _ => false,
        },
        _ => false,
    }
}

struct MathStack {
    items: Vec<MathTree>,
}

impl MathStack {
    fn empty(&self) -> bool {
        self.items.is_empty()
    }

    fn size(&self) -> usize {
        self.items.len()
    }

    fn push(&mut self, t: MathTree) {
        self.items.push(t);
    }

    fn pop_assert(&mut self) -> MathTree {
        assert!(!self.empty(), "Expected a non-empty stack");
        self.items.pop().expect("checked")
    }

    fn pop(&mut self) -> Result<MathTree, DqlError> {
        self.items
            .pop()
            .ok_or_else(|| DqlError::message("Empty stack"))
    }

    fn peek(&self) -> &MathTree {
        assert!(!self.empty(), "Trying to peek empty stack");
        self.items.last().expect("checked")
    }
}

fn eval_math_stack(op_stack: &mut MathStack, value_stack: &mut MathStack) -> Result<(), DqlError> {
    let mut top_op = op_stack
        .pop()
        .map_err(|_| DqlError::message("Invalid Math expression"))?;
    if is_unary(&top_op.op) {
        let top_val = value_stack
            .pop()
            .map_err(|_| DqlError::message("Invalid math statement. Expected 1 operands"))?;
        if op_stack.size() > 1 {
            let peek = op_stack.peek().op.clone();
            if (peek == "/" || peek == "%") && is_zero(&top_op.op, &top_val.constant) {
                return Err(DqlError::message("Division by zero"));
            }
        }
        top_op.child = vec![top_val];
    } else if is_ternary(&top_op.op) {
        if value_stack.size() < 3 {
            return Err(DqlError::message(
                "Invalid Math expression. Expected 3 operands",
            ));
        }
        let v1 = value_stack.pop_assert();
        let v2 = value_stack.pop_assert();
        let v3 = value_stack.pop_assert();
        top_op.child = vec![v3, v2, v1];
    } else {
        if value_stack.size() < 2 {
            return Err(DqlError::message(
                "Invalid Math expression. Expected 2 operands",
            ));
        }
        if is_zero(&top_op.op, &value_stack.peek().constant) {
            return Err(DqlError::message("Division by zero."));
        }
        let v1 = value_stack.pop_assert();
        let v2 = value_stack.pop_assert();
        top_op.child = vec![v2, v1];
    }
    value_stack.push(top_op);
    Ok(())
}

/// Pushes an operator, evaluating higher-precedence stack entries first.
fn push_math_operator(
    it: &mut ItemIterator,
    op_stack: &mut MathStack,
    value_stack: &mut MathStack,
    lval: String,
) -> Result<(), DqlError> {
    let mut op = lval;
    it.prev();
    let last = it.item();
    it.advance();
    if op == "-" && (last.val == "(" || last.val == "," || is_binary_math(&last.val)) {
        "u-".clone_into(&mut op); // Unary minus.
    }
    let pred = math_op_precedence(&op);
    assert!(pred > 0, "Expected opPred > 0 for {op}: {pred}");
    while !op_stack.empty() {
        let top_pred = math_op_precedence(&op_stack.peek().op);
        if top_pred < pred {
            break;
        }
        eval_math_stack(op_stack, value_stack)?;
    }
    op_stack.push(MathTree {
        op,
        ..MathTree::default()
    });
    let peeked = it.peek(1).map_err(DqlError::from)?;
    if peeked[0].typ == ITEM_LEFT_ROUND {
        let mut again = false;
        loop {
            let (child, more) = parse_math_func(it, again)?;
            value_stack.push(child);
            if !more {
                break;
            }
            again = true;
        }
    }
    Ok(())
}

/// Pushes a constant/variable leaf (or a nested function call's values).
fn push_math_leaf(
    it: &mut ItemIterator,
    value_stack: &mut MathStack,
    item: &datastreamdb_lex::Item,
) -> Result<(), DqlError> {
    let peeked = it.peek(1).map_err(DqlError::from)?;
    if peeked[0].typ == ITEM_LEFT_ROUND {
        if !is_math_func(&item.val) {
            return Err(DqlError::message(format!(
                "Unknown math function: {}",
                item.val
            )));
        }
        let mut again = false;
        loop {
            let (child, more) = parse_math_func(it, again)?;
            value_stack.push(child);
            if !more {
                break;
            }
            again = true;
        }
        return Ok(());
    }
    // Try int first, then float, else a variable.
    let mut child = MathTree::default();
    match parse_go_int(&item.val) {
        Ok(i) => {
            child.constant = Val {
                tid: TypeId::Int,
                value: Value::Int(i),
            };
        }
        Err(_) => match parse_go_float(&item.val) {
            Ok(v) => {
                child.constant = Val {
                    tid: TypeId::Float,
                    value: Value::Float(v),
                };
            }
            Err(_) => item.val.clone_into(&mut child.var),
        },
    }
    value_stack.push(child);
    Ok(())
}

/// Whether the name is a math operator or function.
#[must_use]
pub fn is_math_func(name: &str) -> bool {
    matches!(
        name,
        "*" | "%"
            | "+"
            | "-"
            | "/"
            | "exp"
            | "ln"
            | "cond"
            | "<"
            | ">"
            | ">="
            | "<="
            | "=="
            | "!="
            | "min"
            | "max"
            | "sqrt"
            | "pow"
            | "logbase"
            | "floor"
            | "ceil"
            | "since"
            | "dot"
    )
}

/// Parses a `math(...)` call. Returns the tree and whether a comma (another
/// argument) followed.
pub(crate) fn parse_math_func(
    it: &mut ItemIterator,
    again: bool,
) -> Result<(MathTree, bool), DqlError> {
    if !again {
        it.advance();
        let item = it.item();
        if item.typ != ITEM_LEFT_ROUND {
            return Err(DqlError::message("Expected ( after math"));
        }
    }

    let mut op_stack = MathStack { items: Vec::new() };
    op_stack.push(MathTree {
        op: "(".to_owned(),
        ..MathTree::default()
    });
    let mut value_stack = MathStack { items: Vec::new() };

    loop {
        if !it.advance() {
            break;
        }
        let item = it.item();
        let lval = item.val.to_ascii_lowercase();
        if is_math_func(&lval) {
            push_math_operator(it, &mut op_stack, &mut value_stack, lval)?;
            continue;
        }
        if item.typ == ITEM_NAME {
            push_math_leaf(it, &mut value_stack, &item)?;
            continue;
        }
        if item.typ == ITEM_LEFT_ROUND {
            op_stack.push(MathTree {
                op: "(".to_owned(),
                ..MathTree::default()
            });
            continue;
        }
        if item.typ == ITEM_COMMA {
            while !op_stack.empty() {
                if op_stack.peek().op == "(" {
                    break;
                }
                eval_math_stack(&mut op_stack, &mut value_stack)?;
            }
            op_stack
                .pop()
                .map_err(|_| DqlError::message("Invalid Math expression"))?;
            if !op_stack.empty() {
                return Err(DqlError::message("Invalid math expression."));
            }
            if value_stack.size() != 1 {
                return Err(DqlError::message(format!(
                    "Expected one item in value stack, but got {}",
                    value_stack.size()
                )));
            }
            let res = value_stack
                .pop()
                .map_err(|_| DqlError::message("Invalid math expression"))?;
            return Ok((res, true));
        }
        if item.typ == ITEM_RIGHT_ROUND {
            while !op_stack.empty() {
                if op_stack.peek().op == "(" {
                    break;
                }
                eval_math_stack(&mut op_stack, &mut value_stack)?;
            }
            op_stack
                .pop()
                .map_err(|_| DqlError::message("Invalid Math expression"))?;
            if op_stack.empty() {
                break;
            }
            continue;
        }
        if item.typ == ITEM_DOLLAR {
            let var_name = super::directives::parse_var_name(it)?;
            value_stack.push(MathTree {
                var: var_name,
                ..MathTree::default()
            });
            continue;
        }
        return Err(DqlError::message(format!(
            "Unexpected item while parsing math expression: {item}"
        )));
    }

    assert!(op_stack.empty(), "Op stack should be empty when we exit");
    if value_stack.empty() {
        // math() is empty; report an error.
        return Err(DqlError::message("Empty () not allowed in math block."));
    }
    if value_stack.size() != 1 {
        return Err(DqlError::message(format!(
            "Expected one item in value stack, but got {}",
            value_stack.size()
        )));
    }
    let res = value_stack
        .pop()
        .map_err(|_| DqlError::message("Invalid math expression"))?;
    Ok((res, false))
}

impl MathTree {
    /// Substitutes `$...` variables from the variable map.
    pub(crate) fn subs(
        &mut self,
        vmap: &HashMap<String, super::vars::VarInfo>,
    ) -> Result<(), DqlError> {
        if self.var.starts_with('$') {
            let Some(info) = vmap.get(&self.var) else {
                return Err(DqlError::message("Variable not found in math"));
            };
            self.constant = super::vars::parse_value(info)?;
            self.var.clear();
        }
        for child in &mut self.child {
            child.subs(vmap)?;
        }
        Ok(())
    }

    /// Renders the tree (debug helper, port of `debugString`).
    #[must_use]
    pub fn debug_string(&self) -> String {
        let mut buf = String::new();
        self.string_helper(&mut buf);
        buf
    }

    fn string_helper(&self, buf: &mut String) {
        if !self.var.is_empty() {
            buf.push_str(&self.var);
            return;
        }
        if !matches!(self.constant.value, Value::Null) {
            match &self.constant.value {
                Value::Float(v) => buf.push_str(&format_float_e(*v)),
                Value::Int(v) => buf.push_str(&v.to_string()),
                _ => {}
            }
            return;
        }
        buf.push('(');
        match self.op.as_str() {
            "+" | "-" | "/" | "*" | "%" | "exp" | "ln" | "cond" | "min" | "sqrt" | "max" | "<"
            | ">" | "<=" | ">=" | "==" | "!=" | "u-" | "logbase" | "pow" | "dot" => {
                buf.push_str(&self.op);
            }
            other => panic!("Unknown operator: {other:?}"),
        }
        for child in &self.child {
            buf.push(' ');
            child.string_helper(buf);
        }
        buf.push(')');
    }
}

/// Go `%E` float format (mantissa with exponent, e.g. `1E+00`).
fn format_float_e(v: f64) -> String {
    if v.is_nan() {
        return "NaN".to_owned();
    }
    if v.is_infinite() {
        return if v > 0.0 {
            "+Inf".to_owned()
        } else {
            "-Inf".to_owned()
        };
    }
    // Shortest round-trip mantissa with an explicit exponent.
    let text = format!("{v:E}");
    // Rust already emits `E` notation; normalize Go's exponent padding.
    let (mantissa, exp) = text.split_once('E').expect("exponent");
    let exp: i32 = exp.parse().expect("exponent digits");
    format!("{mantissa}E{exp:+03}")
}
