//! GraphQL filter to DQL filter rewriting. Port of `buildFilter`,
//! `buildUnionFilter`, `addFilter`, and the geo builders from
//! `dgraph-main/graphql/resolve/query_rewriter.go`.

#![warn(missing_docs)]

use std::fmt::Write as _;

use datastreamdb_dql::{FilterTree, Function, GraphQuery};

use crate::complete::camel_case;
use crate::op::{BoundField, BoundSchema};
use crate::rewrite_vars::{add_type_filter, convert_ids, plain_arg};

/// Quotes a filter argument for DQL (`%q` for strings, quoted floats).
pub(crate) fn maybe_quote_arg(func: &str, arg: &serde_json::Value) -> String {
    match arg {
        serde_json::Value::String(text) => {
            if func == "regexp" && is_valid_regex_arg(text) {
                return text.clone();
            }
            format!("{text:?}")
        }
        serde_json::Value::Number(num) => {
            if num.is_f64() {
                format!("\"{num}\"")
            } else {
                num.to_string()
            }
        }
        _ => arg.to_string(),
    }
}

/// Whether the string is a single self-contained `/.../flags` literal.
/// Port of Go `isValidRegexArg` (GHSA-33p8-wc97-5qcj, CWE-943).
pub(crate) fn is_valid_regex_arg(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() < 2 || bytes[0] != b'/' {
        return false;
    }
    let mut i = 1;
    let mut closed = false;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                i += 2;
                continue;
            }
            b'/' => {
                i += 1;
                closed = true;
            }
            _ => {}
        }
        if closed {
            break;
        }
        i += 1;
    }
    if !closed {
        return false;
    }
    bytes[i..].iter().all(u8::is_ascii_alphabetic)
}

/// Extracts the `filter` argument as an object map.
fn extract_filter(field: &BoundField) -> serde_json::Map<String, serde_json::Value> {
    field
        .arg("filter")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default()
}

/// Extracts ID values from a filter's ID field.
fn id_filter(
    schema: &BoundSchema,
    filter: &serde_json::Map<String, serde_json::Value>,
    type_name: &str,
) -> Option<Vec<u64>> {
    let id_field = schema.id_field(type_name)?;
    let ids = filter.get(&id_field.name)?;
    let slice = match ids {
        serde_json::Value::String(_) => vec![ids.clone()],
        serde_json::Value::Array(items) => items.clone(),
        _ => return None,
    };
    Some(convert_ids(&slice))
}

/// Adds a filter to a DQL query. Returns `false` when the field must be
/// dropped (union with empty member selection).
pub(crate) fn add_filter(
    schema: &BoundSchema,
    query: &mut GraphQuery,
    type_name: &str,
    filter: &serde_json::Map<String, serde_json::Value>,
) -> bool {
    if filter.is_empty() {
        return true;
    }
    let mut filter = filter.clone();
    let id_name = schema
        .id_field(type_name)
        .map(|field| field.name)
        .unwrap_or_default();
    let at_root = filter.contains_key(&id_name)
        && !id_name.is_empty()
        && query.func.as_ref().is_some_and(|func| func.name == "uid");
    if at_root {
        filter.remove(&id_name);
    }
    if schema.is_union(type_name) {
        let (built, include) = build_union_filter(schema, type_name, &filter);
        if !include {
            return false;
        }
        query.filter = built.map(Box::new);
    } else {
        query.filter = build_filter(schema, type_name, &filter).map(Box::new);
    }
    if at_root {
        add_type_filter(query, schema, type_name);
    }
    true
}

