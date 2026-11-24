//! Completed-schema SDL printing. Port of `Stringify` and the
//! `generate*String` family from `dgraph-main/graphql/schema/gqlschema.go`.

#![warn(missing_docs)]

use std::fmt::Write as _;

use std::collections::HashSet;

use crate::directives::{
    APOLLO_EXTENDS_DIRECTIVE, APOLLO_EXTERNAL_DIRECTIVE, APOLLO_KEY_DIRECTIVE,
    APOLLO_PROVIDES_DIRECTIVE, APOLLO_REQUIRES_DIRECTIVE, APOLLO_SCHEMA_EXTRAS,
    APOLLO_SUPPORTED_DIRECTIVE_DEFS, AUTH_DIRECTIVE, CUSTOM_DIRECTIVE, DATASTREAMDB_DIRECTIVE,
    DEFAULT_DIRECTIVE, DEPRECATED_DIRECTIVE, DIRECTIVE_DEFS, EMBEDDING_DIRECTIVE, FILTER_INPUTS,
    GENERATE_DIRECTIVE, ID_DIRECTIVE, INVERSE_DIRECTIVE, LAMBDA_DIRECTIVE,
    LAMBDA_ON_MUTATE_DIRECTIVE, REMOTE_DIRECTIVE, REMOTE_RESPONSE_DIRECTIVE, SCHEMA_INPUTS,
    SEARCH_DIRECTIVE, SECRET_DIRECTIVE, SUBSCRIPTION_DIRECTIVE,
};
use crate::model::{Directive, FieldDef, InputValueDef, Schema, TypeDef, TypeKind};
use crate::rules_util::is_query_or_mutation;

/// Directives printed into the generated schema (mirrors Go's
/// `directiveValidators` membership test in `genDirectivesString`).
fn is_printed_directive(name: &str) -> bool {
    matches!(
        name,
        INVERSE_DIRECTIVE
            | SEARCH_DIRECTIVE
            | EMBEDDING_DIRECTIVE
            | DATASTREAMDB_DIRECTIVE
            | ID_DIRECTIVE
            | SUBSCRIPTION_DIRECTIVE
            | SECRET_DIRECTIVE
            | AUTH_DIRECTIVE
            | CUSTOM_DIRECTIVE
            | REMOTE_DIRECTIVE
            | DEPRECATED_DIRECTIVE
            | LAMBDA_DIRECTIVE
            | DEFAULT_DIRECTIVE
            | LAMBDA_ON_MUTATE_DIRECTIVE
            | GENERATE_DIRECTIVE
            | APOLLO_KEY_DIRECTIVE
            | APOLLO_EXTENDS_DIRECTIVE
            | APOLLO_EXTERNAL_DIRECTIVE
            | APOLLO_REQUIRES_DIRECTIVE
            | APOLLO_PROVIDES_DIRECTIVE
            | REMOTE_RESPONSE_DIRECTIVE
    )
}

/// `(name: Type, ...)` for field arguments.
fn gen_arguments_defn_string(args: &[InputValueDef]) -> String {
    if args.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = args
        .iter()
        .map(|arg| format!("{}: {}", arg.name, arg.ty))
        .collect();
    format!("({})", parts.join(", "))
}

/// `(name: value, ...)` for directive arguments.
fn gen_arguments_string(args: &[(String, crate::model::Value)]) -> String {
    if args.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = args
        .iter()
        .map(|(name, val)| format!("{name}: {val}"))
        .collect();
    format!("({})", parts.join(", "))
}

/// ` @dir1 @dir2` for known directives.
fn gen_directives_string(directives: &[Directive]) -> String {
    let parts: Vec<String> = directives
        .iter()
        .filter(|dir| is_printed_directive(&dir.name))
        .map(|dir| format!("@{}{}", dir.name, gen_arguments_string(&dir.args)))
        .collect();
    if parts.is_empty() {
        return String::new();
    }
    format!(" {}", parts.join(" "))
}

