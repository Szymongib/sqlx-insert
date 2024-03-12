use sqlx_insert::SQLInsert;
use sqlx::Postgres;

#[derive(SQLInsert, Clone, Debug)]
#[sqlx_insert(database(Postgres))]
pub struct Thing(String, i32, i32);

fn main() {}
