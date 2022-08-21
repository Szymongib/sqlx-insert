use async_trait::async_trait;
use sqlx;
pub use sqlx_insert_derive::SQLInsert;

#[async_trait]
pub trait SQLInsert<DB: sqlx::Database> {
    async fn sql_insert<'c, E: sqlx::Executor<'c, Database = DB>>(
        &self,
        connection: E,
    ) -> ::sqlx::Result<()>;
}
