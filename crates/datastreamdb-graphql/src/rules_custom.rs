//! `@custom` directive validation and body-template parsing. Port of
//! `customDirectiveValidation` from `dgraph-main/graphql/schema/rules.go`
//! plus `parseBodyTemplate` / `parseAsGraphQLArg` / `parseAsJSONTemplate`
//! from `dgraph-main/graphql/schema/wrappers.go`.
//!
//! Delta from Go: step 12 (remote introspection over HTTP) is skipped in
//! embedded mode — the header/secret shape checks still run, but no network
//! call validates the remote operation. Remote `@custom(http:)` execution is
//! deferred; `@custom(dql:)` resolves locally.

#![warn(missing_docs)]

use std::collections::{HashMap, HashSet};

use async_graphql_parser::parse_query;
use async_graphql_value::Value as QueryValue;

use crate::directives::{CUSTOM_DIRECTIVE, DATASTREAMDB_DIRECTIVE, HTTP_ARG, SEARCH_DIRECTIVE};
use crate::errors::{ErrorList, GraphQlError};
use crate::model::{Directive, FieldDef, Schema, TypeDef, Value};
use crate::rules_util::{
    has_custom_or_lambda, has_id_directive, is_query_or_mutation_type, is_scalar,
};

/// Builds the synthetic `@custom` directive used to validate `@lambda`.
pub(crate) fn custom_directive_for_lambda(field: &FieldDef) -> Directive {
    let mut template = String::from("{");
    let mut comma = "";
    for arg in &field.args {
        template.push_str(comma);
        template.push_str(&arg.name);
        template.push_str(": $");
        template.push_str(&arg.name);
        comma = ", ";
    }
    template.push('}');
    Directive {
        name: CUSTOM_DIRECTIVE.to_owned(),
        args: vec![(
            "http".to_owned(),
            Value::Object(vec![
                (
                    "url".to_owned(),
                    Value::String("http://localhost:8686/graphql-worker".to_owned()),
                ),
                ("method".to_owned(), Value::Enum("POST".to_owned())),
                ("body".to_owned(), Value::String(template)),
            ]),
        )],
        position: (0, 0),
    }
}

/// Parses a body template into JSON plus required variables.
///
/// Template syntax: `{ k1: 3.4, k2: $var, k3: "string" }`. Port of Go
/// `parseBodyTemplate` (strict mode rejects block strings and enums).
pub(crate) fn parse_body_template(
    body: &str,
    strict_json: bool,
) -> Result<(serde_json::Value, HashSet<String>), String> {
    if body.trim().is_empty() {
        return Ok((serde_json::Value::Null, HashSet::new()));
    }
    let doc =
        parse_query(format!("query {{ dummy(input: {body}) }}")).map_err(|err| err.to_string())?;
    let Some((_, op)) = doc.operations.iter().next() else {
        return Err("missing operation".to_owned());
    };
    let Some(field) = op.node.selection_set.node.items.first() else {
        return Err("missing field".to_owned());
    };
    let async_graphql_parser::types::Selection::Field(field) = &field.node else {
        return Err("missing field".to_owned());
    };
    let Some((_, val)) = field.node.arguments.first() else {
        return Err("missing argument".to_owned());
    };
    let mut vars = HashSet::new();
    let json = template_value(&val.node, &mut vars, strict_json)?;
    Ok((json, vars))
}

