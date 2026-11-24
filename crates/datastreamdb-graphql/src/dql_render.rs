//! DQL text rendering for rewritten queries. Port of `AsString` and its
//! helpers from `dgraph-main/graphql/dgraph/graphquery.go`.
//!
//! Like Go, rendering never validates: malformed trees may produce text that
//! would not parse as DQL.

#![warn(missing_docs)]

use std::collections::HashMap;
use std::fmt::Write as _;

use datastreamdb_dql::{Arg, FilterTree, Function, GraphQuery};

/// Renders rewritten queries as an indented DQL query string.
#[must_use]
pub fn as_string(queries: &[GraphQuery]) -> String {
    if queries.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let query_name = queries[queries.len() - 1].attr.clone();
    out.push_str("query ");
    add_query_vars(&mut out, &query_name, &queries[0].args);
    out.push_str("{\n");
    for query in queries {
        write_query(&mut out, query, "  ");
    }
    out.push('}');
    out
}

/// Renders `$var: type` query variables (sorted for determinism).
fn add_query_vars(out: &mut String, query_name: &str, args: &HashMap<String, String>) {
    let mut vars: Vec<(&String, &String)> = args
        .iter()
        .filter(|(name, _)| name.starts_with('$'))
        .collect();
    vars.sort_by(|left, right| left.0.cmp(right.0));
    if vars.is_empty() {
        return;
    }
    let _ = write!(out, "{query_name}(");
    for (i, (name, val)) in vars.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        let _ = write!(out, "{name}: {val}");
    }
    out.push_str(") ");
}

/// Renders one query block.
fn write_query(out: &mut String, query: &GraphQuery, prefix: &str) {
    if !query.var.is_empty() || !query.alias.is_empty() || !query.attr.is_empty() {
        out.push_str(prefix);
    }
    if !query.var.is_empty() {
        let _ = write!(out, "{} as ", query.var);
    }
    if !query.alias.is_empty() {
        out.push_str(&query.alias);
        out.push_str(" : ");
    }
    out.push_str(&query.attr);
    if query.func.is_some() {
        write_root(out, query);
    }
    if let Some(filter) = &query.filter {
        out.push_str(" @filter(");
        write_filter(out, filter);
        out.push(')');
    }
    if query.func.is_none() && has_order_or_page(query) {
        out.push_str(" (");
        write_order_and_page(out, query, false);
        out.push(')');
    }
    if !query.cascade.is_empty() {
        if query.cascade[0] == "__all__" {
            out.push_str(" @cascade");
        } else {
            out.push_str(" @cascade(");
            out.push_str(&query.cascade.join(", "));
            out.push(')');
        }
    }
    if !query.children.is_empty() {
        let mut child_prefix = prefix.to_owned();
        if !query.attr.is_empty() {
            out.push_str(" {\n");
            child_prefix.push_str("  ");
        }
        for child in &query.children {
            write_query(out, child, &child_prefix);
        }
        if !query.attr.is_empty() {
            out.push_str(prefix);
            out.push_str("}\n");
        }
    } else if !query.var.is_empty() || !query.alias.is_empty() || !query.attr.is_empty() {
        out.push('\n');
    }
}

/// Renders a `uid(...)` function (UIDs or variables).
fn write_uid_func(out: &mut String, uids: &[u64], args: &[Arg]) {
    out.push_str("uid(");
    if uids.is_empty() {
        for (i, arg) in args.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&arg.value);
        }
    } else {
        for (i, uid) in uids.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            let _ = write!(out, "{uid:#x}");
        }
    }
    out.push(')');
}

/// Renders the root function plus ordering/pagination.
fn write_root(out: &mut String, query: &GraphQuery) {
    let Some(func) = &query.func else {
        return;
    };
    if func.name == "uid" {
        out.push_str("(func: ");
        write_uid_func(out, &func.uid, &func.args);
    } else if func.name == "type" && func.args.len() == 1 {
        let _ = write!(out, "(func: type({})", func.args[0].value);
    } else if func.name == "eq" || func.name == "similar_to" {
        let _ = write!(out, "(func: {}(", func.name);
        write_filter_arguments(out, &func.args);
        out.push(')');
    }
    write_order_and_page(out, query, true);
    out.push(')');
}

/// Renders comma-separated function arguments.
fn write_filter_arguments(out: &mut String, args: &[Arg]) {
    for (i, arg) in args.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&arg.value);
    }
}

/// Renders one filter function.
fn write_filter_function(out: &mut String, func: &Function) {
    if func.name == "uid" {
        write_uid_func(out, &func.uid, &func.args);
    } else {
        let _ = write!(out, "{}(", func.name);
        write_filter_arguments(out, &func.args);
        out.push(')');
    }
}

/// Renders a filter tree.
fn write_filter(out: &mut String, filter: &FilterTree) {
    match filter.op.as_str() {
        "and" | "or" => {
            out.push('(');
            for (i, child) in filter.child.iter().enumerate() {
                if i > 0 {
                    let _ = write!(out, " {} ", filter.op.to_uppercase());
                }
                write_filter(out, child);
            }
            out.push(')');
        }
        "not" => {
            if let Some(child) = filter.child.first() {
                out.push_str("NOT (");
                write_filter(out, child);
                out.push(')');
            }
        }
        _ => {
            if let Some(func) = &filter.func {
                write_filter_function(out, func);
            }
        }
    }
}

/// Whether the block carries ordering or pagination.
fn has_order_or_page(query: &GraphQuery) -> bool {
    !query.order.is_empty() || query.args.contains_key("first") || query.args.contains_key("offset")
}

/// Renders ordering and pagination arguments.
fn write_order_and_page(out: &mut String, query: &GraphQuery, root: bool) {
    let mut wrote_order = false;
    let mut wrote_first = false;
    for order in &query.order {
        if root || wrote_order {
            out.push_str(", ");
        }
        if order.desc {
            out.push_str("orderdesc: ");
        } else {
            out.push_str("orderasc: ");
        }
        out.push_str(&order.attr);
        wrote_order = true;
    }
    if let Some(first) = query.args.get("first") {
        if root || wrote_order {
            out.push_str(", ");
        }
        out.push_str("first: ");
        out.push_str(first);
        wrote_first = true;
    }
    if let Some(offset) = query.args.get("offset") {
        if root || wrote_order || wrote_first {
            out.push_str(", ");
        }
        out.push_str("offset: ");
        out.push_str(offset);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_root_query() {
        let mut child = GraphQuery::new();
        child.alias = "Post.title".to_owned();
        child.attr = "Post.title".to_owned();
        let mut root = GraphQuery::new();
        root.attr = "queryPost".to_owned();
        root.func = Some(Function {
            name: "type".to_owned(),
            args: vec![Arg {
                value: "Post".to_owned(),
                ..Arg::default()
            }],
            ..Function::default()
        });
        root.children = vec![child];
        let queries = vec![root];
        assert_eq!(
            as_string(&queries),
            "query {\n  queryPost(func: type(Post)) {\n    Post.title : Post.title\n  }\n}"
        );
    }

    #[test]
    fn renders_empty_as_empty() {
        assert_eq!(as_string(&[]), String::new());
    }
}
