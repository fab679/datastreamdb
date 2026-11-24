//! Schema handler tests: DQL generation, completion, and validation errors.

use datastreamdb_graphql::schemagen::new_handler;

const BASIC_SCHEMA: &str = r"
type Post {
    id: ID!
    title: String @search(by: [term])
    numLikes: Int @search
    author: Author @hasInverse(field: posts)
}
type Author {
    id: ID!
    name: String @search(by: [hash])
    posts: [Post]
}
";

#[test]
fn generates_datastreamdb_schema() {
    let handler = new_handler(BASIC_SCHEMA, false).expect("valid schema");
    assert_eq!(
        handler.dg_schema(),
        "type Post {\n  Post.title\n  Post.numLikes\n  Post.author\n}\n\
         Post.title: string @index(term) .\n\
         Post.numLikes: int @index(int) .\n\
         Post.author: uid .\n\
         type Author {\n  Author.name\n  Author.posts\n}\n\
         Author.name: string @index(hash) .\n\
         Author.posts: [uid] .\n"
    );
}

#[test]
fn completes_queries_and_mutations() {
    let handler = new_handler(BASIC_SCHEMA, false).expect("valid schema");
    let schema = handler.schema();
    let query = schema.type_def("Query").expect("Query root");
    for name in [
        "getPost",
        "queryPost",
        "aggregatePost",
        "getAuthor",
        "queryAuthor",
        "aggregateAuthor",
    ] {
        assert!(
            query.field(name).is_some(),
            "missing query {name}: {:?}",
            query.fields.iter().map(|f| &f.name).collect::<Vec<_>>()
        );
    }
    let mutation = schema.type_def("Mutation").expect("Mutation root");
    for name in [
        "addPost",
        "updatePost",
        "deletePost",
        "addAuthor",
        "updateAuthor",
        "deleteAuthor",
    ] {
        assert!(mutation.field(name).is_some(), "missing mutation {name}");
    }
    for name in [
        "PostFilter",
        "PostRef",
        "PostPatch",
        "AddPostInput",
        "UpdatePostInput",
        "PostAggregateResult",
        "PostOrder",
        "PostOrderable",
        "PostHasFilter",
    ] {
        assert!(schema.type_def(name).is_some(), "missing type {name}");
    }
}

#[test]
fn backfills_inverse_directives() {
    let handler = new_handler(BASIC_SCHEMA, false).expect("valid schema");
    let posts = &handler
        .schema()
        .type_def("Author")
        .expect("Author")
        .field("posts")
        .expect("posts")
        .directives;
    let inverse = posts
        .iter()
        .find(|dir| dir.name == "hasInverse")
        .expect("backfilled @hasInverse");
    assert_eq!(
        inverse
            .arg("field")
            .map(datastreamdb_graphql::model::Value::raw),
        Some("author".to_owned())
    );
}

#[test]
fn prints_completed_schema_sections() {
    let handler = new_handler(BASIC_SCHEMA, false).expect("valid schema");
    let gql = handler.gql_schema();
    for section in [
        "# Input Schema",
        "# Extended Definitions",
        "# Generated Types",
        "# Generated Enums",
        "# Generated Inputs",
        "# Generated Query",
        "# Generated Mutations",
    ] {
        assert!(gql.contains(section), "missing {section}");
    }
    assert!(gql.contains("type Post {"), "{gql}");
    assert!(gql.contains("queryPost"), "{gql}");
}

#[test]
fn rejects_empty_schema() {
    let errs = new_handler("", false).expect_err("must fail");
    assert_eq!(errs.messages(), vec!["No schema specified".to_owned()]);
}

#[test]
fn rejects_scalar_definitions() {
    let errs = new_handler("scalar Odd\ntype T { f: String }", false).expect_err("must fail");
    assert!(
        errs.messages()
            .iter()
            .any(|msg| msg.contains("You can't add scalar definitions")),
        "{errs}"
    );
}

#[test]
fn rejects_bad_search_argument() {
    let errs =
        new_handler("type T {\n f: String @search(by: [bogus])\n}", false).expect_err("must fail");
    assert!(
        errs.messages()
            .iter()
            .any(|msg| msg.contains("the argument to @search bogus isn't valid")),
        "{errs}"
    );
}

#[test]
fn rejects_colliding_search_arguments() {
    let errs = new_handler("type T {\n f: String @search(by: [hash, exact])\n}", false)
        .expect_err("must fail");
    assert!(
        errs.messages()
            .iter()
            .any(|msg| msg.contains("can't be used together")),
        "{errs}"
    );
}

#[test]
fn rejects_missing_inverse_field() {
    let errs = new_handler(
        "type T {\n f: U @hasInverse(field: missing)\n}\ntype U {\n g: String\n}",
        false,
    )
    .expect_err("must fail");
    assert!(
        errs.messages()
            .iter()
            .any(|msg| msg.contains("inverse field missing doesn't exist")),
        "{errs}"
    );
}

#[test]
fn rejects_reserved_type_names() {
    let errs = new_handler("type IntFilter {\n f: String\n}", false).expect_err("must fail");
    assert!(
        errs.messages()
            .iter()
            .any(|msg| msg.contains("is a reserved word")),
        "{errs}"
    );
}

#[test]
fn rejects_bare_query_fields() {
    let errs = new_handler(
        "type Query {\n hello: String\n}\ntype T {\n f: String\n}",
        false,
    )
    .expect_err("must fail");
    assert!(
        errs.messages()
            .iter()
            .any(|msg| msg.contains("without @custom/@lambda")),
        "{errs}"
    );
}

#[test]
fn accepts_custom_dql_query() {
    let schema = "type Query {\n hello(name: String): String @custom(dql: \"query { hello }\")\n}\ntype T {\n f: String\n}";
    new_handler(schema, false).expect("custom dql query");
}

#[test]
fn rejects_id_on_bad_type() {
    let errs = new_handler("type T {\n f: Float @id\n}", false).expect_err("must fail");
    assert!(
        errs.messages()
            .iter()
            .any(|msg| msg.contains("with @id directive must be of type String, Int or Int64")),
        "{errs}"
    );
}

#[test]
fn rejects_unknown_types() {
    let errs = new_handler("type T {\n f: Nope\n}", false).expect_err("must fail");
    assert!(
        errs.messages()
            .iter()
            .any(|msg| msg.contains("Unknown type")),
        "{errs}"
    );
}

#[test]
fn handles_interfaces_and_enums() {
    let schema = "interface Node {\n id: ID!\n}\ntype T implements Node {\n id: ID!\n f: String @search\n}\nenum Mood {\n HAPPY\n SAD\n}\ntype M {\n mood: Mood @search\n}";
    let handler = new_handler(schema, false).expect("valid schema");
    assert!(handler.dg_schema().contains("type T {"));
    assert!(
        handler
            .dg_schema()
            .contains("M.mood: string @index(hash) .")
    );
}

#[test]
fn parses_meta_comments() {
    let schema = "# DataStreamDB.Secret api_key \"s3cret\"\n# DataStreamDB.Allow-Origin \"http://example.com\"\ntype T {\n f: String\n}";
    let handler = new_handler(schema, false).expect("valid schema");
    assert_eq!(
        handler
            .meta_info()
            .secrets
            .get("api_key")
            .map(String::as_str),
        Some("s3cret")
    );
    assert!(
        handler
            .meta_info()
            .allowed_cors_origins()
            .contains("http://example.com")
    );
}
