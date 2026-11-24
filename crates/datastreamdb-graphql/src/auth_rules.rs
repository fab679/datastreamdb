//! `@auth` rule parsing and static evaluation. Port of
//! `dgraph-main/graphql/schema/auth.go`.
//!
//! GraphQL (non-RBAC) rules keep their source text plus variable names; the
//! resolve stage rewrites them to DQL at query time (Go stores the parsed
//! operation in `RuleNode.Rule` / `DQLRule` for the same purpose).

#![warn(missing_docs)]

use std::collections::HashMap;

use regex::Regex;

use crate::errors::{ErrorList, GraphQlError};
use crate::model::{Schema, TypeDef, Value};
use crate::rules_util::type_name;

/// Prefix marking an RBAC rule (`{...}` object syntax).
pub(crate) const RBAC_QUERY_PREFIX: &str = "{";

/// Static evaluation outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuleResult {
    /// Needs query-time evaluation.
    Uncertain,
    /// Rule passes statically.
    Positive,
    /// Rule fails statically.
    Negative,
}

/// An RBAC rule: `$var: { op: operand }`.
#[derive(Debug, Clone)]
pub(crate) struct RbacQuery {
    /// Variable name without `$`.
    pub variable: String,
    /// Operand (JSON value).
    pub operand: serde_json::Value,
    /// Compiled regex for `regexp` rules.
    pub regex: Option<Regex>,
}

impl RbacQuery {
    /// Evaluates the rule against one token value.
    fn check_if_match(&self, value: &serde_json::Value) -> RuleResult {
        if let Some(operands) = self.operand.as_array() {
            for operand in operands {
                if evaluate(operand, value, self.regex.as_ref()) == RuleResult::Positive {
                    return RuleResult::Positive;
                }
            }
            return RuleResult::Negative;
        }
        evaluate(&self.operand, value, self.regex.as_ref())
    }

    /// Evaluates the rule against an array of token values.
    fn check_if_match_in_array(&self, array: &[serde_json::Value]) -> RuleResult {
        for value in array {
            if self.check_if_match(value) == RuleResult::Positive {
                return RuleResult::Positive;
            }
        }
        RuleResult::Negative
    }

    /// Evaluates the rule against auth variables.
    pub(crate) fn evaluate_rbac_rule(&self, vars: &AuthVars) -> RuleResult {
        match vars.get(&self.variable) {
            Some(serde_json::Value::Array(values)) => self.check_if_match_in_array(values),
            Some(value) => self.check_if_match(value),
            None => self.check_if_match(&serde_json::Value::Null),
        }
    }
}

/// Compares one operand against one value (regex or deep equality).
fn evaluate(
    operand: &serde_json::Value,
    value: &serde_json::Value,
    regex: Option<&Regex>,
) -> RuleResult {
    if let Some(regex) = regex {
        if value.as_str().is_some_and(|text| regex.is_match(text)) {
            return RuleResult::Positive;
        }
        return RuleResult::Negative;
    }
    if value == operand {
        RuleResult::Positive
    } else {
        RuleResult::Negative
    }
}

/// Auth variables extracted from a JWT (`variable -> JSON value`).
pub type AuthVars = HashMap<String, serde_json::Value>;

/// A parsed `@auth` rule tree.
#[derive(Debug, Clone, Default)]
pub(crate) struct RuleNode {
    /// `or` branches.
    pub or: Vec<RuleNode>,
    /// `and` branches.
    pub and: Vec<RuleNode>,
    /// `not` branch.
    pub not: Option<Box<RuleNode>>,
    /// GraphQL rule text (non-RBAC rules).
    pub rule: Option<String>,
    /// Variables used by the GraphQL rule.
    pub variables: Vec<String>,
    /// RBAC rule (leaf).
    pub rbac_rule: Option<RbacQuery>,
}

impl RuleNode {
    /// Checks that all rule variables are present (non-null) in `vars`.
    fn static_evaluation(&self, vars: &AuthVars) -> RuleResult {
        for var in &self.variables {
            if vars.get(var).is_none_or(serde_json::Value::is_null) {
                return RuleResult::Negative;
            }
        }
        RuleResult::Uncertain
    }

