//! Migration import and reconciliation (`organization.migrate`).

use axum::{body::Bytes, extract::State, http::StatusCode, response::Response};
use cf_service_auth::InternalPermission;

use super::{middleware::Caller, respond};
use crate::{
    error::{ApiError, ApiResult},
    import::{self, Export, ImportError},
    state::AppState,
};

/// `POST /internal/v1/migration/import` — idempotent import of a Meeting Service export.
pub async fn import(
    State(state): State<AppState>,
    caller: Caller,
    body: Bytes,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationMigrate)?;
    // serde messages may quote values; the envelope never echoes them.
    let export: Export = serde_json::from_slice(&body)
        .map_err(|_| ApiError::validation("export refused: not a valid export document"))?;
    match import::import(&state.db, &caller.as_operator(), &export).await {
        Ok(report) => Ok(respond(
            StatusCode::OK,
            serde_json::to_value(report)
                .map_err(|_| ApiError::new(crate::error::ErrorCode::InternalError))?,
        )),
        Err(ImportError::Invalid(reason)) => {
            Err(ApiError::validation(format!("export refused: {reason}")))
        }
        Err(ImportError::Db(e)) => Err(e.into()),
    }
}

/// `GET /internal/v1/migration/reconciliation` — counts, checksum, last import, mismatches.
pub async fn reconciliation(State(state): State<AppState>, caller: Caller) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationMigrate)?;
    Ok(respond(
        StatusCode::OK,
        import::reconciliation(&state.db).await?,
    ))
}
