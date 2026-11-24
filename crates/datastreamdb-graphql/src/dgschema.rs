//! `DataStreamDB` schema generation. Split from `schemagen.rs`: `genDgSchema` and its
//! helpers from `dgraph-main/graphql/schema/schemagen.go`.

#![warn(missing_docs)]

use std::fmt::Write as _;

use std::collections::{HashMap, HashSet};

use crate::complete::is_key_field as complete_is_key_field;
use crate::directives::{
    APOLLO_EXTERNAL_DIRECTIVE, ID_DIRECTIVE, SEARCH_ARGS, SEARCH_DIRECTIVE, default_search,
    supported_search,
};
use crate::model::{FieldDef, Schema, TypeDef, TypeKind};
use crate::rules_util::{
    field_name, get_password_field, get_search_options, has_custom_or_lambda, is_geo_type,
    is_query_or_mutation, parent_interface, parent_interface_for_pwd_field, parse_search_type,
    type_name,
};

/// `DataStreamDB` indexes for a `@search(by:)` value.
fn all_search_indexes(val: &crate::model::Value) -> Vec<String> {
    let mut out = Vec::new();
    if let crate::model::Value::List(items) = val {
        for item in items {
            let raw = item.raw();
            if let Some((_, index)) = supported_search(parse_search_type(&raw)) {
                let (options, _) = get_search_options(&raw);
                out.push(format!("{index}{options}"));
            }
        }
    }
    out
}

/// One `DataStreamDB` predicate under construction.
struct DgPred {
    typ: String,
    indexes: HashSet<String>,
    upsert: String,
    reverse: String,
    lang: bool,
}

/// One `DataStreamDB` type under construction.
struct DgType {
    name: String,
    fields: Vec<(String, bool)>,
}

