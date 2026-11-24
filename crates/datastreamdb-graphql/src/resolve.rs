//! GraphQL execution over the embedded engine. Binds operations, rewrites
//! to DQL, executes through `datastreamdb-query`, and normalizes results for
//! async-graphql response building.
//!
//! Response shaping is delegated to async-graphql execution: root resolvers
//! return normalized JSON subtrees, nested resolvers pluck fields from them.
//! This replaces Go's `CompleteObject`/`CompleteValue` (`completion.go`).

#![warn(missing_docs)]

use std::collections::HashMap;
use std::sync::Arc;

use async_graphql::dynamic::{FieldFuture, FieldValue, ResolverContext};
use async_graphql::{
    Error as GqlExecError, Request as GqlRequest, Response as GqlResponse, Variables,
};
use datastreamdb_query::QueryEngine;
use serde_json::{Map as JsonMap, Value as Json};

use crate::auth_rules::AuthVars;
use crate::dql_render::as_string;
use crate::errors::GraphQlError;
use crate::op::{BoundField, BoundOperation, BoundSchema, QueryType};
use crate::rewrite_query::{RewrittenQuery, rewrite_query};

/// Shared execution state available to every resolver.
#[derive(Clone)]
pub struct Shared {
    /// Bound schema.
    pub bound: BoundSchema,
    /// Query engine.
    pub engine: Arc<QueryEngine>,
    /// Namespace.
    pub namespace: u64,
}

/// Embedded GraphQL engine: schema plus executor.
pub struct GraphQlEngine {
    /// Shared state.
    shared: Arc<Shared>,
    /// Executable dynamic schema.
    schema: async_graphql::dynamic::Schema,
}

impl GraphQlEngine {
    /// Builds an engine over a bound schema and query engine.
    ///
    /// # Errors
    ///
    /// Returns [`GraphQlError`] when dynamic schema construction fails.
    pub fn new(
        bound: BoundSchema,
        engine: Arc<QueryEngine>,
        namespace: u64,
    ) -> Result<Self, GraphQlError> {
        let shared = Arc::new(Shared {
            bound,
            engine,
            namespace,
        });
        let schema = crate::resolve_schema::build_dynamic_schema(&shared).map_err(|err| {
            GraphQlError::new(format!("failed to build executable schema: {err}"))
        })?;
        Ok(Self { shared, schema })
    }

    /// Executes a GraphQL request with JSON variables and headers.
    pub async fn execute(
        &self,
        query: &str,
        variables: Json,
        headers: &HashMap<String, String>,
    ) -> GqlResponse {
        self.execute_named(query, None, variables, headers).await
    }

