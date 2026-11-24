//! Dynamic async-graphql schema registration. Registers every completed
//! type with resolvers backed by normalized DQL JSON, so validation,
//! introspection, fragments, and null bubbling come from async-graphql.

#![warn(missing_docs)]

use std::borrow::Cow;
use std::collections::HashSet;

use async_graphql::Value as GqlValue;
use async_graphql::dynamic::{
    Enum, Field, FieldFuture, FieldValue, InputObject, InputValue, Interface, InterfaceField,
    Object, Scalar, TypeRef, Union,
};

use crate::model::{BaseType, FieldDef, GqlType, InputValueDef, Schema, TypeDef, TypeKind};
use crate::resolve::{JsonNode, Shared, resolve_root_query};

/// Converts a model type to a dynamic type reference.
fn type_ref(ty: &GqlType) -> TypeRef {
    let base = match &ty.base {
        BaseType::Named(name) => TypeRef::Named(Cow::Owned(name.clone())),
        BaseType::List(inner) => TypeRef::List(Box::new(type_ref(inner))),
    };
    if ty.nullable {
        base
    } else {
        TypeRef::NonNull(Box::new(base))
    }
}

/// Converts an input value definition to a dynamic argument.
fn input_value(def: &InputValueDef) -> InputValue {
    let mut out = InputValue::new(def.name.clone(), type_ref(&def.ty));
    if let Some(default) = &def.default {
        out = out.default_value(model_value_to_gql(default));
    }
    out
}

/// Converts a model default value to a GraphQL value.
fn model_value_to_gql(val: &crate::model::Value) -> GqlValue {
    match val {
        crate::model::Value::Null => GqlValue::Null,
        crate::model::Value::Int(num) => GqlValue::Number((*num).into()),
        crate::model::Value::Float(num) => {
            serde_json::Number::from_f64(*num).map_or(GqlValue::Null, GqlValue::Number)
        }
        crate::model::Value::String(text) => GqlValue::String(text.clone()),
        crate::model::Value::Bool(val) => GqlValue::Boolean(*val),
        crate::model::Value::Enum(name) => GqlValue::Enum(async_graphql::Name::new(name)),
        crate::model::Value::List(items) => {
            GqlValue::List(items.iter().map(model_value_to_gql).collect())
        }
        crate::model::Value::Object(fields) => GqlValue::Object(
            fields
                .iter()
                .map(|(key, val)| (async_graphql::Name::new(key), model_value_to_gql(val)))
                .collect(),
        ),
    }
}

/// Nested field resolver: plucks the response key from parent JSON.
fn nested_resolver(object: String, field: FieldDef, type_name: String, is_abstract: bool) -> Field {
    let name = field.name.clone();
    let ty = field.ty.clone();
    let args = field.args.clone();
    let mut out = Field::new(name, type_ref(&ty), move |ctx| {
        let object = object.clone();
        let field = field.clone();
        let type_name = type_name.clone();
        FieldFuture::new(async move {
            let response_key = ctx.field().alias().unwrap_or_else(|| ctx.field().name());
            let Some(parent) = ctx.parent_value.downcast_ref::<JsonNode>() else {
                return Ok(None);
            };
            let Some(obj) = parent.value.as_object() else {
                return Ok(None);
            };
            let Some(val) = obj.get(response_key).cloned() else {
                return Ok(None);
            };
            Ok(Some(json_to_field_value(
                &object,
                &field,
                &type_name,
                is_abstract,
                val,
            )))
        })
    });
    for arg in &args {
        out = out.argument(input_value(arg));
    }
    out
}

/// Converts normalized JSON into a field value (scalars, objects, lists).
fn json_to_field_value(
    object: &str,
    field: &FieldDef,
    type_name: &str,
    is_abstract: bool,
    val: serde_json::Value,
) -> FieldValue<'static> {
    let _ = (object, field);
    match val {
        serde_json::Value::Null => FieldValue::value(GqlValue::Null),
        serde_json::Value::Bool(val) => FieldValue::value(GqlValue::Boolean(val)),
        serde_json::Value::Number(num) => FieldValue::value(GqlValue::Number(num)),
        serde_json::Value::String(text) => FieldValue::value(GqlValue::String(text)),
        serde_json::Value::Array(items) => FieldValue::list(
            items
                .into_iter()
                .map(|item| json_to_field_value(object, field, type_name, is_abstract, item)),
        ),
        serde_json::Value::Object(_) => {
            let concrete = concrete_type_name(&val, type_name, is_abstract);
            let node = JsonNode {
                value: val,
                type_name: concrete,
            };
            // NOTE: `with_type` breaks `downcast_ref`; only abstract fields
            // need it (the executor unwraps those itself).
            if is_abstract {
                let concrete = node.type_name.clone();
                FieldValue::owned_any(node).with_type(concrete)
            } else {
                FieldValue::owned_any(node)
            }
        }
    }
}

