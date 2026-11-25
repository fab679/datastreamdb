# Orders with numeric facets

Use facets to model line-item properties on an order-to-product edge.

## Before you start

Pick the embedded binding you want to use:

=== "Rust"

    ```toml
    [dependencies]
    datastreamdb-core = "0.2"
    serde_json = "1"
    ```

=== "TypeScript"

    ```sh
    npm install @datastreamdb/core
    ```

=== "Python"

    ```sh
    pip install datastreamdb
    ```

=== "C"

    Download the `libdatastreamdb_ffi.a` / `datastreamdb.h` assets from the
    [v0.2.0 release](https://github.com/fab679/datastreamdb/releases/tag/v0.2.0).

The blocks below are DQL documents. In Rust:

```rust
use datastreamdb_core::{Instance, InstanceConfig};

let db = Instance::open(InstanceConfig::memory())?;
db.dql_alter(SCHEMA)?;
db.dql_mutate(DATA)?;
let result = db.dql_query(QUERY)?;
```

Here `SCHEMA`, `DATA`, and `QUERY` name the blocks below. This tutorial puts facets on a line-item edge: quantity and price belong to the
order-to-product relationship, not to the order or the product.

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
