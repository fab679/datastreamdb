//! Directive names, schema extras, and lookup tables. Port of the const
//! block and table vars at the top of `dgraph-main/graphql/schema/gqlschema.go`.

#![warn(missing_docs)]

/// `@hasInverse` directive and its `field` argument.
pub(crate) const INVERSE_DIRECTIVE: &str = "hasInverse";
/// Argument of `@hasInverse` naming the inverse field.
pub(crate) const INVERSE_ARG: &str = "field";

/// `@search` directive and its `by` argument.
pub(crate) const SEARCH_DIRECTIVE: &str = "search";
/// Argument of `@search` listing index names.
pub(crate) const SEARCH_ARGS: &str = "by";

/// `@datastreamdb` directive, `type` and `pred` arguments.
pub(crate) const DATASTREAMDB_DIRECTIVE: &str = "datastreamdb";
/// `@datastreamdb(type:)` argument.
pub(crate) const DATASTREAMDB_TYPE_ARG: &str = "type";
/// `@datastreamdb(pred:)` argument.
pub(crate) const DATASTREAMDB_PRED_ARG: &str = "pred";
/// `@embedding` directive.
pub(crate) const EMBEDDING_DIRECTIVE: &str = "embedding";

/// `@id` directive and its `interface` argument.
pub(crate) const ID_DIRECTIVE: &str = "id";
/// `@id(interface:)` argument.
pub(crate) const ID_DIRECTIVE_INTERFACE_ARG: &str = "interface";
/// `@withSubscription` directive.
pub(crate) const SUBSCRIPTION_DIRECTIVE: &str = "withSubscription";
/// `@secret` directive (password fields).
pub(crate) const SECRET_DIRECTIVE: &str = "secret";
/// `@auth` directive.
pub(crate) const AUTH_DIRECTIVE: &str = "auth";
/// `@custom` directive.
pub(crate) const CUSTOM_DIRECTIVE: &str = "custom";
/// `@remote` directive (types not stored in `DataStreamDB`).
pub(crate) const REMOTE_DIRECTIVE: &str = "remote";
/// `@remoteResponse` directive.
pub(crate) const REMOTE_RESPONSE_DIRECTIVE: &str = "remoteResponse";
/// `@lambda` directive.
pub(crate) const LAMBDA_DIRECTIVE: &str = "lambda";
/// `@lambdaOnMutate` directive.
pub(crate) const LAMBDA_ON_MUTATE_DIRECTIVE: &str = "lambdaOnMutate";
/// `@default` directive.
pub(crate) const DEFAULT_DIRECTIVE: &str = "default";

/// `@generate` directive and its arguments/fields.
pub(crate) const GENERATE_DIRECTIVE: &str = "generate";
/// `@generate(query:)` argument.
pub(crate) const GENERATE_QUERY_ARG: &str = "query";
/// `get` field of `GenerateQueryParams`.
pub(crate) const GENERATE_GET_FIELD: &str = "get";
/// `query` field of `GenerateQueryParams`.
pub(crate) const GENERATE_QUERY_FIELD: &str = "query";
/// `password` field of `GenerateQueryParams`.
pub(crate) const GENERATE_PASSWORD_FIELD: &str = "password";
/// `aggregate` field of `GenerateQueryParams`.
pub(crate) const GENERATE_AGGREGATE_FIELD: &str = "aggregate";
/// `@generate(mutation:)` argument.
pub(crate) const GENERATE_MUTATION_ARG: &str = "mutation";
/// `add` field of `GenerateMutationParams`.
pub(crate) const GENERATE_ADD_FIELD: &str = "add";
/// `update` field of `GenerateMutationParams`.
pub(crate) const GENERATE_UPDATE_FIELD: &str = "update";
/// `delete` field of `GenerateMutationParams`.
pub(crate) const GENERATE_DELETE_FIELD: &str = "delete";
/// `@generate(subscription:)` argument.
pub(crate) const GENERATE_SUBSCRIPTION_ARG: &str = "subscription";

/// `@cascade` directive and its `fields` argument.
pub(crate) const CASCADE_DIRECTIVE: &str = "cascade";
/// `@cascade(fields:)` argument.
pub(crate) const CASCADE_ARG: &str = "fields";