/// All fields of a definition, one per line.
fn gen_fields_string(fields: &[FieldDef]) -> String {
    let mut out = String::new();
    for field in fields {
        if field.name.starts_with("__") {
            continue;
        }
        if let Some(desc) = &field.description {
            let _ = writeln!(out, "\t\"\"\"{desc}\"\"\"");
        }
        out.push_str(&gen_field_string(field));
    }
    out
}

/// `\tname(args): Type @dirs\n` for one field.
fn gen_field_string(field: &FieldDef) -> String {
    format!(
        "\t{}{}: {}{}\n",
        field.name,
        gen_arguments_defn_string(&field.args),
        field.ty,
        gen_directives_string(&field.directives)
    )
}

/// Description block, if any.
fn generate_description(description: Option<&String>) -> String {
    description.map_or_else(String::new, |desc| format!("\"\"\"{desc}\"\"\"\n"))
}

/// SDL for an input object.
fn generate_input_string(typ: &TypeDef) -> String {
    format!(
        "{}input {}{} {{\n{}}}\n",
        generate_description(typ.description.as_ref()),
        typ.name,
        gen_directives_string(&typ.directives),
        gen_fields_string(&typ.fields)
    )
}

/// SDL for an enum.
fn generate_enum_string(typ: &TypeDef) -> String {
    let mut out = format!(
        "{}enum {} {{\n",
        generate_description(typ.description.as_ref()),
        typ.name
    );
    for val in &typ.enum_values {
        if val.name.starts_with("__") {
            continue;
        }
        if let Some(desc) = &val.description {
            let _ = writeln!(out, "\t\"\"\"{desc}\"\"\"");
        }
        let _ = writeln!(out, "\t{}", val.name);
    }
    out.push_str("}\n");
    out
}

/// SDL for an interface.
fn generate_interface_string(typ: &TypeDef) -> String {
    format!(
        "{}interface {}{} {{\n{}}}\n",
        generate_description(typ.description.as_ref()),
        typ.name,
        gen_directives_string(&typ.directives),
        gen_fields_string(&typ.fields)
    )
}

/// SDL for an object type.
fn generate_object_string(typ: &TypeDef) -> String {
    if typ.implements.is_empty() {
        return format!(
            "{}type {}{} {{\n{}}}\n",
            generate_description(typ.description.as_ref()),
            typ.name,
            gen_directives_string(&typ.directives),
            gen_fields_string(&typ.fields)
        );
    }
    format!(
        "{}type {} implements {}{} {{\n{}}}\n",
        generate_description(typ.description.as_ref()),
        typ.name,
        typ.implements.join(" & "),
        gen_directives_string(&typ.directives),
        gen_fields_string(&typ.fields)
    )
}

/// SDL for a union.
fn generate_union_string(typ: &TypeDef) -> String {
    format!(
        "{}union {}{} = {}\n",
        generate_description(typ.description.as_ref()),
        typ.name,
        gen_directives_string(&typ.directives),
        typ.possible_types.join(" | ")
    )
}

/// Whether the root has printable (non-`__`) fields.
fn has_stringifiable_fields(typ: &TypeDef) -> bool {
    typ.fields.iter().any(|field| !field.name.starts_with("__"))
}

/// Names defined by the injected extras (marked printed, not re-emitted).
fn extras_names() -> HashSet<String> {
    let mut names = HashSet::new();
    for sdl in [SCHEMA_INPUTS, DIRECTIVE_DEFS, FILTER_INPUTS] {
        if let Ok(doc) = async_graphql_parser::parse_schema(sdl) {
            for def in &doc.definitions {
                match def {
                    async_graphql_parser::types::TypeSystemDefinition::Type(typ) => {
                        names.insert(typ.node.name.node.to_string());
                    }
                    async_graphql_parser::types::TypeSystemDefinition::Directive(dir) => {
                        names.insert(format!("@{}", dir.node.name.node));
                    }
                    async_graphql_parser::types::TypeSystemDefinition::Schema(_) => {}
                }
            }
        }
    }
    names.insert("_Service".to_owned());
    names
}

