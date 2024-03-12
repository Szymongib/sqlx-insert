use sqlx_insert::SQLInsert;
pub struct MariaDB;

#[derive(SQLInsert, Clone, Debug)]
#[sqlx_insert(database(MariaDB))]
pub struct Thing {
    id: String,
    amount: i32,
}

fn main() {}