/// `@cacheControl` directive.
pub(crate) const CACHE_CONTROL_DIRECTIVE: &str = "cacheControl";
/// Apollo federation `@key` directive and `fields` argument.
pub(crate) const APOLLO_KEY_DIRECTIVE: &str = "key";
/// `@key(fields:)` argument.
pub(crate) const APOLLO_KEY_ARG: &str = "fields";
/// Apollo federation `@external` directive.
pub(crate) const APOLLO_EXTERNAL_DIRECTIVE: &str = "external";
/// Apollo federation `@extends` directive.
pub(crate) const APOLLO_EXTENDS_DIRECTIVE: &str = "extends";
/// Apollo federation `@requires` directive.
pub(crate) const APOLLO_REQUIRES_DIRECTIVE: &str = "requires";
/// Apollo federation `@provides` directive.
pub(crate) const APOLLO_PROVIDES_DIRECTIVE: &str = "provides";

/// `@custom(dql:)` argument.
pub(crate) const DQL_ARG: &str = "dql";
/// `@custom(http:)` argument.
pub(crate) const HTTP_ARG: &str = "http";

/// Geo type and field names.
pub(crate) const POINT: &str = "Point";
/// Geo type name.
pub(crate) const POLYGON: &str = "Polygon";
/// Geo type name.
pub(crate) const MULTI_POLYGON: &str = "MultiPolygon";

/// `@deprecated` directive.
pub(crate) const DEPRECATED_DIRECTIVE: &str = "deprecated";
/// `numUids` aggregate field.
pub(crate) const NUM_UID: &str = "numUids";
/// `msg` payload field.
pub(crate) const MSG: &str = "msg";

/// `__typename` meta field.
pub(crate) const TYPENAME: &str = "__typename";

/// ID scalar type name.
pub(crate) const ID_TYPE: &str = "ID";

/// Scalars, ranges, geo inputs, and enums injected before validation.
///
/// Port of Go `schemaInputs` (byte-identical).
pub(crate) const SCHEMA_INPUTS: &str = r#"
"""
The Int64 scalar type represents a signed 64‐bit numeric non‐fractional value.
Int64 can represent values in range [-(2^63),(2^63 - 1)].
"""
scalar Int64

"""
The DateTime scalar type represents date and time as a string in RFC3339 format.
For example: "1985-04-12T23:20:50.52Z" represents 20 mins 50.52 secs after the 23rd hour of Apr 12th 1985 in UTC.
"""
scalar DateTime

input IntRange{
	min: Int!
	max: Int!
}

input FloatRange{
	min: Float!
	max: Float!
}

input Int64Range{
	min: Int64!
	max: Int64!
}

input DateTimeRange{
	min: DateTime!
	max: DateTime!
}

input StringRange{
	min: String!
	max: String!
}

enum DataStreamDBIndex {
	int
	int64
	float
	bool
	hash
	exact
	term
	fulltext
	trigram
	regexp
	year
	month
	day
	hour
	geo
	hnsw
}

input AuthRule {
	and: [AuthRule]
	or: [AuthRule]
	not: AuthRule
	rule: String
}

enum HTTPMethod {
	GET
	POST
	PUT
	PATCH
	DELETE
}

enum Mode {
	BATCH
	SINGLE
}

input CustomHTTP {
	url: String!
	method: HTTPMethod!
	body: String
	graphql: String
	mode: Mode
	forwardHeaders: [String!]
	secretHeaders: [String!]
	introspectionHeaders: [String!]
	skipIntrospection: Boolean
}

input DataStreamDBDefault {
	value: String
}

type Point {
	longitude: Float!
	latitude: Float!
}

input PointRef {
	longitude: Float!
	latitude: Float!
}

input NearFilter {
	distance: Float!
	coordinate: PointRef!
}

input PointGeoFilter {
	near: NearFilter
	within: WithinFilter
}

type PointList {
	points: [Point!]!
}

input PointListRef {
	points: [PointRef!]!
}

type Polygon {
	coordinates: [PointList!]!
}

input PolygonRef {
	coordinates: [PointListRef!]!
}

type MultiPolygon {
	polygons: [Polygon!]!
}

input MultiPolygonRef {
	polygons: [PolygonRef!]!
}

input WithinFilter {
	polygon: PolygonRef!
}

input ContainsFilter {
	point: PointRef
	polygon: PolygonRef
}

input IntersectsFilter {
	polygon: PolygonRef
	multiPolygon: MultiPolygonRef
}

