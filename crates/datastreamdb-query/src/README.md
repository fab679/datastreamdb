# datastreamdb-query/src

Implementation of `datastreamdb-query`. Public API lives in `lib.rs`; one module per Go source file being ported.

- `lib.rs` - crate root, re-exports (`QueryEngine`, `QueryError`, `process_graph`, `SubGraph`).
- `errors.rs` - `QueryError`/`Result` (`thiserror`).
- `subgraph.rs` - `SubGraph`/`Params` execution graph, `to_sub_graph`/`new_graph`/`tree_copy`/`filter_copy`.
- `process.rs` - `ExecCtx`, `process_graph`, expand, pagination, ordering, facet fetch.
- `vars.rs` - variable fill/ineq/populate/aggregate/transform entry points.
- `var_levels.rs` - math-level transforms split from `vars.rs` (level aggregation, path transforms).
- `aggregator.rs` - min/max/sum/avg aggregators plus math/compare helpers.
- `math.rs` - math-tree evaluation (`eval_math_tree`, `process_math`).
- `groupby.rs` - `groupby.go` port (`GroupResults`, `process_group_by`).
- `recurse.rs` - recurse expansion (`recurse_query`).
- `shortest.rs` - Dijkstra + k-shortest-paths expansion.
- `sort.rs` - local single-group sort.
- `output.rs` - `Encoder`/`JsonNode` JSON tree builder (split from render methods).
- `output_render.rs` - `SubGraph` render methods, `process_node_uids`, `to_json`.
- `rdf.rs` - RDF/N-Quad output.
- `mutation.rs` - `to_directed_edges`, `apply_mutations`, `UidAllocator`.
- `request.rs` - top-level request pipeline (`process_query`, latency, blank-node maps).
- `engine.rs` - `QueryEngine::{open, query, query_json, mutate}`.

No file exceeds ~1000 lines; `output.rs`/`vars.rs` were split early (`output_render.rs`, `var_levels.rs`).