    /// Statically evaluates the rule tree against auth variables.
    pub(crate) fn evaluate_static(&self, vars: &AuthVars) -> RuleResult {
        let mut uncertain = false;
        for rule in &self.or {
            match rule.evaluate_static(vars) {
                RuleResult::Positive => return RuleResult::Positive,
                RuleResult::Uncertain => uncertain = true,
                RuleResult::Negative => {}
            }
        }
        if !self.or.is_empty() && !uncertain {
            return RuleResult::Negative;
        }
        for rule in &self.and {
            match rule.evaluate_static(vars) {
                RuleResult::Negative => return RuleResult::Negative,
                RuleResult::Uncertain => uncertain = true,
                RuleResult::Positive => {}
            }
        }
        if !self.and.is_empty() && !uncertain {
            return RuleResult::Positive;
        }
        if let Some(not) = &self.not {
            let result = not.evaluate_static(vars);
            if not.rbac_rule.is_none() {
                return result;
            }
            return match result {
                RuleResult::Uncertain => RuleResult::Uncertain,
                RuleResult::Positive => RuleResult::Negative,
                RuleResult::Negative => RuleResult::Positive,
            };
        }
        if let Some(rbac) = &self.rbac_rule {
            return rbac.evaluate_rbac_rule(vars);
        }
        if self.rule.is_some() {
            return self.static_evaluation(vars);
        }
        RuleResult::Uncertain
    }
}

/// `@auth` rules per operation.
#[derive(Debug, Clone, Default)]
pub(crate) struct AuthContainer {
    /// Password-query rules.
    pub password: Option<RuleNode>,
    /// Query rules.
    pub query: Option<RuleNode>,
    /// Add-mutation rules.
    pub add: Option<RuleNode>,
    /// Update-mutation rules.
    pub update: Option<RuleNode>,
    /// Delete-mutation rules.
    pub delete: Option<RuleNode>,
}

/// Auth rules for a type: type-level plus per-field.
#[derive(Debug, Clone, Default)]
pub(crate) struct TypeAuth {
    /// Type-level rules.
    pub rules: Option<AuthContainer>,
    /// Field-level rules by field name.
    pub fields: HashMap<String, AuthContainer>,
}

/// Parses `@auth` directives across the schema, merging interface rules into
/// implementors with `and`. Port of Go `authRules`.
///
/// # Errors
///
/// Returns [`ErrorList`] when any rule fails to parse.
pub(crate) fn auth_rules(schema: &Schema) -> Result<HashMap<String, TypeAuth>, ErrorList> {
    let mut rules: HashMap<String, TypeAuth> = HashMap::new();
    let mut errs = ErrorList::default();
    for name in &schema.order {
        let typ = &schema.types[name];
        let type_auth_name = type_name(typ);
        let mut entry = TypeAuth::default();
        if let Some(auth) = typ.directive(crate::directives::AUTH_DIRECTIVE) {
            match parse_auth_directive(schema, typ, auth) {
                Ok(container) => entry.rules = container,
                Err(list) => errs.extend(list),
            }
        }
        for field in &typ.fields {
            if let Some(auth) = field.directive(crate::directives::AUTH_DIRECTIVE) {
                match parse_auth_directive(schema, typ, auth) {
                    Ok(Some(container)) => {
                        entry.fields.insert(field.name.clone(), container);
                    }
                    Ok(None) => {}
                    Err(list) => errs.extend(list),
                }
            }
        }
        rules.insert(type_auth_name, entry);
    }
    for name in &schema.order {
        let typ = &schema.types[name];
        if typ.kind != crate::model::TypeKind::Object {
            continue;
        }
        for iface in &typ.implements {
            let Some(interface_def) = schema.type_def(iface) else {
                continue;
            };
            let interface_name = type_name(interface_def);
            let interface_rules = rules
                .get(&interface_name)
                .and_then(|auth| auth.rules.clone());
            let Some(interface_rules) = interface_rules else {
                continue;
            };
            let object_name = type_name(typ);
            let object_rules = rules.get(&object_name).and_then(|auth| auth.rules.clone());
            let merged = merge_auth_rules(object_rules, &interface_rules);
            if let Some(entry) = rules.get_mut(&object_name) {
                entry.rules = Some(merged);
            }
        }
    }
    for name in &schema.order {
        let typ = &schema.types[name];
        if typ.kind == crate::model::TypeKind::Interface {
            rules.insert(type_name(typ), TypeAuth::default());
        }
    }
    if errs.is_empty() {
        Ok(rules)
    } else {
        Err(errs)
    }
}

