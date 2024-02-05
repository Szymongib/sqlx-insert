use async_trait::async_trait;
pub use sqlx_insert_derive::SQLInsert;

#[async_trait]
pub trait SQLInsert<DB: sqlx::Database> {
    async fn sql_insert<'e, 'c, E: 'e + sqlx::Executor<'c, Database = DB>>(
        &self,
        connection: E,
    ) -> ::sqlx::Result<()>;
}
