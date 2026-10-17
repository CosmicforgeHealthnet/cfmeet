//! Database helpers: raw statements and safe error summaries (SQLSTATE + constraint only,
//! never a value).

use sea_orm::{ConnectionTrait, DbBackend, DbErr, FromQueryResult, RuntimeErr, Statement};

pub fn stmt(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

pub async fn exec<C: ConnectionTrait>(
    db: &C,
    sql: &str,
    values: Vec<sea_orm::Value>,
) -> Result<u64, DbErr> {
    Ok(db.execute(stmt(sql, values)).await?.rows_affected())
}

pub async fn one<T: FromQueryResult, C: ConnectionTrait>(
    db: &C,
    sql: &str,
    values: Vec<sea_orm::Value>,
) -> Result<Option<T>, DbErr> {
    T::find_by_statement(stmt(sql, values)).one(db).await
}

pub async fn all<T: FromQueryResult, C: ConnectionTrait>(
    db: &C,
    sql: &str,
    values: Vec<sea_orm::Value>,
) -> Result<Vec<T>, DbErr> {
    T::find_by_statement(stmt(sql, values)).all(db).await
}

pub async fn scalar_i64<C: ConnectionTrait>(
    db: &C,
    sql: &str,
    values: Vec<sea_orm::Value>,
) -> Result<i64, DbErr> {
    Ok(db
        .query_one(stmt(sql, values))
        .await?
        .map(|r| r.try_get_by_index::<i64>(0))
        .transpose()?
        .unwrap_or(0))
}

/// A bounded, value-free summary of a database error.
pub fn summarize_db_error(err: &DbErr) -> String {
    let database_error = match err {
        DbErr::Exec(RuntimeErr::SqlxError(sea_orm::sqlx::Error::Database(e)))
        | DbErr::Query(RuntimeErr::SqlxError(sea_orm::sqlx::Error::Database(e))) => Some(e),
        _ => None,
    };
    match database_error {
        Some(e) => format!(
            "sqlstate {}, constraint {}",
            e.code().as_deref().unwrap_or("?"),
            e.constraint().unwrap_or("-")
        ),
        None if is_connectivity_error(err) => "connection unavailable".to_string(),
        None => "unexpected database error".to_string(),
    }
}

pub fn is_connectivity_error(err: &DbErr) -> bool {
    matches!(err, DbErr::Conn(_) | DbErr::ConnectionAcquire(_))
        || matches!(
            err,
            DbErr::Exec(RuntimeErr::SqlxError(
                sea_orm::sqlx::Error::Io(_)
                    | sea_orm::sqlx::Error::PoolTimedOut
                    | sea_orm::sqlx::Error::PoolClosed
            )) | DbErr::Query(RuntimeErr::SqlxError(
                sea_orm::sqlx::Error::Io(_)
                    | sea_orm::sqlx::Error::PoolTimedOut
                    | sea_orm::sqlx::Error::PoolClosed
            ))
        )
}

/// SQLSTATE of a database error, if it is one.
pub fn sqlstate(err: &DbErr) -> Option<String> {
    match err {
        DbErr::Exec(RuntimeErr::SqlxError(sea_orm::sqlx::Error::Database(e)))
        | DbErr::Query(RuntimeErr::SqlxError(sea_orm::sqlx::Error::Database(e))) => {
            e.code().map(|c| c.to_string())
        }
        _ => None,
    }
}
