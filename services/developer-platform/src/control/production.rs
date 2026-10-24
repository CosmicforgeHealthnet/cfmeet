//! Section 55 §11.2 — the production-access workflow.
//!
//! ```text
//! SANDBOX_READY → PRODUCTION_REQUESTED → UNDER_REVIEW → APPROVED / REJECTED → LIVE / SUSPENDED
//! ```
//!
//! * An OWNER (machine: `production_access.request`) of a project with an ACTIVE sandbox
//!   or test environment submits a request for named CAPABILITIES; it can read and
//!   cancel it, never approve it.
//! * Ordinary software capabilities may be approved by the AUTOMATIC policy (feature
//!   flag `DEVELOPER_PLATFORM_PRODUCTION_AUTO_APPROVAL`); regulated telecom, numbering and
//!   SMS capabilities always need an explicit operator decision. Numbering KYC and
//!   telecom compliance stay in their own services (domain checks still apply).
//! * Operators (`developer-platform.administer` + named operator) review, approve a
//!   subset, reject with a safe reason code, suspend and reinstate. An operator who is a
//!   member of the requesting organization can never decide it (no self-approval).
//! * An approval expires if not activated in time (REJECTED with `APPROVAL_EXPIRED`).
//! * ACTIVATION (by the customer, after approval) delegates to Platform Access: the
//!   PRODUCTION environment is activated with the approval reference, ONLY the approved
//!   capabilities' scopes are allowed for PRODUCTION, and live telecom is enabled only for
//!   an approved regulated capability. Suspension suspends the environment and disables
//!   live telecom in Platform Access, so the gateway and every owning service refuse its
//!   credentials at once.
//!
//! Every transition is recorded in `devplat_production_access_transitions` and the
//! tenant activity history.

use axum::http::StatusCode;
use chrono::{Duration, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    activity::{self, Event},
    authorize, delegate, delegate_with, membership, stmt, Caller,
};
use crate::{
    domain::{rbac::Action, RequestContext},
    http::error::{ApiError, ErrorCode},
    platform_access::{DelegationActor, DelegationCall},
    state::AppState,
};

/// One production capability and the scopes it unlocks in PRODUCTION.
#[derive(Debug, Clone, Copy)]
pub struct Capability {
    pub name: &'static str,
    /// Regulated telecom / numbering / SMS: never automatically approved; enables live
    /// telecom of the environment when approved.
    pub regulated: bool,
    pub scopes: &'static [&'static str],
}

pub const CAPABILITIES: &[Capability] = &[
    Capability {
        name: "PLATFORM_CONTROL",
        regulated: false,
        scopes: &[
            "organizations.read",
            "projects.read",
            "service_accounts.manage",
            "credentials.manage",
            "webhooks.read",
            "webhooks.manage",
            "usage.read",
            "audit.read",
        ],
    },
    Capability {
        name: "MEETINGS",
        regulated: false,
        scopes: &["meetings.read", "meetings.write"],
    },
    Capability {
        name: "MESSAGING",
        regulated: false,
        scopes: &[
            "messages.send",
            "messages.read",
            "conversations.create",
            "conversations.manage",
            "attachments.write",
            "attachments.read",
        ],
    },
    Capability {
        name: "APP_CALLING",
        regulated: false,
        scopes: &[
            "calls.read",
            "calls.write",
            "principals.read",
            "principals.write",
        ],
    },
    Capability {
        name: "PUBLIC_NETWORK_CALLING",
        regulated: true,
        scopes: &[
            "enterprise.config.read",
            "enterprise.config.write",
            "enterprise.calls.use",
            "enterprise.voicemail.read",
            "enterprise.voicemail.manage",
            "enterprise.recording.manage",
            "enterprise.recording.read",
        ],
    },
    Capability {
        name: "PRIVACY_RELAY",
        regulated: true,
        scopes: &[
            "privacy.contexts.read",
            "privacy.contexts.write",
            "privacy.sessions.read",
            "privacy.sessions.write",
            "privacy.evidence.read",
        ],
    },
    Capability {
        name: "NUMBERS",
        regulated: true,
        scopes: &[
            "numbers.search",
            "numbers.read",
            "numbers.manage",
            "numbers.assign",
            "numbers.kyc.read",
            "numbers.kyc.manage",
            "numbers.portability",
        ],
    },
    Capability {
        name: "SMS",
        regulated: true,
        scopes: &["sms.send", "sms.read"],
    },
];

pub fn capability(name: &str) -> Option<&'static Capability> {
    CAPABILITIES.iter().find(|c| c.name == name)
}

/// The capability catalogue as shown to customers.
pub fn catalogue() -> Value {
    Value::Array(
        CAPABILITIES
            .iter()
            .map(|c| json!({ "name": c.name, "regulated": c.regulated, "scopes": c.scopes }))
            .collect(),
    )
}

