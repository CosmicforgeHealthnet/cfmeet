//! Effective platform entitlements of an application environment (Section 54 §10.4
//! precedence: Organization → Project → Environment → Service Account / Credential).
//!
//! The organization plan sets the defaults; a project plan may only make them STRICTER
//! unless an explicit, audited operator override (`devplat_entitlement_overrides`)
//! raises them; SANDBOX / TEST environments are capped further; a service account or
//! credential quota restriction can only reduce them; the most restrictive value wins
//! and the response names the limiting level (`ORGANIZATION_PLAN`, `OPERATOR_OVERRIDE`,
//! `PROJECT_PLAN`, `ENVIRONMENT`, `SERVICE_ACCOUNT`, `CREDENTIAL`). Organizations without a
//! plan get the platform default profile. Domain-specific limits are published for their
//! owning services, which keep final enforcement; the environment-level result is also
//! recorded as a versioned snapshot (`snapshotVersion` / `snapshotDigest`) consuming
//! services may cache.

use sea_orm::ConnectionTrait;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::stmt;
use crate::http::error::{ApiError, ErrorCode};

pub const DEFAULT_REQUESTS_PER_MINUTE: u64 = 600;
pub const DEFAULT_REQUESTS_PER_DAY: u64 = 100_000;

/// Environment caps `(per minute, per day)`.
pub fn environment_cap(kind: &str) -> (u64, u64) {
    match kind {
        "SANDBOX" => (120, 10_000),
        "TEST" => (300, 50_000),
        _ => (u64::MAX, u64::MAX),
    }
}

/// The effective entitlement snapshot of `environment_id` (404 when the environment is
/// unknown to the Developer Platform).
///
/// The environment kind is the Developer Platform profile's; for an environment known
/// only by its anchored reference (created outside the console), `confirmed_kind` — the
/// kind Platform Access confirmed to the caller — is used; with neither, the STRICTEST
/// cap applies (an unknown kind never receives production limits).
pub async fn effective<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    confirmed_kind: Option<&str>,
) -> Result<Value, ApiError> {
    effective_for(db, environment_id, confirmed_kind, None, None).await
}

/// `(per minute, per day)` restriction of one subject.
async fn restriction<C: ConnectionTrait>(
    db: &C,
    kind: &str,
    subject: Option<Uuid>,
    environment_id: Uuid,
) -> Result<(Option<u64>, Option<u64>), ApiError> {
    let Some(subject) = subject else {
        return Ok((None, None));
    };
    let row = db
        .query_one(stmt(
            "SELECT api_requests_per_minute, api_requests_per_day FROM devplat_quota_restrictions \
             WHERE subject_kind = $1 AND subject_id = $2 AND environment_id = $3",
            vec![kind.into(), subject.into(), environment_id.into()],
        ))
        .await?;
    Ok(match row {
        Some(r) => (
            r.try_get::<Option<i64>>("", "api_requests_per_minute")?
                .map(|v| v.max(0) as u64),
            r.try_get::<Option<i64>>("", "api_requests_per_day")?
                .map(|v| v.max(0) as u64),
        ),
        None => (None, None),
    })
}