input PolygonGeoFilter {
	near: NearFilter
	within: WithinFilter
	contains: ContainsFilter
	intersects: IntersectsFilter
}

input GenerateQueryParams {
	get: Boolean
	query: Boolean
	password: Boolean
	aggregate: Boolean
}

input GenerateMutationParams {
	add: Boolean
	update: Boolean
	delete: Boolean
}
"#;

/// `DataStreamDB` directive definitions injected before validation.
///
/// Port of Go `directiveDefs` (byte-identical).
pub(crate) const DIRECTIVE_DEFS: &str = r"
directive @hasInverse(field: String!) on FIELD_DEFINITION
directive @search(by: [String!]) on FIELD_DEFINITION
directive @embedding on FIELD_DEFINITION
directive @datastreamdb(type: String, pred: String) on OBJECT | INTERFACE | FIELD_DEFINITION
directive @id(interface: Boolean) on FIELD_DEFINITION
directive @default(add: DataStreamDBDefault, update: DataStreamDBDefault) on FIELD_DEFINITION
directive @withSubscription on OBJECT | INTERFACE | FIELD_DEFINITION
directive @secret(field: String!, pred: String) on OBJECT | INTERFACE
directive @auth(
	password: AuthRule
	query: AuthRule,
	add: AuthRule,
	update: AuthRule,
	delete: AuthRule) on OBJECT | INTERFACE
directive @custom(http: CustomHTTP, dql: String) on FIELD_DEFINITION
directive @remote on OBJECT | INTERFACE | UNION | INPUT_OBJECT | ENUM
directive @remoteResponse(name: String) on FIELD_DEFINITION
directive @cascade(fields: [String]) on FIELD
directive @lambda on FIELD_DEFINITION
directive @lambdaOnMutate(add: Boolean, update: Boolean, delete: Boolean) on OBJECT | INTERFACE
directive @cacheControl(maxAge: Int!) on QUERY
directive @generate(
	query: GenerateQueryParams,
	mutation: GenerateMutationParams,
	subscription: Boolean) on OBJECT | INTERFACE
";

/// Apollo-gateway-safe directive definitions (no non-scalar args).
///
/// Port of Go `apolloSupportedDirectiveDefs` (byte-identical).
pub(crate) const APOLLO_SUPPORTED_DIRECTIVE_DEFS: &str = r"
directive @hasInverse(field: String!) on FIELD_DEFINITION
directive @search(by: [String!]) on FIELD_DEFINITION
directive @embedding on FIELD_DEFINITION
directive @datastreamdb(type: String, pred: String) on OBJECT | INTERFACE | FIELD_DEFINITION
directive @id(interface: Boolean) on FIELD_DEFINITION
directive @default(add: DataStreamDBDefault, update: DataStreamDBDefault) on FIELD_DEFINITION
directive @withSubscription on OBJECT | INTERFACE | FIELD_DEFINITION
directive @secret(field: String!, pred: String) on OBJECT | INTERFACE
directive @remote on OBJECT | INTERFACE | UNION | INPUT_OBJECT | ENUM
directive @remoteResponse(name: String) on FIELD_DEFINITION
directive @lambda on FIELD_DEFINITION
directive @lambdaOnMutate(add: Boolean, update: Boolean, delete: Boolean) on OBJECT | INTERFACE
";

/// Built-in filter inputs injected before validation.
///
/// Port of Go `filterInputs` (byte-identical).
pub(crate) const FILTER_INPUTS: &str = r"
input IntFilter {
	eq: Int
	in: [Int]
	le: Int
	lt: Int
	ge: Int
	gt: Int
	between: IntRange
}

input Int64Filter {
	eq: Int64
	in: [Int64]
	le: Int64
	lt: Int64
	ge: Int64
	gt: Int64
	between: Int64Range
}

input FloatFilter {
	eq: Float
	in: [Float]
	le: Float
	lt: Float
	ge: Float
	gt: Float
	between: FloatRange
}

input DateTimeFilter {
	eq: DateTime
	in: [DateTime]
	le: DateTime
	lt: DateTime
	ge: DateTime
	gt: DateTime
	between: DateTimeRange
}

input StringTermFilter {
	allofterms: String
	anyofterms: String
}

input StringRegExpFilter {
	regexp: String
}

input StringNgramFilter {
	ngram: String
}

