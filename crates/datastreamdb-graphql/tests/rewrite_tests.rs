//! Query rewrite tests: GraphQL operations to DQL text.

use std::collections::HashMap;

use datastreamdb_graphql::auth_rules::AuthVars;
use datastreamdb_graphql::dql_render::as_string;
use datastreamdb_graphql::op::BoundSchema;
use datastreamdb_graphql::rewrite_query::rewrite_query;
use datastreamdb_graphql::schemagen::new_handler;

const SCHEMA: &str = r"
type Post {
    id: ID!
    title: String @search(by: [term])
    numLikes: Int @search
    isPublished: Boolean @search
    author: Author @hasInverse(field: posts)
}
type Author {
    id: ID!
    name: String @id @search(by: [hash])
    posts: [Post]
}
";

fn bound() -> BoundSchema {
    let handler = new_handler(SCHEMA, false).expect("valid schema");
    BoundSchema::bind(&handler).expect("bound")
}

fn rewrite(schema: &BoundSchema, query: &str) -> String {
    let op = schema
        .parse_operation(query, None, &HashMap::new())
        .expect("operation");
    let root = op.roots.first().expect("root");
    let rewritten = rewrite_query(schema, root, &AuthVars::new()).expect("rewrite");
    as_string(&rewritten.queries)
}

#[test]
fn rewrites_filter_query() {
    let schema = bound();
    let dql = rewrite(
        &schema,
        "{ queryPost(filter: {title: {anyofterms: \"GraphQL\"}}, first: 10) { id title } }",
    );
    assert!(dql.contains("queryPost(func: type(Post),"), "{dql}");
    assert!(dql.contains("anyofterms(Post.title, \"GraphQL\")"), "{dql}");
    assert!(dql.contains("first: 10"), "{dql}");
    assert!(dql.contains("Post.title : Post.title"), "{dql}");
}

#[test]
fn rewrites_get_query() {
    let schema = bound();
    let dql = rewrite(&schema, "{ getPost(id: \"0x123\") { title numLikes } }");
    assert!(dql.contains("getPost(func: uid(0x123))"), "{dql}");
    assert!(dql.contains("type(Post)"), "{dql}");
    assert!(dql.contains("Post.numLikes : Post.numLikes"), "{dql}");
    assert!(dql.contains("datastreamdb.uid : uid"), "{dql}");
}

#[test]
fn rewrites_xid_get_query() {
    let schema = bound();
    let dql = rewrite(&schema, "{ getAuthor(name: \"Ann\") { posts { title } } }");
    assert!(dql.contains("eq(Author.name, \"Ann\")"), "{dql}");
    assert!(dql.contains("Author.posts : Author.posts"), "{dql}");
}

#[test]
fn rewrites_boolean_and_nested_filters() {
    let schema = bound();
    let dql = rewrite(
        &schema,
        "{ queryPost(filter: {isPublished: true, not: {numLikes: {lt: 5}}}) { id } }",
    );
    assert!(dql.contains("eq(Post.isPublished, true)"), "{dql}");
    assert!(dql.contains("NOT (lt(Post.numLikes, 5))"), "{dql}");
}

#[test]
fn rewrites_order_and_pagination() {
    let schema = bound();
    let dql = rewrite(
        &schema,
        "{ queryPost(order: {asc: numLikes}, offset: 3) { id } }",
    );
    assert!(dql.contains("orderasc: Post.numLikes"), "{dql}");
    assert!(dql.contains("offset: 3"), "{dql}");
}

#[test]
fn rewrites_aggregate_query() {
    let schema = bound();
    let dql = rewrite(&schema, "{ aggregatePost { count numLikesMax } }");
    assert!(dql.contains("countVar"), "{dql}");
    assert!(dql.contains("max(val(countVar))"), "{dql}");
    assert!(dql.contains("numLikesVar as Post.numLikes"), "{dql}");
}

#[test]
fn rewrites_nested_aggregate_field() {
    let schema = bound();
    let dql = rewrite(&schema, "{ queryAuthor { postsAggregate { count } } }");
    assert!(dql.contains("count(Author.posts)"), "{dql}");
}

#[test]
fn applies_auth_static_negative() {
    let handler = new_handler(
        "type T @auth(query: {rule: \"{$USER: {eq: \\\"admin\\\"}}\"}) {\n id: ID!\n f: String\n}",
        false,
    )
    .expect("valid schema");
    let schema = BoundSchema::bind(&handler).expect("bound");
    let op = schema
        .parse_operation("{ queryT { f } }", None, &HashMap::new())
        .expect("operation");
    let root = op.roots.first().expect("root");
    let rewritten = rewrite_query(&schema, root, &AuthVars::new()).expect("rewrite");
    let dql = as_string(&rewritten.queries);
    assert!(dql.contains("queryT()"), "{dql}");

    let mut vars = AuthVars::new();
    vars.insert("USER".to_owned(), serde_json::json!("admin"));
    let rewritten = rewrite_query(&schema, root, &vars).expect("rewrite");
    let dql = as_string(&rewritten.queries);
    assert!(dql.contains("queryT(func: uid(TRoot))"), "{dql}");
    assert!(dql.contains("TRoot as var(func: uid(T_1))"), "{dql}");
}
