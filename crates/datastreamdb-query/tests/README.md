# datastreamdb-query tests

Query executor tests (in-memory engine, no cluster).

- `e2e.rs` - 13 end-to-end tests: schema + mutation ingest, root functions
  (`has`/`eq`/`uid`/inequality), filters, cascade, pagination, ordering,
  aggregations, group-by, recurse, shortest path, facets, JSON shape,
  delete mutations, read-your-writes.
- `math_tests.rs` - 10 Go `math_test.go` table groups: binary int/float
  ops, big-float tables, unary functions, boolean comparisons, ternary
  select, error messages (Go-exact strings), vector smoke.

Run: `cargo test -p datastreamdb-query`.
