//! Internal GraphQL schema model. Mirrors the `ast.Schema` / `ast.Definition`
//! shapes from `dgraph-io/gqlparser` closely enough that the completion and
//! validation algorithms in `schemagen.rs` / `rules.rs` read like their Go
//! counterparts, but converted from `async-graphql-parser` documents.
//!
//! Delta from Go: the parser is `async-graphql-parser` (pest-based), not
//! gqlparser, so positions and syntax-error text differ.

#![warn(missing_docs)]

use std::collections::HashMap;
use std::fmt::{self, Display, Formatter, Write as _};

use async_graphql_parser::Pos;
use async_graphql_parser::types::{
    BaseType as ParserBaseType, ConstDirective as ParserDirective, DirectiveLocation,
    ServiceDocument, TypeDefinition, TypeKind as ParserTypeKind,
};
use async_graphql_value::ConstValue as ParserValue;

use crate::directives::APOLLO_EXTENDS_DIRECTIVE;
use crate::errors::{GraphQlError, Result};

/// A 1-based source position.
pub type Position = (usize, usize);

/// Converts a parser position.
fn convert_pos(pos: Pos) -> Position {
    (pos.line, pos.column)
}

/// GraphQL spelling of a directive location.
fn location_name(loc: DirectiveLocation) -> String {
    match loc {
        DirectiveLocation::Query => "QUERY",
        DirectiveLocation::Mutation => "MUTATION",
        DirectiveLocation::Subscription => "SUBSCRIPTION",
        DirectiveLocation::Field => "FIELD",
        DirectiveLocation::FragmentDefinition => "FRAGMENT_DEFINITION",
        DirectiveLocation::FragmentSpread => "FRAGMENT_SPREAD",
        DirectiveLocation::InlineFragment => "INLINE_FRAGMENT",
        DirectiveLocation::Schema => "SCHEMA",
        DirectiveLocation::Scalar => "SCALAR",
        DirectiveLocation::Object => "OBJECT",
        DirectiveLocation::FieldDefinition => "FIELD_DEFINITION",
        DirectiveLocation::ArgumentDefinition => "ARGUMENT_DEFINITION",
        DirectiveLocation::Interface => "INTERFACE",
        DirectiveLocation::Union => "UNION",
        DirectiveLocation::Enum => "ENUM",
        DirectiveLocation::EnumValue => "ENUM_VALUE",
        DirectiveLocation::InputObject => "INPUT_OBJECT",
        DirectiveLocation::InputFieldDefinition => "INPUT_FIELD_DEFINITION",
        DirectiveLocation::VariableDefinition => "VARIABLE_DEFINITION",
    }
    .to_owned()
}

/// A GraphQL value in schema position (directive arguments, defaults).
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// `null`.
    Null,
    /// Integer literal.
    Int(i64),
    /// Float literal.
    Float(f64),
    /// String literal.
    String(String),
    /// Boolean literal.
    Bool(bool),
    /// Enum literal.
    Enum(String),
    /// List literal.
    List(Vec<Value>),
    /// Object literal (insertion order preserved).
    Object(Vec<(String, Value)>),
}

impl Value {
    /// Raw scalar text, like Go `ast.Value.Raw` for scalar arguments.
    pub fn raw(&self) -> String {
        match self {
            Value::Null => "null".to_owned(),
            Value::Int(num) => num.to_string(),
            Value::Float(num) => {
                let text = format!("{num:?}");
                if text.contains('.') || text.contains('e') {
                    text
                } else {
                    format!("{text}.0")
                }
            }
            Value::String(text) | Value::Enum(text) => text.clone(),
            Value::Bool(val) => val.to_string(),
            Value::List(items) => items.iter().map(Value::raw).collect::<Vec<_>>().join(" "),
            Value::Object(fields) => fields
                .iter()
                .map(|(key, val)| format!("{key}: {}", val.raw()))
                .collect::<Vec<_>>()
                .join(" "),
        }
    }

    /// Object field lookup, like Go `Children.ForName`.
    pub(crate) fn field(&self, name: &str) -> Option<&Value> {
        match self {
            Value::Object(fields) => fields
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, val)| val),
            _ => None,
        }
    }

    /// Whether the value is an empty object (`{}`).
    pub(crate) fn is_empty_object(&self) -> bool {
        matches!(self, Value::Object(fields) if fields.is_empty())
    }
}

