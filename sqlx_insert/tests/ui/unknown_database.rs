use sqlx_insert::SQLInsert;
use async_trait::async_trait;

pub struct MariaDB;

#[derive(SQLInsert, Clone, Debug)]
#[sqlx_insert(database(MariaDB))]
pub struct Thing {
    id: String,
    amount: i32,
}

fn main() {}
