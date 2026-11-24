//! Bound operations: executable documents resolved against the completed
//! schema. Port of the `operation` / `field` / `query` / `mutation` model in
//! `dgraph-main/graphql/schema/wrappers.go` plus fragment expansion from
//! `dgraph-main/graphql/schema/request.go`.
//!
//! Delta from Go: deep argument coercion/validation is delegated to
//! async-graphql at serve time; this model checks structure (known
//! fields/arguments/fragments, fragment applicability, variable presence).

#![warn(missing_docs)]

use std::collections::{HashMap, HashSet};

use async_graphql_parser::parse_query;
use async_graphql_parser::types::{
    Directive as ParserDirective, Field as ParserField, FragmentDefinition, OperationDefinition,
    OperationType, Selection,
};
use async_graphql_value::Value as ParserValue;

use crate::auth_rules::{TypeAuth, auth_rules};
use crate::directives::{
    CACHE_CONTROL_DIRECTIVE, CASCADE_ARG, CUSTOM_DIRECTIVE, DQL_ARG, ID_TYPE, MULTI_POLYGON, POINT,
    POLYGON, TYPENAME,
};
use crate::errors::{ErrorList, GraphQlError};
use crate::meta::MetaInfo;
use crate::model::{GqlType, Schema, TypeDef, TypeKind};
use crate::rules_util::{field_name, get_password_field, parent_interface, type_name};
use crate::schemagen::Handler;

/// Query classification. Port of Go `QueryType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QueryType {
    /// `getT` lookup.
    Get,
    /// `queryT` filter query.
    Filter,
    /// `checkTPassword` query.
    Password,
    /// `aggregateT` query.
    Aggregate,
    /// `querySimilarTById` query.
    SimilarById,
    /// `querySimilarTByEmbedding` query.
    SimilarByEmbedding,
    /// Apollo `_entities` query.
    Entities,
    /// `@custom(dql:)` query.
    DqlQuery,
    /// `@custom(http:)` query.
    HttpQuery,
    /// Introspection (`__schema`, `__type`, `__typename`).
    SchemaQuery,
    /// Anything else.
    #[default]
    NotSupported,
}

/// Mutation classification. Port of Go `MutationType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MutationType {
    /// `addT` mutation.
    Add,
    /// `updateT` mutation.
    Update,
    /// `deleteT` mutation.
    Delete,
    /// `@custom` mutation.
    HttpMutation,
    /// Anything else.
    #[default]
    NotSupported,
}

/// Operation kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    /// Query operation.
    Query,
    /// Mutation operation.
    Mutation,
    /// Subscription operation.
    Subscription,
}

/// A schema bound for resolving operations: predicates, auth, metadata.
#[derive(Debug, Clone)]
pub struct BoundSchema {
    /// Completed schema model.
    pub(crate) schema: Schema,
    /// `type -> field -> DataStreamDB predicate` map. Port of Go `dgraphMapping`.
    pub(crate) predicates: HashMap<String, HashMap<String, String>>,
    /// Auth rules by `DataStreamDB` type name.
    pub(crate) auth: HashMap<String, TypeAuth>,
    /// Schema metadata.
    pub(crate) meta: MetaInfo,
}

impl BoundSchema {
    /// Binds a completed handler for resolving operations.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorList`] when `@auth` rules fail to parse.
    pub fn bind(handler: &Handler) -> Result<Self, ErrorList> {
        let schema = handler.schema().clone();
        let predicates = build_predicate_map(&schema);
        let auth = auth_rules(&schema)?;
        Ok(Self {
            schema,
            predicates,
            auth,
            meta: handler.meta_info().clone(),
        })
    }

    /// `DataStreamDB` predicate for a type field, if mapped.
    pub fn predicate(&self, type_name: &str, field: &str) -> Option<&str> {
        self.predicates
            .get(type_name)
            .and_then(|fields| fields.get(field))
            .map(String::as_str)
    }

    /// `DataStreamDB` type name (`@datastreamdb(type:)` wins).
    pub fn datastreamdb_name(&self, type_name: &str) -> String {
        self.schema
            .type_def(type_name)
            .map_or_else(|| type_name.to_owned(), crate::rules_util::type_name)
    }

