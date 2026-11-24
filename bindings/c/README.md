# bindings/c

Reference C consumer of the stable ABI in [`datastreamdb.h`](../../crates/datastreamdb-ffi/datastreamdb.h).
Any language with a C FFI (Go, Zig, Swift, …) follows this same shape.

## Build and run the smoke test

```sh
cargo build -p datastreamdb-ffi
gcc bindings/c/smoke.c -o /tmp/datastreamdb-c-smoke \
  -I crates/datastreamdb-ffi -L target/debug -ldatastreamdb_ffi -lpthread -ldl -lm
LD_LIBRARY_PATH=$PWD/target/debug /tmp/datastreamdb-c-smoke
```
