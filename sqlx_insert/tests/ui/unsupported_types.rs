use sqlx_insert::SQLInsert;
use async_trait::async_trait;
use sqlx::Postgres;

#[derive(SQLInsert, Clone, Debug)]
#[sqlx_insert(database(Postgres))]
pub enum Thing{
    A, B, C
}

fn main() {}
