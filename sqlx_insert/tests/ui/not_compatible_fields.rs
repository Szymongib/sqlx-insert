use sqlx_insert::SQLInsert;
use sqlx::Postgres;

#[derive(SQLInsert, Clone, Debug)]
#[sqlx_insert(database(Postgres))]
pub struct Thing {
    id: String,
    amount: i32,
    complex: ComplexType, // Type cannot be inserted and is not ignored
}

#[derive(Debug, Clone)]
struct ComplexType {
    a: String,
}

fn main() {}
