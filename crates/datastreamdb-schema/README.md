# datastreamdb-schema

Schema model, parser, and live schema state.

- Go source: `dgraph-main/schema` (reference clone at repo root; read-only).
- Status: ported (Milestone 2). See [TODO.md](TODO.md) and [docs/PROJECT_MAP.md](../../docs/PROJECT_MAP.md).
- Embedding: this crate must stay usable from `datastreamdb-core` inside one process with many isolated instances.

## Layout

- `src/` - implementation (see [src/README.md](src/README.md)).
- `tests/` - ported Go test tables.

## API map (Go -> Rust)

| Go (`schema`) | Rust (`datastreamdb_schema`) |
|---|---|
| `pb.SchemaUpdate` etc. | `datastreamdb_proto` prost types, re-exported |
| `Parse`, `ParseWithNamespace` | `parse`, `parse_with_namespace` |
| `ParseBytes` | `SchemaState::parse_bytes` (drops unused `gid`) |
| global `State()` | `SchemaState::new` (instance per embedder) |
| `context.Context` read/write | `write: bool` parameter |
| `InitialSchema(ns)` | `initial_schema(ns, acl_enabled)` |
| `InitialTypes(ns)` | `initial_types(ns, acl_enabled)` |
| `CheckAndModifyPreDefPredicate` | `check_and_modify_pre_def_predicate` |
| `IsPreDefTypeChanged` | `is_pre_def_type_changed` |

## Recorded deltas

- No global state: embedding needs many isolated schemas per process.
- Disk sync (`Init`, `Load`, `Delete` with timestamps) waits for M5.
- `u64::MAX` preserves namespaces, like Go's `math.MaxUint64`.
- Go's `"Typed oes not match the expected"` typo is preserved verbatim.