/// Converts one template value, collecting `$variables`.
fn template_value(
    val: &QueryValue,
    vars: &mut HashSet<String>,
    strict_json: bool,
) -> Result<serde_json::Value, String> {
    match val {
        QueryValue::Object(fields) => {
            let mut map = serde_json::Map::new();
            for (key, val) in fields {
                map.insert(key.to_string(), template_value(val, vars, strict_json)?);
            }
            Ok(serde_json::Value::Object(map))
        }
        QueryValue::List(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(template_value(item, vars, strict_json)?);
            }
            Ok(serde_json::Value::Array(out))
        }
        QueryValue::Variable(name) => {
            vars.insert(name.to_string());
            Ok(serde_json::Value::String(format!("${name}")))
        }
        QueryValue::Number(num) => num.as_i64().map_or_else(
            || {
                num.as_f64().map_or_else(
                    || Err(format!("bad number: {num}")),
                    |num| {
                        serde_json::Number::from_f64(num)
                            .map(serde_json::Value::Number)
                            .ok_or_else(|| format!("bad number: {num}"))
                    },
                )
            },
            |num| Ok(serde_json::Value::Number(num.into())),
        ),
        QueryValue::Boolean(val) => Ok(serde_json::Value::Bool(*val)),
        QueryValue::String(text) => Ok(serde_json::Value::String(text.clone())),
        QueryValue::Enum(name) => {
            if strict_json {
                return Err(format!("found unexpected value: {name}"));
            }
            Ok(serde_json::Value::String(name.to_string()))
        }
        QueryValue::Binary(bytes) => Ok(serde_json::Value::String(format!("{bytes:?}"))),
        QueryValue::Null => Ok(serde_json::Value::Null),
    }
}

/// First ID field of a type, if any.
fn id_field(typ: &TypeDef) -> Option<&FieldDef> {
    typ.fields.iter().find(|f| f.ty.name() == "ID")
}

/// First `@id` field of a type, if any.
fn xid_field(typ: &TypeDef) -> Option<&FieldDef> {
    typ.fields.iter().find(|f| has_id_directive(f))
}

/// Splits a URL into path segments and query values (no decoding).
fn split_url(url: &str) -> (Vec<String>, Vec<String>) {
    let without_scheme = url.split("://").nth(1).unwrap_or(url);
    let path_start = without_scheme
        .find('/')
        .map_or("", |idx| &without_scheme[idx..]);
    let (path, query) = path_start.split_once('?').unwrap_or((path_start, ""));
    let segments = path.split('/').map(str::to_owned).collect();
    let mut values = Vec::new();
    for pair in query.split('&') {
        if let Some((_, val)) = pair.split_once('=') {
            values.push(val.to_owned());
        }
    }
    (segments, values)
}