    /// First ID field of an object/interface type (excluding `@extends`).
    #[must_use]
    pub fn id_field(&self, type_name: &str) -> Option<crate::model::FieldDef> {
        let def = self.schema.type_def(type_name)?;
        if !matches!(def.kind, TypeKind::Object | TypeKind::Interface)
            || def
                .directive(crate::directives::APOLLO_EXTENDS_DIRECTIVE)
                .is_some()
        {
            return None;
        }
        def.fields
            .iter()
            .find(|field| field.ty.name() == "ID")
            .cloned()
    }

    /// Password field of an object/interface type, if any.
    #[must_use]
    pub fn password_field(&self, type_name: &str) -> Option<crate::model::FieldDef> {
        let def = self.schema.type_def(type_name)?;
        if !matches!(def.kind, TypeKind::Object | TypeKind::Interface) {
            return None;
        }
        crate::rules_util::get_password_field(def)
    }

    /// `@id` fields of a type.
    pub fn xid_fields(&self, type_name: &str) -> Vec<crate::model::FieldDef> {
        self.schema
            .type_def(type_name)
            .map_or_else(Vec::new, |def| {
                def.fields
                    .iter()
                    .filter(|field| field.directive(crate::directives::ID_DIRECTIVE).is_some())
                    .cloned()
                    .collect()
            })
    }

    /// Types implementing an interface.
    #[must_use]
    pub fn implementing_types(&self, iface: &str) -> Vec<String> {
        self.schema
            .implementors
            .get(iface)
            .cloned()
            .unwrap_or_default()
    }

    /// Whether the type is an interface.
    #[must_use]
    pub fn is_interface(&self, type_name: &str) -> bool {
        self.schema
            .type_def(type_name)
            .is_some_and(|def| def.kind == TypeKind::Interface)
    }

    /// Whether the type is a union.
    #[must_use]
    pub fn is_union(&self, type_name: &str) -> bool {
        self.schema
            .type_def(type_name)
            .is_some_and(|def| def.kind == TypeKind::Union)
    }

    /// Whether the type is a geo type.
    #[must_use]
    pub fn is_geo(&self, type_name: &str) -> bool {
        matches!(type_name, POINT | POLYGON | MULTI_POLYGON)
    }

    /// Union member names, optionally restricted to a `memberTypes` list.
    pub fn union_members(&self, type_name: &str, only: Option<&[String]>) -> Vec<String> {
        let members = self
            .schema
            .type_def(type_name)
            .map_or_else(Vec::new, |def| def.possible_types.clone());
        match only {
            Some(want) => members
                .into_iter()
                .filter(|member| want.contains(member))
                .collect(),
            None => members,
        }
    }

    /// Type where a field originated (parent interface wins).
    #[must_use]
    pub fn field_originated_from(&self, type_name: &str, field_name: &str) -> String {
        if let Some(def) = self.schema.type_def(type_name) {
            for iface in &def.implements {
                if let Some(interface_def) = self.schema.type_def(iface) {
                    if interface_def.field(field_name).is_some() {
                        return iface.clone();
                    }
                }
            }
            if def.field(field_name).is_some() {
                return def.name.clone();
            }
        }
        type_name.to_owned()
    }

    /// Classifies a root query field.
    pub fn query_type(&self, name: &str) -> QueryType {
        let custom = self
            .schema
            .type_def("Query")
            .and_then(|query| query.field(name))
            .and_then(|field| field.directive(CUSTOM_DIRECTIVE));
        if let Some(custom) = custom {
            if custom.arg(DQL_ARG).is_some() {
                return QueryType::DqlQuery;
            }
            return QueryType::HttpQuery;
        }
        if name == "_entities" {
            return QueryType::Entities;
        }
        if name.starts_with("get") {
            return QueryType::Get;
        }
        if name == "__schema" || name == "__type" || name == "__typename" {
            return QueryType::SchemaQuery;
        }
        if name.starts_with(crate::directives::SIMILAR_QUERY_PREFIX)
            && name.ends_with(crate::directives::SIMILAR_BY_ID_QUERY_SUFFIX)
        {
            return QueryType::SimilarById;
        }
        if name.starts_with(crate::directives::SIMILAR_QUERY_PREFIX)
            && name.ends_with(crate::directives::SIMILAR_BY_EMBEDDING_QUERY_SUFFIX)
        {
            return QueryType::SimilarByEmbedding;
        }
        if let Some(rest) = name.strip_prefix("query") {
            if rest.starts_with(char::is_uppercase) {
                return QueryType::Filter;
            }
        }
        if name.starts_with("check") {
            return QueryType::Password;
        }
        if name.starts_with("aggregate") {
            return QueryType::Aggregate;
        }
        QueryType::NotSupported
    }

