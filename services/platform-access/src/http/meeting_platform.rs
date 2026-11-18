//! Partner meeting chat principals (ADR 0062; contract
//! `docs/platform/build-internal-contracts.md` §2.4): the Meeting Service only
//! (`platform-principal.ensure-meeting-platform`), behind
//! `PLATFORM_ACCESS_MEETING_PLATFORM_ENABLED` (off: `404 NOT_FOUND` like an unknown route).
//!
//! `POST /principals/ensure-meeting-platform` `{environmentId, kind, externalReference}`
//! idempotently ensures exactly one of two principals of an ACTIVE environment (its
//! application and tenant ACTIVE too) of an EXTERNAL tenant:
//!
//! * `BOT_OR_SERVICE` `cfmeet:meeting-system` — the environment's fixed meeting system
//!   principal (owner of partner meeting conversations);
//! * `TEMPORARY_EXTERNAL_PARTICIPANT` `meeting-participant:<lowercase uuid>`.
//!
//! Any other kind or reference is `VALIDATION_ERROR`; a FIRST_PARTY tenant is
//! `POLICY_VIOLATION`; a suspended chain or a suspended / tombstoned principal is
//! `INVALID_STATE`; an unknown environment is `NOT_FOUND`.

use axum::{extract::State, response::IntoResponse, routing::post, Json, Router};
use cf_service_auth::InternalPermission;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::{
    error::{AccessError, AccessResult, ErrorCode},
    middleware::InternalCaller,
};
use crate::{
    domain::{EnvironmentId, PrincipalKind, PrincipalStatus},
    identity::principals::NewPrincipal,
    state::AppState,
};

/// The fixed reference of an environment's meeting system principal.
pub const MEETING_SYSTEM_REFERENCE: &str = "cfmeet:meeting-system";
/// The prefix of a temporary meeting participant's reference.
pub const MEETING_PARTICIPANT_PREFIX: &str = "meeting-participant:";

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/principals/ensure-meeting-platform",
        post(ensure_meeting_platform),
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EnsureBody {
    environment_id: Uuid,
    kind: String,
    external_reference: String,
}

/// The only two allowed `(kind, externalReference)` combinations.
pub fn allowed_combination(kind: &str, reference: &str) -> Option<PrincipalKind> {
    match kind {
        "BOT_OR_SERVICE" if reference == MEETING_SYSTEM_REFERENCE => {
            Some(PrincipalKind::BotOrService)
        }
        "TEMPORARY_EXTERNAL_PARTICIPANT" => reference
            .strip_prefix(MEETING_PARTICIPANT_PREFIX)
            .and_then(|id| {
                Uuid::parse_str(id)
                    .ok()
                    .filter(|u| u.hyphenated().to_string() == id)
            })
            .map(|_| PrincipalKind::TemporaryExternalParticipant),
        _ => None,
    }
}

async fn ensure_meeting_platform(
    State(state): State<AppState>,
    caller: InternalCaller,
    Json(body): Json<EnsureBody>,
) -> AccessResult<impl IntoResponse> {
    if !state.build.meeting_platform_enabled {
        return Err(AccessError::Refused(ErrorCode::NotFound));
    }
    caller.require_permission(InternalPermission::PlatformPrincipalEnsureMeetingPlatform)?;
    let kind = allowed_combination(&body.kind, &body.external_reference).ok_or_else(|| {
        AccessError::Operator(
            ErrorCode::ValidationError,
            "only BOT_OR_SERVICE `cfmeet:meeting-system` and TEMPORARY_EXTERNAL_PARTICIPANT `meeting-participant:<uuid>` are allowed".to_string(),
        )
    })?;
    let row = state
        .platform
        .db()
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT t.classification, \
                    (e.status = 'ACTIVE' AND a.status = 'ACTIVE' AND t.status = 'ACTIVE') AS active \
             FROM platform_environments e \
             JOIN platform_applications a ON a.id = e.application_id AND a.tenant_id = e.tenant_id \
             JOIN platform_tenants t ON t.id = e.tenant_id \
             WHERE e.id = $1",
            [body.environment_id.into()],
        ))
        .await
        .map_err(|e| AccessError::service(e.into()))?
        .ok_or(AccessError::Refused(ErrorCode::NotFound))?;
    let classification: String = row
        .try_get("", "classification")
        .map_err(|e| AccessError::service(e.into()))?;
    let active: bool = row
        .try_get("", "active")
        .map_err(|e| AccessError::service(e.into()))?;
    if classification != "EXTERNAL" {
        return Err(AccessError::Operator(
            ErrorCode::PolicyViolation,
            "meeting platform principals exist only in EXTERNAL tenants".to_string(),
        ));
    }
    if !active {
        return Err(AccessError::Operator(
            ErrorCode::InvalidState,
            "the environment, its application or its tenant is suspended".to_string(),
        ));
    }
    let (principal, created) = state
        .platform
        .register_principal(
            EnvironmentId::from_uuid(body.environment_id),
            NewPrincipal::External {
                kind,
                external_reference: body.external_reference,
            },
        )
        .await
        .map_err(AccessError::service)?;
    if principal.status != PrincipalStatus::Active || principal.tombstoned_at.is_some() {
        return Err(AccessError::Refused(ErrorCode::InvalidState));
    }
    crate::metrics::inc(
        "platform_access_meeting_platform_principals_total",
        &[
            ("kind", kind.as_str()),
            ("created", if created { "true" } else { "false" }),
        ],
    );
    Ok(Json(json!({
        "principalId": principal.id,
        "tenantId": principal.tenant_id,
        "applicationId": principal.application_id,
        "environmentId": principal.environment_id,
        "kind": principal.kind,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_two_combinations_are_allowed() {
        let id = Uuid::new_v4();
        assert_eq!(
            allowed_combination("BOT_OR_SERVICE", "cfmeet:meeting-system"),
            Some(PrincipalKind::BotOrService)
        );
        assert_eq!(
            allowed_combination(
                "TEMPORARY_EXTERNAL_PARTICIPANT",
                &format!("meeting-participant:{id}")
            ),
            Some(PrincipalKind::TemporaryExternalParticipant)
        );
        for (kind, reference) in [
            ("BOT_OR_SERVICE", "cfmeet:other".to_string()),
            ("BOT_OR_SERVICE", format!("meeting-participant:{id}")),
            (
                "TEMPORARY_EXTERNAL_PARTICIPANT",
                "cfmeet:meeting-system".to_string(),
            ),
            (
                "TEMPORARY_EXTERNAL_PARTICIPANT",
                format!("meeting-participant:{}", id.to_string().to_uppercase()),
            ),
            (
                "TEMPORARY_EXTERNAL_PARTICIPANT",
                format!("meeting-participant:{}", id.simple()),
            ),
            (
                "TEMPORARY_EXTERNAL_PARTICIPANT",
                "meeting-participant:".to_string(),
            ),
            (
                "EXTERNAL_APPLICATION_PRINCIPAL",
                "cfmeet:meeting-system".to_string(),
            ),
            ("NATIVE_CF_USER", format!("meeting-participant:{id}")),
        ] {
            assert_eq!(
                allowed_combination(kind, &reference),
                None,
                "{kind} {reference}"
            );
        }
    }
}
