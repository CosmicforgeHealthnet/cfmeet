//! Section 54 — API credential self-service over Platform Access.
//!
//! Platform Access stays the ONLY credential authority: issuance, the credential format,
//! verifier construction, rotation with bounded overlap, revocation, suspension and
//! scope validation all happen there (`/internal/v1/delegation/**`). The Developer
//! Platform authorizes the caller, applies the organization's credential policy (lifetime
//! and overlap caps), keeps machine callers from granting more than they hold, and
//! records the activity. It stores no credential, verifier or secret: the raw secret is
//! relayed from Platform Access's response exactly once and is redacted from every
//! idempotent replay (a lost response is recovered by rotating or creating a new
//! credential, never by redisplaying the secret).
//!
//! * OWNER / ADMIN / DEVELOPER manage credentials (machines: `credentials.manage`).
//! * Credentials of PRODUCTION environments exist only once the environment is LIVE
//!   through the production-access workflow (§55); their scopes are bounded by the
//!   approved capabilities Platform Access granted the application for PRODUCTION.
//! * Revocation and suspension stay possible in a RESTRICTED organization (withdrawing
//!   access is always safe); resumption is not.
//! * Quota restrictions of a credential / service account can only make the effective
//!   API limits stricter (§54 §10.4).

use axum::http::StatusCode;
use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    activity::{self, Event},
    authorize, delegate,
    service_accounts::{environment, EnvironmentRef},
    stmt, Caller, Principal,
};
use crate::{
    domain::rbac::Action,
    http::error::{ApiError, ErrorCode},
    platform_access::DelegationCall,
    state::AppState,
};

/// Removes the one-time secret from the stored idempotent replay.
pub fn redact_secret(v: &mut Value) {
    if let Some(o) = v.as_object_mut() {
        if o.remove("secret").is_some() {
            o.insert("secretRedacted".into(), Value::Bool(true));
        }
    }
}

/// The organization's credential policy (None: Platform Access's platform policy only).
pub async fn policy<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
) -> Result<Option<(i64, i64)>, ApiError> {
    let row = db
        .query_one(stmt(
            "SELECT max_lifetime_days, max_rotation_overlap_minutes FROM devplat_org_credential_policies \
             WHERE tenant_id = $1",
            vec![tenant_id.into()],
        ))
        .await?;
    Ok(match row {
        Some(r) => Some((
            r.try_get::<i32>("", "max_lifetime_days")? as i64,
            r.try_get::<i32>("", "max_rotation_overlap_minutes")? as i64,
        )),
        None => None,
    })
}

/// The expiry to request: the explicit one (within the organization's lifetime cap) or,
/// with a policy, `now + max lifetime`.
fn expiry(
    requested: Option<DateTime<Utc>>,
    policy: Option<(i64, i64)>,
) -> Result<Option<String>, ApiError> {
    let now = Utc::now();
    let cap = policy.map(|(days, _)| now + Duration::days(days));
    let chosen = match (requested, cap) {
        (Some(r), _) if r <= now => return Err(ApiError(ErrorCode::ValidationError)),
        (Some(r), Some(c)) if r > c => return Err(ApiError(ErrorCode::PolicyViolation)),
        (Some(r), _) => Some(r),
        (None, c) => c,
    };
    Ok(chosen.map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)))
}

fn label(value: Option<&str>) -> Result<Option<String>, ApiError> {
    match value.map(str::trim) {
        None | Some("") => Ok(None),
        Some(l) if l.chars().count() <= 100 && !l.chars().any(char::is_control) => {
            Ok(Some(l.to_string()))
        }
        Some(_) => Err(ApiError(ErrorCode::ValidationError)),
    }
}

