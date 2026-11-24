# TODO - datastreamdb-query

Port checklist for `query`. Check items only with tests proving Go parity.

- [x] M0: crate scaffolded, compiles, docs render.
- [x] M3: execution graph (`query/query.go` core): `SubGraph`/`Params`,
  `to_sub_graph`/`new_graph`/`tree_copy`/`filter_copy`, request pipeline.
- [x] M3: executor (`process_graph`, expand, pagination, order, facets).
- [x] M3: variables (`fill_vars`, ineq funcs, `populate_*`, post-aggregation,
  math transforms) with `ShardedMap` value maps.
- [x] M3: aggregations (`aggregator.go`: min/max/sum/avg), math ops
  (`math.go`, Go-exact errors), group-by (`groupby.go`).
- [x] M3: recurse (`query/recurse.go`) and shortest path (`shortest_path.go`,
  Dijkstra + k-shortest) expansion.
- [x] M3: local sort (`worker/sort.go` single-group half).
- [x] M3: JSON output (`outputnode.go`: normalize, facets, counts, `checkpwd`)
  and RDF output.
- [x] M3: mutations (`mutation.go` + `ValidateAndConvert`): blank nodes,
  reverse/count/index maintenance, store-backed UID allocator.
- [x] M3: e2e suite (13 tests) + math table suite (10 groups).
- [ ] M4: trigram candidate extraction (via `datastreamdb-task`).
- [ ] M4: parallel row fan-out (Go uses goroutines; we run sequentially).
- [ ] M4: upsert execution (`query/upsert.go`).
- [ ] M-vector: `similar_to` search.
- [ ] M-graphql: `outputnode_graphql.go` response shape.
- [x] M-doc: rustdoc + examples; update `docs/PROJECT_MAP.md` status.
- [x] M-done: `cargo test -p datastreamdb-query`, clippy `-D warnings`, fmt clean.

## Session notes (M5 distribution support)
- [x] M5: shared blank-node maps, public edge staging
  (`validate_and_convert`/`stage_edge`), N-Quad conversion/validation for
  the coordinator; blocking waits run off runtime threads.