    /// Classifies a root mutation field.
    #[must_use]
    pub fn mutation_type(&self, name: &str) -> MutationType {
        let custom = self
            .schema
            .type_def("Mutation")
            .and_then(|mutation| mutation.field(name))
            .and_then(|field| field.directive(CUSTOM_DIRECTIVE));
        if custom.is_some() {
            return MutationType::HttpMutation;
        }
        if name.starts_with("add") {
            return MutationType::Add;
        }
        if name.starts_with("update") {
            return MutationType::Update;
        }
        if name.starts_with("delete") {
            return MutationType::Delete;
        }
        MutationType::NotSupported
    }

    /// Parses and binds one request operation.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorList`] on parse, binding, or variable failures.
    pub fn parse_operation(
        &self,
        query: &str,
        operation_name: Option<&str>,
        variables: &HashMap<String, serde_json::Value>,
    ) -> Result<BoundOperation, ErrorList> {
        let doc = parse_query(query).map_err(|err| {
            let mut errs = ErrorList::default();
            errs.push(GraphQlError::new(err.to_string()));
            errs
        })?;
        let ops: Vec<(Option<String>, &OperationDefinition)> = doc
            .operations
            .iter()
            .map(|(name, op)| (name.map(ToString::to_string), &op.node))
            .collect();
        if ops.len() > 1 && operation_name.is_none() {
            let mut errs = ErrorList::default();
            errs.push(GraphQlError::new(
                "Operation name must by supplied when query has more than 1 operation.",
            ));
            return Err(errs);
        }
        let selected = if ops.len() == 1 && operation_name.is_none() {
            Some(&ops[0])
        } else {
            ops.iter()
                .find(|(name, _)| name.as_deref() == operation_name)
        };
        let Some((_, op)) = selected else {
            let mut errs = ErrorList::default();
            errs.push(GraphQlError::new(format!(
                "Supplied operation name {} isn't present in the request.",
                operation_name.unwrap_or_default()
            )));
            return Err(errs);
        };
        let kind = match op.ty {
            OperationType::Query => OpKind::Query,
            OperationType::Mutation => OpKind::Mutation,
            OperationType::Subscription => OpKind::Subscription,
        };
        let root = match kind {
            OpKind::Query => "Query",
            OpKind::Mutation => "Mutation",
            OpKind::Subscription => "Subscription",
        };
        let mut binder = Binder {
            schema: self,
            fragments: &doc.fragments,
            variables,
            defaults: HashMap::new(),
            errs: ErrorList::default(),
        };
        for def in &op.variable_definitions {
            if let Some(default) = &def.node.default_value {
                binder.defaults.insert(
                    def.node.name.node.to_string(),
                    parser_value_to_json(&default.node),
                );
            }
        }
        let mut roots = Vec::new();
        for item in &op.selection_set.node.items {
            match &item.node {
                Selection::Field(field) => {
                    // Introspection roots are served natively by async-graphql.
                    if field.node.name.node.as_str().starts_with("__") {
                        continue;
                    }
                    let position = (field.pos.line, field.pos.column);
                    if let Some(bound) = binder.bind_field(root, &field.node, position, true) {
                        roots.push(bound);
                    }
                }
                _ => binder.errs.push(GraphQlError::new(
                    "fragments are not supported at the operation root",
                )),
            }
        }
        let cache_control = op
            .directives
            .iter()
            .find(|dir| dir.node.name.node.as_str() == CACHE_CONTROL_DIRECTIVE)
            .and_then(|dir| dir.node.get_argument("maxAge"))
            .and_then(|arg| binder.substitute(&arg.node).ok())
            .map(|val| format!("public,max-age={}", val.to_string().trim_matches('"')));
        if binder.errs.is_empty() {
            Ok(BoundOperation {
                kind,
                roots,
                cache_control,
            })
        } else {
            Err(binder.errs)
        }
    }
}