/// Days an approval stays activatable by default.
pub const DEFAULT_APPROVAL_DAYS: i64 = 30;

const OPEN: &str = "'PRODUCTION_REQUESTED', 'UNDER_REVIEW', 'APPROVED', 'LIVE', 'SUSPENDED'";

const VIEW: &str = "jsonb_build_object( \
    'id', r.id, 'organizationId', r.tenant_id, 'projectId', r.application_id, 'state', r.state, \
    'requestedCapabilities', to_jsonb(r.requested_capabilities), \
    'approvedCapabilities', to_jsonb(r.approved_capabilities), \
    'useCaseSummary', r.use_case_summary, 'requestedBy', r.requested_by, \
    'decision', CASE WHEN r.decided_at IS NULL THEN NULL ELSE jsonb_build_object( \
        'policy', r.decision_policy, 'reasonCode', r.decision_reason_code, 'decidedAt', r.decided_at) END, \
    'approvalExpiresAt', r.approval_expires_at, 'productionEnvironmentId', r.production_environment_id, \
    'activatedAt', r.activated_at, 'createdAt', r.created_at, 'updatedAt', r.updated_at, \
    'version', r.version)";

struct Request {
    tenant_id: Uuid,
    application_id: Uuid,
    state: String,
    approved: Vec<String>,
    requested: Vec<String>,
    approval_expires_at: Option<chrono::DateTime<Utc>>,
    production_environment_id: Option<Uuid>,
}

