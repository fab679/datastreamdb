//! `@filter` parsing (shunting-yard over function leaves).

#![warn(missing_docs)]

use datastreamdb_lex::ItemIterator;

use super::ast::{FilterTree, filter_op_precedence};
use super::funcs::parse_function;
use super::state::{ITEM_LEFT_CURL, ITEM_LEFT_ROUND, ITEM_NAME, ITEM_RIGHT_CURL, ITEM_RIGHT_ROUND};
use crate::DqlError;

struct FilterStack {
    items: Vec<FilterTree>,
}

impl FilterStack {
    fn empty(&self) -> bool {
        self.items.is_empty()
    }

    fn size(&self) -> usize {
        self.items.len()
    }

    fn push(&mut self, t: FilterTree) {
        self.items.push(t);
    }

    fn pop_assert(&mut self) -> FilterTree {
        assert!(!self.empty());
        self.items.pop().expect("checked")
    }

    fn pop(&mut self) -> Result<FilterTree, DqlError> {
        self.items
            .pop()
            .ok_or_else(|| DqlError::message("Empty stack"))
    }

    fn peek(&self) -> &FilterTree {
        assert!(!self.empty(), "Trying to peek empty stack");
        self.items.last().expect("checked")
    }
}

fn eval_stack(op_stack: &mut FilterStack, value_stack: &mut FilterStack) -> Result<(), DqlError> {
    let mut top_op = op_stack
        .pop()
        .map_err(|_| DqlError::message("Invalid filter statement"))?;
    if top_op.op == "not" {
        // "not" is unary: pop one value.
        let top_val = value_stack
            .pop()
            .map_err(|_| DqlError::message("Invalid filter statement"))?;
        top_op.child = vec![top_val];
    } else {
        // "and"/"or" pop two values.
        if value_stack.size() < 2 {
            return Err(DqlError::message("Invalid filter statement"));
        }
        let v1 = value_stack.pop_assert();
        let v2 = value_stack.pop_assert();
        top_op.child = vec![v2, v1];
    }
    value_stack.push(top_op);
    Ok(())
}

/// Parses `@filter(...)` into a tree.
pub(crate) fn parse_filter(it: &mut ItemIterator) -> Result<Option<FilterTree>, DqlError> {
    it.advance();
    let item = it.item();
    if item.typ != ITEM_LEFT_ROUND {
        return Err(item.errorf("Expected ( after filter directive").into());
    }

    let mut op_stack = FilterStack { items: Vec::new() };
    op_stack.push(FilterTree {
        op: "(".to_owned(),
        ..FilterTree::default()
    });
    let mut value_stack = FilterStack { items: Vec::new() };

    loop {
        if !it.advance() {
            break;
        }
        let item = it.item();
        let lval = item.val.to_ascii_lowercase();
        if lval == "and" || lval == "or" || lval == "not" {
            let op = lval;
            let pred = filter_op_precedence(&op);
            assert!(pred > 0, "Expected opPred > 0: {pred}");
            // Evaluate until an operator with strictly lower precedence.
            while !op_stack.empty() {
                let top_pred = filter_op_precedence(&op_stack.peek().op);
                if top_pred < pred {
                    break;
                }
                eval_stack(&mut op_stack, &mut value_stack)?;
            }
            op_stack.push(FilterTree {
                op,
                ..FilterTree::default()
            });
        } else if item.typ == ITEM_NAME {
            it.prev();
            let func = parse_function(it, None)?;
            value_stack.push(FilterTree {
                func: Some(func),
                ..FilterTree::default()
            });
        } else if item.typ == ITEM_LEFT_ROUND {
            op_stack.push(FilterTree {
                op: "(".to_owned(),
                ..FilterTree::default()
            });
        } else if item.typ == ITEM_RIGHT_ROUND {
            while !op_stack.empty() {
                if op_stack.peek().op == "(" {
                    break;
                }
                eval_stack(&mut op_stack, &mut value_stack)?;
            }
            if op_stack.pop().is_err() {
                return Err(item.errorf("Invalid filter statement").into());
            }
            if op_stack.empty() {
                // Parentheses balanced out.
                break;
            }
        } else if item.typ == ITEM_LEFT_CURL {
            return Err(item
                .errorf("Unrecognized character inside a func: U+007B '{'")
                .into());
        } else if item.typ == ITEM_RIGHT_CURL {
            return Err(item
                .errorf("Unrecognized character inside a func: U+007D '}'")
                .into());
        } else {
            return Err(item
                .errorf(format!("Unexpected item while parsing @filter: {item}"))
                .into());
        }
    }

    // Parentheses must have balanced out inside the loop.
    if !op_stack.empty() {
        return Err(it
            .errorf("Unbalanced parentheses in @filter statement")
            .into());
    }
    if value_stack.empty() {
        // `@filter()` is ignored: pretend there is no filter.
        return Ok(None);
    }
    if value_stack.size() != 1 {
        return Err(it
            .errorf(format!(
                "Expected one item in value stack, but got {}",
                value_stack.size()
            ))
            .into());
    }
    Ok(Some(value_stack.pop().expect("checked")))
}

/// Whether any leaf function needs a variable.
pub(crate) fn has_vars(filter: &FilterTree) -> bool {
    if filter
        .func
        .as_ref()
        .is_some_and(|f| !f.needs_var.is_empty())
    {
        return true;
    }
    filter.child.iter().any(has_vars)
}

impl FilterTree {
    /// Renders the tree (debug helper, port of `debugString`).
    #[must_use]
    pub fn debug_string(&self) -> String {
        let mut buf = String::new();
        self.string_helper(&mut buf);
        buf
    }

    fn string_helper(&self, buf: &mut String) {
        if self.func.as_ref().is_some_and(|f| !f.name.is_empty()) {
            // Leaf node.
            let func = self.func.as_ref().expect("checked");
            buf.push('(');
            buf.push_str(&func.name);
            if !func.attr.is_empty() {
                buf.push(' ');
                if func.is_count {
                    buf.push_str("count(");
                } else if func.is_value_var {
                    buf.push_str("val(");
                } else if func.is_len_var {
                    buf.push_str("len(");
                }
                buf.push_str(&func.attr);
                if func.is_count || func.is_value_var || func.is_len_var {
                    buf.push(')');
                }
                if !func.lang.is_empty() {
                    buf.push('@');
                    buf.push_str(&func.lang);
                }
                for arg in &func.args {
                    if arg.is_value_var {
                        buf.push_str(" val(");
                    } else {
                        buf.push_str(" \"");
                    }
                    buf.push_str(&arg.value);
                    if arg.is_value_var {
                        buf.push(')');
                    } else {
                        buf.push('"');
                    }
                }
            }
            buf.push(')');
            return;
        }
        // Non-leaf node.
        buf.push('(');
        match self.op.as_str() {
            "and" => buf.push_str("AND"),
            "or" => buf.push_str("OR"),
            "not" => buf.push_str("NOT"),
            other => panic!("Unknown operator: {other:?}"),
        }
        for child in &self.child {
            buf.push(' ');
            child.string_helper(buf);
        }
        buf.push(')');
    }
}