/// Merges two rule nodes with `and` (either may be absent).
fn merge_auth_node_with_and(
    object: Option<RuleNode>,
    interface: Option<RuleNode>,
) -> Option<RuleNode> {
    match (object, interface) {
        (None, None) => None,
        (Some(node), None) | (None, Some(node)) => Some(node),
        (Some(object), Some(interface)) => Some(RuleNode {
            and: vec![object, interface],
            ..RuleNode::default()
        }),
    }
}

/// Merges interface rules into object rules, field by field.
fn merge_auth_rules(object: Option<AuthContainer>, interface: &AuthContainer) -> AuthContainer {
    let Some(object) = object else {
        return interface.clone();
    };
    AuthContainer {
        password: merge_auth_node_with_and(object.password, interface.password.clone()),
        query: merge_auth_node_with_and(object.query, interface.query.clone()),
        add: merge_auth_node_with_and(object.add, interface.add.clone()),
        delete: merge_auth_node_with_and(object.delete, interface.delete.clone()),
        update: merge_auth_node_with_and(object.update, interface.update.clone()),
    }
}

/// Parses one `@auth` directive into per-operation rules.
fn parse_auth_directive(
    schema: &Schema,
    typ: &TypeDef,
    dir: &crate::model::Directive,
) -> Result<Option<AuthContainer>, ErrorList> {
    if dir.args.is_empty() {
        return Ok(None);
    }
    let mut errs = ErrorList::default();
    let mut result = AuthContainer::default();
    for (arg, slot) in [
        ("password", &mut result.password),
        ("query", &mut result.query),
        ("add", &mut result.add),
        ("update", &mut result.update),
        ("delete", &mut result.delete),
    ] {
        if let Some(val) = dir.arg(arg) {
            match parse_auth_node(schema, typ, val) {
                Ok(node) => *slot = Some(node),
                Err(list) => errs.extend(list),
            }
        }
    }
    if errs.is_empty() {
        Ok(Some(result))
    } else {
        Err(errs)
    }
}

/// Parses one `AuthRule` value into a rule tree.
fn parse_auth_node(schema: &Schema, typ: &TypeDef, val: &Value) -> Result<RuleNode, ErrorList> {
    let mut errs = ErrorList::default();
    let fields = match val {
        Value::Object(fields) if !fields.is_empty() => fields,
        _ => {
            errs.push(GraphQlError::new(format!(
                "Type {}: @auth: no arguments - there should be only one of \"and\", \"or\", \
                 \"not\" and \"rule\"",
                typ.name
            )));
            return Err(errs);
        }
    };
    let mut children = 0;
    let mut result = RuleNode::default();
    if let Some(Value::List(items)) = val.field("or") {
        if !items.is_empty() {
            for item in items {
                match parse_auth_node(schema, typ, item) {
                    Ok(node) => result.or.push(node),
                    Err(list) => errs.extend(list),
                }
            }
            if result.or.len() < 2 {
                errs.push(GraphQlError::new(format!(
                    "Type {}: @auth: 'OR' should contain at least two rules",
                    typ.name
                )));
            }
            children += 1;
        }
    }
    if let Some(Value::List(items)) = val.field("and") {
        if !items.is_empty() {
            for item in items {
                match parse_auth_node(schema, typ, item) {
                    Ok(node) => result.and.push(node),
                    Err(list) => errs.extend(list),
                }
            }
            if result.and.len() < 2 {
                errs.push(GraphQlError::new(format!(
                    "Type {}: @auth: 'AND' should contain at least two rules",
                    typ.name
                )));
            }
            children += 1;
        }
    }
    if let Some(not) = val.field("not") {
        if let Value::Object(inner) = not {
            if inner.len() == 1 {
                match parse_auth_node(schema, typ, not) {
                    Ok(node) => result.not = Some(Box::new(node)),
                    Err(list) => errs.extend(list),
                }
                children += 1;
            }
        }
    }
    if let Some(rule) = val.field("rule") {
        let raw = rule.raw();
        if raw.starts_with(RBAC_QUERY_PREFIX) {
            match get_rbac_query(typ, &raw) {
                Ok(rbac) => result.rbac_rule = Some(rbac),
                Err(err) => errs.push(err),
            }
        } else if let Err(err) = gql_validate_rule(schema, typ, &raw, &mut result) {
            errs.push(err);
        }
        children += 1;
    }
    if children != 1 || fields.len() > 1 {
        errs.push(GraphQlError::new(format!(
            "Type {}: @auth: there should be only one of \"and\", \"or\", \"not\" and \"rule\"",
            typ.name
        )));
    }
    if errs.is_empty() {
        Ok(result)
    } else {
        Err(errs)
    }
}

