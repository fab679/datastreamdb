//! DQL `$...` variables: declaration, checking, substitution.

#![warn(missing_docs)]

use std::collections::HashMap;

use datastreamdb_lex::ItemIterator;
use datastreamdb_types::strconv::{parse_go_bool, parse_go_float};
use datastreamdb_types::{TypeId, Val, Value, parse_v_float};

use super::ast::{Function, GraphQuery};
use super::state::{
    ITEM_COLON, ITEM_COMMA, ITEM_DOLLAR, ITEM_EQUAL, ITEM_MATH_OP, ITEM_NAME, ITEM_RIGHT_ROUND,
};
use crate::DqlError;

/// Declared `$...` variable info.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct VarInfo {
    /// Provided value (empty until substituted or defaulted).
    pub value: String,
    /// Declared type (`int`, `float`, `bool`, `string`, `float32vector`, `!`-suffixed).
    pub typ: String,
}

/// Map from `$name` to declared info.
pub(crate) type VarMap = HashMap<String, VarInfo>;

/// Builds a value-less map from provided variable values.
pub(crate) fn convert_to_var_map(variables: &HashMap<String, String>) -> VarMap {
    variables
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                VarInfo {
                    value: v.clone(),
                    typ: String::new(),
                },
            )
        })
        .collect()
}

/// Parses a declared value into a typed `Val`.
pub(crate) fn parse_value(v: &VarInfo) -> Result<Val, DqlError> {
    if v.value.is_empty() {
        return Err(DqlError::message("No value found"));
    }
    match v.typ.as_str() {
        "int" => match datastreamdb_types::strconv::parse_go_int_base0(&v.value) {
            Ok(i) => Ok(Val {
                tid: TypeId::Int,
                value: Value::Int(i),
            }),
            Err(e) => Err(DqlError::message(format!(
                "Expected an int but got {}: {e}",
                v.value
            ))),
        },
        "float" => match parse_go_float(&v.value) {
            Ok(f) => Ok(Val {
                tid: TypeId::Float,
                value: Value::Float(f),
            }),
            Err(e) => Err(DqlError::message(format!(
                "Expected a float but got {}: {e}",
                v.value
            ))),
        },
        "bool" => match parse_go_bool(&v.value) {
            Ok(b) => Ok(Val {
                tid: TypeId::Bool,
                value: Value::Bool(b),
            }),
            Err(e) => Err(DqlError::message(format!(
                "Expected a bool but got {}: {e}",
                v.value
            ))),
        },
        "float32vector" => match parse_v_float(&v.value) {
            Ok(vf) => Ok(Val {
                tid: TypeId::VFloat,
                value: Value::VFloat(vf),
            }),
            Err(e) => Err(DqlError::message(format!(
                "Expected a float32vector but got {}: {e}",
                v.value
            ))),
        },
        // Value is a valid string. No checks required.
        "string" => Ok(Val {
            tid: TypeId::String,
            value: Value::String(v.value.clone()),
        }),
        typ => Err(DqlError::message(format!("Type {typ:?} not supported"))),
    }
}

/// Type-checks declared variable values.
pub(crate) fn check_value_type(vm: &VarMap) -> Result<(), DqlError> {
    for (k, v) in vm {
        let mut typ = v.typ.as_str();
        if typ.is_empty() {
            return Err(DqlError::message(format!(
                "Type of variable {k} not specified"
            )));
        }
        // Ensure value is not nil if the variable is required.
        if typ.ends_with('!') {
            if v.value.is_empty() {
                return Err(DqlError::message(format!(
                    "Variable {k} should be initialised"
                )));
            }
            typ = &typ[..typ.len() - 1];
        }
        // Type check the values.
        if !v.value.is_empty() {
            match typ {
                "int" => {
                    if let Err(e) = datastreamdb_types::strconv::parse_go_int_base0(&v.value) {
                        return Err(DqlError::message(format!(
                            "Expected an int but got {}: {e}",
                            v.value
                        )));
                    }
                }
                "float" => {
                    if let Err(e) = parse_go_float(&v.value) {
                        return Err(DqlError::message(format!(
                            "Expected a float but got {}: {e}",
                            v.value
                        )));
                    }
                }
                "bool" => {
                    if let Err(e) = parse_go_bool(&v.value) {
                        return Err(DqlError::message(format!(
                            "Expected a bool but got {}: {e}",
                            v.value
                        )));
                    }
                }
                "float32vector" => {
                    if let Err(e) = parse_v_float(&v.value) {
                        // NOTE: Go's message here says "vector32float" (typo preserved).
                        return Err(DqlError::message(format!(
                            "Expected a vector32float but got {}: {e}",
                            v.value
                        )));
                    }
                }
                // Value is a valid string. No checks required.
                "string" => {}
                _ => return Err(DqlError::message(format!("Type {typ:?} not supported"))),
            }
        }
    }
    Ok(())
}

