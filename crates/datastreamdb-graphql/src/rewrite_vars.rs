//! Shared rewriter helpers: variable generation, ID/XID arguments, and
//! small DQL builders. Port of `VariableGenerator` from
//! `dgraph-main/graphql/resolve/mutation_rewriter.go`, `IDArgValue` from
//! `dgraph-main/graphql/schema/wrappers.go`, and the `add*Func` family from
//! `dgraph-main/graphql/resolve/query_rewriter.go`.

#![warn(missing_docs)]

use std::collections::HashMap;

use datastreamdb_dql::{Arg, FilterTree, Function, GraphQuery};
use datastreamdb_proto::Order;

use crate::errors::GraphQlError;
use crate::op::{BoundField, BoundSchema};

/// Generates unique DQL variable names, deduplicating by xid value.
#[derive(Debug, Default)]
pub(crate) struct VariableGenerator {
    /// Counter for fresh names.
    counter: u64,
    /// xid key to allocated name.
    xid_var_names: HashMap<String, String>,
}

impl VariableGenerator {
    /// Fresh generator.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Next variable name for a type (and optional xid identity).
    pub(crate) fn next(
        &mut self,
        schema: &BoundSchema,
        type_name: &str,
        xid: (&str, &str),
        auth: bool,
    ) -> String {
        let (xid_name, xid_val) = xid;
        let flag_and_name = xid_name.to_owned();
        let short_name = xid_name.split('.').nth(1).unwrap_or(xid_name);
        let key = if short_name.is_empty() || xid_val.is_empty() {
            type_name.to_owned()
        } else {
            let origin = schema.field_originated_from(type_name, short_name);
            format!("{origin}.{flag_and_name}.{xid_val}")
        };
        if let Some(name) = self.xid_var_names.get(&key) {
            return name.clone();
        }
        self.counter += 1;
        let name = if auth {
            format!("{type_name}_Auth{}", self.counter)
        } else {
            format!("{type_name}_{}", self.counter)
        };
        if !short_name.is_empty() && !xid_val.is_empty() {
            self.xid_var_names.insert(key, name.clone());
        }
        name
    }
}

/// Extracts xid arguments and the UID from a get-style field. Port of Go
/// `IDArgValue`.
///
/// Returns `(xids, uid)`.
pub(crate) fn id_arg_value(
    schema: &BoundSchema,
    field: &BoundField,
) -> Result<(HashMap<String, String>, u64), GraphQlError> {
    let type_name = field.type_name().to_owned();
    let id_field = schema.id_field(&type_name);
    let pwd_field = schema.password_field(&type_name);
    let mut xids = HashMap::new();
    for (arg_name, arg_val) in &field.args {
        let is_id = id_field.as_ref().is_some_and(|id| &id.name == arg_name);
        let is_pwd = pwd_field.as_ref().is_some_and(|pwd| &pwd.name == arg_name);
        if is_id || is_pwd || is_similar_meta_arg(field, arg_name) {
            continue;
        }
        match arg_val {
            serde_json::Value::String(text) => {
                xids.insert(arg_name.clone(), text.clone());
            }
            serde_json::Value::Number(num) => {
                xids.insert(arg_name.clone(), num.to_string());
            }
            _ => {
                return Err(GraphQlError::new(format!(
                    "Argument ({arg_name}) of {} was not able to be parsed as a string",
                    field.name
                )));
            }
        }
    }
    let mut uid = 0u64;
    if let Some(id_field) = &id_field {
        if let Some(id_arg) = field.arg(&id_field.name) {
            let text = id_arg.as_str().unwrap_or_default();
            uid = parse_uid(text).ok_or_else(|| {
                GraphQlError::new(format!(
                    "ID argument ({}) of {} was not able to be parsed",
                    id_arg, field.name
                ))
            })?;
        }
    }
    Ok((xids, uid))
}

/// Whether the argument is a similarity-query meta argument.
fn is_similar_meta_arg(field: &BoundField, arg: &str) -> bool {
    matches!(
        field.query_type,
        crate::op::QueryType::SimilarById | crate::op::QueryType::SimilarByEmbedding
    ) && matches!(
        arg,
        "topK" | "by" | "ef" | "distance_threshold" | "vector" | "filter"
    )
}