/// [`effective`] including the service-account and credential restrictions of a caller.
pub async fn effective_for<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    confirmed_kind: Option<&str>,
    service_account_id: Option<Uuid>,
    credential_id: Option<Uuid>,
) -> Result<Value, ApiError> {
    let env = db
        .query_one(stmt(
            "SELECT e.tenant_id, e.application_id, e.kind, o.state \
             FROM devplat_environment_refs r \
             LEFT JOIN devplat_environment_profiles e ON e.environment_id = r.environment_id \
             LEFT JOIN devplat_organizations o ON o.tenant_id = r.tenant_id \
             WHERE r.environment_id = $1",
            vec![environment_id.into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?;
    let tenant_id: Option<Uuid> = env.try_get("", "tenant_id")?;
    let application_id: Option<Uuid> = env.try_get("", "application_id")?;
    let kind: Option<String> = env
        .try_get::<Option<String>>("", "kind")?
        .or_else(|| confirmed_kind.map(str::to_string));
    let org_state: Option<String> = env.try_get("", "state")?;
    let plans = match (tenant_id, application_id) {
        (Some(t), Some(a)) => {
            db.query_all(stmt(
                "SELECT a.application_id IS NULL AS tenant_level, c.code, c.version, c.entitlements \
                 FROM devplat_plan_assignments a JOIN devplat_commercial_plans c ON c.id = a.plan_id \
                 WHERE a.tenant_id = $1 AND (a.application_id IS NULL OR a.application_id = $2) \
                   AND a.superseded_by IS NULL",
                vec![t.into(), a.into()],
            ))
            .await?
        }
        _ => vec![],
    };
    let mut tenant_plan: Option<(String, i32, Map<String, Value>)> = None;
    let mut project_plan: Option<(String, i32, Map<String, Value>)> = None;
    for p in &plans {
        let ent: Value = p.try_get("", "entitlements")?;
        let entry = (
            p.try_get::<String>("", "code")?,
            p.try_get::<i32>("", "version")?,
            ent.as_object().cloned().unwrap_or_default(),
        );
        if p.try_get::<bool>("", "tenant_level")? {
            tenant_plan = Some(entry);
        } else {
            project_plan = Some(entry);
        }
    }
    // Explicit operator overrides (organization-wide or for this project).
    let mut tenant_override: Option<Map<String, Value>> = None;
    let mut project_override: Option<Map<String, Value>> = None;
    if let (Some(t), Some(a)) = (tenant_id, application_id) {
        for o in db
            .query_all(stmt(
                "SELECT application_id, entitlements FROM devplat_entitlement_overrides \
                 WHERE tenant_id = $1 AND (application_id IS NULL OR application_id = $2) \
                   AND revoked_at IS NULL",
                vec![t.into(), a.into()],
            ))
            .await?
        {
            let ent: Value = o.try_get("", "entitlements")?;
            let m = ent.as_object().cloned().unwrap_or_default();
            if o.try_get::<Option<Uuid>>("", "application_id")?.is_some() {
                project_override = Some(m);
            } else {
                tenant_override = Some(m);
            }
        }
    }
    let (cap_min, cap_day) = environment_cap(kind.as_deref().unwrap_or("SANDBOX"));
    let (sa_min, sa_day) =
        restriction(db, "SERVICE_ACCOUNT", service_account_id, environment_id).await?;
    let (cr_min, cr_day) = restriction(db, "CREDENTIAL", credential_id, environment_id).await?;
    let resolve = |key: &str,
                   default: u64,
                   cap: u64,
                   sa: Option<u64>,
                   cred: Option<u64>|
     -> (u64, &'static str) {
        let mut value = tenant_plan
            .as_ref()
            .and_then(|(_, _, e)| e.get(key).and_then(Value::as_u64))
            .map(|v| (v, "ORGANIZATION_PLAN"))
            .unwrap_or((default, "PLATFORM_DEFAULT"));
        if let Some(o) = tenant_override
            .as_ref()
            .and_then(|e| e.get(key).and_then(Value::as_u64))
        {
            value = (o, "OPERATOR_OVERRIDE");
        }
        if let Some(o) = project_override
            .as_ref()
            .and_then(|e| e.get(key).and_then(Value::as_u64))
        {
            // The explicit operator override is the only way past the organization plan.
            value = (o, "OPERATOR_OVERRIDE");
        } else if let Some(p) = project_plan
            .as_ref()
            .and_then(|(_, _, e)| e.get(key).and_then(Value::as_u64))
        {
            // A project plan can only be stricter (no implicit upgrade).
            if p < value.0 {
                value = (p, "PROJECT_PLAN");
            }
        }
        if cap < value.0 {
            value = (cap, "ENVIRONMENT");
        }
        if let Some(v) = sa.filter(|v| *v < value.0) {
            value = (v, "SERVICE_ACCOUNT");
        }
        if let Some(v) = cred.filter(|v| *v < value.0) {
            value = (v, "CREDENTIAL");
        }
        value
    };
    let (rpm, rpm_level) = resolve(
        "apiRequestsPerMinute",
        DEFAULT_REQUESTS_PER_MINUTE,
        cap_min,
        sa_min,
        cr_min,
    );
    let (rpd, rpd_level) = resolve(
        "apiRequestsPerDay",
        DEFAULT_REQUESTS_PER_DAY,
        cap_day,
        sa_day,
        cr_day,
    );
    // Domain entitlements: the stricter of organization and project plan values.
    let mut domain = Map::new();
    for key in super::commercial::ENTITLEMENT_KEYS
        .iter()
        .filter(|k| !k.starts_with("apiRequests"))
    {
        let t = tenant_plan
            .as_ref()
            .and_then(|(_, _, e)| e.get(*key).and_then(Value::as_u64));
        let p = project_plan
            .as_ref()
            .and_then(|(_, _, e)| e.get(*key).and_then(Value::as_u64));
        let o = project_override
            .as_ref()
            .or(tenant_override.as_ref())
            .and_then(|e| e.get(*key).and_then(Value::as_u64));
        let v = match (o, t, p) {
            (Some(o), _, _) => Some(o),
            (None, Some(t), Some(p)) => Some(t.min(p)),
            (None, a, b) => a.or(b),
        };
        if let Some(v) = v {
            domain.insert((*key).to_string(), Value::from(v));
        }
    }
    let suspended = matches!(org_state.as_deref(), Some("SUSPENDED" | "CLOSED"));
    Ok(json!({
        "environmentId": environment_id,
        "organizationId": tenant_id,
        "projectId": application_id,
        "environmentKind": kind,
        "organizationState": org_state,
        "apiRequestsPerMinute": if suspended { 0 } else { rpm },
        "apiRequestsPerDay": if suspended { 0 } else { rpd },
        "limitingLevel": { "apiRequestsPerMinute": rpm_level, "apiRequestsPerDay": rpd_level },
        "plan": tenant_plan.as_ref().map(|(c, v, _)| json!({ "code": c, "version": v })),
        "projectPlan": project_plan.as_ref().map(|(c, v, _)| json!({ "code": c, "version": v })),
        "domainEntitlements": domain,
        "subject": {
            "serviceAccountId": service_account_id,
            "credentialId": credential_id,
        },
    }))
}

/// The environment-level effective entitlements with their snapshot version: a new
/// version is appended whenever the effective value changes (consumers cache by version
/// and must fail safely when no snapshot can be read).
pub async fn snapshot<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    confirmed_kind: Option<&str>,
) -> Result<Value, ApiError> {
    let mut v = effective(db, environment_id, confirmed_kind).await?;
    let mut content = v.clone();
    if let Some(o) = content.as_object_mut() {
        o.remove("subject");
    }
    let digest = crate::control::idempotency::digest(&[content.to_string().as_bytes()]);
    let latest = db
        .query_one(stmt(
            "SELECT version, snapshot_digest, computed_at FROM devplat_entitlement_snapshots \
             WHERE environment_id = $1 ORDER BY version DESC LIMIT 1",
            vec![environment_id.into()],
        ))
        .await?;
    let (version, at) = match latest {
        Some(r) if r.try_get::<String>("", "snapshot_digest")? == digest => (
            r.try_get::<i64>("", "version")?,
            r.try_get::<chrono::DateTime<chrono::Utc>>("", "computed_at")?,
        ),
        other => {
            let next = other
                .map(|r| r.try_get::<i64>("", "version"))
                .transpose()?
                .unwrap_or(0)
                + 1;
            let now = chrono::Utc::now();
            db.execute(stmt(
                "INSERT INTO devplat_entitlement_snapshots (environment_id, version, snapshot, \
                   snapshot_digest, computed_at) VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING",
                vec![
                    environment_id.into(),
                    next.into(),
                    content.clone().into(),
                    digest.clone().into(),
                    now.into(),
                ],
            ))
            .await?;
            (next, now)
        }
    };
    v["snapshotVersion"] = Value::from(version);
    v["snapshotDigest"] = Value::from(digest);
    v["snapshotAt"] = Value::from(at.to_rfc3339());
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::environment_cap;

    #[test]
    fn non_production_environments_are_capped() {
        assert!(environment_cap("SANDBOX").0 < environment_cap("TEST").0);
        assert_eq!(environment_cap("PRODUCTION").0, u64::MAX);
    }
}

/// An explicit operator override (Section 54 §10.4: the only way a project exceeds its
/// organization plan).
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetOverride {
    #[serde(default)]
    pub project_id: Option<Uuid>,
    pub entitlements: Value,
    pub reason_code: String,
}

/// `POST /internal/v1/operator/organizations/{id}/entitlement-overrides` — replaces the
/// active override of the organization (or one of its projects); audited.
pub async fn operator_set_override(
    state: &crate::state::AppState,
    operator: &str,
    request: &crate::domain::RequestContext,
    tenant_id: Uuid,
    body: SetOverride,
) -> Result<Value, ApiError> {
    use sea_orm::TransactionTrait;
    super::commercial::validate_entitlements(&body.entitlements)?;
    if !super::operator::valid_reason(&body.reason_code) {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    if super::organization_state(&state.db, tenant_id)
        .await?
        .is_none()
    {
        return Err(ApiError(ErrorCode::NotFound));
    }
    if let Some(p) = body.project_id {
        let owner: Option<Uuid> = state
            .db
            .query_one(stmt(
                "SELECT tenant_id FROM devplat_project_profiles WHERE application_id = $1",
                vec![p.into()],
            ))
            .await?
            .map(|r| r.try_get("", "tenant_id"))
            .transpose()?;
        if owner != Some(tenant_id) {
            return Err(ApiError(ErrorCode::NotFound));
        }
    }
    let now = chrono::Utc::now();
    let id = Uuid::new_v4();
    let actor = format!("operator:{operator}");
    let txn = state.db.begin().await?;
    txn.execute(stmt(
        "UPDATE devplat_entitlement_overrides SET revoked_at = $3, revoked_by = $4 \
         WHERE tenant_id = $1 AND application_id IS NOT DISTINCT FROM $2 AND revoked_at IS NULL",
        vec![
            tenant_id.into(),
            body.project_id.into(),
            now.into(),
            actor.clone().into(),
        ],
    ))
    .await?;
    txn.execute(stmt(
        "INSERT INTO devplat_entitlement_overrides (id, tenant_id, application_id, entitlements, \
           reason_code, created_by, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        vec![
            id.into(),
            tenant_id.into(),
            body.project_id.into(),
            body.entitlements.clone().into(),
            body.reason_code.clone().into(),
            actor.into(),
            now.into(),
        ],
    ))
    .await?;
    super::activity::record_operator(
        &txn,
        operator,
        request.request_id.as_str(),
        request.correlation_id.as_str(),
        super::activity::Event {
            tenant_id,
            action: "entitlement_override.set",
            target_type: "ORGANIZATION",
            target_id: tenant_id.to_string(),
            summary: serde_json::json!({ "overrideId": id, "projectId": body.project_id,
                "entitlements": body.entitlements, "reasonCode": body.reason_code }),
        },
    )
    .await?;
    txn.commit().await?;
    Ok(
        serde_json::json!({ "id": id, "organizationId": tenant_id, "projectId": body.project_id,
        "entitlements": body.entitlements, "reasonCode": body.reason_code, "createdAt": now }),
    )
}

/// `DELETE /internal/v1/operator/organizations/{id}/entitlement-overrides/{overrideId}`.
pub async fn operator_revoke_override(
    state: &crate::state::AppState,
    operator: &str,
    request: &crate::domain::RequestContext,
    tenant_id: Uuid,
    override_id: Uuid,
) -> Result<Value, ApiError> {
    use sea_orm::TransactionTrait;
    let txn = state.db.begin().await?;
    let n = txn
        .execute(stmt(
            "UPDATE devplat_entitlement_overrides SET revoked_at = $3, revoked_by = $4 \
             WHERE id = $1 AND tenant_id = $2 AND revoked_at IS NULL",
            vec![
                override_id.into(),
                tenant_id.into(),
                chrono::Utc::now().into(),
                format!("operator:{operator}").into(),
            ],
        ))
        .await?
        .rows_affected();
    if n != 1 {
        return Err(ApiError(ErrorCode::NotFound));
    }
    super::activity::record_operator(
        &txn,
        operator,
        request.request_id.as_str(),
        request.correlation_id.as_str(),
        super::activity::Event {
            tenant_id,
            action: "entitlement_override.revoked",
            target_type: "ORGANIZATION",
            target_id: tenant_id.to_string(),
            summary: serde_json::json!({ "overrideId": override_id }),
        },
    )
    .await?;
    txn.commit().await?;
    Ok(serde_json::json!({ "id": override_id, "revoked": true }))
}