/// Builds a DQL filter tree from a GraphQL `filter` argument.
pub(crate) fn build_filter(
    schema: &BoundSchema,
    type_name: &str,
    filter: &serde_json::Map<String, serde_json::Value>,
) -> Option<FilterTree> {
    let mut ands: Vec<FilterTree> = Vec::new();
    let mut or: Option<FilterTree> = None;
    let mut keys: Vec<&String> = filter.keys().collect();
    keys.sort();
    for key in keys {
        let value = &filter[key];
        if value.is_null() {
            continue;
        }
        match key.as_str() {
            "and" => match value {
                serde_json::Value::Object(obj) => {
                    if let Some(built) = build_filter(schema, type_name, obj) {
                        ands.push(built);
                    }
                }
                serde_json::Value::Array(items) => {
                    for item in items {
                        if let Some(obj) = item.as_object() {
                            if let Some(built) = build_filter(schema, type_name, obj) {
                                ands.push(built);
                            }
                        }
                    }
                }
                _ => {}
            },
            "or" => match value {
                serde_json::Value::Object(obj) => {
                    or = build_filter(schema, type_name, obj);
                }
                serde_json::Value::Array(items) => {
                    let mut children = Vec::new();
                    for item in items {
                        if let Some(obj) = item.as_object() {
                            if let Some(built) = build_filter(schema, type_name, obj) {
                                children.push(built);
                            }
                        }
                    }
                    or = Some(FilterTree {
                        op: "or".to_owned(),
                        child: children,
                        func: None,
                    });
                }
                _ => {}
            },
            "not" => {
                if let Some(obj) = value.as_object() {
                    if let Some(built) = build_filter(schema, type_name, obj) {
                        ands.push(FilterTree {
                            op: "not".to_owned(),
                            child: vec![built],
                            func: None,
                        });
                    }
                }
            }
            field => {
                build_field_filter(schema, type_name, field, value, &mut ands);
            }
        }
    }
    let and_tree = match ands.len() {
        0 => None,
        1 => ands.into_iter().next(),
        _ => Some(FilterTree {
            op: "and".to_owned(),
            child: ands,
            func: None,
        }),
    };
    match (and_tree, or) {
        (None, or) => or,
        (Some(and), None) => Some(and),
        (Some(and), Some(or)) => Some(FilterTree {
            op: "and".to_owned(),
            child: vec![and, or],
            func: None,
        }),
    }
}