/// Builds the `type -> field -> predicate` map. Port of Go `dgraphMapping`.
fn build_predicate_map(schema: &Schema) -> HashMap<String, HashMap<String, String>> {
    let mut map: HashMap<String, HashMap<String, String>> = HashMap::new();
    for name in &schema.order {
        let defn = &schema.types[name];
        if defn.builtin
            || matches!(defn.kind, TypeKind::Object | TypeKind::Interface)
                && (name == "Query" || name == "Mutation" || name == "Subscription")
            || !matches!(defn.kind, TypeKind::Object | TypeKind::Interface)
            || matches!(
                name.as_str(),
                "Point" | "PointList" | "Polygon" | "MultiPolygon"
            )
        {
            continue;
        }
        if name.starts_with("Add") && name.ends_with("Payload") {
            continue;
        }
        let mut source = defn;
        let resolved: Option<TypeDef>;
        if (name.starts_with("Update") || name.starts_with("Delete")) && name.ends_with("Payload") {
            let inner = if name.starts_with("Update") {
                name["Update".len()..name.len() - "Payload".len()].to_owned()
            } else {
                name["Delete".len()..name.len() - "Payload".len()].to_owned()
            };
            resolved = schema.type_def(&inner).cloned();
            if let Some(target) = resolved.as_ref() {
                source = target;
            }
        }
        let mut entry: HashMap<String, String> = HashMap::new();
        let mut fields = source.fields.clone();
        if let Some(pwd) = get_password_field(source) {
            fields.push(pwd);
        }
        for field in &fields {
            if field.ty.name() == ID_TYPE
                && !field
                    .directives
                    .iter()
                    .any(|dir| dir.name == crate::directives::APOLLO_EXTERNAL_DIRECTIVE)
            {
                continue;
            }
            let mut typ_name = type_name(source);
            if let Some(parent) = parent_interface(schema, source, &field.name) {
                typ_name = type_name(parent);
            }
            entry.insert(field.name.clone(), field_name(field, &typ_name));
        }
        map.insert(defn.name.clone(), entry);
    }
    map
}

/// A bound operation: kind plus root fields.
#[derive(Debug, Clone)]
pub struct BoundOperation {
    /// Operation kind.
    pub kind: OpKind,
    /// Root fields.
    pub roots: Vec<BoundField>,
    /// `@cacheControl` header value, if present.
    pub cache_control: Option<String>,
}

impl BoundOperation {
    /// Whether this is a query operation.
    #[must_use]
    pub fn is_query(&self) -> bool {
        self.kind == OpKind::Query
    }

    /// Whether this is a mutation operation.
    #[must_use]
    pub fn is_mutation(&self) -> bool {
        self.kind == OpKind::Mutation
    }
}

/// A field bound against the schema with variables substituted.
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools, reason = "mirrors Go field flags")]
pub struct BoundField {
    /// Field name.
    pub name: String,
    /// Response key (alias or name).
    pub alias: String,
    /// Parent type name (predicate lookup scope).
    pub object: String,
    /// Declared field type.
    pub field_type: GqlType,
    /// Arguments with variables substituted.
    pub args: HashMap<String, serde_json::Value>,
    /// Arguments whose declared type is a list.
    pub arg_is_list: HashSet<String>,
    /// Bound children (fragments expanded).
    pub children: Vec<BoundField>,
    /// `@skip(if:)` verdict.
    pub skip: bool,
    /// `@include(if:)` verdict.
    pub include: bool,
    /// `@cascade` predicates (`__all__` for bare), if present.
    pub cascade: Option<Vec<String>>,
    /// Source position.
    pub position: (usize, usize),
    /// Query classification (roots only).
    pub query_type: QueryType,
    /// Mutation classification (roots only).
    pub mutation_type: MutationType,
    /// Whether the field carries `@external`.
    pub is_external: bool,
    /// Whether the field carries `@custom(http:)`.
    pub is_custom_http: bool,
    /// Whether any descendant carries `@custom(http:)`.
    pub has_custom_http_child: bool,
    /// Whether the field carries `@lambda`.
    pub has_lambda: bool,
}

impl BoundField {
    /// Argument value, if present.
    #[must_use]
    pub fn arg(&self, name: &str) -> Option<&serde_json::Value> {
        self.args.get(name)
    }