/// Validates `@custom` (steps 1-11 local; step 12 network skipped).
#[allow(
    clippy::too_many_lines,
    reason = "exact port of Go customDirectiveValidation"
)]
pub(crate) fn custom_directive_validation<H: std::hash::BuildHasher>(
    schema: &Schema,
    typ: &TypeDef,
    field: &FieldDef,
    dir: &Directive,
    secrets: &HashMap<String, String, H>,
) -> ErrorList {
    let mut errs = ErrorList::default();
    let at_dir = |message: String| GraphQlError::at(message, dir.position.0, dir.position.1);

    if field.directive(SEARCH_DIRECTIVE).is_some() {
        errs.push(at_dir(format!(
            "Type {}; Field {}; custom directive not allowed along with @search directive.",
            typ.name, field.name
        )));
    }
    if field.directive(DATASTREAMDB_DIRECTIVE).is_some() {
        errs.push(at_dir(format!(
            "Type {}; Field {}; custom directive not allowed along with @datastreamdb directive.",
            typ.name, field.name
        )));
    }
    if !is_query_or_mutation_type(typ) && id_field(typ).is_none() && xid_field(typ).is_none() {
        errs.push(at_dir(format!(
            "Type {}; Field {}; @custom directive is only allowed on fields where the type \
             definition has a field with type ID! or a field with @id directive.",
            typ.name, field.name
        )));
    }

    if dir.args.is_empty() || dir.args.len() > 1 {
        errs.push(at_dir(format!(
            "Type {}; Field {}: has {} arguments for @custom directive, it should contain \
             exactly one of `http` or `dql` arguments.",
            typ.name,
            field.name,
            dir.args.len()
        )));
    }
    let http_arg = dir.arg(HTTP_ARG);
    let dql_arg = dir.arg("dql");
    if http_arg.is_none() && dql_arg.is_none() {
        errs.push(at_dir(format!(
            "Type {}; Field {}: one of `http` or `dql` arguments must be present for @custom \
             directive.",
            typ.name, field.name
        )));
        return errs;
    }

    if let Some(dql) = dql_arg {
        if typ.name != "Query" {
            errs.push(at_dir(format!(
                "Type {}; Field {}: @custom directive with `dql` can be used only on queries.",
                typ.name, field.name
            )));
        }
        if !matches!(dql, Value::String(_)) {
            errs.push(at_dir(format!(
                "Type {}; Field {}: dql argument for @custom directive must be of type String.",
                typ.name, field.name
            )));
        }
        if dql.raw().trim().is_empty() {
            errs.push(at_dir(format!(
                "Type {}; Field {}: dql argument for @custom directive must not be empty.",
                typ.name, field.name
            )));
        }
        for arg in &field.args {
            if arg.ty.is_list() || !is_scalar(arg.ty.name()) {
                errs.push(at_dir(format!(
                    "Type {}; Field {}: Argument {}: must be of a scalar type. @custom DQL \
                     queries accept only scalar arguments.",
                    typ.name, field.name, arg.name
                )));
            }
        }
        return errs;
    }

    let Some(http) = http_arg else {
        return errs;
    };
    if !matches!(http, Value::Object(_)) || http.is_empty_object() {
        if http.is_empty_object() || !matches!(http, Value::Object(_)) && http.raw().is_empty() {
            errs.push(at_dir(format!(
                "Type {}; Field {}: http argument for @custom directive should not be empty.",
                typ.name, field.name
            )));
            return errs;
        }
        errs.push(at_dir(format!(
            "Type {}; Field {}: http argument for @custom directive should be of type Object.",
            typ.name, field.name
        )));
    }

    let Some(url_val) = http.field("url") else {
        errs.push(at_dir(format!(
            "Type {}; Field {}; url field inside @custom directive is mandatory.",
            typ.name, field.name
        )));
        return errs;
    };
    let url = url_val.raw();
    if url_parse_error(&url) {
        errs.push(at_dir(format!(
            "Type {}; Field {}; url field inside @custom directive is invalid.",
            typ.name, field.name
        )));
        return errs;
    }
    let (segments, query_values) = split_url(&url);
    let mut url_vars: Vec<(String, &str)> = Vec::new();
    for elem in &segments {
        if let Some(name) = elem.strip_prefix('$') {
            url_vars.push((name.to_owned(), "path"));
        }
    }
    for val in &query_values {
        if let Some(name) = val.strip_prefix('$') {
            url_vars.push((name.to_owned(), "query"));
        }
    }
    let url_has_params = !url_vars.is_empty();
    for (var_name, location) in &url_vars {
        if is_query_or_mutation_type(typ) {
            let Some(arg) = field.args.iter().find(|arg| &arg.name == var_name) else {
                errs.push(at_dir(format!(
                    "Type {}; Field {}; url {location} inside @custom directive uses an \
                     argument {var_name} that is not defined.",
                    typ.name, field.name
                )));
                continue;
            };
            if *location == "path" && arg.ty.nullable {
                errs.push(at_dir(format!(
                    "Type {}; Field {}; url {location} inside @custom directive uses an \
                     argument {var_name} that can be null.",
                    typ.name, field.name
                )));
            }
        } else {
            let Some(target) = typ.field(var_name) else {
                errs.push(at_dir(format!(
                    "Type {}; Field {}; url {location} inside @custom directive uses a field \
                     {var_name} that is not defined.",
                    typ.name, field.name
                )));
                continue;
            };
            if *location == "path" && target.ty.nullable {
                errs.push(at_dir(format!(
                    "Type {}; Field {}; url {location} inside @custom directive uses a field \
                     {var_name} that can be null.",
                    typ.name, field.name
                )));
            }
        }
    }

    let method = http.field("method");
    if method.is_none() {
        errs.push(at_dir(format!(
            "Type {}; Field {}; method field inside @custom directive is mandatory.",
            typ.name, field.name
        )));
    } else if !matches!(
        method.map(Value::raw).as_deref(),
        Some("GET" | "POST" | "PUT" | "PATCH" | "DELETE")
    ) {
        errs.push(at_dir(format!(
            "Type {}; Field {}; method field inside @custom directive can only be \
             GET/POST/PUT/PATCH/DELETE.",
            typ.name, field.name
        )));
    }
    let method_raw = method.map(Value::raw).unwrap_or_default();

    let mode = http.field("mode");
    let mut is_batch = false;
    if let Some(mode) = mode {
        if is_query_or_mutation_type(typ) {
            errs.push(at_dir(format!(
                "Type {}; Field {}; mode field inside @custom directive can't be present on \
                 Query/Mutation.",
                typ.name, field.name
            )));
        }
        if mode.raw() != "SINGLE" && mode.raw() != "BATCH" {
            errs.push(at_dir(format!(
                "Type {}; Field {}; mode field inside @custom directive can only be \
                 SINGLE/BATCH.",
                typ.name, field.name
            )));
        }
        is_batch = mode.raw() == "BATCH";
        if is_batch && url_has_params {
            errs.push(at_dir(format!(
                "Type {}; Field {}; has parameters in url inside @custom directive while mode \
                 is BATCH, url can't contain parameters if mode is BATCH.",
                typ.name, field.name
            )));
        }
    }

    let body = http.field("body");
    let graphql = http.field("graphql");
    if graphql.is_some() {
        if url_has_params {
            errs.push(at_dir(format!(
                "Type {}; Field {}; has parameters in url along with graphql field inside \
                 @custom directive, url can't contain parameters if graphql field is present.",
                typ.name, field.name
            )));
        }
        if method_raw != "POST" {
            errs.push(at_dir(format!(
                "Type {}; Field {}; has method {method_raw} while graphql field is also present \
                 inside @custom directive, method can only be POST if graphql field is present.",
                typ.name, field.name
            )));
        }
        if !is_batch {
            if body.is_some() {
                errs.push(at_dir(format!(
                    "Type {}; Field {}; has both body and graphql field inside @custom \
                     directive, they can't be present together.",
                    typ.name, field.name
                )));
            }
        } else if body.is_none() {
            errs.push(at_dir(format!(
                "Type {}; Field {}; both body and graphql field inside @custom directive are \
                 required if mode is BATCH.",
                typ.name, field.name
            )));
        }
    }

    let mut required_fields: HashSet<String> = HashSet::new();
    if let Some(body) = body {
        match parse_body_template(&body.raw(), true) {
            Ok((_, vars)) => required_fields = vars,
            Err(err) => errs.push(at_dir(format!(
                "Type {}; Field {}; body template inside @custom directive could not be \
                 parsed: {err}",
                typ.name, field.name
            ))),
        }
        if is_query_or_mutation_type(typ) {
            for name in &required_fields {
                if !field.args.iter().any(|arg| &arg.name == name) {
                    errs.push(at_dir(format!(
                        "Type {}; Field {}; body template inside @custom directive uses an \
                         argument {name} that is not defined.",
                        typ.name, field.name
                    )));
                }
            }
        }
    }

    if let Some(graphql) = graphql {
        match parse_custom_operation(&graphql.raw()) {
            Ok(op) => {
                validate_custom_operation(schema, typ, field, &op, is_batch, &mut errs);
                if is_query_or_mutation_type(typ) {
                    for name in &op.variables {
                        if !field.args.iter().any(|arg| &arg.name == name) {
                            errs.push(at_dir(format!(
                                "Type {}; Field {}; @custom directive, graphql variables must \
                                 use fields defined within the type, found `{name}`.",
                                typ.name, field.name
                            )));
                        }
                    }
                } else if !is_batch {
                    required_fields = op.variables.iter().cloned().collect();
                }
            }
            Err((message, is_syntax)) => {
                if is_syntax {
                    errs.push(at_dir(format!(
                        "Type {}; Field {}: unable to parse graphql in @custom directive \
                         because: {message}",
                        typ.name, field.name
                    )));
                } else {
                    errs.push(at_dir(format!(
                        "Type {}; Field {}: {message}",
                        typ.name, field.name
                    )));
                }
                return errs;
            }
        }
    }

    if !is_query_or_mutation_type(typ) {
        let id_name = id_field(typ).map(|f| f.name.clone()).unwrap_or_default();
        let xid_name = xid_field(typ).map(|f| f.name.clone()).unwrap_or_default();
        if field.name == id_name || (!xid_name.is_empty() && field.name == xid_name) {
            errs.push(at_dir(format!(
                "Type {}; Field {}; custom directive not allowed on field of type ID! or field \
                 with @id directive.",
                typ.name, field.name
            )));
        }
        if body.is_some() || graphql.is_some() {
            let err_in = if body.is_some() {
                "body template"
            } else {
                "graphql"
            };
            let mut requires_id = false;
            for name in &required_fields {
                if *name == field.name {
                    errs.push(at_dir(format!(
                        "Type {}; Field {}; @custom directive, {err_in} can't require itself.",
                        typ.name, field.name
                    )));
                }
                let Some(target) = typ.field(name) else {
                    errs.push(at_dir(format!(
                        "Type {}; Field {}; @custom directive, {err_in} must use fields defined \
                         within the type, found `{name}`.",
                        typ.name, field.name
                    )));
                    continue;
                };
                if !is_scalar(target.ty.name()) {
                    errs.push(at_dir(format!(
                        "Type {}; Field {}; @custom directive, {err_in} must use scalar fields, \
                         found field `{name}` of type `{}`.",
                        typ.name,
                        field.name,
                        target.ty.name()
                    )));
                }
                if has_custom_or_lambda(target) {
                    errs.push(at_dir(format!(
                        "Type {}; Field {}; @custom directive, {err_in} can't use another field \
                         with @custom/@lambda directive, found field `{name}` with \
                         @custom/@lambda.",
                        typ.name, field.name
                    )));
                }
                if *name == id_name || (!xid_name.is_empty() && *name == xid_name) {
                    requires_id = true;
                }
            }
            if !requires_id {
                errs.push(at_dir(format!(
                    "Type {}; Field {}: @custom directive, {err_in} must use a field with type \
                     ID! or a field with @id directive.",
                    typ.name, field.name
                )));
            }
        }
    }

    if body.is_some() || graphql.is_some() {
        if let Some(skip) = http.field("skipIntrospection") {
            if !matches!(skip.raw().as_str(), "true" | "false") {
                errs.push(at_dir(format!(
                    "Type {}; Field {}; skipIntrospection in @custom directive can only be \
                     true/false, found: `{}`.",
                    typ.name,
                    field.name,
                    skip.raw()
                )));
            }
        }
        let mut forward: HashSet<String> = HashSet::new();
        if let Some(Value::List(headers)) = http.field("forwardHeaders") {
            for header in headers {
                let raw = header.raw();
                let parts: Vec<&str> = raw.split(':').collect();
                if parts.len() > 2 {
                    errs.push(at_dir(format!(
                        "Type {}; Field {}; forwardHeaders in @custom directive should be of \
                         the form 'remote_headername:local_headername' or just 'headername', \
                         found: `{}`.",
                        typ.name,
                        field.name,
                        header.raw()
                    )));
                    return errs;
                }
                forward.insert(parts[0].to_owned());
            }
        }
        if let Some(Value::List(headers)) = http.field("secretHeaders") {
            for header in headers {
                let raw = header.raw();
                let parts: Vec<&str> = raw.split(':').collect();
                if parts.len() > 2 {
                    errs.push(at_dir(format!(
                        "Type {}; Field {}; secretHeaders in @custom directive should be of the \
                         form 'remote_headername:local_headername' or just 'headername', found: \
                         `{}`.",
                        typ.name,
                        field.name,
                        header.raw()
                    )));
                    return errs;
                }
                if forward.contains(parts[0]) {
                    errs.push(at_dir(format!(
                        "Type {}; Field {}; secretHeaders and forwardHeaders in @custom \
                         directive cannot have overlapping headers, found: `{}`.",
                        typ.name,
                        field.name,
                        header.raw()
                    )));
                    return errs;
                }
            }
        }
        if let Some(Value::List(headers)) = http.field("introspectionHeaders") {
            for header in headers {
                let raw = header.raw();
                let parts: Vec<&str> = raw.split(':').collect();
                if parts.len() > 2
                    || (parts.len() == 2 && (parts[0].is_empty() || parts[1].is_empty()))
                {
                    errs.push(at_dir(format!(
                        "Type {}; Field {}; introspectionHeaders in @custom directive should be \
                         of the form 'remote_headername:local_headername' or just 'headername', \
                         found: `{}`.",
                        typ.name,
                        field.name,
                        header.raw()
                    )));
                    return errs;
                }
            }
        }
    }

    if errs.is_empty() && graphql.is_some() {
        // Step 12 (local half): introspection headers must reference declared
        // secrets. The remote introspection HTTP call itself is skipped in
        // embedded mode (no network at schema-build time).
        let skip = http
            .field("skipIntrospection")
            .is_some_and(|val| val.raw() == "true");
        if !skip {
            if let Some(Value::List(headers)) = http.field("introspectionHeaders") {
                for header in headers {
                    let raw = header.raw();
                    let parts: Vec<&str> = raw.split(':').collect();
                    let val = if parts.len() == 1 { parts[0] } else { parts[1] };
                    if !secrets.contains_key(val) {
                        errs.push(at_dir(format!(
                            "Type {}; Field {}; introspectionHeaders in @custom directive should \
                             use secrets to store the header value. To do that specify `{val}` \
                             in this format '#DataStreamDB.Secret name value' at the bottom of your \
                             schema file.",
                            typ.name, field.name
                        )));
                        return errs;
                    }
                }
            }
        }
    }
    errs
}