    /// Executes a request with an explicit operation name.
    pub async fn execute_named(
        &self,
        query: &str,
        operation: Option<&str>,
        variables: Json,
        headers: &HashMap<String, String>,
    ) -> GqlResponse {
        let vars: HashMap<String, Json> = variables
            .as_object()
            .map(|map| {
                map.iter()
                    .map(|(key, val)| (key.clone(), val.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let bound = match self.shared.bound.parse_operation(query, operation, &vars) {
            Ok(op) => op,
            Err(errs) => return error_response(errs.messages()),
        };
        let auth_vars = match self.claims(headers) {
            Ok(vars) => vars,
            Err(err) => return error_response(vec![err.message]),
        };
        let stripped = strip_cascade(query);
        let mut request = GqlRequest::new(stripped);
        if let Some(name) = operation {
            request = request.operation_name(name);
        }
        request = request.variables(Variables::from_json(variables));
        let request = request
            .data(bound)
            .data(auth_vars)
            .data(self.shared.clone());
        self.schema.execute(request).await
    }

    /// Extracts auth variables from request headers.
    fn claims(&self, headers: &HashMap<String, String>) -> Result<AuthVars, GraphQlError> {
        let Some(meta) = self.shared.bound.meta.auth_meta.as_ref() else {
            return Ok(HashMap::new());
        };
        let header = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(meta.get_header()))
            .map(|(_, val)| val.as_str());
        crate::auth::extract_custom_claims(meta, header)
    }

    /// Shared state (for subscriptions).
    #[must_use]
    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }
}

/// Builds an error-only response.
fn error_response(messages: Vec<String>) -> GqlResponse {
    GqlResponse::from_errors(
        messages
            .into_iter()
            .map(|message| async_graphql::ServerError::new(message, None))
            .collect(),
    )
}

/// Strips `@cascade` directives (already consumed by the binder) so strict
/// async-graphql validation accepts the query.
fn strip_cascade(query: &str) -> String {
    let mut out = String::with_capacity(query.len());
    let bytes = query.as_bytes();
    let mut boundaries: Vec<usize> = query.char_indices().map(|(i, _)| i).collect();
    boundaries.push(query.len());
    let mut cursor = 0;
    let mut kept = 0;
    while cursor < boundaries.len() {
        let i = boundaries[cursor];
        if i < bytes.len()
            && query[i..].starts_with("@cascade")
            && (i == 0 || !is_name_char(bytes[i - 1]))
        {
            out.push_str(&query[kept..i]);
            let mut j = i + "@cascade".len();
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'(' {
                let mut depth = 0;
                while j < bytes.len() {
                    match bytes[j] {
                        b'(' => depth += 1,
                        b')' => {
                            depth -= 1;
                            if depth == 0 {
                                j += 1;
                                break;
                            }
                        }
                        b'"' => {
                            j += 1;
                            while j < bytes.len() && bytes[j] != b'"' {
                                if bytes[j] == b'\\' {
                                    j += 1;
                                }
                                j += 1;
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
            }
            while cursor < boundaries.len() && boundaries[cursor] < j {
                cursor += 1;
            }
            kept = boundaries[cursor.min(boundaries.len() - 1)];
        } else {
            cursor += 1;
        }
    }
    out.push_str(&query[kept..]);
    out
}

/// Whether the byte continues a GraphQL name.
fn is_name_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Finds the bound root matching a response key.
fn find_root<'a>(
    op: &'a BoundOperation,
    name: &str,
    alias: Option<&str>,
) -> Option<&'a BoundField> {
    let key = alias.unwrap_or(name);
    op.roots.iter().find(|root| root.alias == key)
}

/// Wraps a normalized root value (lists become `FieldValue::list`).
fn root_field_value(schema: &BoundSchema, root: &BoundField, value: Json) -> FieldValue<'static> {
    if root.field_type.is_list() {
        match value {
            Json::Array(items) => FieldValue::list(items.into_iter().map(|item| {
                let type_name = concrete_type(schema, root.type_name(), &item);
                wrap_object(item, &type_name, is_abstract_type(schema, root.type_name()))
            })),
            Json::Null => FieldValue::value(async_graphql::Value::Null),
            single => FieldValue::list([wrap_object(
                single,
                root.type_name(),
                is_abstract_type(schema, root.type_name()),
            )]),
        }
    } else {
        wrap_value(schema, root.type_name(), &value)
    }
}

/// Wraps one normalized value with its concrete type.
fn wrap_value(schema: &BoundSchema, type_name: &str, value: &Json) -> FieldValue<'static> {
    match value {
        Json::Array(items) => FieldValue::list(
            items
                .iter()
                .cloned()
                .map(|item| wrap_object(item, type_name, is_abstract_type(schema, type_name))),
        ),
        Json::Object(_) => wrap_object(
            value.clone(),
            &concrete_type(schema, type_name, value),
            is_abstract_type(schema, type_name),
        ),
        Json::Null => FieldValue::value(async_graphql::Value::Null),
        Json::Bool(val) => FieldValue::value(async_graphql::Value::Boolean(*val)),
        Json::Number(num) => FieldValue::value(async_graphql::Value::Number(num.clone())),
        Json::String(text) => FieldValue::value(async_graphql::Value::String(text.clone())),
    }
}

/// Wraps one object, adding `with_type` only for abstract types.
///
/// NOTE: `with_type` wraps the value so `downcast_ref` fails; only
/// abstract-typed fields need it (the executor unwraps those itself).
fn wrap_object(value: Json, type_name: &str, is_abstract: bool) -> FieldValue<'static> {
    let node = JsonNode {
        value,
        type_name: type_name.to_owned(),
    };
    if is_abstract {
        FieldValue::owned_any(node).with_type(type_name.to_owned())
    } else {
        FieldValue::owned_any(node)
    }
}

/// Root query resolver: rewrite, execute, normalize.
pub(crate) fn resolve_root_query(ctx: ResolverContext<'_>) -> FieldFuture<'_> {
    let field_name = ctx.field().name().to_owned();
    let alias = ctx.field().alias().map(str::to_owned);
    FieldFuture::new(async move {
        let shared = ctx.data::<Arc<Shared>>()?.clone();
        let op = ctx.data::<BoundOperation>()?.clone();
        let auth_vars = ctx.data::<AuthVars>()?.clone();
        let Some(root) = find_root(&op, &field_name, alias.as_deref()) else {
            return Err(GqlExecError::new(format!(
                "unknown root field \"{field_name}\""
            )));
        };
        if root.query_type == QueryType::HttpQuery {
            return Err(GqlExecError::new(format!(
                "custom HTTP query \"{}\" is not supported in embedded mode",
                root.name
            )));
        }
        if root.query_type == QueryType::DqlQuery {
            return resolve_custom_dql(&shared, root);
        }
        let rewritten = rewrite_query(&shared.bound, root, &auth_vars)
            .map_err(|err| GqlExecError::new(err.message))?;
        let value = run_rewritten(&shared, &rewritten)?;
        let normalized = normalize_root(&shared.bound, root, &value);
        Ok(Some(root_field_value(&shared.bound, root, normalized)))
    })
}

/// Executes rewritten DQL blocks and returns the root block's JSON value.
fn run_rewritten(shared: &Shared, rewritten: &RewrittenQuery) -> Result<Json, GqlExecError> {
    if rewritten.queries.is_empty() {
        return Ok(Json::Null);
    }
    let alias = rewritten.queries[0].alias.clone();
    let attr = rewritten.queries[0].attr.clone();
    let request = datastreamdb_dql::Request {
        text: as_string(&rewritten.queries),
        variables: rewritten.vars.clone(),
    };
    let parsed = datastreamdb_dql::parse(request)
        .map_err(|err| GqlExecError::new(format!("DQL parse failed: {err}")))?;
    let read_ts = shared.engine.posting().read_ts();
    let (mut result, _) = shared
        .engine
        .query_parsed(shared.namespace, parsed.query, read_ts)
        .map_err(|err| GqlExecError::new(format!("DQL execution failed: {err}")))?;
    let bytes = datastreamdb_query::output_render::to_json(&mut result.subgraphs)
        .map_err(|err| GqlExecError::new(format!("DQL encoding failed: {err}")))?;
    let json: Json = serde_json::from_slice(&bytes).unwrap_or(Json::Null);
    let key = if alias.is_empty() { attr } else { alias };
    let key = key.trim_end_matches("()").to_owned();
    Ok(json.get(&key).cloned().unwrap_or(Json::Null))
}

/// Executes a `@custom(dql:)` root query.
fn resolve_custom_dql(
    shared: &Shared,
    root: &BoundField,
) -> Result<Option<FieldValue<'static>>, GqlExecError> {
    let dql = shared
        .bound
        .schema
        .type_def("Query")
        .and_then(|query| query.field(&root.name))
        .and_then(|def| def.directive(crate::directives::CUSTOM_DIRECTIVE))
        .and_then(|dir| dir.arg(crate::directives::DQL_ARG))
        .map(super::model::Value::raw)
        .unwrap_or_default();
    let mut text = dql;
    for (name, val) in &root.args {
        let rendered = match val {
            Json::String(text) => format!("\"{text}\""),
            _ => val.to_string(),
        };
        text = text.replace(&format!("${name}"), &rendered);
    }
    let request = datastreamdb_dql::Request {
        text,
        variables: HashMap::new(),
    };
    let parsed = datastreamdb_dql::parse(request)
        .map_err(|err| GqlExecError::new(format!("custom DQL parse failed: {err}")))?;
    let read_ts = shared.engine.posting().read_ts();
    let (mut result, _) = shared
        .engine
        .query_parsed(shared.namespace, parsed.query, read_ts)
        .map_err(|err| GqlExecError::new(format!("custom DQL execution failed: {err}")))?;
    let bytes = datastreamdb_query::output_render::to_json(&mut result.subgraphs)
        .map_err(|err| GqlExecError::new(format!("DQL encoding failed: {err}")))?;
    let json: Json = serde_json::from_slice(&bytes).unwrap_or(Json::Null);
    let value = json
        .as_object()
        .and_then(|obj| obj.values().next().cloned())
        .unwrap_or(Json::Null);
    Ok(Some(root_field_value(&shared.bound, root, value)))
}

/// Normalized JSON node with its GraphQL type.
#[derive(Clone)]
pub struct JsonNode {
    /// Normalized value (GraphQL response keys).
    pub value: Json,
    /// GraphQL type name.
    pub type_name: String,
}

/// Normalizes one root block's DQL JSON using the bound selection.
pub(crate) fn normalize_root(schema: &BoundSchema, root: &BoundField, value: &Json) -> Json {
    match value {
        Json::Array(items) => Json::Array(
            items
                .iter()
                .map(|item| normalize_object(schema, root, item))
                .collect(),
        ),
        Json::Object(_) => normalize_object(schema, root, value),
        _ => value.clone(),
    }
}

/// Normalizes one object: DQL aliases to GraphQL response keys.
fn normalize_object(schema: &BoundSchema, field: &BoundField, value: &Json) -> Json {
    let Some(obj) = value.as_object() else {
        return value.clone();
    };
    let mut out = JsonMap::new();
    if let Some(types) = obj.get("datastreamdb.type") {
        out.insert("datastreamdb.type".to_owned(), types.clone());
    }
    for child in &field.children {
        if child.skip || !child.include {
            continue;
        }
        if child.name == "__typename" {
            continue;
        }
        if crate::rewrite_auth::is_aggregate_field(child) {
            out.insert(
                child.alias.clone(),
                normalize_aggregate(schema, field, child, obj),
            );
            continue;
        }
        let key = format!("{}.{}", child.object, child.alias);
        let Some(val) = obj.get(&key) else {
            continue;
        };
        out.insert(child.alias.clone(), normalize_value(schema, child, val));
    }
    // Preserve scalar/vector extras selected directly (e.g. vector_distance
    // on similarity results uses a plain predicate alias).
    for child in &field.children {
        if out.contains_key(&child.alias) {
            continue;
        }
        if let Some(val) = obj.get(&child.alias) {
            out.insert(child.alias.clone(), val.clone());
        }
    }
    Json::Object(out)
}

/// Normalizes a child value (object, list, or scalar).
fn normalize_value(schema: &BoundSchema, field: &BoundField, value: &Json) -> Json {
    match value {
        Json::Array(items) => Json::Array(
            items
                .iter()
                .map(|item| normalize_object(schema, field, item))
                .collect(),
        ),
        Json::Object(_) => {
            if field.children.is_empty() {
                convert_geo_value(schema, field, value)
            } else {
                normalize_object(schema, field, value)
            }
        }
        _ => value.clone(),
    }
}

/// Normalizes a nested aggregate field from aliased aggregate keys.
fn normalize_aggregate(
    schema: &BoundSchema,
    parent: &BoundField,
    field: &BoundField,
    obj: &JsonMap<String, Json>,
) -> Json {
    let _ = (schema, parent);
    let parent_alias = format!("{}.{}", field.object, field.alias);
    let mut out = JsonMap::new();
    for child in &field.children {
        let key = format!("{}.{}_{parent_alias}", field.type_name(), child.alias);
        if let Some(val) = obj.get(&key) {
            out.insert(child.alias.clone(), val.clone());
        }
    }
    Json::Object(out)
}

/// Converts DQL geo JSON into the GraphQL geo object shape.
fn convert_geo_value(schema: &BoundSchema, field: &BoundField, value: &Json) -> Json {
    let type_name = field.type_name();
    if !schema.is_geo(type_name) {
        return value.clone();
    }
    let Some(obj) = value.as_object() else {
        return value.clone();
    };
    let coords = obj.get("coordinates").cloned().unwrap_or(Json::Null);
    match type_name {
        "Point" => {
            let (lng, lat) = coord_pair(&coords, 0);
            Json::Object(JsonMap::from_iter([
                ("longitude".to_owned(), Json::from(lng)),
                ("latitude".to_owned(), Json::from(lat)),
            ]))
        }
        "Polygon" => Json::Object(JsonMap::from_iter([(
            "coordinates".to_owned(),
            rings_to_points(&coords),
        )])),
        "MultiPolygon" => {
            let empty = Vec::new();
            let polygons = coords.as_array().unwrap_or(&empty);
            Json::Object(JsonMap::from_iter([(
                "polygons".to_owned(),
                Json::Array(
                    polygons
                        .iter()
                        .map(|polygon| {
                            Json::Object(JsonMap::from_iter([(
                                "coordinates".to_owned(),
                                rings_to_points(polygon),
                            )]))
                        })
                        .collect(),
                ),
            )]))
        }
        _ => value.clone(),
    }
}

/// Extracts one `[lng, lat]` pair.
fn coord_pair(coords: &Json, _index: usize) -> (f64, f64) {
    let pair = coords.as_array();
    let lng = pair
        .and_then(|items| items.first())
        .and_then(Json::as_f64)
        .unwrap_or(0.0);
    let lat = pair
        .and_then(|items| items.get(1))
        .and_then(Json::as_f64)
        .unwrap_or(0.0);
    (lng, lat)
}

/// Converts polygon rings to `[{points: [{longitude, latitude}]}]`.
fn rings_to_points(coords: &Json) -> Json {
    let empty = Vec::new();
    let rings = coords.as_array().unwrap_or(&empty);
    Json::Array(
        rings
            .iter()
            .map(|ring| {
                let empty = Vec::new();
                let points = ring.as_array().unwrap_or(&empty);
                Json::Object(JsonMap::from_iter([(
                    "points".to_owned(),
                    Json::Array(
                        points
                            .iter()
                            .map(|point| {
                                let (lng, lat) = coord_pair(point, 0);
                                Json::Object(JsonMap::from_iter([
                                    ("longitude".to_owned(), Json::from(lng)),
                                    ("latitude".to_owned(), Json::from(lat)),
                                ]))
                            })
                            .collect(),
                    ),
                )]))
            })
            .collect(),
    )
}

/// Whether the named type is abstract (interface or union).
fn is_abstract_type(schema: &BoundSchema, name: &str) -> bool {
    schema.schema.type_def(name).is_some_and(|def| {
        matches!(
            def.kind,
            crate::model::TypeKind::Interface | crate::model::TypeKind::Union
        )
    })
}

/// Resolves the concrete GraphQL type from `datastreamdb.type` entries.
fn concrete_type(schema: &BoundSchema, static_type: &str, value: &Json) -> String {
    if !schema.is_interface(static_type) && !schema.is_union(static_type) {
        return static_type.to_owned();
    }
    let types = value
        .as_object()
        .and_then(|obj| obj.get("datastreamdb.type"))
        .and_then(|val| val.as_array())
        .cloned()
        .unwrap_or_default();
    for entry in types {
        if let Some(name) = entry.as_str() {
            if let Some(gql) = schema.schema.order.iter().find(|def_name| {
                schema.schema.type_def(def_name).is_some_and(|def| {
                    def.kind == crate::model::TypeKind::Object
                        && crate::rules_util::type_name(def) == name
                })
            }) {
                return gql.clone();
            }
        }
    }
    static_type.to_owned()
}