/// Parses an RBAC rule string (`{$var: {op: operand}}`).
fn get_rbac_query(typ: &TypeDef, rule: &str) -> Result<RbacQuery, GraphQlError> {
    let fail = |message: String| GraphQlError::new(message);
    let Some((variable, operator, operand_raw)) = split_rbac_rule(rule) else {
        return Err(fail(format!(
            "Type {}: @auth: `{rule}` is not a valid rule.",
            typ.name
        )));
    };
    let operand: serde_json::Value = serde_json::from_str(operand_raw).map_err(|_| {
        fail(format!(
            "Type {}: @auth: `{variable}` is not a valid GraphQL variable.",
            typ.name
        ))
    })?;
    if operand.is_null() {
        return Err(fail(format!(
            "Type {}: @auth: `{operator}` operator has invalid value. null values aren't \
             supported.",
            typ.name
        )));
    }
    validate_rbac_operators(typ, &operator, &operand)?;
    if !variable.starts_with('$') {
        return Err(fail(format!(
            "Type {}: @auth: `{variable}` is not a valid GraphQL variable.",
            typ.name
        )));
    }
    let regex = if operator == "regexp" {
        let pattern = operand.as_str().unwrap_or_default();
        Some(Regex::new(pattern).map_err(|_| {
            fail(format!(
                "Type {}: @auth: `{}` does not have a valid regex expression.",
                typ.name,
                variable.trim_start_matches('$')
            ))
        })?)
    } else {
        None
    };
    Ok(RbacQuery {
        variable: variable.trim_start_matches('$').to_owned(),
        operand,
        regex,
    })
}

/// Splits `{$var: {op: operand}}` into parts (mirrors Go's RBAC regex).
fn split_rbac_rule(rule: &str) -> Option<(String, String, &str)> {
    let trimmed = rule.trim();
    if !trimmed.starts_with('{') || !trimmed.ends_with('}') {
        return None;
    }
    let inner = trimmed[1..trimmed.len() - 1].trim();
    let colon = inner.find(':')?;
    let variable = inner[..colon].trim().to_owned();
    let rest = inner[colon + 1..].trim();
    if !rest.starts_with('{') || !rest.ends_with('}') {
        return None;
    }
    let inner = rest[1..rest.len() - 1].trim();
    let colon = inner.find(':')?;
    let operator = inner[..colon].trim().to_owned();
    if operator
        .chars()
        .any(|ch| !(ch.is_alphanumeric() || ch == '_'))
    {
        return None;
    }
    let operand = inner[colon + 1..].trim();
    Some((variable, operator, operand))
}

