# datastreamdb

DataStreamDB, ported to Rust — runnable as standalone `zero`/`alpha`
servers or embedded in-process with many isolated instances.

- **Standalone**: DQL queries, mutations, upserts, schema alters,
  GraphQL data layer, ACL, backups, multi-tenancy.
- **Embedded**: open any number of isolated instances from Rust,
  TypeScript, Python, or C with no servers and no ports.

!!! note "0.1.0 scope"
    The embedded instance currently speaks key-value operations
    (`put`/`get`/`delete`/`scan`/`batch`). Full DQL through the
    embedded handle is the next milestone; DQL today runs against
    the standalone servers. See [Limitations](limitations.md).

## Install

=== "Rust (embedded)"

    ```toml
    [dependencies]
    datastreamdb-core = "0.1"
    ```

=== "TypeScript (embedded)"

    ```sh
    npm install @datastreamdb/core
    ```

=== "Python (embedded)"

    ```sh
    pip install datastreamdb
    ```

=== "C (embedded)"

    Download the release asset (`libdatastreamdb_ffi.a` + `datastreamdb.h`) and link it:

    ```sh
    cc app.c -ldatastreamdb_ffi -o app
    ```

=== "Servers (standalone)"

    ```sh
    cargo install datastreamdb-server --bin zero --bin alpha
    ```

## Sixty seconds, embedded (TypeScript)

```js
import { Instance } from '@datastreamdb/core'

const a = new Instance('{}')          // in-memory, isolated
const b = new Instance('{"tenant": "b"}')
a.put('name', 'Ada')
console.log(a.get('name'))            // Ada
console.log(b.get('name'))            // null — instances are isolated
```

Continue with [Embedding](embedding/index.md) or, for the full
database, the [Quickstart](quickstart.md).
