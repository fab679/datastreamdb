# Relationships and facets

Graphs earn their keep at the edges. DataStreamDB stores relationships as
**uid predicates** and describes each edge with optional **facets**.

## Nodes, not lists of strings

A relationship is a node, not a value copied into both records. The difference
shows up as soon as the relationship has its own attributes or is shared:

```text
type Movie {
    name
    genre
}
type Genre {
    name
}
genre: [uid] @reverse .

_:matrix <name> "The Matrix" .
_:matrix <datastreamdb.type> "Movie" .
_:scifi <name> "Sci-Fi" .
_:scifi <datastreamdb.type> "Genre" .
_:matrix <genre> _:scifi .
```

`genre` is a `[uid]` predicate. A genre can belong to many movies, be queried on
its own, and be traversed in reverse. A string property (`genre: "Sci-Fi"`) would
duplicate the label, give the genre no identity, and make reverse traversal
impossible.

## Traversal

Query the edge to read the related node:

```text
{
  movie(func: eq(name, "The Matrix")) {
    name
    genre { name }
  }
}
```

Declare `@reverse` to walk the edge backwards:

```text
{
  g(func: eq(name, "Sci-Fi")) {
    name
    ~genre { name year }
  }
}
```

The reverse predicate is derived from the forward name. It is not a second
predicate you declare or write.

## Facets: properties on the edge

A facet belongs to the relationship, not to either endpoint. Cast roles and
billing order are properties of the movie→person edge:

```text
_:matrix <acted_in> _:keanu (role="Neo", billing=1) .
```

Read them with `@facets(...)`:

```text
{
  movie(func: eq(name, "The Matrix")) {
    name
    cast: acted_in @facets(role, billing) { name }
  }
}
```

Facet values appear under the alias in the response
(`cast|role`, `cast|billing`). Without an alias, the predicate name is the
prefix (`acted_in|role`).

Filter on a facet to select edges rather than nodes:

```text
{
  movie(func: eq(name, "The Matrix")) {
    acted_in @facets(gt(billing, 1)) { name }
  }
}
```

This returns only the cast members whose billing on this edge is greater than
one. The person node itself is unchanged; the filter applies to the edge.

Facets are a DQL feature. GraphQL in 0.2.1 does not expose facet syntax — model
relationship properties as nodes or as separate predicates when the GraphQL
layer needs them.

## GraphQL relationships

In SDL, an object-typed field is a uid relationship. `@hasInverse` keeps both
sides consistent:

```graphql
type Movie {
  id: ID!
  title: String @search(by: [term])
  genres: [Genre] @hasInverse(field: movies)
}
type Genre {
  id: ID!
  name: String @id @search(by: [hash])
  movies: [Movie]
}
```

A nested mutation creates the related nodes and the edges in one operation:

```graphql
mutation {
  addMovie(input: [{
    title: "The Matrix",
    genres: [{name: "Sci-Fi"}]
  }]) {
    movie { title genres { name } }
    numUids
  }
}
```

## Choosing the shape

| Question | Model |
|---|---|
| Is the target a thing with its own attributes or queries? | A node and a uid predicate |
| Is the value only meaningful on this edge? | A facet (DQL) or a node if GraphQL needs it |
| Is the relationship one-to-many? | A list predicate (`[uid]`) on the side that points to many |
| Do you need reverse traversal? | Declare `@reverse` |

Next: [timestamps and transactions](transactions.md).
