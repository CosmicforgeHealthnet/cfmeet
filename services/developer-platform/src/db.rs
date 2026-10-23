//! Database helpers: safe error summaries (SQLSTATE + constraint only, never a value).

use sea_orm::{DbErr, RuntimeErr};

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
        None if matches!(err, DbErr::Conn(_) | DbErr::ConnectionAcquire(_)) => {
            "connection unavailable".to_string()
        }
        None => "unexpected database error".to_string(),
    }
}