fn scopes(requested: &[String]) -> Result<Vec<String>, ApiError> {
    if requested.len() > 64
        || requested.iter().any(|s| {
            s.is_empty()
                || s.len() > 100
                || !s
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
        })
    {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    let mut v = requested.to_vec();
    v.sort();
    v.dedup();
    Ok(v)
}

/// A machine caller can never grant a credential more than it holds itself.
async fn within_caller(
    state: &AppState,
    caller: &Caller,
    requested: &[String],
) -> Result<(), ApiError> {
    if let Principal::Machine(m) = &caller.principal {
        let refs: Vec<&str> = requested.iter().map(String::as_str).collect();
        let ok = state
            .platform_access
            .credential_allows(
                &m.delegated,
                &caller.request.correlation_id,
                m.credential_id,
                &refs,
            )
            .await
            .map_err(|_| ApiError(ErrorCode::ServiceUnavailable))?;
        if !ok {
            return Err(ApiError(ErrorCode::InsufficientScope));
        }
    }
    Ok(())
}

/// The quota restriction of a subject, as shown on its view.
pub async fn restriction<C: ConnectionTrait>(
    db: &C,
    kind: &str,
    subject: Uuid,
) -> Result<Value, ApiError> {
    let row = db
        .query_one(stmt(
            "SELECT api_requests_per_minute, api_requests_per_day FROM devplat_quota_restrictions \
             WHERE subject_kind = $1 AND subject_id = $2",
            vec![kind.into(), subject.into()],
        ))
        .await?;
    Ok(match row {
        Some(r) => json!({
            "apiRequestsPerMinute": r.try_get::<Option<i64>>("", "api_requests_per_minute")?,
            "apiRequestsPerDay": r.try_get::<Option<i64>>("", "api_requests_per_day")?,
        }),
        None => Value::Null,
    })
}

/// The customer view of a Platform Access credential of `env` (no verifier fields; any
/// inconsistency fails closed).
fn view(pa: &Value, environment_id: Uuid, env: &EnvironmentRef) -> Result<Value, ApiError> {
    let id_of = |k: &str| {
        pa.get(k)
            .and_then(Value::as_str)
            .and_then(|s| Uuid::parse_str(s).ok())
    };
    let consistent = id_of("environmentId") == Some(environment_id)
        && id_of("tenantId") == Some(env.tenant_id)
        && id_of("applicationId") == Some(env.project_id);
    let Some(id) = id_of("id").filter(|_| consistent) else {
        tracing::warn!("platform access returned an inconsistent credential view");
        return Err(ApiError(ErrorCode::ServiceUnavailable));
    };
    let f = |k: &str| pa.get(k).cloned().unwrap_or(Value::Null);
    Ok(json!({
        "id": id,
        "organizationId": env.tenant_id,
        "projectId": env.project_id,
        "environmentId": environment_id,
        "environmentKind": f("environmentKind"),
        "serviceAccountId": f("serviceAccountId"),
        "publicId": f("publicId"),
        "masked": f("masked"),
        "label": f("label"),
        "scopes": f("scopes"),
        "status": f("status"),
        "effectiveState": f("effectiveState"),
        "notBefore": f("notBefore"),
        "expiresAt": f("expiresAt"),
        "revokedAt": f("revokedAt"),
        "suspendedAt": f("suspendedAt"),
        "rotatedFromCredentialId": f("rotatedFromCredentialId"),
        "supersededByCredentialId": f("supersededByCredentialId"),
        "supersessionValidUntil": f("supersessionValidUntil"),
        "createdAt": f("createdAt"),
        "updatedAt": f("updatedAt"),
    }))
}

async fn with_limits<C: ConnectionTrait>(db: &C, mut v: Value) -> Result<Value, ApiError> {
    let id = v["id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or(ApiError(ErrorCode::ServiceUnavailable))?;
    v["limits"] = restriction(db, "CREDENTIAL", id).await?;
    Ok(v)
}

async fn record(
    state: &AppState,
    caller: &Caller,
    env: &EnvironmentRef,
    action: &str,
    target: &str,
    summary: Value,
) -> Result<(), ApiError> {
    let txn = state.db.begin().await?;
    activity::record(
        &txn,
        caller,
        Event {
            tenant_id: env.tenant_id,
            action,
            target_type: "CREDENTIAL",
            target_id: target.to_string(),
            summary,
        },
    )
    .await?;
    txn.commit().await?;
    Ok(())
}

/// Common preconditions of an issuing operation (create / rotate / resume).
async fn issuable(
    state: &AppState,
    caller: &Caller,
    environment_id: Uuid,
    action: Action,
) -> Result<EnvironmentRef, ApiError> {
    let env = environment(&state.db, environment_id).await?;
    authorize(state, caller, env.tenant_id, action).await?;
    if !env.delegable() {
        return Err(ApiError(ErrorCode::ProductionAccessRequired));
    }
    if env.status != "ACTIVE" {
        return Err(ApiError(ErrorCode::InvalidState));
    }
    Ok(env)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateCredential {
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
}

/// `POST /v1/environments/{id}/service-accounts/{sa}/credentials` — the raw secret is in
/// this response only.
pub async fn create(
    state: &AppState,
    caller: &Caller,
    environment_id: Uuid,
    service_account_id: Uuid,
    body: CreateCredential,
) -> Result<(StatusCode, Value), ApiError> {
    let env = issuable(state, caller, environment_id, Action::CredentialsManage).await?;
    let scopes = scopes(&body.scopes)?;
    let label = label(body.label.as_deref())?;
    within_caller(state, caller, &scopes).await?;
    let expires_at = expiry(body.expires_at, policy(&state.db, env.tenant_id).await?)?;
    let pa = delegate(
        state,
        caller,
        DelegationCall::IssueCredential {
            actor: caller.delegation_actor(),
            environment_id,
            service_account_id,
            scopes: scopes.clone(),
            label,
            expires_at,
        },
    )
    .await?;
    let credential = with_limits(&state.db, view(&pa["credential"], environment_id, &env)?).await?;
    let secret = pa
        .get("secret")
        .and_then(Value::as_str)
        .ok_or(ApiError(ErrorCode::ServiceUnavailable))?
        .to_string();
    let id = credential["id"].as_str().unwrap_or_default().to_string();
    record(
        state,
        caller,
        &env,
        "credential.created",
        &id,
        json!({ "environmentId": environment_id, "serviceAccountId": service_account_id,
                "scopes": scopes, "publicId": credential["publicId"] }),
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        json!({ "credential": credential, "secret": secret }),
    ))
}

/// `GET /v1/environments/{id}/service-accounts/{sa}/credentials`.
pub async fn list(
    state: &AppState,
    caller: &Caller,
    environment_id: Uuid,
    service_account_id: Uuid,
    page: &super::pagination::Page,
) -> Result<Value, ApiError> {
    if page.has_cursor() && page.cursor_key().is_none() {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    let env = environment(&state.db, environment_id).await?;
    authorize(state, caller, env.tenant_id, Action::CredentialsRead).await?;
    let pa = delegate(
        state,
        caller,
        DelegationCall::ListCredentials {
            environment_id,
            service_account_id,
        },
    )
    .await?;
    // Newest first, keyset `(createdAt, id)` over the service account's credentials.
    let mut all: Vec<(DateTime<Utc>, Uuid, Value)> = Vec::new();
    for c in pa
        .get("credentials")
        .and_then(Value::as_array)
        .ok_or(ApiError(ErrorCode::ServiceUnavailable))?
    {
        let v = view(c, environment_id, &env)?;
        let at = v["createdAt"]
            .as_str()
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&Utc))
            .ok_or(ApiError(ErrorCode::ServiceUnavailable))?;
        let id = v["id"]
            .as_str()
            .and_then(|i| Uuid::parse_str(i).ok())
            .ok_or(ApiError(ErrorCode::ServiceUnavailable))?;
        all.push((at, id, v));
    }
    all.sort_by_key(|x| std::cmp::Reverse((x.0, x.1)));
    let after = page.cursor_key();
    let mut rest = all
        .into_iter()
        .filter(|(at, id, _)| {
            after.is_none_or(|(ca, ci)| (at.timestamp_micros(), *id) < (ca.timestamp_micros(), ci))
        })
        .peekable();
    let mut items = Vec::new();
    let mut last = None;
    while items.len() < page.limit {
        let Some((at, id, v)) = rest.next() else {
            break;
        };
        last = Some((at, id));
        items.push(with_limits(&state.db, v).await?);
    }
    let next = rest
        .peek()
        .is_some()
        .then(|| last.map(|(at, id)| super::pagination::Page::key_cursor(at, id)))
        .flatten();
    Ok(json!({ "items": items, "nextCursor": next }))
}

/// `GET /v1/environments/{id}/credentials/{credentialId}`.
pub async fn get(
    state: &AppState,
    caller: &Caller,
    environment_id: Uuid,
    credential_id: Uuid,
) -> Result<Value, ApiError> {
    let env = environment(&state.db, environment_id).await?;
    authorize(state, caller, env.tenant_id, Action::CredentialsRead).await?;
    let pa = delegate(
        state,
        caller,
        DelegationCall::GetCredential {
            environment_id,
            credential_id,
        },
    )
    .await?;
    with_limits(&state.db, view(&pa, environment_id, &env)?).await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RotateCredential {
    #[serde(default)]
    pub overlap_minutes: Option<i64>,
    #[serde(default)]
    pub scopes: Option<Vec<String>>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
}

/// `POST …/credentials/{id}/rotate` — an independent successor (secret once); the
/// predecessor keeps working for at most the bounded overlap.
pub async fn rotate(
    state: &AppState,
    caller: &Caller,
    environment_id: Uuid,
    credential_id: Uuid,
    body: RotateCredential,
) -> Result<(StatusCode, Value), ApiError> {
    let env = issuable(state, caller, environment_id, Action::CredentialsManage).await?;
    let policy = policy(&state.db, env.tenant_id).await?;
    if let Some(m) = body.overlap_minutes {
        let max = policy.map(|(_, o)| o).unwrap_or(i64::MAX);
        if m < 0 {
            return Err(ApiError(ErrorCode::ValidationError));
        }
        if m > max {
            return Err(ApiError(ErrorCode::PolicyViolation));
        }
    }
    // Without an explicit overlap, an organization policy bounds it at its maximum.
    let overlap = body.overlap_minutes.or(policy.map(|(_, o)| o));
    let new_scopes = match &body.scopes {
        Some(s) => {
            let s = scopes(s)?;
            within_caller(state, caller, &s).await?;
            Some(s)
        }
        None => None,
    };
    let label = label(body.label.as_deref())?;
    let expires_at = expiry(body.expires_at, policy)?;
    let pa = delegate(
        state,
        caller,
        DelegationCall::RotateCredential {
            actor: caller.delegation_actor(),
            environment_id,
            credential_id,
            overlap_minutes: overlap,
            scopes: new_scopes,
            label,
            expires_at,
        },
    )
    .await?;
    let credential = with_limits(&state.db, view(&pa["credential"], environment_id, &env)?).await?;
    let secret = pa
        .get("secret")
        .and_then(Value::as_str)
        .ok_or(ApiError(ErrorCode::ServiceUnavailable))?
        .to_string();
    record(
        state,
        caller,
        &env,
        "credential.rotated",
        &credential_id.to_string(),
        json!({ "environmentId": environment_id, "successorCredentialId": credential["id"] }),
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        json!({ "credential": credential, "secret": secret, "rotatedFromCredentialId": credential_id }),
    ))
}

/// `POST …/credentials/{id}/revoke` — immediate and idempotent.
pub async fn revoke(
    state: &AppState,
    caller: &Caller,
    environment_id: Uuid,
    credential_id: Uuid,
) -> Result<Value, ApiError> {
    let env = environment(&state.db, environment_id).await?;
    authorize(state, caller, env.tenant_id, Action::CredentialsRevoke).await?;
    let pa = delegate(
        state,
        caller,
        DelegationCall::RevokeCredential {
            actor: caller.delegation_actor(),
            environment_id,
            credential_id,
        },
    )
    .await?;
    let v = with_limits(&state.db, view(&pa, environment_id, &env)?).await?;
    record(
        state,
        caller,
        &env,
        "credential.revoked",
        &credential_id.to_string(),
        json!({ "environmentId": environment_id }),
    )
    .await?;
    Ok(v)
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SuspendCredential {
    #[serde(default)]
    pub reason_code: Option<String>,
}

/// `POST …/credentials/{id}/suspend` (reversible) / `…/resume`.
pub async fn set_suspended(
    state: &AppState,
    caller: &Caller,
    environment_id: Uuid,
    credential_id: Uuid,
    suspend: Option<SuspendCredential>,
) -> Result<Value, ApiError> {
    let env = match &suspend {
        Some(_) => {
            let env = environment(&state.db, environment_id).await?;
            authorize(state, caller, env.tenant_id, Action::CredentialsRevoke).await?;
            env
        }
        None => issuable(state, caller, environment_id, Action::CredentialsManage).await?,
    };
    let call = match &suspend {
        Some(b) => {
            let reason = b
                .reason_code
                .clone()
                .unwrap_or_else(|| "CUSTOMER_REQUEST".into());
            if !super::operator::valid_reason(&reason) {
                return Err(ApiError(ErrorCode::ValidationError));
            }
            DelegationCall::SuspendCredential {
                actor: caller.delegation_actor(),
                environment_id,
                credential_id,
                reason_code: reason,
            }
        }
        None => DelegationCall::ResumeCredential {
            actor: caller.delegation_actor(),
            environment_id,
            credential_id,
        },
    };
    let pa = delegate(state, caller, call).await?;
    let v = with_limits(&state.db, view(&pa, environment_id, &env)?).await?;
    record(
        state,
        caller,
        &env,
        if suspend.is_some() {
            "credential.suspended"
        } else {
            "credential.resumed"
        },
        &credential_id.to_string(),
        json!({ "environmentId": environment_id }),
    )
    .await?;
    Ok(v)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuotaRestriction {
    #[serde(default)]
    pub api_requests_per_minute: Option<u64>,
    #[serde(default)]
    pub api_requests_per_day: Option<u64>,
}

/// `PUT …/credentials/{id}/limits` and `PUT …/service-accounts/{id}/limits` — a stricter
/// API quota for one credential / service account (`null` values lift a dimension).
pub async fn set_restriction(
    state: &AppState,
    caller: &Caller,
    environment_id: Uuid,
    subject_kind: &'static str,
    subject_id: Uuid,
    body: QuotaRestriction,
) -> Result<Value, ApiError> {
    let env = environment(&state.db, environment_id).await?;
    authorize(state, caller, env.tenant_id, Action::CredentialsManage).await?;
    if body
        .api_requests_per_minute
        .is_some_and(|v| v > 1_000_000_000)
        || body
            .api_requests_per_day
            .is_some_and(|v| v > 1_000_000_000_000)
    {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    // The subject must belong to exactly this environment (Platform Access decides).
    let call = match subject_kind {
        "CREDENTIAL" => DelegationCall::GetCredential {
            environment_id,
            credential_id: subject_id,
        },
        _ => DelegationCall::GetServiceAccount {
            environment_id,
            service_account_id: subject_id,
        },
    };
    let pa = delegate(state, caller, call).await?;
    if pa.get("environmentId").and_then(Value::as_str) != Some(environment_id.to_string().as_str())
    {
        return Err(ApiError(ErrorCode::NotFound));
    }
    let now = Utc::now();
    let txn = state.db.begin().await?;
    txn.execute(stmt(
        "INSERT INTO devplat_quota_restrictions (id, tenant_id, application_id, environment_id, \
           subject_kind, subject_id, api_requests_per_minute, api_requests_per_day, updated_by, \
           created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10) \
         ON CONFLICT (subject_kind, subject_id) DO UPDATE SET \
           api_requests_per_minute = EXCLUDED.api_requests_per_minute, \
           api_requests_per_day = EXCLUDED.api_requests_per_day, updated_by = EXCLUDED.updated_by, \
           updated_at = EXCLUDED.updated_at, version = devplat_quota_restrictions.version + 1",
        vec![
            Uuid::new_v4().into(),
            env.tenant_id.into(),
            env.project_id.into(),
            environment_id.into(),
            subject_kind.into(),
            subject_id.into(),
            body.api_requests_per_minute.map(|v| v as i64).into(),
            body.api_requests_per_day.map(|v| v as i64).into(),
            caller.actor_ref().into(),
            now.into(),
        ],
    ))
    .await?;
    activity::record(
        &txn,
        caller,
        Event {
            tenant_id: env.tenant_id,
            action: "quota_restriction.set",
            target_type: subject_kind,
            target_id: subject_id.to_string(),
            summary: json!({ "environmentId": environment_id,
                             "apiRequestsPerMinute": body.api_requests_per_minute,
                             "apiRequestsPerDay": body.api_requests_per_day }),
        },
    )
    .await?;
    let v = restriction(&txn, subject_kind, subject_id).await?;
    txn.commit().await?;
    Ok(
        json!({ "subjectKind": subject_kind, "subjectId": subject_id,
               "environmentId": environment_id, "limits": v }),
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialPolicy {
    pub max_lifetime_days: i64,
    pub max_rotation_overlap_minutes: i64,
}

fn policy_view(tenant_id: Uuid, p: Option<(i64, i64)>) -> Value {
    match p {
        Some((days, overlap)) => json!({ "organizationId": tenant_id, "configured": true,
            "maxLifetimeDays": days, "maxRotationOverlapMinutes": overlap }),
        None => json!({ "organizationId": tenant_id, "configured": false,
            "maxLifetimeDays": null, "maxRotationOverlapMinutes": null }),
    }
}

/// `GET /v1/organizations/{id}/credential-policy`.
pub async fn get_policy(
    state: &AppState,
    caller: &Caller,
    tenant_id: Uuid,
) -> Result<Value, ApiError> {
    authorize(state, caller, tenant_id, Action::OrganizationRead).await?;
    Ok(policy_view(tenant_id, policy(&state.db, tenant_id).await?))
}

/// `PUT /v1/organizations/{id}/credential-policy` (OWNER / ADMIN).
pub async fn put_policy(
    state: &AppState,
    caller: &Caller,
    tenant_id: Uuid,
    body: CredentialPolicy,
) -> Result<Value, ApiError> {
    authorize(state, caller, tenant_id, Action::OrganizationUpdate).await?;
    if !(1..=730).contains(&body.max_lifetime_days)
        || !(0..=10_080).contains(&body.max_rotation_overlap_minutes)
    {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    let txn = state.db.begin().await?;
    txn.execute(stmt(
        "INSERT INTO devplat_org_credential_policies (tenant_id, max_lifetime_days, \
           max_rotation_overlap_minutes, updated_by, updated_at) VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (tenant_id) DO UPDATE SET max_lifetime_days = EXCLUDED.max_lifetime_days, \
           max_rotation_overlap_minutes = EXCLUDED.max_rotation_overlap_minutes, \
           updated_by = EXCLUDED.updated_by, updated_at = EXCLUDED.updated_at, \
           version = devplat_org_credential_policies.version + 1",
        vec![
            tenant_id.into(),
            (body.max_lifetime_days as i32).into(),
            (body.max_rotation_overlap_minutes as i32).into(),
            caller.actor_ref().into(),
            Utc::now().into(),
        ],
    ))
    .await?;
    activity::record(
        &txn,
        caller,
        Event {
            tenant_id,
            action: "credential_policy.updated",
            target_type: "ORGANIZATION",
            target_id: tenant_id.to_string(),
            summary: json!({ "maxLifetimeDays": body.max_lifetime_days,
                             "maxRotationOverlapMinutes": body.max_rotation_overlap_minutes }),
        },
    )
    .await?;
    txn.commit().await?;
    Ok(policy_view(
        tenant_id,
        Some((body.max_lifetime_days, body.max_rotation_overlap_minutes)),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_redacted_from_replays() {
        let mut v = json!({ "credential": { "id": "x" }, "secret": "cfmk_live_x" });
        redact_secret(&mut v);
        assert!(v.get("secret").is_none());
        assert_eq!(v["secretRedacted"], true);
    }

    #[test]
    fn the_organization_policy_caps_lifetime() {
        let p = Some((30, 60));
        assert!(expiry(None, p).unwrap().is_some());
        assert!(expiry(None, None).unwrap().is_none());
        assert!(matches!(
            expiry(Some(Utc::now() + Duration::days(31)), p),
            Err(ApiError(ErrorCode::PolicyViolation))
        ));
        assert!(expiry(Some(Utc::now() - Duration::days(1)), p).is_err());
        assert!(expiry(Some(Utc::now() + Duration::days(10)), p).is_ok());
    }

    #[test]
    fn requested_scopes_are_bounded_registry_names() {
        assert_eq!(
            scopes(&["calls.read".into(), "calls.read".into()]).unwrap(),
            ["calls.read"]
        );
        assert!(scopes(&["Calls.Read".into()]).is_err());
        assert!(scopes(&["".into()]).is_err());
    }
}