impl Display for Value {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => f.write_str("null"),
            Value::Int(num) => write!(f, "{num}"),
            Value::Float(num) => write!(f, "{num:?}"),
            Value::String(text) => write!(f, "\"{text}\""),
            Value::Bool(val) => write!(f, "{val}"),
            Value::Enum(name) => f.write_str(name),
            Value::List(items) => {
                f.write_str("[")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{item}")?;
                }
                f.write_str("]")
            }
            Value::Object(fields) => {
                f.write_str("{")?;
                for (i, (key, val)) in fields.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{key}: {val}")?;
                }
                f.write_str("}")
            }
        }
    }
}

/// Converts a parser constant value.
fn convert_value(val: &ParserValue) -> Value {
    match val {
        ParserValue::Null => Value::Null,
        ParserValue::Number(num) => num
            .as_i64()
            .map_or_else(|| Value::Float(num.as_f64().unwrap_or(0.0)), Value::Int),
        ParserValue::String(text) => Value::String(text.clone()),
        ParserValue::Boolean(val) => Value::Bool(*val),
        ParserValue::Binary(bytes) => Value::String(format!("{bytes:?}")),
        ParserValue::Enum(name) => Value::Enum(name.to_string()),
        ParserValue::List(items) => Value::List(items.iter().map(convert_value).collect()),
        ParserValue::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, val)| (key.to_string(), convert_value(val)))
                .collect(),
        ),
    }
}

/// A GraphQL base type: named (`String`) or list (`[String]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaseType {
    /// Named type.
    Named(String),
    /// List of an inner type.
    List(Box<GqlType>),
}

/// A GraphQL field/argument type with nullability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GqlType {
    /// Base type.
    pub base: BaseType,
    /// Whether the type is nullable.
    pub nullable: bool,
}

impl GqlType {
    /// Named nullable type (`String`).
    pub(crate) fn named(name: impl Into<String>) -> Self {
        Self {
            base: BaseType::Named(name.into()),
            nullable: true,
        }
    }

    /// Marks the type non-null (`String!`).
    pub(crate) fn required(mut self) -> Self {
        self.nullable = false;
        self
    }

    /// Wraps the type in a list (`[T]`).
    pub(crate) fn list(inner: GqlType) -> Self {
        Self {
            base: BaseType::List(Box::new(inner)),
            nullable: true,
        }
    }

    /// Innermost named type (`String` for `[String!]!`).
    pub(crate) fn name(&self) -> &str {
        match &self.base {
            BaseType::Named(name) => name,
            BaseType::List(inner) => inner.name(),
        }
    }

    /// Whether the outermost level is a list.
    pub(crate) fn is_list(&self) -> bool {
        matches!(self.base, BaseType::List(_))
    }
}

impl Display for GqlType {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match &self.base {
            BaseType::Named(name) => f.write_str(name)?,
            BaseType::List(inner) => write!(f, "[{inner}]")?,
        }
        if !self.nullable {
            f.write_char('!')?;
        }
        Ok(())
    }
}

/// Converts a parser type reference.
fn convert_type(ty: &async_graphql_parser::types::Type) -> GqlType {
    let base = match &ty.base {
        ParserBaseType::Named(name) => BaseType::Named(name.to_string()),
        ParserBaseType::List(inner) => BaseType::List(Box::new(convert_type(inner))),
    };
    GqlType {
        base,
        nullable: ty.nullable,
    }
}

/// A directive application with ordered arguments.
#[derive(Debug, Clone, PartialEq)]
pub struct Directive {
    /// Directive name without `@`.
    pub name: String,
    /// Arguments in source order.
    pub args: Vec<(String, Value)>,
    /// Source position.
    pub position: Position,
}

impl Directive {
    /// New directive without arguments.
    pub(crate) fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            args: Vec::new(),
            position: (0, 0),
        }
    }

    /// Argument lookup, like Go `Arguments.ForName`.
    #[must_use]
    pub fn arg(&self, name: &str) -> Option<&Value> {
        self.args
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, val)| val)
    }
}