/// Validates RBAC operator/operand shapes.
fn validate_rbac_operators(
    typ: &TypeDef,
    operator: &str,
    operand: &serde_json::Value,
) -> Result<(), GraphQlError> {
    match operator {
        "eq" => {
            if operand.is_array() {
                return Err(GraphQlError::new(format!(
                    "Type {}: @auth: `{operator}` operator has invalid value `{operand}`. Array \
                     values in eq operator will not be supported.",
                    typ.name
                )));
            }
        }
        "regexp" => {
            if !operand.is_string() {
                return Err(GraphQlError::new(format!(
                    "Type {}: @auth: `{operator}` operator has invalid value `{operand}`. Value \
                     should be of type String.",
                    typ.name
                )));
            }
        }
        "in" => {
            if !operand.is_array() {
                return Err(GraphQlError::new(format!(
                    "Type {}: @auth: `{operator}` operator has invalid value `{operand}`. Value \
                     should be an array.",
                    typ.name
                )));
            }
        }
        _ => {
            return Err(GraphQlError::new(format!(
                "Type {}: @auth: `{operator}` operator is not supported.",
                typ.name
            )));
        }
    }
    Ok(())
}

/// Validates a GraphQL rule query against the completed schema.
fn gql_validate_rule(
    schema: &Schema,
    typ: &TypeDef,
    rule: &str,
    node: &mut RuleNode,
) -> Result<(), GraphQlError> {
    let doc = async_graphql_parser::parse_query(rule).map_err(|err| {
        GraphQlError::new(format!(
            "Type {}: @auth: failed to parse GraphQL rule [reason : {err}]",
            typ.name
        ))
    })?;
    let ops: Vec<_> = doc.operations.iter().collect();
    if ops.len() != 1 {
        return Err(GraphQlError::new(format!(
            "Type {}: @auth: a rule should be exactly one query, found {} GraphQL operations",
            typ.name,
            ops.len()
        )));
    }
    let op = ops[0].1;
    if op.node.ty != async_graphql_parser::types::OperationType::Query {
        let name = ops[0].0.map(ToString::to_string).unwrap_or_default();
        return Err(GraphQlError::new(format!(
            "Type {}: @auth: a rule should be exactly one query, found an {name}",
            typ.name
        )));
    }
    if op.node.selection_set.node.items.len() != 1 {
        return Err(GraphQlError::new(format!(
            "Type {}: @auth: a rule should be exactly one query, found {} queries",
            typ.name,
            op.node.selection_set.node.items.len()
        )));
    }
    let async_graphql_parser::types::Selection::Field(field) =
        &op.node.selection_set.node.items[0].node
    else {
        return Err(GraphQlError::new(format!(
            "Type {}: @auth: error couldn't generate query from rule",
            typ.name
        )));
    };
    let expected = format!("query{}", typ.name);
    if field.node.name.node.as_str() != expected {
        return Err(GraphQlError::new(format!(
            "Type {}: @auth: expected only query{} rules,but found {}",
            typ.name, typ.name, field.node.name.node
        )));
    }
    validate_rule_selection(schema, typ, &field.node, &expected)?;
    node.rule = Some(rule.to_owned());
    node.variables = op
        .node
        .variable_definitions
        .iter()
        .map(|def| def.node.name.node.to_string())
        .collect();
    Ok(())
}

/// Checks that a rule's field path exists in the completed schema.
fn validate_rule_selection(
    schema: &Schema,
    typ: &TypeDef,
    field: &async_graphql_parser::types::Field,
    expected: &str,
) -> Result<(), GraphQlError> {
    let Some(query) = schema.type_def("Query") else {
        return Err(GraphQlError::new(format!(
            "Type {}: @auth: failed to validate GraphQL rule [reason : missing Query root]",
            typ.name
        )));
    };
    let Some(root) = query.field(expected) else {
        return Err(GraphQlError::new(format!(
            "Type {}: @auth: failed to validate GraphQL rule [reason : unknown root {expected}]",
            typ.name
        )));
    };
    check_selection_set(schema, typ, root.ty.name(), &field.selection_set.node)
}