/// Builds the filter for one field entry.
#[allow(clippy::too_many_lines, reason = "exact port of Go buildFilter")]
fn build_field_filter(
    schema: &BoundSchema,
    type_name: &str,
    field: &str,
    value: &serde_json::Value,
    ands: &mut Vec<FilterTree>,
) {
    match value {
        serde_json::Value::Object(obj) => {
            let Some((func, val)) = obj.iter().next() else {
                return;
            };
            if val.is_null() {
                if func == "eq" {
                    let mut has = serde_json::Map::new();
                    has.insert(
                        "has".to_owned(),
                        serde_json::Value::Array(vec![serde_json::Value::String(field.to_owned())]),
                    );
                    let mut not = serde_json::Map::new();
                    not.insert("not".to_owned(), serde_json::Value::Object(has));
                    if let Some(built) = build_filter(schema, type_name, &not) {
                        ands.push(built);
                    }
                }
                return;
            }
            let pred = schema.predicate(type_name, field).unwrap_or(field);
            let mut args = vec![plain_arg(pred)];
            let mut func = func.clone();
            match func.as_str() {
                "in" => {
                    "eq".clone_into(&mut func);
                    if let Some(vals) = val.as_array() {
                        for item in vals {
                            args.push(plain_arg(&maybe_quote_arg(&func, item)));
                        }
                    }
                }
                "between" => {
                    if let Some(obj) = val.as_object() {
                        let min = obj.get("min").cloned().unwrap_or_default();
                        let max = obj.get("max").cloned().unwrap_or_default();
                        args.push(plain_arg(&maybe_quote_arg(&func, &min)));
                        args.push(plain_arg(&maybe_quote_arg(&func, &max)));
                    }
                }
                "near" => {
                    if let Some(obj) = val.as_object() {
                        let mut point = String::new();
                        if let Some(coord) = obj.get("coordinate").and_then(|val| val.as_object()) {
                            build_point(coord, &mut point);
                        }
                        args.push(plain_arg(&point));
                        let distance = obj
                            .get("distance")
                            .map(ToString::to_string)
                            .unwrap_or_default();
                        args.push(plain_arg(&distance));
                    }
                }
                "within" => {
                    if let Some(polygon) = val
                        .as_object()
                        .and_then(|obj| obj.get("polygon"))
                        .and_then(|val| val.as_object())
                    {
                        let mut buf = String::new();
                        build_polygon(polygon, &mut buf);
                        args.push(plain_arg(&buf));
                    }
                }
                "contains" => {
                    if let Some(obj) = val.as_object() {
                        let mut buf = String::new();
                        if let Some(polygon) = obj.get("polygon").and_then(|val| val.as_object()) {
                            build_polygon(polygon, &mut buf);
                        } else if let Some(point) = obj.get("point").and_then(|val| val.as_object())
                        {
                            build_point(point, &mut buf);
                        }
                        args.push(plain_arg(&buf));
                    }
                }
                "intersects" => {
                    if let Some(obj) = val.as_object() {
                        let mut buf = String::new();
                        if let Some(polygon) = obj.get("polygon").and_then(|val| val.as_object()) {
                            build_polygon(polygon, &mut buf);
                        } else if let Some(multi) =
                            obj.get("multiPolygon").and_then(|val| val.as_object())
                        {
                            build_multi_polygon(multi, &mut buf);
                        }
                        args.push(plain_arg(&buf));
                    }
                }
                _ => {
                    args.push(plain_arg(&maybe_quote_arg(&func, val)));
                }
            }
            ands.push(FilterTree {
                op: String::new(),
                child: Vec::new(),
                func: Some(Function {
                    name: func,
                    args,
                    ..Function::default()
                }),
            });
        }
        serde_json::Value::Array(items) => {
            if field == "has" {
                ands.extend(build_has_filter_list(schema, type_name, items));
            } else if schema
                .schema
                .type_def(type_name)
                .and_then(|def| def.field(field))
                .is_some_and(|def| {
                    def.directive(crate::directives::APOLLO_EXTERNAL_DIRECTIVE)
                        .is_some()
                })
            {
                let pred = schema.predicate(type_name, field).unwrap_or(field);
                let mut args = vec![plain_arg(pred)];
                for item in items {
                    args.push(plain_arg(&maybe_quote_arg("eq", item)));
                }
                ands.push(FilterTree {
                    op: String::new(),
                    child: Vec::new(),
                    func: Some(Function {
                        name: "eq".to_owned(),
                        args,
                        ..Function::default()
                    }),
                });
            } else {
                ands.push(FilterTree {
                    op: String::new(),
                    child: Vec::new(),
                    func: Some(Function {
                        name: "uid".to_owned(),
                        uid: convert_ids(items),
                        ..Function::default()
                    }),
                });
            }
        }
        _ => {
            let pred = schema.predicate(type_name, field).unwrap_or(field);
            ands.push(FilterTree {
                op: String::new(),
                child: Vec::new(),
                func: Some(Function {
                    name: "eq".to_owned(),
                    args: vec![plain_arg(pred), plain_arg(&value.to_string())],
                    ..Function::default()
                }),
            });
        }
    }
}

/// Builds `has(...)` filters for a `has` list.
fn build_has_filter_list(
    schema: &BoundSchema,
    type_name: &str,
    fields: &[serde_json::Value],
) -> Vec<FilterTree> {
    fields
        .iter()
        .filter_map(|field| field.as_str())
        .map(|field| FilterTree {
            op: String::new(),
            child: Vec::new(),
            func: Some(Function {
                name: "has".to_owned(),
                args: vec![plain_arg(
                    schema.predicate(type_name, field).unwrap_or(field),
                )],
                ..Function::default()
            }),
        })
        .collect()
}

/// Renders a `GeoJSON` point (`[lng,lat]`).
fn build_point(point: &serde_json::Map<String, serde_json::Value>, out: &mut String) {
    let lng = point
        .get("longitude")
        .map(ToString::to_string)
        .unwrap_or_default();
    let lat = point
        .get("latitude")
        .map(ToString::to_string)
        .unwrap_or_default();
    let _ = write!(out, "[{lng},{lat}]");
}