/// Resolves the concrete type from `datastreamdb.type`, falling back to static.
fn concrete_type_name(val: &serde_json::Value, static_type: &str, is_abstract: bool) -> String {
    if !is_abstract {
        return static_type.to_owned();
    }
    val.as_object()
        .and_then(|obj| obj.get("datastreamdb.type"))
        .and_then(|val| val.as_array())
        .and_then(|types| types.first())
        .and_then(|val| val.as_str())
        .map_or_else(|| static_type.to_owned(), str::to_owned)
}

/// Whether the named type is abstract (interface or union).
fn is_abstract(schema: &Schema, name: &str) -> bool {
    schema
        .type_def(name)
        .is_some_and(|def| matches!(def.kind, TypeKind::Interface | TypeKind::Union))
}

/// Registers one object type with nested resolvers.
fn register_object(schema: &Schema, def: &TypeDef) -> Object {
    let mut out = Object::new(def.name.clone());
    for field in &def.fields {
        let nested = nested_resolver(
            def.name.clone(),
            field.clone(),
            field.ty.name().to_owned(),
            is_abstract(schema, field.ty.name()),
        );
        out = out.field(nested);
    }
    for iface in &def.implements {
        out = out.implement(iface.clone());
    }
    out
}

/// Registers the Query root with executing resolvers.
fn register_query_root(def: &TypeDef) -> Object {
    let mut out = Object::new(def.name.clone());
    for field in &def.fields {
        let mut registered =
            Field::new(field.name.clone(), type_ref(&field.ty), resolve_root_query);
        for arg in &field.args {
            registered = registered.argument(input_value(arg));
        }
        out = out.field(registered);
    }
    out
}

/// Registers the Mutation root with executing resolvers.
fn register_mutation_root(def: &TypeDef) -> Object {
    let mut out = Object::new(def.name.clone());
    for field in &def.fields {
        let mut registered = Field::new(
            field.name.clone(),
            type_ref(&field.ty),
            crate::resolve_mutation::resolve_root_mutation,
        );
        for arg in &field.args {
            registered = registered.argument(input_value(arg));
        }
        out = out.field(registered);
    }
    out
}

/// Builds the executable dynamic schema for a bound schema.
///
/// # Errors
///
/// Returns [`SchemaError`](async_graphql::dynamic::SchemaError) on invalid
/// type registration.
pub fn build_dynamic_schema(
    shared: &std::sync::Arc<Shared>,
) -> Result<async_graphql::dynamic::Schema, async_graphql::dynamic::SchemaError> {
    let schema = &shared.bound.schema;
    let mutation = schema
        .type_def("Mutation")
        .filter(|def| !def.fields.is_empty())
        .map(|_| "Mutation");
    let mut builder = async_graphql::dynamic::Schema::build("Query", mutation, None);
    let mut seen: HashSet<String> = HashSet::new();
    for name in &schema.order {
        let Some(def) = schema.type_def(name) else {
            continue;
        };
        if def.builtin || !seen.insert(name.clone()) {
            continue;
        }
        match def.kind {
            TypeKind::Object if name == "Query" => {
                builder = builder.register(register_query_root(def));
            }
            TypeKind::Object if name == "Mutation" => {
                builder = builder.register(register_mutation_root(def));
            }
            // Subscriptions wire up in the subscription stage; skip the
            // placeholder so the schema stays valid.
            TypeKind::Object if name == "Subscription" => {}
            TypeKind::Object if def.fields.is_empty() => {}
            TypeKind::Object => {
                builder = builder.register(register_object(schema, def));
            }
            TypeKind::Interface => {
                let mut iface = Interface::new(def.name.clone());
                for field in &def.fields {
                    let mut nested = InterfaceField::new(field.name.clone(), type_ref(&field.ty));
                    for arg in &field.args {
                        nested = nested.argument(input_value(arg));
                    }
                    iface = iface.field(nested);
                }
                builder = builder.register(iface);
            }
            TypeKind::Union => {
                let mut union = Union::new(def.name.clone());
                for member in &def.possible_types {
                    union = union.possible_type(member.clone());
                }
                builder = builder.register(union);
            }
            TypeKind::Enum => {
                let mut en = Enum::new(def.name.clone());
                for value in &def.enum_values {
                    en = en.item(value.name.clone());
                }
                builder = builder.register(en);
            }
            TypeKind::InputObject => {
                let mut input = InputObject::new(def.name.clone());
                for field in &def.fields {
                    input = input.field(InputValue::new(field.name.clone(), type_ref(&field.ty)));
                }
                builder = builder.register(input);
            }
            TypeKind::Scalar => {
                builder = builder.register(Scalar::new(def.name.clone()));
            }
        }
    }
    // Ensure referenced-but-undefined scalars exist (Int64, DateTime).
    for scalar in ["Int64", "DateTime"] {
        if schema.type_def(scalar).is_none() {
            builder = builder.register(Scalar::new(scalar));
        }
    }
    builder.finish()
}
