//! Thin SQL helpers over SeaORM (`Statement` + `FromQueryResult`). Every statement is
//! parameterized; no value is ever interpolated into SQL text.

use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement, Value};

use crate::error::TelephonyResult;

pub fn stmt(sql: &str, values: Vec<Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

pub async fn exec<C: ConnectionTrait>(
    db: &C,
    sql: &str,
    values: Vec<Value>,
) -> TelephonyResult<u64> {
    Ok(db.execute(stmt(sql, values)).await?.rows_affected())
}

pub async fn one<T: FromQueryResult, C: ConnectionTrait>(
    db: &C,
    sql: &str,
    values: Vec<Value>,
) -> TelephonyResult<Option<T>> {
    Ok(T::find_by_statement(stmt(sql, values)).one(db).await?)
}

pub async fn all<T: FromQueryResult, C: ConnectionTrait>(
    db: &C,
    sql: &str,
    values: Vec<Value>,
) -> TelephonyResult<Vec<T>> {
    Ok(T::find_by_statement(stmt(sql, values)).all(db).await?)
}

pub async fn scalar_i64<C: ConnectionTrait>(
    db: &C,
    sql: &str,
    values: Vec<Value>,
) -> TelephonyResult<i64> {
    let row = db.query_one(stmt(sql, values)).await?;
    Ok(match row {
        Some(row) => row.try_get_by_index::<i64>(0)?,
        None => 0,
    })
}

/// Serializes the rest of `txn` on `key` (transaction-scoped advisory lock).
pub async fn advisory_xact_lock<C: ConnectionTrait>(db: &C, key: &str) -> TelephonyResult<()> {
    exec(
        db,
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        vec![key.into()],
    )
    .await
    .map(|_| ())
}