/// Renders the completed schema as SDL with section banners.
///
/// `original` holds the user definition names in source order. Port of Go
/// `Stringify`.
#[allow(clippy::too_many_lines, reason = "exact port of Go Stringify")]
pub(crate) fn stringify(
    schema: &Schema,
    original: &[String],
    apollo_service_query: bool,
) -> String {
    let mut printed = extras_names();
    let mut original_out = String::new();
    for name in original {
        if is_query_or_mutation(name) {
            continue;
        }
        let Some(typ) = schema.type_def(name) else {
            continue;
        };
        match typ.kind {
            TypeKind::Interface => {
                let _ = writeln!(original_out, "{}", generate_interface_string(typ));
            }
            TypeKind::Object => {
                let _ = writeln!(original_out, "{}", generate_object_string(typ));
            }
            TypeKind::Union => {
                let _ = writeln!(original_out, "{}", generate_union_string(typ));
            }
            TypeKind::Enum => {
                let _ = writeln!(original_out, "{}", generate_enum_string(typ));
            }
            TypeKind::InputObject => {
                let _ = writeln!(original_out, "{}", generate_input_string(typ));
            }
            TypeKind::Scalar => {}
        }
        printed.insert(name.clone());
    }

    let mut generated: Vec<&String> = schema
        .types
        .keys()
        .filter(|name| {
            !is_query_or_mutation(name) && !printed.contains(*name) && !schema.types[*name].builtin
        })
        .collect();
    generated.sort();

    let mut objects = String::new();
    let mut inputs = String::new();
    let mut enums = String::new();
    for name in generated {
        let Some(typ) = schema.type_def(name) else {
            continue;
        };
        match typ.kind {
            TypeKind::Object => {
                let _ = writeln!(objects, "{}", generate_object_string(typ));
            }
            TypeKind::InputObject => {
                let _ = writeln!(inputs, "{}", generate_input_string(typ));
            }
            TypeKind::Enum => {
                let _ = writeln!(enums, "{}", generate_enum_string(typ));
            }
            _ => {}
        }
    }

    let extras = if apollo_service_query {
        format!("{SCHEMA_INPUTS}{APOLLO_SUPPORTED_DIRECTIVE_DEFS}{FILTER_INPUTS}")
    } else {
        format!("{SCHEMA_INPUTS}{DIRECTIVE_DEFS}{FILTER_INPUTS}")
    };

    let mut out =
        String::from("#######################\n# Input Schema\n#######################\n\n");
    out.push_str(&original_out);
    out.push_str("#######################\n# Extended Definitions\n#######################\n");
    out.push_str(&extras);
    out.push('\n');
    if let Some(entity) = schema.type_def("_Entity") {
        out.push_str(
            "#######################\n# Extended Apollo Definitions\n#######################\n",
        );
        out.push_str(&generate_union_string(entity));
        out.push_str(APOLLO_SCHEMA_EXTRAS);
        out.push('\n');
    }
    if !objects.is_empty() {
        out.push_str("#######################\n# Generated Types\n#######################\n\n");
        out.push_str(&objects);
    }
    if !enums.is_empty() {
        out.push_str("#######################\n# Generated Enums\n#######################\n\n");
        out.push_str(&enums);
    }
    if !inputs.is_empty() {
        out.push_str("#######################\n# Generated Inputs\n#######################\n\n");
        out.push_str(&inputs);
    }
    if let Some(query) = schema.type_def("Query") {
        if has_stringifiable_fields(query) {
            out.push_str("#######################\n# Generated Query\n#######################\n\n");
            let _ = writeln!(out, "{}", generate_object_string(query));
        }
    }
    if let Some(mutation) = schema.type_def("Mutation") {
        if !mutation.fields.is_empty() {
            out.push_str(
                "#######################\n# Generated Mutations\n#######################\n\n",
            );
            let _ = writeln!(out, "{}", generate_object_string(mutation));
        }
    }
    if let Some(subscription) = schema.type_def("Subscription") {
        if !subscription.fields.is_empty() {
            out.push_str(
                "#######################\n# Generated Subscriptions\n#######################\n",
            );
            out.push_str(&generate_object_string(subscription));
        }
    }
    out
}
