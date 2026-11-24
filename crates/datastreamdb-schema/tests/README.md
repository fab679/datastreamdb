# datastreamdb-schema tests

Ported Go test tables (integration tests).

- `schema_tests.rs` - all of `parse_test.go` (`TestMain`'s badger backend
  waits for M5; `TestCompareSchemaUpdates` lives as a unit test in
  `src/state.rs`, like Go's in-package test).