impl Display for Directive {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "@{}", self.name)?;
        if !self.args.is_empty() {
            f.write_str("(")?;
            for (i, (key, val)) in self.args.iter().enumerate() {
                if i > 0 {
                    f.write_str(", ")?;
                }
                write!(f, "{key}: {val}")?;
            }
            f.write_str(")")?;
        }
        Ok(())
    }
}

/// Converts a parser directive application.
fn convert_directive(dir: &ParserDirective, pos: Pos) -> Directive {
    Directive {
        name: dir.name.node.to_string(),
        args: dir
            .arguments
            .iter()
            .map(|(name, val)| (name.node.to_string(), convert_value(&val.node)))
            .collect(),
        position: convert_pos(pos),
    }
}

/// An input value (field argument or input-object field).
#[derive(Debug, Clone, PartialEq)]
pub struct InputValueDef {
    /// Argument name.
    pub name: String,
    /// Description, if present.
    pub description: Option<String>,
    /// Value type.
    pub ty: GqlType,
    /// Default value, if present.
    pub default: Option<Value>,
    /// Directives on the value.
    pub directives: Vec<Directive>,
    /// Source position.
    pub position: Position,
}

/// A field of an object, interface, or input object.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldDef {
    /// Field name.
    pub name: String,
    /// Description, if present.
    pub description: Option<String>,
    /// Field arguments.
    pub args: Vec<InputValueDef>,
    /// Field type.
    pub ty: GqlType,
    /// Directives on the field.
    pub directives: Vec<Directive>,
    /// Source position.
    pub position: Position,
}

impl FieldDef {
    /// Directive lookup, like Go `Directives.ForName`.
    #[must_use]
    pub fn directive(&self, name: &str) -> Option<&Directive> {
        self.directives.iter().find(|dir| dir.name == name)
    }
}

/// An enum value definition.
#[derive(Debug, Clone, PartialEq)]
pub struct EnumValueDef {
    /// Value name.
    pub name: String,
    /// Description, if present.
    pub description: Option<String>,
    /// Directives on the value.
    pub directives: Vec<Directive>,
}

/// Definition kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeKind {
    /// Scalar type.
    Scalar,
    /// Object type.
    Object,
    /// Interface type.
    Interface,
    /// Union type.
    Union,
    /// Enum type.
    Enum,
    /// Input object type.
    InputObject,
}

impl TypeKind {
    /// Go `ast.DefinitionKind` spelling used in error messages.
    pub(crate) fn go_name(self) -> &'static str {
        match self {
            TypeKind::Scalar => "SCALAR",
            TypeKind::Object => "OBJECT",
            TypeKind::Interface => "INTERFACE",
            TypeKind::Union => "UNION",
            TypeKind::Enum => "ENUM",
            TypeKind::InputObject => "INPUT_OBJECT",
        }
    }
}

/// A named type definition.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeDef {
    /// Type name.
    pub name: String,
    /// Kind of the definition.
    pub kind: TypeKind,
    /// Description, if present.
    pub description: Option<String>,
    /// Directives on the definition.
    pub directives: Vec<Directive>,
    /// Fields (object, interface, input object).
    pub fields: Vec<FieldDef>,
    /// Interfaces implemented (object, interface).
    pub implements: Vec<String>,
    /// Enum values (enum).
    pub enum_values: Vec<EnumValueDef>,
    /// Union member names (union).
    pub possible_types: Vec<String>,
    /// Source position.
    pub position: Position,
    /// Prelude-provided (never printed, never validated as user input).
    pub builtin: bool,
}

impl TypeDef {
    /// Directive lookup, like Go `Directives.ForName`.
    #[must_use]
    pub fn directive(&self, name: &str) -> Option<&Directive> {
        self.directives.iter().find(|dir| dir.name == name)
    }

    /// Field lookup, like Go `Fields.ForName`.
    #[must_use]
    pub fn field(&self, name: &str) -> Option<&FieldDef> {
        self.fields.iter().find(|field| field.name == name)
    }

    /// Mutable field lookup.
    pub(crate) fn field_mut(&mut self, name: &str) -> Option<&mut FieldDef> {
        self.fields.iter_mut().find(|field| field.name == name)
    }
}

/// A directive definition (name + locations).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectiveDef {
    /// Directive name without `@`.
    pub name: String,
    /// Locations (`FIELD_DEFINITION`, ...).
    pub locations: Vec<String>,
}

