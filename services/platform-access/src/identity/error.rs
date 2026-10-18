//! Domain errors of the platform trust foundation.
//!
//! Messages are written for platform operators (admin CLI, future administration API).
//! They never contain credential secrets, verifiers or raw database messages: database
//! failures are summarised by SQLSTATE and constraint name only (PostgreSQL error detail
//! can echo whole rows).

use sea_orm::{DbErr, RuntimeErr, SqlErr};

#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    /// Malformed or out-of-range input.
    #[error("{0}")]
    Validation(String),
    /// The resource does not exist in the caller's scope.
    #[error("{0} not found")]
    NotFound(&'static str),
    /// A uniqueness rule (slug, key, environment kind, …) would be violated.
    #[error("{0}")]
    Conflict(String),
    /// A platform policy forbids the operation (scope not allowed, live telecom outside
    /// production, first-party-only scope for an external tenant, …).
    #[error("{0}")]
    PolicyViolation(String),
    /// The resource is in a state that does not allow the operation.
    #[error("{0}")]
    InvalidState(String),
    /// Unexpected database failure. `Display` is deliberately generic.
    #[error("database error ({summary})")]
    Database { summary: String, connectivity: bool },
}

pub type PlatformResult<T> = Result<T, PlatformError>;

/// Safe one-line summary of a database error: SQLSTATE + constraint name, never the
/// server message or row detail.
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

impl From<DbErr> for PlatformError {
    fn from(err: DbErr) -> Self {
        match err.sql_err() {
            Some(SqlErr::UniqueConstraintViolation(_)) => {
                PlatformError::Conflict("the resource already exists".to_string())
            }
            Some(SqlErr::ForeignKeyConstraintViolation(_)) => PlatformError::PolicyViolation(
                "the referenced resource does not exist in the same ownership scope".to_string(),
            ),
            _ => PlatformError::Database {
                summary: summarize_db_error(&err),
                connectivity: is_connectivity_error(&err),
            },
        }
    }
}