/// Recursively checks a selection set against the schema.
fn check_selection_set(
    schema: &Schema,
    typ: &TypeDef,
    type_name: &str,
    set: &async_graphql_parser::types::SelectionSet,
) -> Result<(), GraphQlError> {
    let Some(def) = schema.type_def(type_name) else {
        return Err(GraphQlError::new(format!(
            "Type {}: @auth: failed to validate GraphQL rule [reason : unknown type {type_name}]",
            typ.name
        )));
    };
    for item in &set.items {
        match &item.node {
            async_graphql_parser::types::Selection::Field(field) => {
                let name = field.node.name.node.as_str();
                if name.starts_with("__") {
                    continue;
                }
                let Some(target) = def.field(name) else {
                    return Err(GraphQlError::new(format!(
                        "Type {}: @auth: failed to validate GraphQL rule [reason : unknown field \
                         {name} on {type_name}]",
                        typ.name
                    )));
                };
                check_selection_set(
                    schema,
                    typ,
                    target.ty.name(),
                    &field.node.selection_set.node,
                )?;
            }
            async_graphql_parser::types::Selection::FragmentSpread(_) => {
                return Err(GraphQlError::new(format!(
                    "Type {}: @auth: failed to validate GraphQL rule [reason : fragment spreads \
                     are not supported in rules]",
                    typ.name
                )));
            }
            async_graphql_parser::types::Selection::InlineFragment(_) => {
                return Err(GraphQlError::new(format!(
                    "Type {}: @auth: failed to validate GraphQL rule [reason : inline fragments \
                     are not supported in rules]",
                    typ.name
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rbac(rule: &str) -> RbacQuery {
        let typ = TypeDef {
            name: "T".to_owned(),
            kind: crate::model::TypeKind::Object,
            description: None,
            directives: Vec::new(),
            fields: Vec::new(),
            implements: Vec::new(),
            enum_values: Vec::new(),
            possible_types: Vec::new(),
            position: (0, 0),
            builtin: false,
        };
        get_rbac_query(&typ, rule).expect("rbac")
    }

    #[test]
    fn evaluates_eq_rules() {
        let rule = rbac(r#"{$USER: {eq: "uid"}}"#);
        let mut vars = AuthVars::new();
        vars.insert("USER".to_owned(), serde_json::json!("uid"));
        assert_eq!(rule.evaluate_rbac_rule(&vars), RuleResult::Positive);
        vars.insert("USER".to_owned(), serde_json::json!(["u", "id", "uid"]));
        assert_eq!(rule.evaluate_rbac_rule(&vars), RuleResult::Positive);
        vars.insert("USER".to_owned(), serde_json::json!("other"));
        assert_eq!(rule.evaluate_rbac_rule(&vars), RuleResult::Negative);
    }

    #[test]
    fn evaluates_in_and_regexp_rules() {
        let rule = rbac(r#"{$USER: {in: ["uid", "xid"]}}"#);
        let mut vars = AuthVars::new();
        vars.insert("USER".to_owned(), serde_json::json!(["u", "uid"]));
        assert_eq!(rule.evaluate_rbac_rule(&vars), RuleResult::Positive);

        let rule = rbac(r#"{$EMAIL: {regexp: "^[^@]+@example\\.com$"}}"#);
        let mut vars = AuthVars::new();
        vars.insert("EMAIL".to_owned(), serde_json::json!("a@example.com"));
        assert_eq!(rule.evaluate_rbac_rule(&vars), RuleResult::Positive);
        vars.insert("EMAIL".to_owned(), serde_json::json!("a@other.com"));
        assert_eq!(rule.evaluate_rbac_rule(&vars), RuleResult::Negative);
    }

    #[test]
    fn rejects_bad_rbac_shapes() {
        let typ = TypeDef {
            name: "T".to_owned(),
            kind: crate::model::TypeKind::Object,
            description: None,
            directives: Vec::new(),
            fields: Vec::new(),
            implements: Vec::new(),
            enum_values: Vec::new(),
            possible_types: Vec::new(),
            position: (0, 0),
            builtin: false,
        };
        assert!(get_rbac_query(&typ, "not a rule").is_err());
        assert!(get_rbac_query(&typ, r"{$V: {eq: null}}").is_err());
        assert!(get_rbac_query(&typ, r"{$V: {eq: [1]}}").is_err());
        assert!(get_rbac_query(&typ, r"{$V: {in: 1}}").is_err());
        assert!(get_rbac_query(&typ, r"{V: {eq: 1}}").is_err());
    }
}