    /// DQL alias: root name, else `Object.alias`.
    #[must_use]
    pub fn datastreamdb_alias(&self, is_root: bool) -> String {
        if is_root {
            return self.name.clone();
        }
        format!("{}.{}", self.object, self.alias)
    }

    /// Innermost named type.
    #[must_use]
    pub fn type_name(&self) -> &str {
        self.field_type.name()
    }
}

/// Operation binder state.
struct Binder<'a> {
    schema: &'a BoundSchema,
    fragments: &'a HashMap<
        async_graphql_value::Name,
        async_graphql_parser::Positioned<FragmentDefinition>,
    >,
    variables: &'a HashMap<String, serde_json::Value>,
    defaults: HashMap<String, serde_json::Value>,
    errs: ErrorList,
}

impl Binder<'_> {
    /// Binds one field against a parent type.
    #[allow(clippy::too_many_lines, reason = "binder with fragment expansion")]
    fn bind_field(
        &mut self,
        parent: &str,
        field: &ParserField,
        position: (usize, usize),
        is_root: bool,
    ) -> Option<BoundField> {
        let name = field.name.node.to_string();
        if name == TYPENAME {
            return Some(BoundField {
                name: name.clone(),
                alias: field
                    .alias
                    .as_ref()
                    .map_or_else(|| name.clone(), |alias| alias.node.to_string()),
                object: parent.to_owned(),
                field_type: GqlType::named("String"),
                args: HashMap::new(),
                arg_is_list: HashSet::new(),
                children: Vec::new(),
                skip: false,
                include: true,
                cascade: None,
                position,
                query_type: QueryType::NotSupported,
                mutation_type: MutationType::NotSupported,
                is_external: false,
                is_custom_http: false,
                has_custom_http_child: false,
                has_lambda: false,
            });
        }
        let Some(parent_def) = self.schema.schema.type_def(parent) else {
            self.errs.push(GraphQlError::new(format!(
                "unknown parent type \"{parent}\" for field \"{name}\""
            )));
            return None;
        };
        let Some(def) = parent_def.field(&name).cloned() else {
            self.errs.push(GraphQlError::new(format!(
                "unknown field \"{name}\" on type \"{parent}\""
            )));
            return None;
        };
        let mut args = HashMap::new();
        let mut arg_is_list = HashSet::new();
        for (arg_name, arg_val) in &field.arguments {
            let arg_name = arg_name.node.to_string();
            let Some(decl) = def.args.iter().find(|arg| arg.name == arg_name) else {
                self.errs.push(GraphQlError::new(format!(
                    "unknown argument \"{arg_name}\" on field \"{parent}.{name}\""
                )));
                continue;
            };
            if decl.ty.is_list() {
                arg_is_list.insert(arg_name.clone());
            }
            match self.substitute(&arg_val.node) {
                Ok(val) => {
                    args.insert(arg_name, val);
                }
                Err(err) => self.errs.push(err),
            }
        }
        let mut skip = false;
        let mut include = true;
        let mut cascade = None;
        for dir in &field.directives {
            let dir_name = dir.node.name.node.as_str();
            match dir_name {
                "skip" => {
                    if dir
                        .node
                        .get_argument("if")
                        .is_some_and(|val| self.directive_flag(&val.node))
                    {
                        skip = true;
                    }
                }
                "include" => {
                    if !dir
                        .node
                        .get_argument("if")
                        .is_some_and(|val| self.directive_flag(&val.node))
                    {
                        include = false;
                    }
                }
                "cascade" => {
                    cascade = Some(self.cascade_fields(parent_def, &def, &dir.node));
                }
                _ => {}
            }
        }
        let mut bound = BoundField {
            name: name.clone(),
            alias: field
                .alias
                .as_ref()
                .map_or_else(|| name.clone(), |alias| alias.node.to_string()),
            object: parent.to_owned(),
            field_type: def.ty.clone(),
            args,
            arg_is_list,
            children: Vec::new(),
            skip,
            include,
            cascade,
            position,
            query_type: QueryType::NotSupported,
            mutation_type: MutationType::NotSupported,
            is_external: def.directive("external").is_some(),
            is_custom_http: def
                .directive(CUSTOM_DIRECTIVE)
                .is_some_and(|dir| dir.arg("http").is_some()),
            has_custom_http_child: false,
            has_lambda: def.directive(crate::directives::LAMBDA_DIRECTIVE).is_some(),
        };
        if is_root {
            if parent == "Query" {
                bound.query_type = self.schema.query_type(&name);
            } else if parent == "Mutation" {
                bound.mutation_type = self.schema.mutation_type(&name);
            }
        }
        let child_type = def.ty.name().to_owned();
        for item in &field.selection_set.node.items {
            self.bind_selection(&child_type, &item.node, &mut bound);
        }
        bound.has_custom_http_child = bound
            .children
            .iter()
            .any(|child| child.is_custom_http || child.has_custom_http_child);
        Some(bound)
    }

    /// Binds one selection (field or fragment) into a parent.
    fn bind_selection(&mut self, parent: &str, selection: &Selection, bound: &mut BoundField) {
        match selection {
            Selection::Field(field) => {
                if let Some(child) = self.bind_field(parent, &field.node, (0, 0), false) {
                    bound.children.push(child);
                }
            }
            Selection::FragmentSpread(spread) => {
                let name = spread.node.fragment_name.node.to_string();
                let Some(fragment) = self.fragments.get(name.as_str()).cloned() else {
                    self.errs
                        .push(GraphQlError::new(format!("unknown fragment \"{name}\"")));
                    return;
                };
                self.bind_fragment(parent, &fragment.node, bound);
            }
            Selection::InlineFragment(fragment) => {
                let node = &fragment.node;
                let condition = node
                    .type_condition
                    .as_ref()
                    .map(|cond| cond.node.on.node.to_string());
                self.bind_fragment_selection(
                    parent,
                    condition.as_deref(),
                    &node.selection_set.node,
                    bound,
                );
            }
        }
    }

    /// Binds a named fragment's selections.
    fn bind_fragment(
        &mut self,
        parent: &str,
        fragment: &FragmentDefinition,
        bound: &mut BoundField,
    ) {
        let condition = fragment.type_condition.node.on.node.to_string();
        self.bind_fragment_selection(
            parent,
            Some(&condition),
            &fragment.selection_set.node,
            bound,
        );
    }

    /// Binds fragment selections under an optional type condition.
    fn bind_fragment_selection(
        &mut self,
        parent: &str,
        condition: Option<&str>,
        set: &async_graphql_parser::types::SelectionSet,
        bound: &mut BoundField,
    ) {
        let Some(condition) = condition else {
            for item in &set.items {
                self.bind_selection(parent, &item.node, bound);
            }
            return;
        };
        if !fragment_applies(&self.schema.schema, condition, parent) {
            return;
        }
        let scope = if self.schema.schema.type_def(condition).is_some() {
            condition
        } else {
            parent
        };
        for item in &set.items {
            match &item.node {
                Selection::Field(field) => {
                    let name = field.node.name.node.as_str();
                    let resolved = if self
                        .schema
                        .schema
                        .type_def(scope)
                        .is_some_and(|def| def.field(name).is_some())
                    {
                        scope
                    } else {
                        parent
                    };
                    if let Some(child) = self.bind_field(resolved, &field.node, (0, 0), false) {
                        bound.children.push(child);
                    }
                }
                _ => self.bind_selection(scope, &item.node, bound),
            }
        }
    }

    /// Substitutes variables in an argument value.
    fn substitute(&self, val: &ParserValue) -> Result<serde_json::Value, GraphQlError> {
        match val {
            ParserValue::Variable(name) => self
                .variables
                .get(name.as_str())
                .or_else(|| self.defaults.get(name.as_str()))
                .cloned()
                .ok_or_else(|| GraphQlError::new(format!("variable \"${name}\" has no value"))),
            ParserValue::Null => Ok(serde_json::Value::Null),
            ParserValue::Number(num) => num.as_i64().map_or_else(
                || {
                    num.as_f64().map_or_else(
                        || Ok(serde_json::Value::Null),
                        |num| {
                            serde_json::Number::from_f64(num)
                                .map(serde_json::Value::Number)
                                .ok_or_else(|| GraphQlError::new("bad float argument"))
                        },
                    )
                },
                |num| Ok(serde_json::Value::Number(num.into())),
            ),
            ParserValue::String(text) => Ok(serde_json::Value::String(text.clone())),
            ParserValue::Boolean(val) => Ok(serde_json::Value::Bool(*val)),
            ParserValue::Enum(name) => Ok(serde_json::Value::String(name.to_string())),
            ParserValue::Binary(bytes) => Ok(serde_json::Value::String(format!("{bytes:?}"))),
            ParserValue::List(items) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    out.push(self.substitute(item)?);
                }
                Ok(serde_json::Value::Array(out))
            }
            ParserValue::Object(fields) => {
                let mut out = serde_json::Map::new();
                for (key, val) in fields {
                    out.insert(key.to_string(), self.substitute(val)?);
                }
                Ok(serde_json::Value::Object(out))
            }
        }
    }

    /// Evaluates a `@skip(if:)` / `@include(if:)` flag.
    fn directive_flag(&self, val: &ParserValue) -> bool {
        self.substitute(val)
            .ok()
            .and_then(|val| val.as_bool())
            .unwrap_or(false)
    }

    /// Resolves `@cascade(fields:)` to `DataStreamDB` predicates.
    fn cascade_fields(
        &self,
        parent_def: &TypeDef,
        def: &crate::model::FieldDef,
        dir: &ParserDirective,
    ) -> Vec<String> {
        let Some(fields) = dir.get_argument(CASCADE_ARG) else {
            return vec!["__all__".to_owned()];
        };
        let Ok(serde_json::Value::Array(names)) = self.substitute(&fields.node) else {
            return vec!["__all__".to_owned()];
        };
        if names.is_empty() {
            return vec!["__all__".to_owned()];
        }
        let id_field = parent_def
            .fields
            .iter()
            .find(|field| field.ty.name() == ID_TYPE)
            .map(|field| field.name.clone());
        let mut out = Vec::new();
        for name in names {
            if let Some(name) = name.as_str() {
                if id_field.as_deref() == Some(name) {
                    out.push("uid".to_owned());
                } else if let Some(pred) = self.schema.predicate(&parent_def.name, name) {
                    out.push(pred.to_owned());
                } else if def.name == name {
                    out.push(name.to_owned());
                }
            }
        }
        out
    }
}

