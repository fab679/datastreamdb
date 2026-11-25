# Shortest path

DQL finds a shortest route between two nodes with a `shortest` root. The root
names the two endpoints and one level of predicates to traverse.

```text
{
  shortest(from: 0x2, to: 0x4) {
    friend
  }
}
```

`from` and `to` take UIDs or `uid(...)` variables. The response contains the
route under `_path_`:

```json
{
  "shortest": [],
  "_path_": [{
    "uid": "0x2",
    "_weight_": 2.0,
    "friend": { "uid": "0x3", "friend": { "uid": "0x4" } }
  }]
}
```

The `_weight_` is the route weight; with no edge weights it counts the edges
traversed.

## Bind the route to a variable

Use an alias and a second block to fetch node properties along the path:

```text
{
  path as shortest(from: 0x2, to: 0x4) {
    friend
  }
  path(func: uid(path)) {
    name
  }
}
```

## Options

| Argument | Meaning |
|---|---|
| `numpaths` | Number of loopless routes to return (k-shortest paths). |
| `minweight` | Minimum route weight to accept. |
| `maxweight` | Maximum route weight to accept. |
| `maxfrontiersize` | Frontier cap per expansion step. |

```text
{
  shortest(from: 0x2, to: 0x4, numpaths: 3, maxweight: 6) {
    friend
  }
}
```

## Rules

- `from` and `to` are required and must be UIDs or `uid(var)`.
- Put every traversed predicate in the single `shortest` block; `expand()` is
  rejected inside it.
- The search stops at the same 1,000,000-edge query budget as `@recurse`.
- Embedded 0.2.1 has no query variables, so use literal UIDs there. Standalone
  accepts `uid(var)` endpoints from the variables header.
- A missing route returns an empty `_path_`, not an error.

The implementation is Dijkstra for one route and a loopless k-shortest-paths
expansion when `numpaths > 1`.
