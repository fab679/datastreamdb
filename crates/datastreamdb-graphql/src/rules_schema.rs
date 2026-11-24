//! Schema-level `@datastreamdb` predicate validation. Port of
//! `datastreamdbDirectivePredicateValidation` from
//! `dgraph-main/graphql/schema/rules.go`.

#![warn(missing_docs)]

use std::collections::{HashMap, HashSet};

use crate::directives::ID_DIRECTIVE;
use crate::errors::{ErrorList, GraphQlError};
use crate::model::{Position, Schema, TypeDef, TypeKind};
use crate::rules_util::{
    field_name, get_datastreamdb_pred_arg, get_password_field, parent_interface,
    parent_interface_for_pwd_field, type_name,
};

/// One predicate use under validation.
struct Pred {
    name: String,
    parent_name: String,
    typ: String,
    position: Position,
    is_id: bool,
    is_secret: bool,
}

fn secret_error(secret: &Pred, new: &Pred) -> GraphQlError {
    GraphQlError::at(
        format!(
            "Type {}; Field {}: has the @datastreamdb predicate, but that conflicts with type {} \
             @secret directive on the same predicate. @secret predicates are stored encrypted \
             and so the same predicate can't be used as a {}.",
            new.parent_name, new.name, secret.parent_name, new.typ
        ),
        new.position.0,
        new.position.1,
    )
}

fn type_error(existing: &Pred, new: &Pred) -> GraphQlError {
    GraphQlError::at(
        format!(
            "Type {}; Field {}: has type {}, which is different to type {}; field {}, which has \
             the same @datastreamdb directive but type {}. These fields must have either the same \
             GraphQL types, or use different DataStreamDB predicates.",
            new.parent_name, new.name, new.typ, existing.parent_name, existing.name, existing.typ
        ),
        new.position.0,
        new.position.1,
    )
}

fn id_error(id: &Pred, new: &Pred) -> GraphQlError {
    GraphQlError::at(
        format!(
            "Type {}; Field {}: doesn't have @id directive, which conflicts with type {}; field \
             {}, which has the same @datastreamdb directive along with @id directive. Both these \
             fields must either use @id directive, or use different DataStreamDB predicates.",
            new.parent_name, new.name, id.parent_name, id.name
        ),
        new.position.0,
        new.position.1,
    )
}

fn existing_interface_field_error(interface: &Pred, new: &Pred) -> GraphQlError {
    GraphQlError::at(
        format!(
            "Type {}; Field {}: has the @datastreamdb directive, which conflicts with interface {}; \
             field {}, that this type implements. These fields must use different DataStreamDB \
             predicates.",
            new.parent_name, new.name, interface.parent_name, interface.name
        ),
        new.position.0,
        new.position.1,
    )
}

