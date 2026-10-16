//! Domain errors. `Display` never contains row content, numbers or provider references.

use sea_orm::{DbErr, RuntimeErr, SqlErr};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DomainError {
    /// Malformed or out-of-range input.
    #[error("{0}")]
    Validation(String),
    /// Not found in the caller's scope (also for other tenants' records: no oracle).
    #[error("{0} not found")]
    NotFound(&'static str),
    /// The expected revision is stale.
    #[error("the {0} was changed concurrently (revision conflict)")]
    VersionConflict(&'static str),
    /// The transition is not an edge of the state model.
    #[error("{entity} transition {from} -> {to} is not allowed")]
    InvalidTransition {
        entity: &'static str,
        from: String,
        to: String,
    },
    /// A prerequisite of the operation is not met (code is stable, e.g. ELIGIBILITY_REQUIRED).
    #[error("{code}: {message}")]
    Precondition { code: &'static str, message: String },
    /// A uniqueness / exclusivity rule (e.g. an open assignment exists).
    #[error("{code}: {message}")]
    Conflict { code: &'static str, message: String },
    /// Unexpected database failure (safe summary only).
    #[error("database error ({summary})")]
    Database { summary: String, connectivity: bool },
}

pub type DomainResult<T> = Result<T, DomainError>;

impl DomainError {
    pub fn precondition(code: &'static str, message: impl Into<String>) -> Self {
        DomainError::Precondition {
            code,
            message: message.into(),
        }
    }

    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        DomainError::Conflict {
            code,
            message: message.into(),
        }
    }

    /// The stable machine code of this error.
    pub fn code(&self) -> &'static str {
        match self {
            DomainError::Validation(_) => "VALIDATION_ERROR",
            DomainError::NotFound(_) => "NOT_FOUND",
            DomainError::VersionConflict(_) => "VERSION_CONFLICT",
            DomainError::InvalidTransition { .. } => "INVALID_TRANSITION",
            DomainError::Precondition { code, .. } => code,
            DomainError::Conflict { code, .. } => code,
            DomainError::Database { .. } => "SERVICE_UNAVAILABLE",
        }
    }
}

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

/// Constraint name of a database error, if any.
pub fn constraint(err: &DbErr) -> Option<String> {
    match err {
        DbErr::Exec(RuntimeErr::SqlxError(sea_orm::sqlx::Error::Database(e)))
        | DbErr::Query(RuntimeErr::SqlxError(sea_orm::sqlx::Error::Database(e))) => {
            e.constraint().map(str::to_string)
        }
        _ => None,
    }
}

impl From<DbErr> for DomainError {
    fn from(err: DbErr) -> Self {
        // The database enforces the same invariants as the domain; map its refusals to the
        // same domain errors (a race the domain check could not see).
        let state = sqlstate(&err);
        let name = constraint(&err).unwrap_or_default();
        match (err.sql_err(), state.as_deref()) {
            (Some(SqlErr::UniqueConstraintViolation(_)), _) => match name.as_str() {
                "uq_number_reservations_current" => DomainError::conflict(
                    "RESERVATION_EXISTS",
                    "the number already has a current reservation",
                ),
                "uq_dcn_assignments_open" => DomainError::conflict(
                    "ASSIGNMENT_EXISTS",
                    "the number already has an open assignment",
                ),
                "uq_number_resources_real_e164" | "uq_number_resources_synthetic_e164" => {
                    DomainError::conflict(
                        "NUMBER_EXISTS",
                        "the number already exists in this inventory class",
                    )
                }
                "uq_number_provider_bindings_active_ref" => DomainError::conflict(
                    "BINDING_CONFLICT",
                    "the provider reference is already actively bound",
                ),
                _ => DomainError::conflict("CONFLICT", "the record already exists"),
            },
            (Some(SqlErr::ForeignKeyConstraintViolation(_)), _) => DomainError::precondition(
                "SCOPE_MISMATCH",
                "a referenced record does not exist in the same ownership scope",
            ),
            (_, Some("23P01")) => DomainError::conflict(
                "BINDING_CONFLICT",
                "an active provider binding already serves a capability of this number",
            ),
            (_, Some("40001")) => DomainError::VersionConflict("record"),
            (_, Some("23514")) => {
                DomainError::precondition("RULE_VIOLATION", "a domain rule refused the change")
            }
            (_, Some("42501")) => {
                DomainError::precondition("RULE_VIOLATION", "the change is not permitted")
            }
            _ => DomainError::Database {
                summary: summarize_db_error(&err),
                connectivity: is_connectivity_error(&err),
            },
        }
    }
}
