# datastreamdb-task tests

Task processor tests (in-memory engine, no cluster).

- `smoke.rs` - `has` root scans, `eq`/`gt` index roots, UID value/uid
  fetch, `eq` filters, `do_count`, `regexp` filters.