/// Whether the URL fails request-URI parsing (absolute URI or path required).
fn url_parse_error(url: &str) -> bool {
    if url.is_empty() || url.chars().any(char::is_whitespace) {
        return true;
    }
    if let Some(rest) = url.split("://").nth(1) {
        return rest.is_empty() || rest.starts_with('/');
    }
    !url.starts_with('/')
}

/// A parsed `@custom(graphql:)` operation.
struct ParsedCustomOp {
    /// `query` or `mutation`.
    operation: String,
    /// Variable names.
    variables: Vec<String>,
    /// Single root field name and alias.
    field: String,
    /// Field alias.
    alias: String,
    /// Whether the field carries directives.
    field_has_directives: bool,
    /// Whether the field has a selection set.
    field_has_selection: bool,
    /// Whether the operation carries directives.
    op_has_directives: bool,
    /// Arguments as (name, variable-name-or-empty).
    args: Vec<(String, String)>,
    /// Argument template for variable checks.
    arg_template: String,
    /// Operation name, if any.
    name: String,
    /// Selection count.
    selection_count: usize,
}

/// Parses the `graphql` template of `@custom(http:)`.
///
/// Returns the operation or (message, `is_syntax_error`).
fn parse_custom_operation(raw: &str) -> Result<ParsedCustomOp, (String, bool)> {
    let doc = parse_query(raw).map_err(|err| (err.to_string(), true))?;
    let ops: Vec<_> = doc.operations.iter().collect();
    if ops.len() != 1 {
        return Err((
            format!(
                "inside graphql in @custom directive, found {} operations, it can have exactly \
                 one operation.",
                ops.len()
            ),
            false,
        ));
    }
    let (name, op) = ops[0];
    let operation = op.node.ty.to_string();
    let variables: Vec<String> = op
        .node
        .variable_definitions
        .iter()
        .map(|def| def.node.name.node.to_string())
        .collect();
    let selection_count = op.node.selection_set.node.items.len();
    let empty = ParsedCustomOp {
        operation: operation.clone(),
        variables,
        field: String::new(),
        alias: String::new(),
        field_has_directives: false,
        field_has_selection: false,
        op_has_directives: !op.node.directives.is_empty(),
        args: Vec::new(),
        arg_template: String::new(),
        name: name.map(ToString::to_string).unwrap_or_default(),
        selection_count,
    };
    if selection_count != 1 {
        return Ok(empty);
    }
    let selection = &op.node.selection_set.node.items[0].node;
    let async_graphql_parser::types::Selection::Field(field) = selection else {
        return Ok(empty);
    };
    let mut args = Vec::new();
    let mut template = String::from("{");
    for (i, (arg_name, arg_val)) in field.node.arguments.iter().enumerate() {
        if i > 0 {
            template.push(',');
        }
        template.push_str(arg_name.node.as_str());
        template.push(':');
        template.push_str(&arg_value_template(&arg_val.node));
        let var_name = match &arg_val.node {
            QueryValue::Variable(name) => name.to_string(),
            _ => String::new(),
        };
        args.push((arg_name.node.to_string(), var_name));
    }
    template.push('}');
    Ok(ParsedCustomOp {
        field: field.node.name.node.to_string(),
        alias: field.node.alias.as_ref().map_or_else(
            || field.node.name.node.to_string(),
            |alias| alias.node.to_string(),
        ),
        field_has_directives: !field.node.directives.is_empty(),
        field_has_selection: !field.node.selection_set.node.items.is_empty(),
        args,
        arg_template: template,
        ..empty
    })
}