/// Parses a UID in decimal or hex (`0x`) form.
fn parse_uid(text: &str) -> Option<u64> {
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()
    } else {
        text.parse::<u64>().ok()
    }
}

/// Maps xid argument names to `DataStreamDB` predicates. Port of Go `XIDArgs`.
pub(crate) fn xid_args(schema: &BoundSchema, field: &BoundField) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let type_name = field.type_name().to_owned();
    let pwd_name = schema
        .password_field(&type_name)
        .map(|pwd| pwd.name)
        .unwrap_or_default();
    let Some(def) = schema.schema.type_def(&field.object) else {
        return out;
    };
    let Some(field_def) = def.field(&field.name) else {
        return out;
    };
    for arg in &field_def.args {
        if arg.ty.name() == "ID" || arg.name == pwd_name {
            continue;
        }
        if let Some(pred) = schema.predicate(&type_name, &arg.name) {
            out.insert(arg.name.clone(), pred.to_owned());
        }
    }
    out
}

/// Builds a `uid(...)` root/leaf function.
pub(crate) fn uid_func(uids: Vec<u64>, args: Vec<Arg>) -> Function {
    Function {
        name: "uid".to_owned(),
        uid: uids,
        args,
        ..Function::default()
    }
}

/// Builds a `type(...)` function.
pub(crate) fn type_func(type_name: &str) -> Function {
    Function {
        name: "type".to_owned(),
        args: vec![plain_arg(type_name)],
        ..Function::default()
    }
}

/// Builds a plain string argument.
pub(crate) fn plain_arg(value: &str) -> Arg {
    Arg {
        value: value.to_owned(),
        ..Arg::default()
    }
}

/// Builds a variable argument (`uid(Var)`).
pub(crate) fn var_arg(name: &str) -> Arg {
    Arg {
        value: name.to_owned(),
        ..Arg::default()
    }
}

/// Sets a `uid(...)` root function.
pub(crate) fn add_uid_func(query: &mut GraphQuery, uids: Vec<u64>) {
    query.func = Some(uid_func(uids, Vec::new()));
}

/// Sets a `type(...)` root function.
pub(crate) fn add_type_func(query: &mut GraphQuery, type_name: &str) {
    query.func = Some(type_func(type_name));
}

/// Sets an `eq(pred, values...)` root function.
pub(crate) fn add_eq_func(query: &mut GraphQuery, pred: &str, values: &[String]) {
    let mut args = vec![plain_arg(pred)];
    args.extend(values.iter().map(|val| plain_arg(val)));
    query.func = Some(Function {
        name: "eq".to_owned(),
        args,
        ..Function::default()
    });
}

/// Conjoins a filter onto a query's existing filter.
pub(crate) fn add_to_filter_tree(query: &mut GraphQuery, filter: FilterTree) {
    match query.filter.take() {
        None => query.filter = Some(Box::new(filter)),
        Some(existing) => {
            query.filter = Some(Box::new(FilterTree {
                op: "and".to_owned(),
                child: vec![*existing, filter],
                func: None,
            }));
        }
    }
}

/// Conjoins a `type(...)` filter onto a query.
pub(crate) fn add_type_filter(query: &mut GraphQuery, schema: &BoundSchema, type_name: &str) {
    add_to_filter_tree(
        query,
        FilterTree {
            op: String::new(),
            child: Vec::new(),
            func: Some(type_func(&schema.datastreamdb_name(type_name))),
        },
    );
}

/// Builds an order instruction.
pub(crate) fn order_by(attr: &str, desc: bool) -> Order {
    Order {
        attr: attr.to_owned(),
        desc,
        ..Order::default()
    }
}

/// Converts ID strings to UIDs, skipping unparsable values.
pub(crate) fn convert_ids(ids: &[serde_json::Value]) -> Vec<u64> {
    ids.iter()
        .filter_map(|id| id.as_str().and_then(parse_uid))
        .collect()
}