/// Generates the `DataStreamDB` schema for the completed GraphQL schema.
#[allow(clippy::too_many_lines, reason = "exact port of Go genDgSchema")]
pub(crate) fn gen_dg_schema(
    schema: &Schema,
    definitions: &[String],
    provides: &HashMap<String, HashSet<String>>,
) -> String {
    let mut dg_types: Vec<DgType> = Vec::new();
    let mut dg_preds: HashMap<String, DgPred> = HashMap::new();

    let update_pred = |preds: &mut HashMap<String, DgPred>,
                       fname: &str,
                       typ: &str,
                       upsert: &str,
                       indexes: &[String],
                       lang: bool| {
        let pred = preds.entry(fname.to_owned()).or_insert_with(|| DgPred {
            typ: typ.to_owned(),
            indexes: HashSet::new(),
            upsert: upsert.to_owned(),
            reverse: String::new(),
            lang: false,
        });
        pred.indexes.extend(indexes.iter().cloned());
        pred.lang = lang;
    };

    for key in definitions {
        if is_query_or_mutation(key) {
            continue;
        }
        let Some(def) = schema.type_def(key) else {
            continue;
        };
        if !matches!(def.kind, TypeKind::Object | TypeKind::Interface) {
            continue;
        }
        let mut dg_type = DgType {
            name: type_name(def),
            fields: Vec::new(),
        };
        let pwd_field = get_password_field(def);
        for field in &def.fields {
            if has_custom_or_lambda(field) {
                continue;
            }
            if crate::complete::external_and_non_key_field(
                field,
                def,
                provides
                    .get(&def.name)
                    .cloned()
                    .as_ref()
                    .unwrap_or(&HashSet::new()),
            ) {
                continue;
            }
            if field.ty.name() == "ID" && !(has_external(field) && is_key_field(field, def)) {
                continue;
            }
            let mut field_typ_name = type_name(def);
            if let Some(parent) = parent_interface(schema, def, &field.name) {
                field_typ_name = type_name(parent);
            }
            let mut fname = field_name(field, &field_typ_name);
            let (prefix, suffix) = if field.ty.is_list() {
                ("[", "]")
            } else {
                ("", "")
            };
            let target_kind = schema.type_def(field.ty.name()).map(|target| target.kind);
            match target_kind {
                Some(TypeKind::Object | TypeKind::Interface | TypeKind::Union) => {
                    let mut typ_str = format!("{prefix}uid{suffix}");
                    if is_geo_type(field.ty.name()) {
                        geo_datastreamdb_type(field.ty.name()).clone_into(&mut typ_str);
                        let mut indexes = Vec::new();
                        if field.directive(SEARCH_DIRECTIVE).is_some() {
                            if let Some(default) = default_search(field.ty.name()) {
                                if let Some((_, index)) = supported_search(default) {
                                    indexes.push(index.to_owned());
                                }
                            }
                        }
                        if parent_interface(schema, def, &field.name).is_none() {
                            update_pred(&mut dg_preds, &fname, &typ_str, "", &indexes, false);
                        }
                    } else if parent_interface(schema, def, &field.name).is_none() {
                        if let Some(forward) = fname.strip_prefix('~') {
                            if let Some(pred) = dg_preds.get_mut(forward) {
                                "@reverse ".clone_into(&mut pred.reverse);
                            } else {
                                dg_preds.insert(
                                    forward.to_owned(),
                                    DgPred {
                                        typ: String::new(),
                                        indexes: HashSet::new(),
                                        upsert: String::new(),
                                        reverse: "@reverse ".to_owned(),
                                        lang: false,
                                    },
                                );
                            }
                        } else if let Some(forward) = fname.strip_prefix("<~") {
                            let forward = forward.trim_end_matches('>');
                            if let Some(pred) = dg_preds.get_mut(forward) {
                                "@reverse ".clone_into(&mut pred.reverse);
                            }
                        } else {
                            dg_preds
                                .entry(fname.clone())
                                .and_modify(|pred| pred.typ.clone_from(&typ_str))
                                .or_insert_with(|| DgPred {
                                    typ: typ_str.clone(),
                                    indexes: HashSet::new(),
                                    upsert: String::new(),
                                    reverse: String::new(),
                                    lang: false,
                                });
                        }
                    }
                    let inherited = parent_interface(schema, def, &field.name).is_some();
                    dg_type.fields.push((fname, inherited));
                }
                Some(TypeKind::Scalar) => {
                    let mut fld_type =
                        crate::directives::inbuilt_type_to_datastreamdb(field.ty.name())
                            .unwrap_or("string");
                    if fld_type == "uid" {
                        fld_type = "string";
                    }
                    let mut typ_str = format!("{prefix}{fld_type}{suffix}");
                    let mut indexes = Vec::new();
                    let mut upsert = String::new();
                    if let Some(search) = field.directive(SEARCH_DIRECTIVE) {
                        if let Some(arg) = search.arg(SEARCH_ARGS) {
                            indexes.extend(all_search_indexes(arg));
                        } else if let Some(default) = default_search(field.ty.name()) {
                            if let Some((_, index)) = supported_search(default) {
                                indexes.push(index.to_owned());
                            }
                        }
                    }
                    if field.directive(ID_DIRECTIVE).is_some() || field.ty.name() == "ID" {
                        "@upsert ".clone_into(&mut upsert);
                        match field.ty.name() {
                            "Int" | "Int64" => indexes.push("int".to_owned()),
                            "String" | "ID" if !indexes.iter().any(|index| index == "exact") => {
                                indexes.push("hash".to_owned());
                            }
                            _ => {}
                        }
                    }
                    if field
                        .directive(crate::directives::EMBEDDING_DIRECTIVE)
                        .is_some()
                    {
                        "float32vector".clone_into(&mut typ_str);
                    }
                    if parent_interface(schema, def, &field.name).is_none() {
                        let mut lang = false;
                        if fname.contains('@') {
                            let prefix = fname.split('@').next().unwrap_or_default().to_owned();
                            fname = prefix;
                            lang = true;
                        }
                        update_pred(&mut dg_preds, &fname, &typ_str, &upsert, &indexes, lang);
                    }
                    let inherited = parent_interface(schema, def, &field.name).is_some();
                    dg_type.fields.push((fname, inherited));
                }
                Some(TypeKind::Enum) => {
                    let typ_str = format!("{prefix}string{suffix}");
                    let mut indexes = vec!["hash".to_owned()];
                    if let Some(search) = field.directive(SEARCH_DIRECTIVE) {
                        if let Some(arg) = search.arg(SEARCH_ARGS) {
                            indexes = all_search_indexes(arg);
                        }
                    }
                    if parent_interface(schema, def, &field.name).is_none() {
                        update_pred(&mut dg_preds, &fname, &typ_str, "", &indexes, false);
                    }
                    let inherited = parent_interface(schema, def, &field.name).is_some();
                    dg_type.fields.push((fname, inherited));
                }
                _ => {}
            }
        }
        if let Some(pwd) = pwd_field {
            let mut pwd_typ_name = type_name(def);
            if let Some(parent) = parent_interface_for_pwd_field(schema, def, &pwd.name) {
                pwd_typ_name = type_name(parent);
            }
            let fname = field_name(&pwd, &pwd_typ_name);
            if parent_interface_for_pwd_field(schema, def, &pwd.name).is_none() {
                dg_preds.insert(
                    fname.clone(),
                    DgPred {
                        typ: "password".to_owned(),
                        indexes: HashSet::new(),
                        upsert: String::new(),
                        reverse: String::new(),
                        lang: false,
                    },
                );
            }
            let inherited = parent_interface_for_pwd_field(schema, def, &pwd.name).is_some();
            dg_type.fields.push((fname, inherited));
        }
        dg_types.push(dg_type);
    }

    let mut written: HashSet<String> = HashSet::new();
    let mut type_strings = Vec::new();
    for dg_type in &dg_types {
        let mut added: HashSet<String> = HashSet::new();
        let mut type_def = format!("type {} {{\n", dg_type.name);
        let mut preds = String::new();
        for (name, inherited) in &dg_type.fields {
            let Some(pred) = dg_preds.get(name) else {
                continue;
            };
            if !added.insert(name.clone()) {
                continue;
            }
            let _ = writeln!(type_def, "  {name}");
            if !inherited && !written.contains(name) {
                let mut indexes: Vec<&String> = pred.indexes.iter().collect();
                indexes.sort();
                let index_str = if indexes.is_empty() {
                    String::new()
                } else {
                    format!(
                        " @index({})",
                        indexes
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                };
                let lang_str = if pred.lang { " @lang" } else { "" };
                let _ = writeln!(
                    preds,
                    "{}: {}{}{} {}{}.",
                    name, pred.typ, index_str, lang_str, pred.upsert, pred.reverse
                );
                written.insert(name.clone());
            }
        }
        type_def.push_str("}\n");
        type_strings.push(format!("{type_def}{preds}"));
    }
    type_strings.join("")
}

/// `DataStreamDB` scalar for a geo type name.
fn geo_datastreamdb_type(name: &str) -> &str {
    crate::directives::inbuilt_type_to_datastreamdb(name).unwrap_or("geo")
}

/// Whether the field carries `@external`.
fn has_external(field: &FieldDef) -> bool {
    field
        .directives
        .iter()
        .any(|dir| dir.name == APOLLO_EXTERNAL_DIRECTIVE)
}

/// Whether the field is a `@key` field (verbatim comparison like Go).
fn is_key_field(field: &FieldDef, typ: &TypeDef) -> bool {
    complete_is_key_field(field, typ)
}
