//! `SQLInsert` trait and derive macro for sqlx.

/// Derive macro for automatically implementing [`SQLInsert`] trait for a struct.
/// All struct fields that are supposed to be inserted into the database need to
/// be: [`sqlx::types::Type`] + [`sqlx::encode::Encode`] + [Clone] + [Send] + [Sync].
/// As of now, the proc macro supports only named struct fields.
///
/// Fields can be renamed using `#[sqlx_insert(rename = "new_name")]` attribute
/// or ignored using `#[sqlx_insert(ignore)]`.
///
/// Database is specified by passing type to `#[sqlx_insert(database(DbType))]` attribute.
/// The `DbType` is a type that implements [`sqlx::Database`] trait.
///
/// Table name defaults to the type name, but can be overridden using
/// `#[sqlx_insert(table = "new_name")]` attribute.
///
/// Example:
/// ```rust
/// use sqlx::Postgres;
/// use sqlx::Sqlite;
/// use sqlx_insert::SQLInsert;
///
/// // If using macros feature, only a single database is supported
/// #[derive(SQLInsert, Clone, Debug)]
/// #[sqlx_insert(table = "thingy")]
/// #[cfg_attr(feature = "use-macros", sqlx_insert(database(Postgres)))]
/// #[cfg_attr(not(feature = "use-macros"), sqlx_insert(database(Postgres, Sqlite)))]
/// pub struct Thing {
///     id: String,
///     name: String,
///     amount: i32,
///     pear: String,
///     #[sqlx_insert(ignore)]
///     ignore_me: Option<String>,
///     #[sqlx_insert(rename = "param_extra")]
///     param: String,
/// }
/// ```
pub use sqlx_insert_derive::SQLInsert;

/// Trait for inserting a struct into a SQL database.
///
/// Example:
/// ```rust
/// use sqlx::Postgres;
/// use sqlx::Sqlite;
/// use sqlx::PgConnection;
/// use sqlx_insert::SQLInsert;
///
/// // If using macros feature, only a single database is supported
/// #[derive(SQLInsert, Clone, Debug, PartialEq)]
/// #[cfg_attr(feature = "use-macros", sqlx_insert(database(Postgres)))]
/// #[cfg_attr(not(feature = "use-macros"), sqlx_insert(database(Postgres, Sqlite)))]
/// struct MyStruct {
///     id: i32,
///     name: String,
/// }
///
/// async fn insert_my_struct(connection: &mut PgConnection) -> sqlx::Result<()> {
///     let my_struct = MyStruct { id: 1, name: "test".to_string() };
///     my_struct.sql_insert(connection).await
/// }
///```
pub trait SQLInsert<DB: sqlx::Database, R> {
    fn sql_insert<'e, 'c, E: 'e + sqlx::Executor<'c, Database = DB>>(
        &self,
        connection: E,
    ) -> impl std::future::Future<Output = ::sqlx::Result<R>>;
}

pub trait BatchInsert<DB: sqlx::Database>: Sized {
    fn batch_insert<'e, 'c, E: 'e + sqlx::Executor<'c, Database = DB>>(
        data: &[Self],
        connection: E,
    ) -> impl std::future::Future<Output = ::sqlx::Result<()>>;
}