/// Substitutes one `$...` value from the map (no-op for other strings).
fn substitute_var(text: &str, out: &mut String, vmap: &VarMap) -> Result<(), DqlError> {
    if text.starts_with('$') {
        let Some(info) = vmap.get(text) else {
            return Err(DqlError::message(format!("Variable not defined {text}")));
        };
        if info.typ.is_empty() {
            return Err(DqlError::message(format!("Variable not defined {text}")));
        }
        info.value.clone_into(out);
    }
    Ok(())
}

/// Substitutes variables throughout a query tree.
pub(crate) fn substitute_variables(gq: &mut GraphQuery, vmap: &VarMap) -> Result<(), DqlError> {
    let keys: Vec<String> = gq.args.keys().cloned().collect();
    for key in &keys {
        // Values are never empty (handled in parseDqlVariables).
        let value = gq.args[key].clone();
        let mut val = value.clone();
        substitute_var(&value, &mut val, vmap)?;
        gq.args.insert(key.clone(), val);
    }

    if let Some(id_val) = gq.args.get("id").cloned() {
        if gq.uid.is_empty() {
            let uids = super::root::parse_id(&id_val)?;
            gq.uid.extend(uids);
            // Deleted here because query.go must not see it.
            gq.args.remove("id");
        }
    }

    if let Some(math) = gq.math_exp.as_mut() {
        math.subs(vmap)?;
    }

    if let Some(func) = gq.func.as_mut() {
        let attr = func.attr.clone();
        substitute_var(&attr, &mut func.attr, vmap)?;
        for idx in 0..func.args.len() {
            if !func.args[idx].is_dql_var {
                continue;
            }
            let value = func.args[idx].value.clone();
            substitute_var(&value, &mut func.args[idx].value, vmap)?;
            if func.name == "regexp" {
                regexp_variable_filter(func, idx)?;
            }
        }
    }

    for child in &mut gq.children {
        substitute_variables(child, vmap)?;
    }
    if let Some(filter) = gq.filter.as_mut() {
        substitute_variables_filter(filter, vmap)?;
    }
    if let Some(filter) = gq.facets_filter.as_mut() {
        substitute_variables_filter(filter, vmap)?;
    }
    if !gq.recurse_args.var_map.is_empty() {
        // Update the depth from the variable.
        if let Some(var_name) = gq.recurse_args.var_map.get("depth").cloned() {
            let Some(info) = vmap.get(&var_name) else {
                return Err(DqlError::message(format!(
                    "variable {var_name} not defined"
                )));
            };
            match datastreamdb_types::strconv::parse_go_uint_base0(&info.value) {
                Ok(depth) => gq.recurse_args.depth = depth,
                Err(e) => {
                    return Err(DqlError::message(format!(
                        "{var_name} should be type of integer: {e}"
                    )));
                }
            }
        }
        // Update the loop flag from the variable.
        if let Some(var_name) = gq.recurse_args.var_map.get("loop").cloned() {
            let Some(info) = vmap.get(&var_name) else {
                return Err(DqlError::message(format!(
                    "variable {var_name} not defined"
                )));
            };
            match parse_go_bool(&info.value) {
                Ok(allow) => gq.recurse_args.allow_loop = allow,
                Err(e) => {
                    return Err(DqlError::message(format!(
                        "{var_name} should be type of boolean: {e}"
                    )));
                }
            }
        }
    }
    Ok(())
}

/// Re-parses a substituted regexp into expression + flags args.
fn regexp_variable_filter(func: &mut Function, idx: usize) -> Result<(), DqlError> {
    // The value came from the user-supplied variable map; split expression
    // and flags. Regex functions end up with two args.
    let parsed = super::funcs::parse_regex_args(&func.args[idx].value)?;
    func.args[idx].value = parsed.expr;
    func.args.push(super::ast::Arg {
        value: parsed.flags,
        ..super::ast::Arg::default()
    });
    Ok(())
}

/// Substitutes variables throughout a filter tree.
fn substitute_variables_filter(
    filter: &mut super::ast::FilterTree,
    vmap: &VarMap,
) -> Result<(), DqlError> {
    if let Some(func) = filter.func.as_mut() {
        let attr = func.attr.clone();
        substitute_var(&attr, &mut func.attr, vmap)?;
        for idx in 0..func.args.len() {
            if !func.args[idx].is_dql_var {
                continue;
            }
            if func.name == super::ast::UID_FUNC {
                // DQL variables in uid functions.
                let value = func.args[idx].value.clone();
                let Some(info) = vmap.get(&value) else {
                    return Err(DqlError::message(format!(
                        "Couldn't find value for DQL variable: [{value}]"
                    )));
                };
                let uids = super::root::parse_id(&info.value)?;
                func.uid.extend(uids);
                continue;
            }
            let value = func.args[idx].value.clone();
            substitute_var(&value, &mut func.args[idx].value, vmap)?;
            // Re-parse regexps substituted from a DQL variable.
            if func.name == "regexp" && vmap.contains_key(&value) {
                regexp_variable_filter(func, idx)?;
            }
        }
    }
    for child in &mut filter.child {
        substitute_variables_filter(child, vmap)?;
    }
    Ok(())
}