/// A validated-naming schema document: types plus roots.
#[derive(Debug, Clone, Default)]
pub struct Schema {
    /// Types by name, in definition order via [`Schema::order`].
    pub types: HashMap<String, TypeDef>,
    /// Definition order.
    pub order: Vec<String>,
    /// Directive definitions by name.
    pub directives: HashMap<String, DirectiveDef>,
    /// Query root type name.
    pub query_root: Option<String>,
    /// Mutation root type name.
    pub mutation_root: Option<String>,
    /// Subscription root type name.
    pub subscription_root: Option<String>,
    /// Interface name to implementor names.
    pub implementors: HashMap<String, Vec<String>>,
}

impl Schema {
    /// Looks up a type by name.
    #[must_use]
    pub fn type_def(&self, name: &str) -> Option<&TypeDef> {
        self.types.get(name)
    }

    /// Mutable type lookup.
    pub(crate) fn type_def_mut(&mut self, name: &str) -> Option<&mut TypeDef> {
        self.types.get_mut(name)
    }

    /// Inserts a type, recording definition order once.
    pub(crate) fn insert_type(&mut self, def: TypeDef) {
        if !self.types.contains_key(&def.name) {
            self.order.push(def.name.clone());
        }
        self.types.insert(def.name.clone(), def);
    }

    /// Removes a type (completion cleanup).
    pub(crate) fn remove_type(&mut self, name: &str) {
        self.types.remove(name);
    }

    /// Rebuilds the implementor index after mutation.
    pub(crate) fn reindex(&mut self) {
        self.implementors.clear();
        for def in self.types.values() {
            if def.kind == TypeKind::Object {
                for iface in &def.implements {
                    self.implementors
                        .entry(iface.clone())
                        .or_default()
                        .push(def.name.clone());
                }
            }
        }
    }
}

/// Converts a parser field definition.
fn convert_field(field: &async_graphql_parser::types::FieldDefinition, pos: Pos) -> FieldDef {
    FieldDef {
        name: field.name.node.to_string(),
        description: field.description.as_ref().map(|desc| desc.node.clone()),
        args: field
            .arguments
            .iter()
            .map(|arg| {
                let def = &arg.node;
                InputValueDef {
                    name: def.name.node.to_string(),
                    description: def.description.as_ref().map(|desc| desc.node.clone()),
                    ty: convert_type(&def.ty.node),
                    default: def
                        .default_value
                        .as_ref()
                        .map(|val| convert_value(&val.node)),
                    directives: def
                        .directives
                        .iter()
                        .map(|dir| convert_directive(&dir.node, dir.pos))
                        .collect(),
                    position: convert_pos(arg.pos),
                }
            })
            .collect(),
        ty: convert_type(&field.ty.node),
        directives: field
            .directives
            .iter()
            .map(|dir| convert_directive(&dir.node, dir.pos))
            .collect(),
        position: convert_pos(pos),
    }
}

