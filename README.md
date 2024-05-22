# sqlx-insert

This crate provides a proc macro that generates code for inserting structs into the database using [sqlx](https://github.com/launchbadge/sqlx) to save you from excessive typing, and not inserted columns.

## Example

```rust
use sqlx::Postgres;
use sqlx::Sqlite;
use sqlx_insert::SQLInsert;

#[derive(SQLInsert, Clone, Debug, PartialEq)]
#[sqlx_insert(database(Postgres, Sqlite))]
pub struct Thing {
    id: String,
    name: String,
    amount: i32,
    pear: String,
}
```

Then you can simply use it as:
```rust
use sqlx_insert::SQLInsert;
//...
let mut connection = pool.acquire().await.unwrap();

let thing = Thing {
    // Create a thing...
};

thing.sql_insert(cnn.as_mut()).await.unwrap();
```

The `SQLInsert` macro will generate the following insert query:
```rust
let _ = sqlx::query(
        "INSERT INTO thing (id, name, amount, pear) VALUES ($1, $2, $3, $4)",
    )
    .bind(thing.id.clone())
    .bind(thing.name.clone())
    .bind(thing.amount.clone())
    .bind(thing.pear.clone())
    .execute(connection)
    .await?;
```

### Attributes

`SQLInsert` macro supports the following struct attributes:
- `database` - list of types implementing `sqlx::Database` for which insert query is implemented.
- `table` - the name of the table to which generated query inserts.

And field level attributes:
- `ignore` - ignore field in generated insert query.
- `rename` - column name for which the field should be inserted.

Example struct using those:
```rust
#[derive(SQLInsert, Clone, Debug, PartialEq)]
#[sqlx_insert(table = "thingy")]
#[sqlx_insert(database(Postgres))]
pub struct Thing {
    id: String,
    name: String,
    amount: i32,
    pear: String,
    #[sqlx_insert(ignore)]
    ignore_me: Option<String>,
    #[sqlx_insert(rename = "param_extra")]
    param: String,
    #[sqlx_insert(ignore)]
    complex_type: ComplexType,
}
```

For detailed examples, see [tests](./sqlx_insert/tests/tests.rs).

## Motivation

I created this macro some time ago for use in personal projects, and it proved itself very useful. [sqlx](https://github.com/launchbadge/sqlx) is a great library giving users a lot of control over the shape of queries, however, it requires them to write insert queries for all types "by hand".

As the types grow in size and change over time I often find it tedious to maintain, with this macro I only add a filed to a type and do not need to update insert queries. If more sophisticated insert logic is required, I can always switch back to hand written query.

The drawback of this approach is that we do not leverage compile-time query verification, however, I personally rarely used it anyway due to additional dependencies during development.
