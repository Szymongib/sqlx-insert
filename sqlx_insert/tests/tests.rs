use sqlx::FromRow;
use sqlx::Postgres;
use sqlx::Row;
use sqlx::Sqlite;
use sqlx_insert::SQLInsert;

#[derive(SQLInsert, Clone, Debug, PartialEq)]
#[sqlx_insert(table = "thingy")]
#[sqlx_insert(database(Postgres, Sqlite))]
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
    complex_type: ComplexType, // Ignored parameters should not need to satisfy trait bounds.
}

#[derive(Debug, Clone, Default, PartialEq)]
struct ComplexType {
    a: String,
    b: usize,
    c: Vec<u8>,
}

// Implement custom FromRow due to ignored field.
impl<'r> FromRow<'r, sqlx::postgres::PgRow> for Thing {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        Ok(Thing {
            id: row.get("id"),
            name: row.get("name"),
            amount: row.get("amount"),
            pear: row.get("pear"),
            ignore_me: row.get("ignore_me"), // It should not be inserted, but it should be fetched.
            param: row.get("param_extra"),
            complex_type: ComplexType::default(),
        })
    }
}

impl<'r> FromRow<'r, sqlx::sqlite::SqliteRow> for Thing {
    fn from_row(row: &'r sqlx::sqlite::SqliteRow) -> Result<Self, sqlx::Error> {
        Ok(Thing {
            id: row.get("id"),
            name: row.get("name"),
            amount: row.get("amount"),
            pear: row.get("pear"),
            ignore_me: row.get("ignore_me"), // It should not be inserted, but it should be fetched.
            param: row.get("param_extra"),
            complex_type: ComplexType::default(),
        })
    }
}

#[derive(SQLInsert, Clone, Debug, PartialEq, FromRow)]
#[sqlx_insert(database(Postgres))]
pub struct GenericThing<T: ToString> {
    id: String,
    text: T,
    value: Option<i32>,
}

#[derive(SQLInsert, Clone, Debug, PartialEq, FromRow)]
#[sqlx_insert(database(Postgres))]
#[sqlx_insert(table = "lifetimey_thing")]
pub struct LifetimeyThing<'l, T: ToString + Sync> {
    id: T,
    text: T,
    maybe_text: Option<T>,
    #[sqlx_insert(ignore)]
    some_ref: Option<&'l T>,
}

#[cfg(test)]
mod tests {
    use crate::{ComplexType, GenericThing, LifetimeyThing, SQLInsert, Thing};
    use anyhow::Context;
    use sqlx::migrate::MigrateDatabase;
    use sqlx::postgres::PgPoolOptions;
    use sqlx::{Pool, Postgres, Row, Sqlite, SqlitePool};
    use std::collections::HashMap;

    use testcontainers::{clients, Docker};

    pub async fn connection_pool(db_url: &str) -> sqlx::Result<Pool<Postgres>> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(db_url)
            .await?;

        Ok(pool)
    }

    const CREATE_THINGY_TABLE_QUERY: &str = r"
create table thingy (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    amount INTEGER NOT NULL,
    pear TEXT NOT NULL,
    ignore_me TEXT NULL,
    param_extra TEXT NOT NULL
);";
    const CREATE_GENERIC_THING_TABLE_QUERY: &str = r"
create table genericthing (
    id TEXT PRIMARY KEY,
    text TEXT NOT NULL,
    value INTEGER NULL
);";
    const CREATE_LIFETIMEY_THING_TABLE_QUERY: &str = r"