input StringFullTextFilter {
	alloftext: String
	anyoftext: String
}

input StringExactFilter {
	eq: String
	in: [String]
	le: String
	lt: String
	ge: String
	gt: String
	between: StringRange
}

input StringHashFilter {
	eq: String
	in: [String]
}
";

/// Apollo federation extras parsed only when `@key` types exist.
///
/// Port of Go `apolloSchemaExtras` (byte-identical).
pub(crate) const APOLLO_SCHEMA_EXTRAS: &str = r"
scalar _Any
scalar _FieldSet

type _Service {
	sdl: String
}

directive @external on FIELD_DEFINITION
directive @requires(fields: _FieldSet!) on FIELD_DEFINITION
directive @provides(fields: _FieldSet!) on FIELD_DEFINITION
directive @key(fields: _FieldSet!) on OBJECT | INTERFACE
directive @extends on OBJECT | INTERFACE
";

/// Apollo federation root queries (`_entities`, `_service`).
///
/// Port of Go `apolloSchemaQueries` (byte-identical).
pub(crate) const APOLLO_SCHEMA_QUERIES: &str = r"
type Query {
	_entities(representations: [_Any!]!): [_Entity]!
	_service: _Service!
}
";

/// Search argument to (GraphQL type, `DataStreamDB` index).
///
/// Port of Go `supportedSearches`.
pub(crate) const SUPPORTED_SEARCHES: &[(&str, &str, &str)] = &[
    ("int", "Int", "int"),
    ("int64", "Int64", "int"),
    ("float", "Float", "float"),
    ("bool", "Boolean", "bool"),
    ("hash", "String", "hash"),
    ("exact", "String", "exact"),
    ("term", "String", "term"),
    ("fulltext", "String", "fulltext"),
    ("trigram", "String", "trigram"),
    ("regexp", "String", "trigram"),
    ("ngram", "String", "ngram"),
    ("year", "DateTime", "year"),
    ("month", "DateTime", "month"),
    ("day", "DateTime", "day"),
    ("hour", "DateTime", "hour"),
    ("point", "Point", "geo"),
    ("polygon", "Polygon", "geo"),
    ("multiPolygon", "MultiPolygon", "geo"),
    ("hnsw", "Float", "hnsw"),
];

/// Looks up a search argument by name.
pub(crate) fn supported_search(name: &str) -> Option<(&'static str, &'static str)> {
    SUPPORTED_SEARCHES
        .iter()
        .find(|(arg, _, _)| *arg == name)
        .map(|(_, gql, dg)| (*gql, *dg))
}

/// GraphQL type to default `@search` argument.
///
/// Port of Go `defaultSearches`.
pub(crate) const DEFAULT_SEARCHES: &[(&str, &str)] = &[
    ("Boolean", "bool"),
    ("Int", "int"),
    ("Int64", "int64"),
    ("Float", "float"),
    ("String", "term"),
    ("DateTime", "year"),
    ("Point", "point"),
    ("Polygon", "polygon"),
    ("MultiPolygon", "multiPolygon"),
];

/// Looks up the default search argument for a GraphQL type.
pub(crate) fn default_search(gql_type: &str) -> Option<&'static str> {
    DEFAULT_SEARCHES
        .iter()
        .find(|(typ, _)| *typ == gql_type)
        .map(|(_, arg)| *arg)
}

/// Filter inputs that collide and must merge. Port of Go `filtersCollisions`.
pub(crate) const FILTERS_COLLISIONS: &[(&str, &[&str])] = &[
    ("StringHashFilter", &["StringExactFilter"]),
    ("StringExactFilter", &["StringHashFilter"]),
];

/// Looks up collision partners of a filter input.
pub(crate) fn filters_collisions(name: &str) -> &'static [&'static str] {
    FILTERS_COLLISIONS
        .iter()
        .find(|(filter, _)| *filter == name)
        .map_or(&[], |(_, others)| *others)
}

/// GraphQL types usable in `orderasc`/`orderdesc`. Port of Go `orderable`.
pub(crate) const ORDERABLE: &[&str] = &["Int", "Int64", "Float", "String", "DateTime"];

/// Whether the type supports ordering.
pub(crate) fn is_orderable(name: &str) -> bool {
    ORDERABLE.contains(&name)
}

