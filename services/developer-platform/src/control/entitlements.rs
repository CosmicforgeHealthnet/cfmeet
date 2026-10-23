//! Effective platform entitlements of an application environment (Section 54 §10.4
//! precedence, the part the Section 51 gateway quotas need).
//!
//! The organization plan sets the defaults; a project plan may only make them STRICTER;
//! SANDBOX / TEST environments are capped further; the most restrictive value wins and
//! the response names the limiting level. Organizations without a plan get the platform
//! default profile. Domain-specific limits are published for their owning services,
//! which keep final enforcement.

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
pub async fn effective<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
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
    let kind: Option<String> = env.try_get("", "kind")?;
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
    let (cap_min, cap_day) = environment_cap(kind.as_deref().unwrap_or("PRODUCTION"));
    let resolve = |key: &str, default: u64, cap: u64| -> (u64, &'static str) {
        let mut value = tenant_plan
            .as_ref()
            .and_then(|(_, _, e)| e.get(key).and_then(Value::as_u64))
            .map(|v| (v, "ORGANIZATION_PLAN"))
            .unwrap_or((default, "PLATFORM_DEFAULT"));
        if let Some(p) = project_plan
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
        value
    };
    let (rpm, rpm_level) = resolve("apiRequestsPerMinute", DEFAULT_REQUESTS_PER_MINUTE, cap_min);
    let (rpd, rpd_level) = resolve("apiRequestsPerDay", DEFAULT_REQUESTS_PER_DAY, cap_day);
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
        let v = match (t, p) {
            (Some(t), Some(p)) => Some(t.min(p)),
            (a, b) => a.or(b),
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
    }))
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
