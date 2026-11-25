//! Executable versions of the embedded tutorials.

#![cfg(feature = "query")]

use datastreamdb_core::{Instance, InstanceConfig};

fn memory() -> std::sync::Arc<Instance> {
    Instance::open(InstanceConfig::memory()).expect("open")
}

#[test]
fn movies_dql_tutorial() {
    let db = memory();
    db.dql_alter(
        r"
name: string @index(exact, term) .
year: int @index(int) .
rating: float @index(float) .
budget: int .
directed: [uid] @reverse .
acted_in: [uid] @reverse .
genre: [uid] @reverse .
type Movie {
 name
 year
 rating
 budget
 directed
 acted_in
 genre
}
type Person {
 name
 directed
 acted_in
}
type Genre {
 name
}
",
    )
    .expect("schema");

    db.dql_mutate(
        r#"
{
 set {
  _:matrix <name> "The Matrix" .
  _:matrix <year> "1999"^^<xs:int> .
  _:matrix <rating> "8.7"^^<xs:float> .
  _:matrix <budget> "63"^^<xs:int> .
  _:matrix <datastreamdb.type> "Movie" .
  _:scifi <name> "Sci-Fi" .
  _:scifi <datastreamdb.type> "Genre" .
  _:matrix <genre> _:scifi .
  _:keanu <name> "Keanu Reeves" .
  _:keanu <datastreamdb.type> "Person" .
  _:carrie <name> "Carrie-Anne Moss" .
  _:carrie <datastreamdb.type> "Person" .
  _:lana <name> "Lana Wachowski" .
  _:lana <datastreamdb.type> "Person" .
  _:matrix <directed> _:lana .
  _:matrix <acted_in> _:keanu (role="Neo", billing=1) .
  _:matrix <acted_in> _:carrie (role="Trinity", billing=2) .
 }
}
"#,
    )
    .expect("mutate");

    let movie = db
        .dql_query(
            r#"{
 movie(func: eq(name, "The Matrix")) {
  name year rating budget
  genre { name }
  directed { name }
  cast: acted_in @facets(role, billing) { name }
 }
}"#,
        )
        .expect("query");
    let text = movie.to_string();
    assert!(text.contains("The Matrix"), "{text}");
    assert!(text.contains("Keanu Reeves"), "{text}");
    assert!(text.contains("cast|role"), "{text}");
    assert!(text.contains("Trinity"), "{text}");
    assert!(text.contains("Sci-Fi"), "{text}");

    let reverse = db
        .dql_query(r#"{ p(func: eq(name, "Keanu Reeves")) { name ~acted_in { name } } }"#)
        .expect("reverse");
    assert!(reverse.to_string().contains("The Matrix"));

    let by_genre = db
        .dql_query(r#"{ g(func: eq(name, "Sci-Fi")) { name ~genre { name year } } }"#)
        .expect("genre reverse");
    assert!(by_genre.to_string().contains("The Matrix"));

    let filtered = db
        .dql_query(
            r#"{ movie(func: eq(name, "The Matrix")) {
                acted_in @facets(gt(billing, 1)) { name }
            } }"#,
        )
        .expect("facet filter");
    let filtered = filtered.to_string();
    assert!(filtered.contains("Carrie-Anne Moss"), "{filtered}");
    assert!(!filtered.contains("Keanu Reeves"), "{filtered}");

    // Alter after data: add an index and query it.
    db.dql_alter("budget: int @index(int) .")
        .expect("alter index");
    let budget = db
        .dql_query(r"{ m(func: ge(budget, 60)) { name budget } }")
        .expect("budget query");
    assert!(budget.to_string().contains("The Matrix"));
}