/// Validates `@datastreamdb` predicate reuse across the schema.
pub(crate) fn datastreamdb_directive_predicate_validation(
    schema: &Schema,
    definitions: &[String],
) -> ErrorList {
    let mut errs = ErrorList::default();
    let mut preds: HashMap<String, Pred> = HashMap::new();
    let mut interface_preds: HashMap<String, HashSet<String>> = HashMap::new();

    let mut interfaces: Vec<&String> = Vec::new();
    let mut concrete: Vec<&String> = Vec::new();
    for name in definitions {
        if schema.type_def(name).map(|def| def.kind) == Some(TypeKind::Interface) {
            interfaces.push(name);
        } else {
            concrete.push(name);
        }
    }
    let ordered: Vec<&String> = interfaces.into_iter().chain(concrete).collect();

    for key in ordered {
        let Some(def) = schema.type_def(key) else {
            continue;
        };
        if !matches!(def.kind, TypeKind::Object | TypeKind::Interface) {
            continue;
        }
        if def.kind == TypeKind::Interface {
            interface_preds.insert(def.name.clone(), HashSet::new());
        } else {
            check_conflicting_interfaces(def, &interface_preds, &mut errs);
        }
        let typ_name = type_name(def);
        for field in &def.fields {
            if field.ty.name() == "ID" {
                continue;
            }
            let mut field_typ_name = type_name(def);
            if let Some(parent) = parent_interface(schema, def, &field.name) {
                field_typ_name = type_name(parent);
            }
            let fname = field_name(field, &field_typ_name);
            if parent_interface(schema, def, &field.name).is_none() {
                if def.kind == TypeKind::Interface {
                    interface_preds
                        .entry(def.name.clone())
                        .or_default()
                        .insert(fname.clone());
                }
                let (prefix, suffix) = if field.ty.is_list() {
                    ("[", "]")
                } else {
                    ("", "")
                };
                let this = Pred {
                    name: field.name.clone(),
                    parent_name: def.name.clone(),
                    typ: format!("{prefix}{}{suffix}", field.ty.name()),
                    position: field.position,
                    is_id: field.directive(ID_DIRECTIVE).is_some(),
                    is_secret: false,
                };
                if fname.starts_with('~') || fname.starts_with("<~") {
                    continue;
                }
                if let Some(existing) = preds.get(&fname) {
                    if existing.is_secret {
                        errs.push(secret_error(existing, &this));
                    } else if this.typ != existing.typ {
                        errs.push(type_error(existing, &this));
                    }
                    if existing.is_id != this.is_id {
                        if existing.is_id {
                            errs.push(id_error(existing, &this));
                        } else {
                            errs.push(id_error(&this, existing));
                        }
                    }
                    if def.kind == TypeKind::Object
                        && def.implements.contains(&existing.parent_name)
                    {
                        errs.push(existing_interface_field_error(existing, &this));
                    }
                } else {
                    preds.insert(fname, this);
                }
            }
        }
        validate_password_pred(def, schema, &typ_name, &mut preds, &mut errs);
    }
    errs
}

/// Reports interfaces of `def` sharing one `@datastreamdb` predicate.
fn check_conflicting_interfaces(
    def: &TypeDef,
    interface_preds: &HashMap<String, HashSet<String>>,
    errs: &mut ErrorList,
) {
    let mut reported: HashMap<String, Vec<String>> = HashMap::new();
    for (i, first) in def.implements.iter().enumerate() {
        let Some(first_preds) = interface_preds.get(first) else {
            continue;
        };
        for second in def.implements.iter().skip(i + 1) {
            let Some(second_preds) = interface_preds.get(second) else {
                continue;
            };
            for fname in second_preds {
                if first_preds.contains(fname) {
                    let entry = reported.entry(fname.clone()).or_default();
                    if entry.is_empty() {
                        entry.push(first.clone());
                    }
                    entry.push(second.clone());
                }
            }
        }
    }
    let mut names: Vec<&String> = reported.keys().collect();
    names.sort();
    for fname in names {
        // Go prints string slices as `[a b]`; mirrored exactly.
        let interfaces = format!("[{}]", reported[fname].join(" "));
        errs.push(GraphQlError::at(
            format!(
                "Type {}; implements interfaces {interfaces}, all of which have fields with \
                 @datastreamdb predicate: {fname}. These fields must use different DataStreamDB predicates.",
                def.name
            ),
            def.position.0,
            def.position.1,
        ));
    }
}

/// Validates the `@secret` password predicate of one type.
fn validate_password_pred(
    def: &TypeDef,
    schema: &Schema,
    typ_name: &str,
    preds: &mut HashMap<String, Pred>,
    errs: &mut ErrorList,
) {
    let Some(pwd) = get_password_field(def) else {
        return;
    };
    let fname = field_name(&pwd, typ_name);
    if get_datastreamdb_pred_arg(&pwd).is_none()
        || parent_interface_for_pwd_field(schema, def, &pwd.name).is_some()
    {
        return;
    }
    let this = Pred {
        name: pwd.name.clone(),
        parent_name: def.name.clone(),
        typ: pwd.ty.name().to_owned(),
        position: pwd.position,
        is_id: false,
        is_secret: true,
    };
    if let Some(existing) = preds.get(&fname) {
        if this.typ != existing.typ || !existing.is_secret {
            errs.push(secret_error(&this, existing));
        }
        if def.kind == TypeKind::Object && def.implements.contains(&existing.parent_name) {
            errs.push(existing_interface_field_error(existing, &this));
        }
    } else {
        preds.insert(fname, this);
    }
}
