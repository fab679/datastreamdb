# bindings/c

Reference C consumer of the stable ABI in [`datastreamdb.h`](../../crates/datastreamdb-ffi/datastreamdb.h).
Any language with a C FFI (Go, Zig, Swift, …) follows this same shape.

The ABI takes JSON envelopes. Graph applications use the `dql_alter`,
`dql_mutate`, `dql_query`, `set_graphql_schema`, `graphql`, and subscription
operations; the raw key/value ops remain available for low-level storage.

Guide: <https://fab679.github.io/datastreamdb/embedding/c/>

## Build and run the smoke test

```sh
cargo build -p datastreamdb-ffi
gcc bindings/c/smoke.c -o /tmp/datastreamdb-c-smoke \
  -I crates/datastreamdb-ffi -L target/debug -ldatastreamdb_ffi -lpthread -ldl -lm
LD_LIBRARY_PATH=$PWD/target/debug /tmp/datastreamdb-c-smoke
```

`smoke.c` opens two isolated instances, runs DQL alter/mutate/query envelopes,
checks that the instances do not share data, and verifies error handling.
