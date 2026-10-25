//! Service accounts of an environment — Section 50 (Platform Access delegation table:
//! "Developer Platform delegates create / suspend operations to Platform Access").
//!
//! * Platform Access stays the ONLY store of service accounts: nothing is persisted here
//!   but the activity event of each mutation. Every read and change is a delegated
//!   Platform Access operation scoped to the environment (a service account of another
//!   environment or tenant is NOT_FOUND there too).
//! * OWNER / ADMIN create, suspend and resume; DEVELOPER reads the allowed service
//!   accounts; ANALYST / BILLING have no service-account authority. Machine credentials
//!   need `service_accounts.manage`.
//! * Service accounts are never deleted and never revoked here; their API credentials
//!   (issuance, rotation, revocation) are Section 54 and stay Platform Access's.
//! * A PRODUCTION environment's service accounts follow the environment's own rule: an
//!   operator / production-access decision (Platform Access refuses them on the
//!   delegation path too).
//! * RESTRICTED organizations cannot create or resume service accounts (suspending —
//!   withdrawing machine access — stays possible); SUSPENDED / CLOSED ones change nothing.

use axum::http::StatusCode;
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    activity::{self, Event},
    authorize, delegate,
    pagination::Page,
    stmt, Caller,
};
use crate::{
    domain::rbac::Action,
    http::error::{ApiError, ErrorCode},
    platform_access::DelegationCall,
    state::AppState,
};

/// The Developer Platform profile of an environment the caller may address.
pub(crate) struct EnvironmentRef {
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub kind: String,
    pub status: String,
    /// `LIVE` for a PRODUCTION environment activated by an approved request (§55).
    pub production_access_state: String,
}

impl EnvironmentRef {
    /// Whether delegated machine-identity changes are allowed here: SANDBOX / TEST, or a
    /// PRODUCTION environment that is LIVE through the production-access workflow.
    pub fn delegable(&self) -> bool {
        self.kind != "PRODUCTION" || self.production_access_state == "LIVE"
    }
}