/// Renders an argument value for template parsing.
fn arg_value_template(val: &QueryValue) -> String {
    match val {
        QueryValue::Variable(name) => format!("${name}"),
        QueryValue::Number(num) => num.to_string(),
        QueryValue::String(text) => format!("\"{text}\""),
        QueryValue::Boolean(val) => val.to_string(),
        QueryValue::Enum(name) => name.to_string(),
        QueryValue::Binary(bytes) => format!("{bytes:?}"),
        QueryValue::Null => "null".to_owned(),
        QueryValue::List(items) => {
            let inner: Vec<String> = items.iter().map(arg_value_template).collect();
            format!("[{}]", inner.join(","))
        }
        QueryValue::Object(fields) => {
            let inner: Vec<String> = fields
                .iter()
                .map(|(key, val)| format!("{key}:{}", arg_value_template(val)))
                .collect();
            format!("{{{}}}", inner.join(","))
        }
    }
}

/// Validates the shape of a `@custom(graphql:)` operation.
fn validate_custom_operation(
    _schema: &Schema,
    typ: &TypeDef,
    field: &FieldDef,
    op: &ParsedCustomOp,
    is_batch: bool,
    errs: &mut ErrorList,
) {
    let mut here = |message: String| {
        errs.push(GraphQlError::new(format!(
            "Type {}; Field {}: {message}",
            typ.name, field.name
        )));
    };
    if op.operation != "query" && op.operation != "mutation" {
        here(format!(
            "inside graphql in @custom directive, found `{}` operation, it can only have \
             query/mutation.",
            op.operation
        ));
    }
    if !op.name.is_empty() {
        here(format!(
            "inside graphql in @custom directive, found operation with name `{}`, it can't \
             have a name.",
            op.name
        ));
    }
    if op.op_has_directives {
        here(
            "inside graphql in @custom directive, found operation with directives, it can't \
             have any directives."
                .to_owned(),
        );
    }
    if op.selection_count != 1 {
        here(format!(
            "inside graphql in @custom directive, found {} fields inside operation `{}`, it can \
             have exactly one field.",
            op.selection_count, op.operation
        ));
        return;
    }
    if op.alias != op.field {
        here(format!(
            "inside graphql in @custom directive, found {} `{}` with alias `{}`, it can't have \
             any alias.",
            op.operation, op.field, op.alias
        ));
    }
    if op.field_has_directives {
        here(format!(
            "inside graphql in @custom directive, found {} `{}` with directives, it can't have \
             any directives.",
            op.operation, op.field
        ));
    }
    if op.field_has_selection {
        here(format!(
            "inside graphql in @custom directive, found {} `{}` with a selection set, it can't \
             have any selection set.",
            op.operation, op.field
        ));
    }
    if op.args.is_empty() {
        return;
    }
    if is_batch {
        if op.args.len() != 1 || op.args[0].1.is_empty() {
            here(format!(
                "inside graphql in @custom directive, for BATCH mode, {} `{}` can have only one \
                 argument whose value should be a variable.",
                op.operation, op.field
            ));
            return;
        }
        if !op.variables.iter().any(|var| var == &op.args[0].1) {
            here(format!(
                "Type {}; Field {}; @custom directive, graphql must use fields with a variable \
                 definition, found `{}`.",
                typ.name, field.name, op.args[0].1
            ));
        }
        return;
    }
    match parse_body_template(&op.arg_template, false) {
        Ok((_, vars)) => {
            for name in vars {
                if !op.variables.iter().any(|var| var == &name) {
                    here(format!(
                        "@custom directive, graphql must use fields with a variable definition, \
                         found `{name}`."
                    ));
                }
            }
        }
        Err(err) => here(format!(
            "inside graphql in @custom directive, error in parsing arguments for {} `{}`: \
             {err}.",
            op.operation, op.field
        )),
    }
}