/// Converts a parser type definition (non-extension) into the model.
fn convert_type_def(def: &TypeDefinition, pos: Pos) -> TypeDef {
    let mut out = TypeDef {
        name: def.name.node.to_string(),
        kind: TypeKind::Scalar,
        description: def.description.as_ref().map(|desc| desc.node.clone()),
        directives: def
            .directives
            .iter()
            .map(|dir| convert_directive(&dir.node, dir.pos))
            .collect(),
        fields: Vec::new(),
        implements: Vec::new(),
        enum_values: Vec::new(),
        possible_types: Vec::new(),
        position: convert_pos(pos),
        builtin: false,
    };
    match &def.kind {
        ParserTypeKind::Scalar => out.kind = TypeKind::Scalar,
        ParserTypeKind::Object(obj) => {
            out.kind = TypeKind::Object;
            out.implements = obj
                .implements
                .iter()
                .map(|name| name.node.to_string())
                .collect();
            out.fields = obj
                .fields
                .iter()
                .map(|field| convert_field(&field.node, field.pos))
                .collect();
        }
        ParserTypeKind::Interface(iface) => {
            out.kind = TypeKind::Interface;
            out.implements = iface
                .implements
                .iter()
                .map(|name| name.node.to_string())
                .collect();
            out.fields = iface
                .fields
                .iter()
                .map(|field| convert_field(&field.node, field.pos))
                .collect();
        }
        ParserTypeKind::Union(union) => {
            out.kind = TypeKind::Union;
            out.possible_types = union
                .members
                .iter()
                .map(|name| name.node.to_string())
                .collect();
        }
        ParserTypeKind::Enum(en) => {
            out.kind = TypeKind::Enum;
            out.enum_values = en
                .values
                .iter()
                .map(|val| {
                    let node = &val.node;
                    EnumValueDef {
                        name: node.value.node.to_string(),
                        description: node.description.as_ref().map(|desc| desc.node.clone()),
                        directives: node
                            .directives
                            .iter()
                            .map(|dir| convert_directive(&dir.node, dir.pos))
                            .collect(),
                    }
                })
                .collect();
        }
        ParserTypeKind::InputObject(input) => {
            out.kind = TypeKind::InputObject;
            out.fields = input
                .fields
                .iter()
                .map(|field| {
                    let node = &field.node;
                    FieldDef {
                        name: node.name.node.to_string(),
                        description: node.description.as_ref().map(|desc| desc.node.clone()),
                        args: Vec::new(),
                        ty: convert_type(&node.ty.node),
                        directives: node
                            .directives
                            .iter()
                            .map(|dir| convert_directive(&dir.node, dir.pos))
                            .collect(),
                        position: convert_pos(field.pos),
                    }
                })
                .collect();
        }
    }
    out
}

/// Builds a [`Schema`] from a parsed service document.
///
/// Extensions merge into their base types (like Go's extension fold in
/// `NewHandler`); non-root extensions gain an `@extends` marker directive.
///
/// # Errors
///
/// Returns [`GraphQlError`] on duplicate definitions or dangling extensions.
pub(crate) fn schema_from_document(doc: &ServiceDocument) -> Result<Schema> {
    let mut schema = Schema::default();
    let mut extensions: Vec<TypeDef> = Vec::new();

    for def in &doc.definitions {
        match def {
            async_graphql_parser::types::TypeSystemDefinition::Schema(sch) => {
                let node = &sch.node;
                if !node.extend {
                    if let Some(query) = &node.query {
                        schema.query_root = Some(query.node.to_string());
                    }
                    if let Some(mutation) = &node.mutation {
                        schema.mutation_root = Some(mutation.node.to_string());
                    }
                    if let Some(subscription) = &node.subscription {
                        schema.subscription_root = Some(subscription.node.to_string());
                    }
                }
            }
            async_graphql_parser::types::TypeSystemDefinition::Type(typ) => {
                let node = &typ.node;
                let converted = convert_type_def(node, typ.pos);
                if node.extend {
                    extensions.push(converted);
                } else {
                    if schema.types.contains_key(&converted.name) {
                        return Err(GraphQlError::new(format!(
                            "type {} is defined twice",
                            converted.name
                        )));
                    }
                    schema.insert_type(converted);
                }
            }
            async_graphql_parser::types::TypeSystemDefinition::Directive(dir) => {
                let node = &dir.node;
                schema.directives.insert(
                    node.name.node.to_string(),
                    DirectiveDef {
                        name: node.name.node.to_string(),
                        locations: node
                            .locations
                            .iter()
                            .map(|loc| location_name(loc.node))
                            .collect(),
                    },
                );
            }
        }
    }

    for mut ext in extensions {
        if ext.name != "Query" && ext.name != "Mutation" {
            ext.directives
                .push(Directive::new(APOLLO_EXTENDS_DIRECTIVE));
        }
        match schema.types.get_mut(&ext.name) {
            Some(base) => {
                base.fields.extend(ext.fields);
                base.directives.extend(ext.directives);
                for iface in ext.implements {
                    if !base.implements.contains(&iface) {
                        base.implements.push(iface);
                    }
                }
            }
            None => {
                if ext.name == "Query" || ext.name == "Mutation" {
                    schema.insert_type(ext);
                } else {
                    return Err(GraphQlError::new(format!(
                        "extension of unknown type {}",
                        ext.name
                    )));
                }
            }
        }
    }

    schema.reindex();
    Ok(schema)
}