create table lifetimey_thing (
    id CHAR(36) PRIMARY KEY,
    text TEXT NOT NULL,
    maybe_text TEXT NULL
);";

    async fn create_tables<'c, DB: sqlx::Database, E>(connection: E) -> anyhow::Result<()>
    where
        E: sqlx::Executor<'c, Database = DB> + Copy,
    {
        connection
            .execute(CREATE_THINGY_TABLE_QUERY)
            .await
            .context("failed to setup thing table")?;
        connection
            .execute(CREATE_GENERIC_THING_TABLE_QUERY)
            .await
            .context("failed to setup generic thing table")?;
        connection
            .execute(CREATE_LIFETIMEY_THING_TABLE_QUERY)
            .await
            .context("failed to setup lifetimy thing table")?;
        Ok(())
    }

    #[tokio::test]
    async fn test_postgres() {
        let docker = clients::Cli::default();

        let mut envs = HashMap::new();
        envs.insert("POSTGRES_PASSWORD".to_string(), "password".to_string());
        let pg_img = testcontainers::images::postgres::Postgres::default().with_env_vars(envs);
        let pg = docker.run(pg_img);
        let db_url = format!(
            "postgres://postgres:password@localhost:{}/postgres",
            pg.get_host_port(5432).unwrap()
        );

        let pool = connection_pool(&db_url)
            .await
            .expect("failed to create connection pool");

        create_tables(&pool).await.expect("failed to create tables");

        let mut cnn = pool.acquire().await.expect("failed to acquire connection");

        // Thing
        let thing = Thing {
            id: uuid::Uuid::new_v4().to_string(),
            name: "name".to_string(),
            amount: 10,
            pear: "yas!".to_string(),
            ignore_me: Some("ignored".to_string()),
            param: "param_param_param".to_string(),
            complex_type: ComplexType::default(),
        };

        thing
            .sql_insert(cnn.as_mut())
            .await
            .expect("failed to insert thing");

        let mut fetched_thing: Thing = sqlx::query_as("SELECT * FROM thingy WHERE ID = $1")
            .bind(&thing.id)
            .fetch_one(cnn.as_mut())
            .await
            .expect("failed to fetch inserted thing");
        assert_eq!(None, fetched_thing.ignore_me); // It was ignored so should be empty

        // Manually set ignored field and compare
        fetched_thing.ignore_me = Some("ignored".to_string());
        assert_eq!(thing, fetched_thing);

        // GenericThing
        let generic_thing = GenericThing::<String> {
            id: uuid::Uuid::new_v4().to_string(),
            text: "some text".to_string(),
            value: None,
        };

        generic_thing
            .sql_insert::<&mut sqlx::PgConnection>(&mut cnn)
            .await
            .expect("err");

        let fetched_gen_thing: GenericThing<String> =
            sqlx::query_as("SELECT * FROM genericthing WHERE ID = $1")
                .bind(&generic_thing.id)
                .fetch_one(cnn.as_mut())
                .await
                .expect("failed to fetch inserted generic thing");
        assert_eq!(fetched_gen_thing, generic_thing);

        // Lifetimey Thing
        let some_text = "some text".to_string();
        let lifetimey_thing = LifetimeyThing {
            id: uuid::Uuid::new_v4().to_string(),
            text: some_text.clone(),
            maybe_text: Some(some_text.to_uppercase()),
            some_ref: Some(&some_text),
        };

        lifetimey_thing
            .sql_insert(cnn.as_mut())
            .await
            .expect("failed to insert lifetimey thing");

        let row = sqlx::query("SELECT * FROM lifetimey_thing WHERE ID = $1")
            .bind(&lifetimey_thing.id)
            .fetch_one(cnn.as_mut())
            .await
            .expect("failed to fetch inserted lifetimey thing");
        let fetched_lifetimey_thing = LifetimeyThing {
            id: row.get("id"),
            text: row.get("text"),
            maybe_text: row.get("maybe_text"),
            some_ref: Some(&some_text),
        };
        assert_eq!(fetched_lifetimey_thing, lifetimey_thing);

        // Transaction
        let mut tx = pool.begin().await.expect("failed to start transaction");

        let new_thing = Thing {
            id: uuid::Uuid::new_v4().to_string(),
            name: "name".to_string(),
            amount: 10,
            pear: "yas!".to_string(),
            ignore_me: None,
            param: "param_param_param".to_string(),
            complex_type: ComplexType::default(),
        };
        new_thing
            .sql_insert(&mut *tx)
            .await
            .expect("failed to insert as part of tx");

        tx.commit().await.expect("failed to commit tx");

        let fetched_new_thing: Thing = sqlx::query_as("SELECT * FROM thingy WHERE ID = $1")
            .bind(&new_thing.id)
            .fetch_one(cnn.as_mut())
            .await
            .expect("failed to fetch inserted thing");
        assert_eq!(new_thing, fetched_new_thing);
    }

    #[tokio::test]
    async fn test_sqlite() {
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let db_url = format!("{}/test.db", temp_dir.path().display());

        Sqlite::create_database(&db_url)
            .await
            .expect("failed to create sqlite database");

        let db = SqlitePool::connect(&db_url).await.unwrap();

        create_tables(&db).await.expect("failed to create tables");

        let mut cnn = db.acquire().await.unwrap();

        let thing = Thing {
            id: uuid::Uuid::new_v4().to_string(),
            name: "name".to_string(),
            amount: 10,
            pear: "yas!".to_string(),
            ignore_me: None,
            param: "param_param_param".to_string(),
            complex_type: ComplexType::default(),
        };

        thing.sql_insert(cnn.as_mut()).await.expect("err");

        let fetched_new_thing: Thing = sqlx::query_as("SELECT * FROM thingy WHERE ID = $1")
            .bind(&thing.id)
            .fetch_one(cnn.as_mut())
            .await
            .expect("failed to fetch inserted thing");
        assert_eq!(thing, fetched_new_thing);
    }
}