/// Parses a `($var: type, ...)` DQL variable declaration.
pub(crate) fn parse_dql_variables(
    it: &mut ItemIterator,
    vmap: &mut VarMap,
) -> Result<(), DqlError> {
    let mut expect_arg = true;
    if let (item, true) = it.peek_one() {
        if item.typ == ITEM_RIGHT_ROUND {
            return Ok(());
        }
    }
    loop {
        if !it.advance() {
            break;
        }
        let var_name: String;
        // Get variable name.
        let item = it.item();
        match item.typ {
            ITEM_DOLLAR => {
                if !expect_arg {
                    return Err(item.errorf("Missing comma in var declaration").into());
                }
                it.advance();
                let item = it.item();
                if item.typ == ITEM_NAME {
                    var_name = format!("${}", item.val);
                } else {
                    return Err(item
                        .errorf(format!("Expecting a variable name. Got: {item}"))
                        .into());
                }
            }
            ITEM_RIGHT_ROUND => {
                if expect_arg {
                    return Err(item.errorf("Invalid comma in var block").into());
                }
                break;
            }
            ITEM_COMMA => {
                if expect_arg {
                    return Err(item.errorf("Invalid comma in var block").into());
                }
                expect_arg = true;
                continue;
            }
            _ => {
                return Err(item
                    .errorf(format!(
                        "Unexpected item in place of variable. Got: {item} {}",
                        item.typ == ITEM_DOLLAR
                    ))
                    .into());
            }
        }

        it.advance();
        let item = it.item();
        if item.typ != ITEM_COLON {
            return Err(item
                .errorf(format!("Expecting a colon. Got: {item}"))
                .into());
        }

        // Get variable type.
        it.advance();
        let item = it.item();
        if item.typ != ITEM_NAME {
            return Err(item
                .errorf(format!("Expecting a variable type. Got: {item}"))
                .into());
        }

        // Ensure that the type is not nil.
        let mut var_type = item.val.clone();
        if var_type.is_empty() {
            return Err(item.errorf("Type of a variable can't be empty").into());
        }
        it.advance();
        let mut item = it.item();
        if item.typ == ITEM_MATH_OP && item.val == "!" {
            var_type.push('!');
            it.advance();
            item = it.item();
        }
        // Insert the variable into the map. It might already be defined in
        // the variable list passed with the query.
        if let Some(existing) = vmap.get(&var_name) {
            let value = existing.value.clone();
            vmap.insert(
                var_name.clone(),
                VarInfo {
                    value,
                    typ: var_type.clone(),
                },
            );
        } else {
            vmap.insert(
                var_name.clone(),
                VarInfo {
                    value: String::new(),
                    typ: var_type.clone(),
                },
            );
        }

        // Check for '=' sign and optional default value.
        if parse_dql_var_default(it, vmap, &var_name, &var_type, &item)? {
            break;
        }
        expect_arg = false;
    }
    Ok(())
}

/// Handles the optional `= default` after a declaration. Returns whether the
/// declaration list ended (`)` seen).
fn parse_dql_var_default(
    it: &mut ItemIterator,
    vmap: &mut VarMap,
    var_name: &str,
    var_type: &str,
    item: &datastreamdb_lex::Item,
) -> Result<bool, DqlError> {
    match item.typ {
        ITEM_EQUAL => {
            it.advance();
            let def = it.item();
            if def.typ != ITEM_NAME {
                return Err(item
                    .errorf(format!(
                        "Expecting default value of a variable. Got: {item}"
                    ))
                    .into());
            }
            if var_type.ends_with('!') {
                return Err(item
                    .errorf(format!(
                        "Type ending with ! can't have default value: Got: {var_type}"
                    ))
                    .into());
            }
            // If value is empty replace, otherwise ignore the default
            // value as the initialised value overrides it.
            if vmap[var_name].value.is_empty() {
                let unquoted = unquote_if_quoted(&def.val)?;
                vmap.insert(
                    var_name.to_owned(),
                    VarInfo {
                        value: unquoted,
                        typ: var_type.to_owned(),
                    },
                );
            }
            Ok(false)
        }
        ITEM_RIGHT_ROUND => Ok(true),
        _ => {
            // We consumed an extra item to see if it was '=', move back.
            it.prev();
            Ok(false)
        }
    }
}

/// Unquotes double-quoted strings, passing anything else through.
pub(crate) fn unquote_if_quoted(text: &str) -> Result<String, DqlError> {
    if text.len() < 2 || !text.starts_with('"') || !text.ends_with('"') {
        return Ok(text.to_owned());
    }
    datastreamdb_tok::facets::unquote(text)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .map_err(|e| DqlError::message(format!("could not unquote {text:?}: {e}")))
}
