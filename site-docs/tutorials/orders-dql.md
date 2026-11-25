# Orders with numeric facets

Use facets to model line-item properties on an order-to-product edge.

## Before you start

Pick a binding. Each tab shows the install, how to open an instance, and the
three calls this tutorial uses. `SCHEMA`, `DATA`, and `QUERY` are the blocks in
the sections below.

=== "Rust"

    ```toml
    [dependencies]
    datastreamdb-core = "0.2"
    serde_json = "1"
    ```

    ```rust
    use datastreamdb_core::{Instance, InstanceConfig};

    let db = Instance::open(InstanceConfig::memory())?;

    db.dql_alter(SCHEMA)?;
    db.dql_mutate(DATA)?;
    let result = db.dql_query(QUERY)?;
    println!("{result}");
    ```

    For a persistent instance, open with
    `InstanceConfig::from_json(r#"{"data_dir": "./data"}"#)?`.

=== "TypeScript"

    ```sh
    npm install @datastreamdb/core
    ```

    ```js
    import { Instance } from '@datastreamdb/core'

    const db = new Instance('{}')

    db.dqlAlter(SCHEMA)
    db.dqlMutate(DATA)
    const result = JSON.parse(db.dqlQuery(QUERY))
    console.log(result)
    ```

    For a persistent instance, pass
    `JSON.stringify({ data_dir: './data' })` to `new Instance(...)`.

=== "Python"

    ```sh
    pip install datastreamdb
    ```

    ```python
    import json
    from datastreamdb import Instance

    db = Instance("{}")

    db.dql_alter(SCHEMA)
    db.dql_mutate(DATA)
    result = json.loads(db.dql_query(QUERY))
    print(result)
    ```

    For a persistent instance, pass
    `json.dumps({"data_dir": "./data"})` to `Instance(...)`.

=== "C"

    Download `libdatastreamdb_ffi.a` and `datastreamdb.h` from the
    [v0.2.0 release](https://github.com/fab679/datastreamdb/releases/tag/v0.2.0).

    ```c
    #include <stdio.h>
    #include <string.h>
    #include "datastreamdb.h"

    static int exec(datastreamdb_handle_t *db, const char *request) {
      uint8_t *out = NULL;
      size_t len = 0;
      int rc = datastreamdb_execute(db, (const uint8_t *)request,
                                    strlen(request), &out, &len);
      if (rc == DATASTREAMDB_OK) {
        fwrite(out, 1, len, stdout);
        datastreamdb_free_bytes(out, len);
      }
      return rc;
    }

    int main(void) {
      datastreamdb_handle_t *db = NULL;
      datastreamdb_open("{}", &db);

      exec(db, "{\"op\":\"dql_alter\",\"schema\":\"...\"}");
      exec(db, "{\"op\":\"dql_mutate\",\"query\":\"...\"}");
      exec(db, "{\"op\":\"dql_query\",\"query\":\"...\"}");

      datastreamdb_close(db);
      return 0;
    }
    ```

    The `...` placeholders are the JSON-escaped blocks below. Use a JSON
    library to build them rather than escaping by hand.

The blocks below are the DQL documents referenced as `SCHEMA`, `DATA`, and
`QUERY`. This tutorial puts facets on a line-item edge: quantity and price
belong to the order-to-product relationship, not to the order or the product.

## Schema

```text
name: string @index(exact) .
email: string @index(exact) .
sku: string @index(exact) .
price: float @index(float) .
quantity: int @index(int) .
ordered: [uid] @reverse .
type Customer {
    name
    email
}
type Product {
    name
    sku
    price
}
type Order {
    name
    ordered
}
```

## Data

```text
{
 set {
  _:ada <name> "Ada" .
  _:ada <email> "ada@example.com" .
  _:ada <datastreamdb.type> "Customer" .
  _:laptop <name> "Laptop" .
  _:laptop <sku> "LAP-1" .
  _:laptop <price> "999.00"^^<xs:float> .
  _:laptop <datastreamdb.type> "Product" .
  _:mouse <name> "Mouse" .
  _:mouse <sku> "MOU-1" .
  _:mouse <price> "25.00"^^<xs:float> .
  _:mouse <datastreamdb.type> "Product" .
  _:order <name> "Order #1001" .
  _:order <datastreamdb.type> "Order" .
  _:ada <ordered> _:order .
  _:order <ordered> _:laptop (quantity=1, price=999.00) .
  _:order <ordered> _:mouse (quantity=2, price=25.00) .
 }
}
```

## Query nested relationships and facets

```text
{
 c(func: eq(name, "Ada")) {
  name email
  ordered @facets(quantity, price) {
   name
   ordered @facets(quantity, price) { name sku price }
  }
 }
}
```

## Reverse lookup

```text
{ p(func: eq(sku, "LAP-1")) { name ~ordered { name } } }
```

---

## Next

See [all tutorials](index.md) and the [DQL & GraphQL reference](../embedding/queries.md).
