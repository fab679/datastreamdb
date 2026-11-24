//! Mutation rewrite tests: existence queries and RDF fragments.

use std::collections::HashMap;

use datastreamdb_graphql::auth_rules::AuthVars;
use datastreamdb_graphql::dql_render::as_string;
use datastreamdb_graphql::op::BoundSchema;
use datastreamdb_graphql::rewrite_mutation::{
    MutationRewriter, add_rewrite, add_rewrite_queries, delete_rewrite, update_rewrite,
    update_rewrite_queries,
};
use datastreamdb_graphql::schemagen::new_handler;

const SCHEMA: &str = r"
type Post {
    id: ID!
    title: String @search(by: [term])
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

fn root(schema: &BoundSchema, mutation: &str) -> datastreamdb_graphql::op::BoundField {
    schema
        .parse_operation(mutation, None, &HashMap::new())
        .expect("operation")
        .roots
        .into_iter()
        .next()
        .expect("root")
}

#[test]
fn rewrites_add_existence_queries() {
    let schema = bound();
    let field = root(
        &schema,
        r#"mutation { addPost(input: [{title: "Hi", author: {name: "Ann"}}]) { post { id } } }"#,
    );
    let mut rewriter = MutationRewriter::new();
    let (queries, types, errs) = add_rewrite_queries(&schema, &field, &mut rewriter);
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(types, vec!["Author".to_owned()]);
    let dql = as_string(&queries);
    assert!(dql.contains("eq(Author.name, \"Ann\")"), "{dql}");
}

#[test]
fn rewrites_add_fragment() {
    let schema = bound();
    let field = root(
        &schema,
        r#"mutation { addPost(input: [{title: "Hi"}]) { post { id title } } }"#,
    );
    let mut rewriter = MutationRewriter::new();
    let (upserts, errs) = add_rewrite(
        &schema,
        &field,
        &mut rewriter,
        &mut HashMap::new(),
        &AuthVars::new(),
    );
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(upserts.len(), 1);
    let set = upserts[0].set_nquads.join("\n");
    assert!(set.contains("<Post.title> \"Hi\""), "{set}");
    assert!(set.contains("datastreamdb.type"), "{set}");
}

#[test]
fn links_existing_xid() {
    let schema = bound();
    let field = root(
        &schema,
        r#"mutation { addPost(input: [{title: "Hi", author: {name: "Ann"}}]) { post { id } } }"#,
    );
    let mut rewriter = MutationRewriter::new();
    let _ = add_rewrite_queries(&schema, &field, &mut rewriter);
    let mut existence = HashMap::new();
    existence.insert("Author_1".to_owned(), "0xabc".to_owned());
    let (upserts, errs) = add_rewrite(
        &schema,
        &field,
        &mut rewriter,
        &mut existence,
        &AuthVars::new(),
    );
    assert!(errs.is_empty(), "{errs:?}");
    let set = upserts[0].set_nquads.join("\n");
    assert!(set.contains("0xabc"), "{set}");
}

#[test]
fn rewrites_update_set_remove() {
    let schema = bound();
    let field = root(
        &schema,
        r#"mutation { updatePost(input: {filter: {title: {anyofterms: "Hi"}}, set: {title: "Yo"}, remove: {title: "Yo"}}) { post { id } } }"#,
    );
    let mut rewriter = MutationRewriter::new();
    let (upserts, errs) = update_rewrite(
        &schema,
        &field,
        &mut rewriter,
        &mut HashMap::new(),
        &AuthVars::new(),
    );
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(upserts.len(), 1);
    assert!(
        upserts[0]
            .conditions
            .iter()
            .any(|cond| cond.contains("len(x)"))
    );
    let set = upserts[0].set_nquads.join("\n");
    assert!(set.contains("<Post.title> \"Yo\""), "{set}");
    let del = upserts[0].del_nquads.join("\n");
    assert!(del.contains("<Post.title>"), "{del}");
}

#[test]
fn rewrites_update_existence_queries() {
    let schema = bound();
    let field = root(
        &schema,
        r#"mutation { updateAuthor(input: {filter: {}, set: {posts: [{id: "0x5"}]}}) { author { id } } }"#,
    );
    let mut rewriter = MutationRewriter::new();
    let (queries, _, errs) = update_rewrite_queries(&schema, &field, &mut rewriter);
    assert!(errs.is_empty(), "{errs:?}");
    let dql = as_string(&queries);
    assert!(dql.contains("uid(0x5)"), "{dql}");
}

#[test]
fn rewrites_delete() {
    let schema = bound();
    let field = root(
        &schema,
        r#"mutation { deletePost(filter: {title: {anyofterms: "Hi"}}) { msg } }"#,
    );
    let mut rewriter = MutationRewriter::new();
    let upserts = delete_rewrite(&schema, &field, &mut rewriter, &AuthVars::new()).expect("delete");
    assert_eq!(upserts.len(), 1);
    let dql = as_string(&upserts[0].queries);
    assert!(dql.contains("anyofterms(Post.title, \"Hi\")"), "{dql}");
    let del = upserts[0].del_nquads.join("\n");
    assert!(del.contains("uid(x)"), "{del}");
}