/// Converts a constant parser value to JSON (defaults, cache control).
fn parser_value_to_json(val: &async_graphql_value::ConstValue) -> serde_json::Value {
    match val {
        async_graphql_value::ConstValue::Null => serde_json::Value::Null,
        async_graphql_value::ConstValue::Number(num) => num.as_i64().map_or_else(
            || {
                num.as_f64()
                    .and_then(serde_json::Number::from_f64)
                    .map_or(serde_json::Value::Null, serde_json::Value::Number)
            },
            |num| serde_json::Value::Number(num.into()),
        ),
        async_graphql_value::ConstValue::String(text) => serde_json::Value::String(text.clone()),
        async_graphql_value::ConstValue::Boolean(val) => serde_json::Value::Bool(*val),
        async_graphql_value::ConstValue::Enum(name) => serde_json::Value::String(name.to_string()),
        async_graphql_value::ConstValue::Binary(bytes) => {
            serde_json::Value::String(format!("{bytes:?}"))
        }
        async_graphql_value::ConstValue::List(items) => {
            serde_json::Value::Array(items.iter().map(parser_value_to_json).collect())
        }
        async_graphql_value::ConstValue::Object(fields) => serde_json::Value::Object(
            fields
                .iter()
                .map(|(key, val)| (key.to_string(), parser_value_to_json(val)))
                .collect(),
        ),
    }
}

/// Whether a fragment on `condition` applies within `current`.
fn fragment_applies(schema: &Schema, condition: &str, current: &str) -> bool {
    if condition == current {
        return true;
    }
    let (Some(cond_def), Some(current_def)) =
        (schema.type_def(condition), schema.type_def(current))
    else {
        return false;
    };
    match (cond_def.kind, current_def.kind) {
        (TypeKind::Interface, TypeKind::Object) => current_def
            .implements
            .iter()
            .any(|iface| iface == condition),
        (TypeKind::Object, TypeKind::Interface) => {
            cond_def.implements.iter().any(|iface| iface == current)
        }
        (TypeKind::Object, TypeKind::Union) => current_def
            .possible_types
            .iter()
            .any(|member| member == condition),
        (TypeKind::Interface, TypeKind::Interface) => {
            cond_def.implements.iter().any(|iface| iface == current)
                || current_def
                    .implements
                    .iter()
                    .any(|iface| iface == condition)
        }
        _ => false,
    }
}