#[test]
fn todo_dql_tutorial() {
    let db = memory();
    db.dql_alter(
        r"
name: string @index(exact, term) .
status: string @index(exact) .
priority: int @index(int) .
due: datetime .
estimate: int @index(int) .
belongs_to: [uid] @reverse .
tagged: [uid] @reverse .
assigned_to: [uid] @reverse .
type Task {
 name
 status
 priority
 due
 estimate
 belongs_to
 tagged
 assigned_to
}
type Project {
 name
}
type Tag {
 name
}
type Person {
 name
}
",
    )
    .expect("schema");

    db.dql_mutate(
        r#"
{ set {
 _:proj <name> "Website launch" .
 _:proj <datastreamdb.type> "Project" .
 _:task1 <name> "Write release notes" .
 _:task1 <status> "todo" .
 _:task1 <priority> "2"^^<xs:int> .
 _:task1 <due> "2026-11-30T17:00:00Z"^^<xs:dateTime> .
 _:task1 <estimate> "3"^^<xs:int> .
 _:task1 <belongs_to> _:proj .
 _:task1 <datastreamdb.type> "Task" .
 _:task2 <name> "Deploy staging" .
 _:task2 <status> "doing" .
 _:task2 <priority> "1"^^<xs:int> .
 _:task2 <estimate> "2"^^<xs:int> .
 _:task2 <belongs_to> _:proj .
 _:task2 <datastreamdb.type> "Task" .
 _:ada <name> "Ada" .
 _:ada <datastreamdb.type> "Person" .
 _:task1 <assigned_to> _:ada (role="owner", since="2026-11-01") .
 _:task2 <assigned_to> _:ada (role="reviewer", since="2026-11-05") .
 _:urgent <name> "urgent" .
 _:urgent <datastreamdb.type> "Tag" .
 _:task1 <tagged> _:urgent .
} }
"#,
    )
    .expect("mutate");

    let tasks = db
        .dql_query(
            r"{
 tasks(func: type(Task)) {
  name status priority due estimate
  belongs_to { name }
  tagged { name }
  assigned_to @facets(role, since) { name }
 }
}",
        )
        .expect("query");
    let text = tasks.to_string();
    assert!(text.contains("Write release notes"), "{text}");
    assert!(text.contains("assigned_to|role"), "{text}");
    assert!(text.contains("2026-11-30"), "{text}");

    let filtered = db
        .dql_query(r#"{ t(func: eq(status, "doing")) { name priority } }"#)
        .expect("status filter");
    assert!(filtered.to_string().contains("Deploy staging"));

    let reverse = db
        .dql_query(r#"{ p(func: eq(name, "Website launch")) { name ~belongs_to { name } } }"#)
        .expect("reverse");
    assert!(reverse.to_string().contains("Write release notes"));

    db.dql_alter("estimate: int @index(int) .")
        .expect("alter index");
    let estimates = db
        .dql_query(r"{ t(func: ge(estimate, 3)) { name estimate } }")
        .expect("estimate query");
    assert!(estimates.to_string().contains("Write release notes"));
}

#[test]
fn orders_dql_tutorial() {
    let db = memory();
    db.dql_alter(
        r"
name: string @index(exact) .
email: string @index(exact) .
sku: string @index(exact) .
price: float @index(float) .
quantity: int @index(int) .
ordered: [uid] @reverse .
type Customer {
 name
 email
}
type Product {
 name
 sku
 price
}
type Order {
 name
 ordered
}
",
    )
    .expect("schema");

    db.dql_mutate(
        r#"
{ set {
 _:ada <name> "Ada" .
 _:ada <email> "ada@example.com" .
 _:ada <datastreamdb.type> "Customer" .
 _:laptop <name> "Laptop" .
 _:laptop <sku> "LAP-1" .
 _:laptop <price> "999.00"^^<xs:float> .
 _:laptop <datastreamdb.type> "Product" .
 _:mouse <name> "Mouse" .
 _:mouse <sku> "MOU-1" .
 _:mouse <price> "25.00"^^<xs:float> .
 _:mouse <datastreamdb.type> "Product" .
 _:order <name> "Order #1001" .
 _:order <datastreamdb.type> "Order" .
 _:ada <ordered> _:order .
 _:order <ordered> _:laptop (quantity=1, price=999.00) .
 _:order <ordered> _:mouse (quantity=2, price=25.00) .
} }
"#,
    )
    .expect("mutate");

    let orders = db
        .dql_query(
            r#"{
 c(func: eq(name, "Ada")) {
  name email
  ordered @facets(quantity, price) {
   name
   ordered @facets(quantity, price) { name sku price }
  }
 }
}"#,
        )
        .expect("query");
    let text = orders.to_string();
    assert!(text.contains("Order #1001"), "{text}");
    assert!(text.contains("ordered|quantity"), "{text}");
    assert!(text.contains("Laptop"), "{text}");

    let reverse = db
        .dql_query(r#"{ p(func: eq(sku, "LAP-1")) { name ~ordered { name } } }"#)
        .expect("reverse");
    assert!(reverse.to_string().contains("Order #1001"));
}

#[cfg(feature = "graphql")]
const MOVIE_GQL: &str = r"
type Movie {
 id: ID!
 title: String @search(by: [term])
 year: Int @search
 rating: Float @search
 director: Person @hasInverse(field: directed)
 actors: [Person] @hasInverse(field: actedIn)
 genres: [Genre] @hasInverse(field: movies)
}
type Person {
 id: ID!
 name: String @id @search(by: [hash])
 directed: [Movie]
 actedIn: [Movie]
}
type Genre {
 id: ID!
 name: String @id @search(by: [hash])
 movies: [Movie]
}
";

#[cfg(feature = "graphql")]
#[test]
fn movies_graphql_tutorial() {
    let db = memory();
    db.set_graphql_schema(MOVIE_GQL).expect("schema");
    let headers = std::collections::HashMap::new();

    let added = db
        .graphql(
            r#"mutation {
 addMovie(input: [{
  title: "The Matrix", year: 1999, rating: 8.7,
  director: {name: "Lana Wachowski"},
  actors: [{name: "Keanu Reeves"}, {name: "Carrie-Anne Moss"}],
  genres: [{name: "Sci-Fi"}]
 }]) { movie { id title year rating director { name } actors { name } genres { name } } numUids }
}"#,
            None,
            serde_json::json!({}),
            &headers,
        )
        .expect("add");
    assert_eq!(added["data"]["addMovie"]["numUids"], serde_json::json!(1));

    let queried = db
        .graphql(
            r#"{ queryMovie(filter: {title: {anyofterms: "Matrix"}}) {
 title year rating director { name } actors { name } genres { name }
} }"#,
            None,
            serde_json::json!({}),
            &headers,
        )
        .expect("query");
    let text = queried.to_string();
    assert!(text.contains("The Matrix"), "{text}");
    assert!(text.contains("Keanu Reeves"), "{text}");
    assert!(text.contains("Sci-Fi"), "{text}");

    let updated = db
        .graphql(
            r#"mutation {
 updateMovie(input: {filter: {title: {anyofterms: "Matrix"}}, set: {rating: 8.8}}) {
  numUids
 }
}"#,
            None,
            serde_json::json!({}),
            &headers,
        )
        .expect("update");
    assert_eq!(
        updated["data"]["updateMovie"]["numUids"],
        serde_json::json!(1)
    );

    let filtered = db
        .graphql(
            r"{ queryMovie(filter: {rating: {ge: 8.8}}) { title rating } }",
            None,
            serde_json::json!({}),
            &headers,
        )
        .expect("filter");
    assert!(filtered.to_string().contains("8.8"));
}

