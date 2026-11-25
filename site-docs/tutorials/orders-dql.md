# Orders with numeric facets

Use facets to model line-item properties on an order-to-product edge.

## Step by step

The Python, TypeScript, and Rust tabs are complete programs: install, open an
instance, apply the schema, add the data, run the query, and print the result.
The C tab shows the ABI lifecycle; the [C guide](../embedding/c.md) has the full
envelope example. This tutorial puts facets on a line-item edge: quantity and
price belong to the order-to-product relationship, not to the order or the
product.

=== "Python"

    ```sh
    pip install datastreamdb
    ```

    ```python
    import json
    from datastreamdb import Instance

    db = Instance("{}")

    # 1. Schema
    db.dql_alter("""
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
    """)

    # 2. Data
    db.dql_mutate("""
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
    """)

    # 3. Query
    result = json.loads(db.dql_query("""
    {
     c(func: eq(name, "Ada")) {
      name email
      ordered @facets(quantity, price) {
       name
       ordered @facets(quantity, price) { name sku price }
      }
     }
    }
    """))
    print(json.dumps(result, indent=2))
    ```

    For a persistent instance, replace `Instance("{}")` with
    `Instance(json.dumps({"data_dir": "./data"}))`.

=== "TypeScript"

    ```sh
    npm install @datastreamdb/core
    ```

    ```js
    import { Instance } from '@datastreamdb/core'

    const db = new Instance('{}')

    // 1. Schema
    db.dqlAlter(`
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
    `)

    // 2. Data
    db.dqlMutate(`
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
    `)

    // 3. Query
    const result = JSON.parse(db.dqlQuery(`
    {
     c(func: eq(name, "Ada")) {
      name email
      ordered @facets(quantity, price) {
       name
       ordered @facets(quantity, price) { name sku price }
      }
     }
    }
    `))
    console.log(JSON.stringify(result, null, 2))
    ```

    For a persistent instance, pass
    `JSON.stringify({ data_dir: './data' })` to `new Instance(...)`.

=== "Rust"

    ```toml
    [dependencies]
    datastreamdb-core = "0.2"
    serde_json = "1"
    ```

    ```rust
    use datastreamdb_core::{Instance, InstanceConfig};

    fn main() -> Result<(), Box<dyn std::error::Error>> {
        let db = Instance::open(InstanceConfig::memory())?;

        // 1. Schema
        db.dql_alter(r#"
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
    "#)?;

        // 2. Data
        db.dql_mutate(r#"
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
    "#)?;

        // 3. Query
        let result = db.dql_query(r#"
    {
     c(func: eq(name, "Ada")) {
      name email
      ordered @facets(quantity, price) {
       name
       ordered @facets(quantity, price) { name sku price }
      }
     }
    }
    "#)?;
        println!("{result}");
        Ok(())
    }
    ```

    For a persistent instance, open with
    `InstanceConfig::from_json(r#"{"data_dir": "./data"}"#)?`.

=== "C"

    Download `libdatastreamdb_ffi.a` and `datastreamdb.h` from the
    [v0.2.0 release](https://github.com/fab679/datastreamdb/releases/tag/v0.2.0).

    ```c
    datastreamdb_handle_t *db = NULL;
    datastreamdb_open("{}", &db);

    /* Build these envelopes from the DQL strings in the steps below. */
    exec(db, "{\"op\":\"dql_alter\",\"schema\":\"...\"}");
    exec(db, "{\"op\":\"dql_mutate\",\"query\":\"...\"}");
    exec(db, "{\"op\":\"dql_query\",\"query\":\"...\"}");

    datastreamdb_close(db);
    ```

    The [C embedding guide](../embedding/c.md) has the complete `exec` helper,
    status codes, and buffer ownership rules.

## Step 1: schema

This is the schema used in the complete program above.

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

## Step 2: data

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

## Step 3: query nested relationships and facets

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

Run this with the same `dql_query` call from step 3.

```text
{ p(func: eq(sku, "LAP-1")) { name ~ordered { name } } }
```

---

## Next

See [all tutorials](index.md) and the [DQL & GraphQL reference](../embedding/queries.md).