async fn load<C: ConnectionTrait>(db: &C, id: Uuid) -> Result<Request, ApiError> {
    let r = db
        .query_one(stmt(
            "SELECT tenant_id, application_id, state, approved_capabilities, requested_capabilities, \
                    approval_expires_at, production_environment_id \
             FROM devplat_production_access_requests WHERE id = $1",
            vec![id.into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?;
    Ok(Request {
        tenant_id: r.try_get("", "tenant_id")?,
        application_id: r.try_get("", "application_id")?,
        state: r.try_get("", "state")?,
        approved: r.try_get("", "approved_capabilities")?,
        requested: r.try_get("", "requested_capabilities")?,
        approval_expires_at: r.try_get("", "approval_expires_at")?,
        production_environment_id: r.try_get("", "production_environment_id")?,
    })
}

async fn view<C: ConnectionTrait>(db: &C, id: Uuid, history: bool) -> Result<Value, ApiError> {
    let mut v: Value = db
        .query_one(stmt(
            &format!(
                "SELECT {VIEW} AS v FROM devplat_production_access_requests r WHERE r.id = $1"
            ),
            vec![id.into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?
        .try_get("", "v")?;
    if history {
        let rows = db
            .query_all(stmt(
                "SELECT jsonb_build_object('fromState', from_state, 'toState', to_state, \
                   'actorKind', actor_kind, 'reasonCode', reason_code, 'occurredAt', occurred_at) AS v \
                 FROM devplat_production_access_transitions WHERE request_id = $1 ORDER BY seq",
                vec![id.into()],
            ))
            .await?;
        let h: Result<Vec<Value>, _> = rows.iter().map(|r| r.try_get::<Value>("", "v")).collect();
        v["history"] = Value::Array(h?);
    }
    Ok(v)
}

/// Who performs a transition.
#[derive(Debug, Clone)]
pub enum Actor<'a> {
    Customer(&'a Caller),
    Operator {
        name: &'a str,
        request: &'a RequestContext,
    },
    Policy(&'a RequestContext),
}

impl Actor<'_> {
    fn kind_ref(&self) -> (&'static str, String) {
        match self {
            Actor::Customer(c) => (c.actor_kind(), c.actor_ref()),
            Actor::Operator { name, .. } => ("OPERATOR", format!("operator:{name}")),
            Actor::Policy(_) => ("POLICY", "policy:automatic".into()),
        }
    }
}

/// Moves `id` from one of `from` to `to` (optimistic: refused when the state moved).
#[allow(clippy::too_many_arguments)]
async fn transition<C: ConnectionTrait>(
    db: &C,
    id: Uuid,
    tenant_id: Uuid,
    from: &[&str],
    to: &str,
    actor: &Actor<'_>,
    reason: Option<&str>,
    set: &str,
    values: Vec<sea_orm::Value>,
) -> Result<String, ApiError> {
    let from_list: Vec<String> = from.iter().map(|s| s.to_string()).collect();
    let mut all = vec![id.into(), to.into(), from_list.into(), Utc::now().into()];
    let base = all.len();
    all.extend(values);
    let set = (1..=10).rev().fold(set.to_string(), |acc, i| {
        acc.replace(&format!("${{{i}}}"), &format!("${}", base + i))
    });
    let row = db
        .query_one(stmt(
            &format!(
                "WITH old AS (SELECT state FROM devplat_production_access_requests WHERE id = $1 FOR UPDATE) \
                 UPDATE devplat_production_access_requests r SET state = $2, updated_at = $4, \
                   version = r.version + 1{set} \
                 FROM old WHERE r.id = $1 AND old.state = ANY($3) RETURNING old.state AS old_state"
            ),
            all,
        ))
        .await?
        .ok_or(ApiError(ErrorCode::InvalidState))?;
    let old: String = row.try_get("", "old_state")?;
    let (kind, aref) = actor.kind_ref();
    db.execute(stmt(
        "INSERT INTO devplat_production_access_transitions (id, request_id, tenant_id, from_state, \
           to_state, actor_kind, actor_ref, reason_code, occurred_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        vec![
            Uuid::new_v4().into(),
            id.into(),
            tenant_id.into(),
            old.clone().into(),
            to.into(),
            kind.into(),
            aref.into(),
            reason.map(str::to_string).into(),
            Utc::now().into(),
        ],
    ))
    .await?;
    Ok(old)
}

async fn record_activity<C: ConnectionTrait>(
    db: &C,
    actor: &Actor<'_>,
    tenant_id: Uuid,
    action: &str,
    id: Uuid,
    summary: Value,
) -> Result<(), ApiError> {
    let event = Event {
        tenant_id,
        action,
        target_type: "PRODUCTION_ACCESS_REQUEST",
        target_id: id.to_string(),
        summary,
    };
    match actor {
        Actor::Customer(c) => activity::record(db, c, event).await,
        Actor::Operator { name, request } => {
            activity::record_operator(
                db,
                name,
                request.request_id.as_str(),
                request.correlation_id.as_str(),
                event,
            )
            .await
        }
        Actor::Policy(request) => {
            activity::record_operator(
                db,
                "policy.automatic",
                request.request_id.as_str(),
                request.correlation_id.as_str(),
                event,
            )
            .await
        }
    }
}

fn capabilities(requested: &[String]) -> Result<Vec<String>, ApiError> {
    let mut out: Vec<String> = Vec::new();
    for c in requested {
        capability(c).ok_or(ApiError(ErrorCode::ValidationError))?;
        if !out.contains(c) {
            out.push(c.clone());
        }
    }
    if out.is_empty() || out.len() > 16 {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    Ok(out)
}

async fn project_tenant<C: ConnectionTrait>(
    db: &C,
    project: Uuid,
) -> Result<(Uuid, String), ApiError> {
    let r = db
        .query_one(stmt(
            "SELECT tenant_id, status FROM devplat_project_profiles WHERE application_id = $1",
            vec![project.into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?;
    Ok((r.try_get("", "tenant_id")?, r.try_get("", "status")?))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Submit {
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub use_case_summary: Option<String>,
}

/// `POST /v1/projects/{id}/production-access-requests`.
pub async fn submit(
    state: &AppState,
    caller: &Caller,
    project_id: Uuid,
    body: Submit,
) -> Result<(StatusCode, Value), ApiError> {
    let (tenant_id, project_status) = project_tenant(&state.db, project_id).await?;
    authorize(state, caller, tenant_id, Action::ProductionAccessRequest).await?;
    let caps = capabilities(&body.capabilities)?;
    let summary = match body.use_case_summary.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(s) if s.chars().count() <= 1000 && !s.chars().any(|c| c.is_control() && c != '\n') => {
            Some(s.to_string())
        }
        Some(_) => return Err(ApiError(ErrorCode::ValidationError)),
    };
    if project_status != "ACTIVE" {
        return Err(ApiError(ErrorCode::InvalidState));
    }
    // SANDBOX_READY: the project integrates in an ACTIVE sandbox / test environment first.
    let ready = state
        .db
        .query_one(stmt(
            "SELECT 1 FROM devplat_environment_profiles WHERE application_id = $1 \
               AND kind IN ('SANDBOX', 'TEST') AND status = 'ACTIVE' LIMIT 1",
            vec![project_id.into()],
        ))
        .await?
        .is_some();
    if !ready {
        return Err(ApiError(ErrorCode::InvalidState));
    }
    let id = Uuid::new_v4();
    let now = Utc::now();
    let txn = state.db.begin().await?;
    let inserted = txn
        .execute(stmt(
            "INSERT INTO devplat_production_access_requests (id, tenant_id, application_id, state, \
               requested_capabilities, use_case_summary, requested_by, created_at, updated_at) \
             VALUES ($1, $2, $3, 'PRODUCTION_REQUESTED', $4, $5, $6, $7, $7) ON CONFLICT DO NOTHING",
            vec![
                id.into(),
                tenant_id.into(),
                project_id.into(),
                caps.clone().into(),
                summary.into(),
                caller.actor_ref().into(),
                now.into(),
            ],
        ))
        .await?
        .rows_affected();
    if inserted != 1 {
        // Another request of this project is still open.
        return Err(ApiError(ErrorCode::InvalidState));
    }
    let customer = Actor::Customer(caller);
    txn.execute(stmt(
        "INSERT INTO devplat_production_access_transitions (id, request_id, tenant_id, from_state, \
           to_state, actor_kind, actor_ref, occurred_at) VALUES ($1, $2, $3, NULL, 'PRODUCTION_REQUESTED', $4, $5, $6)",
        vec![
            Uuid::new_v4().into(),
            id.into(),
            tenant_id.into(),
            caller.actor_kind().into(),
            caller.actor_ref().into(),
            now.into(),
        ],
    ))
    .await?;
    record_activity(
        &txn,
        &customer,
        tenant_id,
        "production_access.requested",
        id,
        json!({ "projectId": project_id, "capabilities": caps }),
    )
    .await?;
    // Authorized automatic policy: software-only requests of an ACTIVE organization.
    let software_only = caps
        .iter()
        .all(|c| capability(c).is_some_and(|c| !c.regulated));
    if state.flags.production_auto_approval && software_only {
        let policy = Actor::Policy(&caller.request);
        transition(
            &txn,
            id,
            tenant_id,
            &["PRODUCTION_REQUESTED"],
            "APPROVED",
            &policy,
            Some("AUTOMATIC_SOFTWARE_POLICY"),
            ", approved_capabilities = ${1}, decision_policy = 'AUTOMATIC', \
             decision_reason_code = 'AUTOMATIC_SOFTWARE_POLICY', reviewer_ref = 'policy:automatic', \
             decided_at = $4, approval_expires_at = ${2}",
            vec![
                caps.clone().into(),
                (now + Duration::days(DEFAULT_APPROVAL_DAYS)).into(),
            ],
        )
        .await?;
        record_activity(
            &txn,
            &policy,
            tenant_id,
            "production_access.approved",
            id,
            json!({ "capabilities": caps, "policy": "AUTOMATIC" }),
        )
        .await?;
    }
    let v = view(&txn, id, true).await?;
    txn.commit().await?;
    Ok((StatusCode::CREATED, v))
}

/// `GET /v1/projects/{id}/production-access-requests`.
pub async fn list(state: &AppState, caller: &Caller, project_id: Uuid) -> Result<Value, ApiError> {
    let (tenant_id, _) = project_tenant(&state.db, project_id).await?;
    authorize(state, caller, tenant_id, Action::ProductionAccessRead).await?;
    expire_due(&state.db).await?;
    let rows = state
        .db
        .query_all(stmt(
            &format!(
                "SELECT {VIEW} AS v FROM devplat_production_access_requests r \
                 WHERE r.application_id = $1 ORDER BY r.created_at DESC, r.id LIMIT 100"
            ),
            vec![project_id.into()],
        ))
        .await?;
    let items: Result<Vec<Value>, _> = rows.iter().map(|r| r.try_get::<Value>("", "v")).collect();
    Ok(json!({ "items": items?, "nextCursor": null }))
}

/// `GET /v1/production-access-requests/{id}` (with history).
pub async fn get(state: &AppState, caller: &Caller, id: Uuid) -> Result<Value, ApiError> {
    let r = load(&state.db, id).await?;
    authorize(state, caller, r.tenant_id, Action::ProductionAccessRead).await?;
    expire_due(&state.db).await?;
    view(&state.db, id, true).await
}

/// `GET /v1/projects/{id}/production-access` — the project's production status.
pub async fn status(
    state: &AppState,
    caller: &Caller,
    project_id: Uuid,
) -> Result<Value, ApiError> {
    let (tenant_id, _) = project_tenant(&state.db, project_id).await?;
    authorize(state, caller, tenant_id, Action::ProductionAccessRead).await?;
    expire_due(&state.db).await?;
    project_status(&state.db, project_id).await
}

pub async fn project_status<C: ConnectionTrait>(
    db: &C,
    project_id: Uuid,
) -> Result<Value, ApiError> {
    let current = db
        .query_one(stmt(
            &format!(
                "SELECT r.id, r.state, r.approved_capabilities, r.production_environment_id, {VIEW} AS v \
                 FROM devplat_production_access_requests r \
                 WHERE r.application_id = $1 ORDER BY r.created_at DESC LIMIT 1"
            ),
            vec![project_id.into()],
        ))
        .await?;
    let sandbox_ready = db
        .query_one(stmt(
            "SELECT 1 FROM devplat_environment_profiles WHERE application_id = $1 \
               AND kind IN ('SANDBOX', 'TEST') AND status = 'ACTIVE' LIMIT 1",
            vec![project_id.into()],
        ))
        .await?
        .is_some();
    Ok(match current {
        Some(r) => {
            let st: String = r.try_get("", "state")?;
            let enabled: Vec<String> = if st == "LIVE" {
                r.try_get("", "approved_capabilities")?
            } else {
                vec![]
            };
            let state = match st.as_str() {
                "REJECTED" | "CANCELLED" if sandbox_ready => "SANDBOX_READY".to_string(),
                _ => st.clone(),
            };
            json!({
                "projectId": project_id,
                "state": state,
                "enabledCapabilities": enabled,
                "productionEnvironmentId": r.try_get::<Option<Uuid>>("", "production_environment_id")?,
                "currentRequest": r.try_get::<Value>("", "v")?,
                "capabilityCatalogue": catalogue(),
            })
        }
        None => json!({
            "projectId": project_id,
            "state": if sandbox_ready { "SANDBOX_READY" } else { "NOT_READY" },
            "enabledCapabilities": [],
            "productionEnvironmentId": null,
            "currentRequest": null,
            "capabilityCatalogue": catalogue(),
        }),
    })
}

/// `POST /v1/production-access-requests/{id}/cancel`.
pub async fn cancel(state: &AppState, caller: &Caller, id: Uuid) -> Result<Value, ApiError> {
    let r = load(&state.db, id).await?;
    authorize(state, caller, r.tenant_id, Action::ProductionAccessRequest).await?;
    let actor = Actor::Customer(caller);
    let txn = state.db.begin().await?;
    transition(
        &txn,
        id,
        r.tenant_id,
        &["PRODUCTION_REQUESTED", "UNDER_REVIEW", "APPROVED"],
        "CANCELLED",
        &actor,
        Some("CUSTOMER_CANCELLED"),
        "",
        vec![],
    )
    .await?;
    record_activity(
        &txn,
        &actor,
        r.tenant_id,
        "production_access.cancelled",
        id,
        json!({}),
    )
    .await?;
    let v = view(&txn, id, true).await?;
    txn.commit().await?;
    Ok(v)
}

/// The STANDARD scopes of `caps` (FIRST_PARTY / privileged scopes are never granted by
/// this workflow).
async fn standard_scopes(state: &AppState, caps: &[String]) -> Result<Vec<String>, ApiError> {
    let registry = state
        .platform_access
        .scope_registry()
        .await
        .map_err(|_| ApiError(ErrorCode::ServiceUnavailable))?;
    let standard = |s: &str| {
        registry.iter().any(|r| {
            r.get("name").and_then(Value::as_str) == Some(s)
                && r.get("trust").and_then(Value::as_str) == Some("STANDARD")
        })
    };
    let mut out = Vec::new();
    for c in caps.iter().filter_map(|c| capability(c)) {
        for s in c.scopes.iter().filter(|s| standard(s)) {
            if !out.iter().any(|o: &String| o == s) {
                out.push(s.to_string());
            }
        }
    }
    Ok(out)
}

fn regulated(caps: &[String]) -> bool {
    caps.iter()
        .any(|c| capability(c).is_some_and(|c| c.regulated))
}

/// `POST /v1/production-access-requests/{id}/activate` — APPROVED → LIVE.
pub async fn activate(state: &AppState, caller: &Caller, id: Uuid) -> Result<Value, ApiError> {
    let r = load(&state.db, id).await?;
    authorize(state, caller, r.tenant_id, Action::ProductionAccessRequest).await?;
    if r.state == "LIVE" {
        return view(&state.db, id, true).await;
    }
    if r.state != "APPROVED" {
        return Err(ApiError(ErrorCode::InvalidState));
    }
    if r.approval_expires_at.is_some_and(|t| t <= Utc::now()) {
        expire_due(&state.db).await?;
        return Err(ApiError(ErrorCode::ApprovalExpired));
    }
    let actor = caller.delegation_actor();
    let env = delegate(
        state,
        caller,
        DelegationCall::ActivateProduction {
            actor,
            application_id: r.application_id,
            approval_reference: format!("par_{}", id.simple()),
        },
    )
    .await?;
    let environment_id = super::id_of(&env)?;
    if env.get("kind").and_then(Value::as_str) != Some("PRODUCTION") {
        return Err(ApiError(ErrorCode::ServiceUnavailable));
    }
    for scope in standard_scopes(state, &r.approved).await? {
        delegate(
            state,
            caller,
            DelegationCall::GrantApplicationScope {
                actor,
                application_id: r.application_id,
                scope,
                environment_kinds: vec!["SANDBOX".into(), "TEST".into(), "PRODUCTION".into()],
            },
        )
        .await?;
    }
    // Regulated capabilities: the explicit approval is what enables live telecom.
    delegate(
        state,
        caller,
        DelegationCall::SetLiveTelecom {
            actor,
            environment_id,
            allowed: regulated(&r.approved),
        },
    )
    .await?;
    let now = Utc::now();
    let txn = state.db.begin().await?;
    txn.execute(stmt(
        "INSERT INTO devplat_environment_profiles (environment_id, application_id, tenant_id, kind, \
           display_name, status, production_access_state, created_by, created_at, updated_at) \
         VALUES ($1, $2, $3, 'PRODUCTION', 'Production', 'ACTIVE', 'LIVE', $4, $5, $5) \
         ON CONFLICT (environment_id) DO UPDATE SET production_access_state = 'LIVE', status = 'ACTIVE', \
           updated_at = EXCLUDED.updated_at, version = devplat_environment_profiles.version + 1",
        vec![
            environment_id.into(),
            r.application_id.into(),
            r.tenant_id.into(),
            caller.actor_ref().into(),
            now.into(),
        ],
    ))
    .await?;
    txn.execute(stmt(
        "INSERT INTO devplat_environment_refs (id, tenant_id, application_id, environment_id, first_seen_at, last_seen_at) \
         VALUES ($1, $2, $3, $4, $5, $5) ON CONFLICT (environment_id) DO NOTHING",
        vec![
            Uuid::new_v4().into(),
            r.tenant_id.into(),
            r.application_id.into(),
            environment_id.into(),
            now.into(),
        ],
    ))
    .await?;
    let customer = Actor::Customer(caller);
    transition(
        &txn,
        id,
        r.tenant_id,
        &["APPROVED"],
        "LIVE",
        &customer,
        None,
        ", production_environment_id = ${1}, activated_at = $4",
        vec![environment_id.into()],
    )
    .await?;
    record_activity(
        &txn,
        &customer,
        r.tenant_id,
        "production_access.activated",
        id,
        json!({ "environmentId": environment_id, "capabilities": r.approved }),
    )
    .await?;
    let v = view(&txn, id, true).await?;
    txn.commit().await?;
    Ok(v)
}

/// Approvals not activated in time become REJECTED (`APPROVAL_EXPIRED`).
pub async fn expire_due(db: &DatabaseConnection) -> Result<u64, ApiError> {
    let rows = db
        .query_all(stmt(
            "SELECT id, tenant_id FROM devplat_production_access_requests \
             WHERE state = 'APPROVED' AND approval_expires_at <= $1 LIMIT 100",
            vec![Utc::now().into()],
        ))
        .await?;
    let mut n = 0;
    for row in rows {
        let id: Uuid = row.try_get("", "id")?;
        let tenant: Uuid = row.try_get("", "tenant_id")?;
        let ctx = RequestContext::generate();
        let policy = Actor::Policy(&ctx);
        let txn = db.begin().await?;
        if transition(
            &txn,
            id,
            tenant,
            &["APPROVED"],
            "REJECTED",
            &policy,
            Some("APPROVAL_EXPIRED"),
            ", decision_reason_code = 'APPROVAL_EXPIRED', decided_at = $4",
            vec![],
        )
        .await
        .is_ok()
        {
            record_activity(
                &txn,
                &policy,
                tenant,
                "production_access.expired",
                id,
                json!({}),
            )
            .await?;
            txn.commit().await?;
            n += 1;
        }
    }
    Ok(n)
}

// ---------------------------------------------------------------------------------------
// Operator decisions
// ---------------------------------------------------------------------------------------

/// `GET /internal/v1/operator/production-access-requests?state=`.
pub async fn operator_list(
    db: &DatabaseConnection,
    state: Option<&str>,
) -> Result<Value, ApiError> {
    expire_due(db).await?;
    if let Some(s) = state {
        if !matches!(
            s,
            "PRODUCTION_REQUESTED"
                | "UNDER_REVIEW"
                | "APPROVED"
                | "REJECTED"
                | "CANCELLED"
                | "LIVE"
                | "SUSPENDED"
        ) {
            return Err(ApiError(ErrorCode::ValidationError));
        }
    }
    let rows = db
        .query_all(stmt(
            &format!(
                "SELECT {VIEW} AS v FROM devplat_production_access_requests r \
                 WHERE ($1::text IS NULL OR r.state = $1) ORDER BY r.created_at, r.id LIMIT 200"
            ),
            vec![state.map(str::to_string).into()],
        ))
        .await?;
    let items: Result<Vec<Value>, _> = rows.iter().map(|r| r.try_get::<Value>("", "v")).collect();
    Ok(json!({ "items": items?, "nextCursor": null }))
}

pub async fn operator_get(db: &DatabaseConnection, id: Uuid) -> Result<Value, ApiError> {
    view(db, id, true).await
}

/// An operator decision.
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Decision {
    /// approve: the approved subset (default: everything requested).
    #[serde(default)]
    pub capabilities: Option<Vec<String>>,
    /// approve: days the approval stays activatable (1–90, default 30).
    #[serde(default)]
    pub approval_valid_days: Option<i64>,
    /// reject / suspend: safe reason code.
    #[serde(default)]
    pub reason_code: Option<String>,
}

/// The decision kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperatorCommand {
    Review,
    Approve,
    Reject,
    Suspend,
    Reinstate,
}

fn reason(d: &Decision, default: &str) -> Result<String, ApiError> {
    let r = d.reason_code.clone().unwrap_or_else(|| default.to_string());
    if super::operator::valid_reason(&r) {
        Ok(r)
    } else {
        Err(ApiError(ErrorCode::ValidationError))
    }
}

/// Runs an operator command. `operator_user` is the reviewing operator's CF Meet user id
/// (`x-cf-operator-user-id`): a member of the requesting organization can never decide.
pub async fn operator_decide(
    state: &AppState,
    operator: &str,
    operator_user: Option<Uuid>,
    request: &RequestContext,
    id: Uuid,
    command: OperatorCommand,
    d: Decision,
) -> Result<Value, ApiError> {
    let r = load(&state.db, id).await?;
    if matches!(command, OperatorCommand::Approve | OperatorCommand::Reject) {
        let Some(user) = operator_user else {
            return Err(ApiError(ErrorCode::OperatorRequired));
        };
        if membership(&state.db, r.tenant_id, user).await?.is_some() {
            return Err(ApiError(ErrorCode::SelfApprovalForbidden));
        }
    }
    let actor = Actor::Operator {
        name: operator,
        request,
    };
    let ops = DelegationActor::Operator(Uuid::new_v4());
    match command {
        OperatorCommand::Review => {
            let txn = state.db.begin().await?;
            transition(
                &txn,
                id,
                r.tenant_id,
                &["PRODUCTION_REQUESTED"],
                "UNDER_REVIEW",
                &actor,
                None,
                ", reviewer_ref = ${1}",
                vec![format!("operator:{operator}").into()],
            )
            .await?;
            record_activity(
                &txn,
                &actor,
                r.tenant_id,
                "production_access.under_review",
                id,
                json!({}),
            )
            .await?;
            txn.commit().await?;
        }
        OperatorCommand::Approve => {
            let caps = match &d.capabilities {
                Some(c) => capabilities(c)?,
                None => r.requested.clone(),
            };
            if caps.iter().any(|c| !r.requested.contains(c)) {
                return Err(ApiError(ErrorCode::ValidationError));
            }
            let days = d.approval_valid_days.unwrap_or(DEFAULT_APPROVAL_DAYS);
            if !(1..=90).contains(&days) {
                return Err(ApiError(ErrorCode::ValidationError));
            }
            let txn = state.db.begin().await?;
            // Regulated capabilities are only approved after an explicit review.
            let from: &[&str] = if regulated(&caps) {
                &["UNDER_REVIEW"]
            } else {
                &["PRODUCTION_REQUESTED", "UNDER_REVIEW"]
            };
            transition(
                &txn,
                id,
                r.tenant_id,
                from,
                "APPROVED",
                &actor,
                Some("OPERATOR_APPROVED"),
                ", approved_capabilities = ${1}, decision_policy = 'OPERATOR', \
                 decision_reason_code = 'OPERATOR_APPROVED', reviewer_ref = ${2}, decided_at = $4, \
                 approval_expires_at = ${3}",
                vec![
                    caps.clone().into(),
                    format!("operator:{operator}").into(),
                    (Utc::now() + Duration::days(days)).into(),
                ],
            )
            .await?;
            record_activity(
                &txn,
                &actor,
                r.tenant_id,
                "production_access.approved",
                id,
                json!({ "capabilities": caps, "policy": "OPERATOR" }),
            )
            .await?;
            txn.commit().await?;
        }
        OperatorCommand::Reject => {
            let code = reason(&d, "NOT_ELIGIBLE")?;
            let txn = state.db.begin().await?;
            transition(
                &txn,
                id,
                r.tenant_id,
                &["PRODUCTION_REQUESTED", "UNDER_REVIEW", "APPROVED"],
                "REJECTED",
                &actor,
                Some(&code),
                ", decision_policy = 'OPERATOR', decision_reason_code = ${1}, reviewer_ref = ${2}, decided_at = $4",
                vec![code.clone().into(), format!("operator:{operator}").into()],
            )
            .await?;
            record_activity(
                &txn,
                &actor,
                r.tenant_id,
                "production_access.rejected",
                id,
                json!({ "reasonCode": code }),
            )
            .await?;
            txn.commit().await?;
        }
        OperatorCommand::Suspend | OperatorCommand::Reinstate => {
            let suspend = command == OperatorCommand::Suspend;
            let env = r
                .production_environment_id
                .ok_or(ApiError(ErrorCode::InvalidState))?;
            let (from, to) = if suspend {
                ("LIVE", "SUSPENDED")
            } else {
                ("SUSPENDED", "LIVE")
            };
            if r.state != from {
                return Err(ApiError(ErrorCode::InvalidState));
            }
            let code = reason(
                &d,
                if suspend {
                    "OPERATOR_SUSPENDED"
                } else {
                    "OPERATOR_REINSTATED"
                },
            )?;
            // Platform Access first: suspension withdraws machine authority before the
            // control plane reports it.
            if suspend {
                delegate_with(
                    state,
                    &request.correlation_id,
                    DelegationCall::SetLiveTelecom {
                        actor: ops,
                        environment_id: env,
                        allowed: false,
                    },
                )
                .await?;
            }
            delegate_with(
                state,
                &request.correlation_id,
                DelegationCall::SetEnvironmentStatus {
                    actor: ops,
                    environment_id: env,
                    suspended: suspend,
                },
            )
            .await?;
            if !suspend && regulated(&r.approved) {
                delegate_with(
                    state,
                    &request.correlation_id,
                    DelegationCall::SetLiveTelecom {
                        actor: ops,
                        environment_id: env,
                        allowed: true,
                    },
                )
                .await?;
            }
            let txn = state.db.begin().await?;
            transition(
                &txn,
                id,
                r.tenant_id,
                &[from],
                to,
                &actor,
                Some(&code),
                "",
                vec![],
            )
            .await?;
            txn.execute(stmt(
                "UPDATE devplat_environment_profiles SET status = $2, production_access_state = $3, \
                   updated_at = $4, version = version + 1 WHERE environment_id = $1",
                vec![
                    env.into(),
                    if suspend { "SUSPENDED" } else { "ACTIVE" }.into(),
                    to.into(),
                    Utc::now().into(),
                ],
            ))
            .await?;
            record_activity(
                &txn,
                &actor,
                r.tenant_id,
                if suspend {
                    "production_access.suspended"
                } else {
                    "production_access.reinstated"
                },
                id,
                json!({ "reasonCode": code, "environmentId": env }),
            )
            .await?;
            txn.commit().await?;
        }
    }
    view(&state.db, id, true).await
}

/// Whether `scope` may be allowed for PRODUCTION in `project` (approved and LIVE).
pub async fn production_scope_approved<C: ConnectionTrait>(
    db: &C,
    project: Uuid,
    scope: &str,
) -> Result<bool, ApiError> {
    let row = db
        .query_one(stmt(
            "SELECT approved_capabilities FROM devplat_production_access_requests \
             WHERE application_id = $1 AND state = 'LIVE'",
            vec![project.into()],
        ))
        .await?;
    let caps: Vec<String> = match row {
        Some(r) => r.try_get("", "approved_capabilities")?,
        None => return Ok(false),
    };
    Ok(caps
        .iter()
        .filter_map(|c| capability(c))
        .any(|c| c.scopes.contains(&scope)))
}

/// The organization's production status for its support profile.
pub async fn organization_production_status<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
) -> Result<&'static str, ApiError> {
    let rows = db
        .query_all(stmt(
            &format!(
                "SELECT DISTINCT state FROM devplat_production_access_requests \
                 WHERE tenant_id = $1 AND state IN ({OPEN})"
            ),
            vec![tenant_id.into()],
        ))
        .await?;
    let states: Vec<String> = rows
        .iter()
        .filter_map(|r| r.try_get::<String>("", "state").ok())
        .collect();
    Ok(if states.iter().any(|s| s == "LIVE") {
        "LIVE"
    } else if states.iter().any(|s| s == "SUSPENDED") {
        "SUSPENDED"
    } else if states.is_empty() {
        "SANDBOX_ONLY"
    } else {
        "PRODUCTION_PENDING"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regulated_capabilities_are_the_telecom_numbering_and_sms_ones() {
        let r: Vec<_> = CAPABILITIES
            .iter()
            .filter(|c| c.regulated)
            .map(|c| c.name)
            .collect();
        assert_eq!(
            r,
            ["PUBLIC_NETWORK_CALLING", "PRIVACY_RELAY", "NUMBERS", "SMS"]
        );
        assert!(regulated(&["SMS".into()]));
        assert!(!regulated(&["MEETINGS".into(), "APP_CALLING".into()]));
        assert!(capabilities(&["NOPE".into()]).is_err());
        assert!(capabilities(&[]).is_err());
        assert_eq!(
            capabilities(&["SMS".into(), "SMS".into()]).unwrap(),
            ["SMS"]
        );
    }

    #[test]
    fn privileged_and_first_party_scopes_are_never_capability_scopes() {
        for c in CAPABILITIES {
            for s in c.scopes {
                assert!(!s.ends_with(".admin"), "{s}");
                assert_ne!(*s, "privacy.contacts.verify");
                assert_ne!(*s, "developer_platform.admin");
            }
        }
    }
}
