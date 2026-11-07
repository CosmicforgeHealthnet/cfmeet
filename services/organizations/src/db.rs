//! Database helpers: raw-SQL statements, typed row reads and safe error summaries
//! (SQLSTATE + constraint only, never a value).

use sea_orm::{ConnectionTrait, DbBackend, DbErr, FromQueryResult, RuntimeErr, Statement, Value};

pub fn stmt(sql: &str, values: Vec<Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

/// Executes a statement; returns the affected row count.
pub async fn exec<C: ConnectionTrait>(c: &C, sql: &str, values: Vec<Value>) -> Result<u64, DbErr> {
    Ok(c.execute(stmt(sql, values)).await?.rows_affected())
}

/// At most one typed row.
pub async fn one<T: FromQueryResult, C: ConnectionTrait>(
    c: &C,
    sql: &str,
    values: Vec<Value>,
) -> Result<Option<T>, DbErr> {
    T::find_by_statement(stmt(sql, values)).one(c).await
}

/// Every typed row.
pub async fn all<T: FromQueryResult, C: ConnectionTrait>(
    c: &C,
    sql: &str,
    values: Vec<Value>,
) -> Result<Vec<T>, DbErr> {
    T::find_by_statement(stmt(sql, values)).all(c).await
}

/// A single `BIGINT` scalar (column `n`).
pub async fn scalar_i64<C: ConnectionTrait>(
    c: &C,
    sql: &str,
    values: Vec<Value>,
) -> Result<i64, DbErr> {
    let row = c
        .query_one(stmt(sql, values))
        .await?
        .ok_or_else(|| DbErr::Custom("scalar query returned no row".into()))?;
    row.try_get::<i64>("", "n")
}

/// The database clock of the current transaction (`now()`): every timestamp written by
/// one change comes from it, so rows, audit, history and events agree.
pub async fn now<C: ConnectionTrait>(c: &C) -> Result<chrono::DateTime<chrono::Utc>, DbErr> {
    let row = c
        .query_one(stmt("SELECT now() AS now", vec![]))
        .await?
        .ok_or_else(|| DbErr::Custom("now() returned no row".into()))?;
    row.try_get::<chrono::DateTime<chrono::Utc>>("", "now")
}

fn database_error(err: &DbErr) -> Option<&(dyn sea_orm::sqlx::error::DatabaseError + 'static)> {
    match err {
        DbErr::Exec(RuntimeErr::SqlxError(sea_orm::sqlx::Error::Database(e)))
        | DbErr::Query(RuntimeErr::SqlxError(sea_orm::sqlx::Error::Database(e))) => {
            Some(e.as_ref())
        }
        _ => None,
    }
}

/// The violated constraint of a database error, if any.
pub fn constraint_of(err: &DbErr) -> Option<String> {
    database_error(err).and_then(|e| e.constraint().map(str::to_string))
}

/// Whether `err` is a unique violation of `constraint`.
pub fn is_unique_violation(err: &DbErr, constraint: &str) -> bool {
    database_error(err)
        .is_some_and(|e| e.code().as_deref() == Some("23505") && e.constraint() == Some(constraint))
}

/// A bounded, value-free summary of a database error.
pub fn summarize_db_error(err: &DbErr) -> String {
    match database_error(err) {
        Some(e) => format!(
            "sqlstate {}, constraint {}",
            e.code().as_deref().unwrap_or("?"),
            e.constraint().unwrap_or("-")
        ),
        None if matches!(err, DbErr::Conn(_) | DbErr::ConnectionAcquire(_)) => {
            "connection unavailable".to_string()
        }
        None => "unexpected database error".to_string(),
    }
}
