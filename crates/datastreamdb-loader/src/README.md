# datastreamdb-loader/src

Implementation of `datastreamdb-loader`.

- `lib.rs` - crate root.
- `assign.rs` - blank-node to UID assignment seeded above stored UIDs.
- `live.rs` - live loader: stream files into a live engine in batches.
- `bulk.rs` - bulk loader: files into a fresh engine + schema inference.