pub(crate) async fn environment<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
) -> Result<EnvironmentRef, ApiError> {
    let row = db
        .query_one(stmt(
            "SELECT tenant_id, application_id, kind, status, production_access_state \
             FROM devplat_environment_profiles WHERE environment_id = $1",
            vec![environment_id.into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?;
    Ok(EnvironmentRef {
        tenant_id: row.try_get("", "tenant_id")?,
        project_id: row.try_get("", "application_id")?,
        kind: row.try_get("", "kind")?,
        status: row.try_get("", "status")?,
        production_access_state: row.try_get("", "production_access_state")?,
    })
}

/// A Platform Access key (`^[a-z0-9][a-z0-9-]{0,62}$`), the service account's stable name.
fn name(value: &str) -> Result<String, ApiError> {
    let b = value.as_bytes();
    let ok = (1..=63).contains(&b.len())
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-');
    ok.then(|| value.to_string())
        .ok_or(ApiError(ErrorCode::ValidationError))
}

fn description(value: Option<&str>) -> Result<Option<String>, ApiError> {
    match value.map(str::trim) {
        None | Some("") => Ok(None),
        Some(d) if d.chars().count() <= 500 && !d.chars().any(char::is_control) => {
            Ok(Some(d.to_string()))
        }
        Some(_) => Err(ApiError(ErrorCode::ValidationError)),
    }
}

/// The customer view of a Platform Access service account of `env` (anything else from
/// the authority — another environment or tenant, a malformed view — fails closed).
pub(crate) fn view(
    pa: &Value,
    environment_id: Uuid,
    env: &EnvironmentRef,
) -> Result<Value, ApiError> {
    let id_of = |k: &str| {
        pa.get(k)
            .and_then(Value::as_str)
            .and_then(|s| Uuid::parse_str(s).ok())
    };
    let status = pa.get("status").and_then(Value::as_str);
    let consistent = id_of("environmentId") == Some(environment_id)
        && id_of("tenantId") == Some(env.tenant_id)
        && id_of("applicationId") == Some(env.project_id)
        && matches!(status, Some("ACTIVE" | "SUSPENDED" | "REVOKED"));
    let id = id_of("id");
    match (consistent, id) {
        (true, Some(id)) => Ok(json!({
            "id": id,
            "organizationId": env.tenant_id,
            "projectId": env.project_id,
            "environmentId": environment_id,
            "name": pa.get("name").cloned().unwrap_or(Value::Null),
            "description": pa.get("description").cloned().unwrap_or(Value::Null),
            "status": status,
            "createdAt": pa.get("createdAt").cloned().unwrap_or(Value::Null),
            "updatedAt": pa.get("updatedAt").cloned().unwrap_or(Value::Null),
        })),
        _ => {
            tracing::warn!("platform access returned an inconsistent service account view");
            Err(ApiError(ErrorCode::ServiceUnavailable))
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateServiceAccount {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// `POST /v1/environments/{id}/service-accounts`.
pub async fn create(
    state: &AppState,
    caller: &Caller,
    environment_id: Uuid,
    body: CreateServiceAccount,
) -> Result<(StatusCode, Value), ApiError> {
    let env = environment(&state.db, environment_id).await?;
    authorize(state, caller, env.tenant_id, Action::ServiceAccountsManage).await?;
    if !env.delegable() {
        return Err(ApiError(ErrorCode::PolicyViolation));
    }
    let name = name(&body.name)?;
    let desc = description(body.description.as_deref())?;
    let project_status: String = state
        .db
        .query_one(stmt(
            "SELECT status FROM devplat_project_profiles WHERE application_id = $1",
            vec![env.project_id.into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?
        .try_get("", "status")?;
    if env.status != "ACTIVE" || project_status != "ACTIVE" {
        return Err(ApiError(ErrorCode::InvalidState));
    }
    let pa = delegate(
        state,
        caller,
        DelegationCall::CreateServiceAccount {
            actor: caller.delegation_actor(),
            environment_id,
            name: name.clone(),
            description: desc,
        },
    )
    .await?;
    let v = view(&pa, environment_id, &env)?;
    let txn = state.db.begin().await?;
    activity::record(
        &txn,
        caller,
        Event {
            tenant_id: env.tenant_id,
            action: "service_account.created",
            target_type: "SERVICE_ACCOUNT",
            target_id: v["id"].as_str().unwrap_or_default().to_string(),
            summary: json!({
                "projectId": env.project_id, "environmentId": environment_id, "name": name,
            }),
        },
    )
    .await?;
    txn.commit().await?;
    Ok((StatusCode::CREATED, v))
}

/// `GET /v1/environments/{id}/service-accounts` — `(createdAt, id)` keyset pages over
/// Platform Access's list.
pub async fn list(
    state: &AppState,
    caller: &Caller,
    environment_id: Uuid,
    page: &Page,
) -> Result<Value, ApiError> {
    let env = environment(&state.db, environment_id).await?;
    authorize(state, caller, env.tenant_id, Action::ServiceAccountsRead).await?;
    let after = page.cursor_key();
    if page.has_cursor() && after.is_none() {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    let pa = delegate(
        state,
        caller,
        DelegationCall::ListServiceAccounts { environment_id },
    )
    .await?;
    let mut keyed = Vec::new();
    for sa in pa
        .get("serviceAccounts")
        .and_then(Value::as_array)
        .ok_or(ApiError(ErrorCode::ServiceUnavailable))?
    {
        let v = view(sa, environment_id, &env)?;
        let at = v["createdAt"]
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&chrono::Utc))
            .ok_or(ApiError(ErrorCode::ServiceUnavailable))?;
        let id = Uuid::parse_str(v["id"].as_str().unwrap_or_default())
            .map_err(|_| ApiError(ErrorCode::ServiceUnavailable))?;
        // Microsecond keys: the cursor's own precision.
        let at = chrono::DateTime::<chrono::Utc>::from_timestamp_micros(at.timestamp_micros())
            .ok_or(ApiError(ErrorCode::ServiceUnavailable))?;
        keyed.push((at, id, v));
    }
    keyed.sort_by_key(|(at, id, _)| (*at, *id));
    let mut rest = keyed
        .into_iter()
        .filter(|(at, id, _)| after.is_none_or(|k| (*at, *id) > k));
    let mut items = Vec::new();
    let mut last = None;
    for (at, id, v) in rest.by_ref().take(page.limit) {
        items.push(v);
        last = Some((at, id));
    }
    let next = match (rest.next(), last) {
        (Some(_), Some((at, id))) => Some(Page::key_cursor(at, id)),
        _ => None,
    };
    Ok(json!({ "items": items, "nextCursor": next }))
}

/// `GET /v1/environments/{id}/service-accounts/{serviceAccountId}`.
pub async fn get(
    state: &AppState,
    caller: &Caller,
    environment_id: Uuid,
    service_account_id: Uuid,
) -> Result<Value, ApiError> {
    let env = environment(&state.db, environment_id).await?;
    authorize(state, caller, env.tenant_id, Action::ServiceAccountsRead).await?;
    let pa = delegate(
        state,
        caller,
        DelegationCall::GetServiceAccount {
            environment_id,
            service_account_id,
        },
    )
    .await?;
    view(&pa, environment_id, &env)
}

/// `POST …/{serviceAccountId}/suspend` (`suspend`) or `…/resume`: ACTIVE ⇄ SUSPENDED.
pub async fn set_status(
    state: &AppState,
    caller: &Caller,
    environment_id: Uuid,
    service_account_id: Uuid,
    suspend: bool,
) -> Result<Value, ApiError> {
    let env = environment(&state.db, environment_id).await?;
    let action = if suspend {
        Action::ServiceAccountsSuspend
    } else {
        Action::ServiceAccountsManage
    };
    authorize(state, caller, env.tenant_id, action).await?;
    if !env.delegable() {
        return Err(ApiError(ErrorCode::PolicyViolation));
    }
    let (from, to) = if suspend {
        ("ACTIVE", "SUSPENDED")
    } else {
        ("SUSPENDED", "ACTIVE")
    };
    let current = delegate(
        state,
        caller,
        DelegationCall::GetServiceAccount {
            environment_id,
            service_account_id,
        },
    )
    .await?;
    let current = view(&current, environment_id, &env)?;
    if current["status"] != from {
        return Err(ApiError(ErrorCode::InvalidState));
    }
    let pa = delegate(
        state,
        caller,
        DelegationCall::SetServiceAccountStatus {
            actor: caller.delegation_actor(),
            environment_id,
            service_account_id,
            suspended: suspend,
        },
    )
    .await?;
    let v = view(&pa, environment_id, &env)?;
    if v["status"] != to {
        return Err(ApiError(ErrorCode::ServiceUnavailable));
    }
    let txn = state.db.begin().await?;
    activity::record(
        &txn,
        caller,
        Event {
            tenant_id: env.tenant_id,
            action: if suspend {
                "service_account.suspended"
            } else {
                "service_account.resumed"
            },
            target_type: "SERVICE_ACCOUNT",
            target_id: service_account_id.to_string(),
            summary: json!({
                "projectId": env.project_id, "environmentId": environment_id,
                "fromStatus": from, "toStatus": to,
            }),
        },
    )
    .await?;
    txn.commit().await?;
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_platform_access_keys_and_descriptions_are_bounded() {
        for ok in ["ci", "ci-runner-2", "0abc", &"a".repeat(63)] {
            assert!(name(ok).is_ok(), "{ok}");
        }
        for bad in ["", "-a", "CI", "a_b", "a b", &"a".repeat(64)] {
            assert!(name(bad).is_err(), "{bad}");
        }
        assert_eq!(description(Some("  ")).unwrap(), None);
        assert!(description(Some(&"x".repeat(501))).is_err());
        assert!(description(Some("a\u{0007}")).is_err());
    }

    #[test]
    fn only_a_consistent_authority_view_is_shown() {
        let env = EnvironmentRef {
            tenant_id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            kind: "SANDBOX".into(),
            status: "ACTIVE".into(),
            production_access_state: "NOT_APPLICABLE".into(),
        };
        let environment_id = Uuid::new_v4();
        let pa = json!({
            "id": Uuid::new_v4(), "tenantId": env.tenant_id, "applicationId": env.project_id,
            "environmentId": environment_id, "name": "ci", "description": null,
            "status": "ACTIVE", "revision": 1, "revokedAt": null,
            "createdAt": "2026-11-02T10:00:00Z", "updatedAt": "2026-11-02T10:00:00Z",
        });
        let v = view(&pa, environment_id, &env).unwrap();
        assert_eq!(v["organizationId"], json!(env.tenant_id));
        assert!(v.get("revision").is_none());
        let mut other = pa.clone();
        other["tenantId"] = json!(Uuid::new_v4());
        assert!(view(&other, environment_id, &env).is_err());
        assert!(view(&pa, Uuid::new_v4(), &env).is_err());
    }
}