/// GraphQL types with a well-defined addition. Port of Go `summable`.
pub(crate) const SUMMABLE: &[&str] = &["Int", "Int64", "Float"];

/// Whether the type supports `sum` aggregation.
pub(crate) fn is_summable(name: &str) -> bool {
    SUMMABLE.contains(&name)
}

/// Search arguments allowed on enum fields. Port of Go `enumDirectives`.
pub(crate) const ENUM_DIRECTIVES: &[&str] = &["trigram", "hash", "exact", "regexp", "ngram"];

/// Whether the search argument applies to enums.
pub(crate) fn is_enum_directive(name: &str) -> bool {
    ENUM_DIRECTIVES.contains(&name)
}

/// `DataStreamDB` index to GraphQL filter input. Port of Go `builtInFilters`.
pub(crate) const BUILT_IN_FILTERS: &[(&str, &str)] = &[
    ("bool", "Boolean"),
    ("int", "IntFilter"),
    ("int64", "Int64Filter"),
    ("float", "FloatFilter"),
    ("year", "DateTimeFilter"),
    ("month", "DateTimeFilter"),
    ("day", "DateTimeFilter"),
    ("hour", "DateTimeFilter"),
    ("term", "StringTermFilter"),
    ("trigram", "StringRegExpFilter"),
    ("regexp", "StringRegExpFilter"),
    ("ngram", "StringNgramFilter"),
    ("fulltext", "StringFullTextFilter"),
    ("exact", "StringExactFilter"),
    ("hash", "StringHashFilter"),
    ("point", "PointGeoFilter"),
    ("polygon", "PolygonGeoFilter"),
    ("multiPolygon", "PolygonGeoFilter"),
    ("hnsw", "HNSWSearchFilter"),
];

/// Looks up the filter input for a `DataStreamDB` index name.
pub(crate) fn built_in_filter(index: &str) -> Option<&'static str> {
    BUILT_IN_FILTERS
        .iter()
        .find(|(name, _)| *name == index)
        .map(|(_, filter)| *filter)
}

/// GraphQL built-in type to `DataStreamDB` scalar. Port of Go `inbuiltTypeToDgraph`.
pub(crate) const INBUILT_TYPE_TO_DATASTREAMDB: &[(&str, &str)] = &[
    ("ID", "uid"),
    ("Boolean", "bool"),
    ("Int", "int"),
    ("Int64", "int"),
    ("Float", "float"),
    ("String", "string"),
    ("DateTime", "dateTime"),
    ("Password", "password"),
    ("Point", "geo"),
    ("Polygon", "geo"),
    ("MultiPolygon", "geo"),
];

/// Looks up the `DataStreamDB` scalar for a GraphQL built-in type.
pub(crate) fn inbuilt_type_to_datastreamdb(name: &str) -> Option<&'static str> {
    INBUILT_TYPE_TO_DATASTREAMDB
        .iter()
        .find(|(gql, _)| *gql == name)
        .map(|(_, dg)| *dg)
}

/// `by` argument of similarity queries.
pub(crate) const SIMILAR_BY_ARG_NAME: &str = "by";
/// `topK` argument of similarity queries.
pub(crate) const SIMILAR_TOP_K_ARG_NAME: &str = "topK";
/// `vector` argument of similarity queries.
pub(crate) const SIMILAR_VECTOR_ARG_NAME: &str = "vector";
/// `ef` argument of similarity queries.
pub(crate) const SIMILAR_EF_ARG_NAME: &str = "ef";
/// `distance_threshold` argument of similarity queries.
pub(crate) const SIMILAR_DISTANCE_THRESHOLD_ARG_NAME: &str = "distance_threshold";
/// Suffix of the per-type embedding enum.
pub(crate) const EMBEDDING_ENUM_SUFFIX: &str = "Embedding";
/// Prefix of similarity queries.
pub(crate) const SIMILAR_QUERY_PREFIX: &str = "querySimilar";
/// Suffix of by-ID similarity queries.
pub(crate) const SIMILAR_BY_ID_QUERY_SUFFIX: &str = "ById";
/// Suffix of by-embedding similarity queries.
pub(crate) const SIMILAR_BY_EMBEDDING_QUERY_SUFFIX: &str = "ByEmbedding";
/// Distance field added to similarity results.
pub(crate) const SIMILAR_QUERY_DISTANCE_FIELD_NAME: &str = "vector_distance";
