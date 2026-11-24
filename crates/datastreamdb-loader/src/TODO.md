# TODO - datastreamdb-loader/src

- [x] `assign.rs`: UID seeder + batch XID map with tests.
- [x] `live.rs`: file discovery, RDF line batching, JSON docs, commits.
- [x] `bulk.rs`: fresh-engine load, schema inference, UID lease handoff.
- [x] No global mutable state (multi-instance embedding requirement).
- [ ] Remote gRPC live client; sharded bulk map/shuffle/reduce.
