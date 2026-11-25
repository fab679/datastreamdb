# Instances

An **instance** is one isolated database: its own storage backend, its own
data, its own handle. Nothing is shared implicitly between instances.

## Two backends

| Backend | Created with | Lifetime |
|---|---|---|
| In-memory | `InstanceConfig::memory()` / `new Instance('{}')` | Lost when the handle closes |
| Directory | `{"data_dir": "..."}` config | Persisted by the fjall backend |

A directory instance takes an exclusive `LOCK` file. One process may open it;
a second opener fails instead of corrupting data. In-memory instances have no
such restriction — open as many as the process needs.

=== "Rust"

    ```rust
    use datastreamdb_core::{Instance, InstanceConfig};

    let scratch = Instance::open(InstanceConfig::memory())?;
    let durable = Instance::open(InstanceConfig::from_json(
        r#"{"data_dir": "./data/app", "tenant": "app"}"#,
    )?)?;
    ```

=== "TypeScript"

    ```js
    import { Instance } from '@datastreamdb/core'

    const scratch = new Instance('{}')
    const durable = new Instance(JSON.stringify({ data_dir: './data/app' }))
    ```

=== "Python"

    ```python
    from datastreamdb import Instance

    scratch = Instance("{}")
    durable = Instance('{"data_dir": "./data/app"}')
    ```

## Isolation

Each instance has separate storage, separate schema history, and separate
timestamps. Two instances opened from the same configuration still see only
their own writes:

```js
const a = new Instance('{}')
const b = new Instance('{}')

a.dqlMutate('{ set { _:x <name> "Ada" . } }')
console.log(a.dqlQuery('{ q(func: has(name)) { name } }')) // Ada
console.log(b.dqlQuery('{ q(func: has(name)) { name } }')) // empty
```

## Graph API

DQL and GraphQL run against the instance's posting partition, with their own
schema history, GraphQL SDL, and timestamp metadata. Reopening a directory
instance restores the data, the schema, and the timestamps together.

The low-level key/value methods (`put`, `get`, `scan`, `batch`) are a separate
storage API for hosts that need raw ordered storage. They are not part of the
graph model and are documented in the
[low-level key/value appendix](../embedding/key-value.md).

## Embedded lifecycle

The instance owns the store. Drop the last handle to close it; for directory
instances that releases the `LOCK`. GraphQL subscriptions hold a reference to
the engine, so close subscriptions before expecting the directory to be
released.

Continue with [predicates and schema](schema.md), or see the language-specific
guides: [Rust](../embedding/rust.md), [TypeScript](../embedding/typescript.md),
[Python](../embedding/python.md), [C](../embedding/c.md).