#[cfg(feature = "graphql")]
const TODO_GQL: &str = r"
type Task {
 id: ID!
 title: String @search(by: [term])
 status: String @search(by: [exact])
 priority: Int @search
 due: DateTime
 project: Project @hasInverse(field: tasks)
 assignee: Person @hasInverse(field: assigned)
}
type Project {
 id: ID!
 name: String @id @search(by: [hash])
 tasks: [Task]
}
type Person {
 id: ID!
 name: String @id @search(by: [hash])
 assigned: [Task]
}
";

#[cfg(feature = "graphql")]
#[test]
fn todo_graphql_tutorial() {
    let db = memory();
    db.set_graphql_schema(TODO_GQL).expect("schema");
    let headers = std::collections::HashMap::new();

    db.graphql(
        r#"mutation { addProject(input: [{name: "Website launch"}]) { numUids } }"#,
        None,
        serde_json::json!({}),
        &headers,
    )
    .expect("project");
    db.graphql(
        r#"mutation { addPerson(input: [{name: "Ada"}]) { numUids } }"#,
        None,
        serde_json::json!({}),
        &headers,
    )
    .expect("person");

    let added = db
        .graphql(
            r#"mutation {
 addTask(input: [{
  title: "Write release notes", status: "todo", priority: 2,
  due: "2026-11-30T17:00:00Z",
  project: {name: "Website launch"},
  assignee: {name: "Ada"}
 }]) { task { title status priority due project { name } assignee { name } } numUids }
}"#,
            None,
            serde_json::json!({}),
            &headers,
        )
        .expect("task");
    assert_eq!(added["data"]["addTask"]["numUids"], serde_json::json!(1));

    let queried = db
        .graphql(
            r#"{ queryTask(filter: {status: {eq: "todo"}}) {
 title status priority due project { name } assignee { name }
} }"#,
            None,
            serde_json::json!({}),
            &headers,
        )
        .expect("query");
    let text = queried.to_string();
    assert!(text.contains("Write release notes"), "{text}");
    assert!(text.contains("Website launch"), "{text}");

    db.graphql(
        r#"mutation {
 updateTask(input: {filter: {title: {anyofterms: "release"}}, set: {status: "doing"}}) {
  numUids
 }
}"#,
        None,
        serde_json::json!({}),
        &headers,
    )
    .expect("update");

    let done = db
        .graphql(
            r#"{ queryTask(filter: {status: {eq: "doing"}}) { title status } }"#,
            None,
            serde_json::json!({}),
            &headers,
        )
        .expect("filter");
    assert!(done.to_string().contains("doing"));
}