/// Renders a `GeoJSON` polygon.
fn build_polygon(polygon: &serde_json::Map<String, serde_json::Value>, out: &mut String) {
    let empty = Vec::new();
    let coordinates = polygon
        .get("coordinates")
        .and_then(|val| val.as_array())
        .unwrap_or(&empty);
    out.push('[');
    for (i, ring) in coordinates.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let empty = Vec::new();
        let points = ring
            .as_object()
            .and_then(|ring| ring.get("points"))
            .and_then(|val| val.as_array())
            .unwrap_or(&empty);
        out.push('[');
        for (j, point) in points.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            if let Some(point) = point.as_object() {
                build_point(point, out);
            }
        }
        out.push(']');
    }
    out.push(']');
}

/// Renders a `GeoJSON` multi-polygon.
fn build_multi_polygon(multi: &serde_json::Map<String, serde_json::Value>, out: &mut String) {
    let empty = Vec::new();
    let polygons = multi
        .get("polygons")
        .and_then(|val| val.as_array())
        .unwrap_or(&empty);
    out.push('[');
    for (i, polygon) in polygons.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        if let Some(polygon) = polygon.as_object() {
            build_polygon(polygon, out);
        }
    }
    out.push(']');
}

/// Builds a union filter (`memberTypes` + per-member filters).
fn build_union_filter(
    schema: &BoundSchema,
    type_name: &str,
    filter: &serde_json::Map<String, serde_json::Value>,
) -> (Option<FilterTree>, bool) {
    if let Some(members) = filter.get("memberTypes").and_then(|val| val.as_array()) {
        if members.is_empty() {
            return (None, false);
        }
        let want: Vec<String> = members
            .iter()
            .filter_map(|val| val.as_str().map(str::to_owned))
            .collect();
        return (
            Some(union_filter_for(schema, type_name, filter, &want)),
            true,
        );
    }
    (Some(union_filter_for(schema, type_name, filter, &[])), true)
}

/// Builds the `or` filter across union members.
fn union_filter_for(
    schema: &BoundSchema,
    type_name: &str,
    filter: &serde_json::Map<String, serde_json::Value>,
    want: &[String],
) -> FilterTree {
    let members = if want.is_empty() {
        schema.union_members(type_name, None)
    } else {
        schema.union_members(type_name, Some(want))
    };
    let mut children = Vec::new();
    for member in members {
        let key = format!("{}Filter", camel_case(&member));
        let member_filter = filter.get(&key).and_then(|val| val.as_object());
        if member_filter.is_none_or(serde_json::Map::is_empty) {
            children.push(FilterTree {
                op: String::new(),
                child: Vec::new(),
                func: Some(crate::rewrite_vars::type_func(
                    &schema.datastreamdb_name(&member),
                )),
            });
        } else if let Some(built) =
            build_filter(schema, &member, member_filter.unwrap_or(&empty_map()))
        {
            children.push(FilterTree {
                op: "and".to_owned(),
                child: vec![
                    FilterTree {
                        op: String::new(),
                        child: Vec::new(),
                        func: Some(crate::rewrite_vars::type_func(
                            &schema.datastreamdb_name(&member),
                        )),
                    },
                    built,
                ],
                func: None,
            });
        }
    }
    FilterTree {
        op: "or".to_owned(),
        child: children,
        func: None,
    }
}

/// Empty filter map helper.
fn empty_map() -> serde_json::Map<String, serde_json::Value> {
    serde_json::Map::new()
}

/// Applies a field's `filter` argument to a child query.
pub(crate) fn filter_child(
    schema: &BoundSchema,
    query: &mut GraphQuery,
    field: &BoundField,
    type_name: &str,
) -> bool {
    add_filter(schema, query, type_name, &extract_filter(field))
}

/// Extracts ID values from a field's filter argument.
pub(crate) fn field_id_filter(
    schema: &BoundSchema,
    field: &BoundField,
    type_name: &str,
) -> Option<Vec<u64>> {
    id_filter(schema, &extract_filter(field), type_name)
}
