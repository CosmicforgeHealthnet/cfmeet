//! App registration on a project (integration contract §5, console, OWNER / ADMIN).
//!
//! * `GET` / `PUT /v1/projects/{projectId}/app` — the manifest. Every change is a new
//!   immutable version (`devplat_app_versions`); an unchanged manifest adds none.
//! * `POST …/app/secret` — Platform Access issues / rotates the client secret (shown once,
//!   never stored here; the previous one keeps Platform Access's overlap).
//! * `POST …/app/submit` — DRAFT / REJECTED (or an APPROVED / PUBLISHED app with pending
//!   changes) → SUBMITTED; decided by the operator review ([`super::review`]).
//! * `GET …/app/grants` — the app's grants from Platform Access (no account id).
//! * `PUT …/app/test-accounts` — the draft test accounts (CF Meet account ids, at most 25).
//!
//! **Live version.** Platform Access's client profile always reflects the reviewed
//! configuration once there is one: the published version, else the approved one, else
//! the current draft. Changes to an approved / published app are kept as the current
//! version (`pendingChanges`) until a new review approves and publishes them.
//!
//! **Platform Access sync.** Every change of the app (manifest, status, test accounts,
//! membership-derived allow list) replaces the client profile
//! (`PUT /internal/v1/oauth/clients/{applicationId}`) inside the change's transaction:
//! when Platform Access refuses or is unreachable, nothing changes here either.

use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    manifest::{self, strings},
    port::OAuthReply,
};
use crate::{
    control::{
        activity::{self, Event},
        authorize, stmt, Caller,
    },
    domain::rbac::Action,
    http::error::{ApiError, ErrorCode},
    state::AppState,
};

/// The review statuses.
pub const STATUSES: &[&str] = &[
    "DRAFT",
    "SUBMITTED",
    "IN_REVIEW",
    "APPROVED",
    "REJECTED",
    "PUBLISHED",
    "SUSPENDED",
];

/// One `devplat_apps` row.
#[derive(Debug, Clone)]
pub struct AppRow {
    pub app_id: Uuid,
    pub tenant_id: Uuid,
    pub status: String,
    pub current_version: i32,
    pub submitted_version: Option<i32>,
    pub reviewed_version: Option<i32>,
    pub published_version: Option<i32>,
    pub test_account_ids: Vec<Uuid>,
    pub secret_id: Option<String>,
    pub secret_issued_at: Option<DateTime<Utc>>,
    pub decision_reason_code: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub published_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl AppRow {
    /// The version Platform Access serves (see the module documentation).
    pub fn live_version(&self) -> i32 {
        self.published_version
            .or(self.reviewed_version)
            .unwrap_or(self.current_version)
    }

    /// The Platform Access client status.
    pub fn client_status(&self) -> &'static str {
        if self.status == "SUSPENDED" {
            "SUSPENDED"
        } else if self.published_version.is_some() {
            "PUBLISHED"
        } else if self.reviewed_version.is_some() {
            "APPROVED"
        } else {
            "DRAFT"
        }
    }

    /// Whether the current version differs from the reviewed one.
    pub fn pending_changes(&self) -> bool {
        self.reviewed_version
            .is_some_and(|r| r != self.current_version)
    }
}

const COLUMNS: &str = "app_id, tenant_id, status, current_version, submitted_version, reviewed_version, \
    published_version, test_account_ids, secret_id, secret_issued_at, decision_reason_code, decided_at, \
    published_at, created_at, updated_at";

fn row_of(r: &sea_orm::QueryResult) -> Result<AppRow, sea_orm::DbErr> {
    Ok(AppRow {
        app_id: r.try_get("", "app_id")?,
        tenant_id: r.try_get("", "tenant_id")?,
        status: r.try_get("", "status")?,
        current_version: r.try_get("", "current_version")?,
        submitted_version: r.try_get("", "submitted_version")?,
        reviewed_version: r.try_get("", "reviewed_version")?,
        published_version: r.try_get("", "published_version")?,
        test_account_ids: r.try_get("", "test_account_ids")?,
        secret_id: r.try_get("", "secret_id")?,
        secret_issued_at: r.try_get("", "secret_issued_at")?,
        decision_reason_code: r.try_get("", "decision_reason_code")?,
        decided_at: r.try_get("", "decided_at")?,
        published_at: r.try_get("", "published_at")?,
        created_at: r.try_get("", "created_at")?,
        updated_at: r.try_get("", "updated_at")?,
    })
}

/// The app of `app_id` (`lock`: `FOR UPDATE`, inside a transaction).
pub async fn load<C: ConnectionTrait>(
    db: &C,
    app_id: Uuid,
    lock: bool,
) -> Result<Option<AppRow>, ApiError> {
    let row = db
        .query_one(stmt(
            &format!(
                "SELECT {COLUMNS} FROM devplat_apps WHERE app_id = $1{}",
                if lock { " FOR UPDATE" } else { "" }
            ),
            vec![app_id.into()],
        ))
        .await?;
    Ok(row.as_ref().map(row_of).transpose()?)
}

/// The manifest of `version`.
pub async fn manifest_of<C: ConnectionTrait>(
    db: &C,
    app_id: Uuid,
    version: i32,
) -> Result<Value, ApiError> {
    Ok(db
        .query_one(stmt(
            "SELECT manifest FROM devplat_app_versions WHERE app_id = $1 AND version = $2",
            vec![app_id.into(), version.into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::InternalError))?
        .try_get("", "manifest")?)
}

fn ts(at: Option<DateTime<Utc>>) -> Value {
    at.map(|t| Value::String(t.to_rfc3339()))
        .unwrap_or(Value::Null)
}

/// The console / operator view of an app (current manifest).
pub async fn view<C: ConnectionTrait>(db: &C, row: &AppRow) -> Result<Value, ApiError> {
    let manifest = manifest_of(db, row.app_id, row.current_version).await?;
    let listed = row.status != "SUSPENDED"
        && match row.published_version {
            Some(v) => manifest_of(db, row.app_id, v).await?["distribution"] == "PUBLIC",
            None => false,
        };
    Ok(json!({
        "appId": row.app_id,
        "clientId": row.app_id,
        "projectId": row.app_id,
        "organizationId": row.tenant_id,
        "status": row.status,
        "currentVersion": row.current_version,
        "submittedVersion": row.submitted_version,
        "reviewedVersion": row.reviewed_version,
        "publishedVersion": row.published_version,
        "liveVersion": row.live_version(),
        "pendingChanges": row.pending_changes(),
        "manifest": manifest,
        "testAccountIds": row.test_account_ids,
        "secretIssuedAt": ts(row.secret_issued_at),
        "review": row.decided_at.map(|d| json!({
            "reasonCode": row.decision_reason_code, "decidedAt": d.to_rfc3339() })),
        "listedInDirectory": listed,
        "publishedAt": ts(row.published_at),
        "createdAt": row.created_at.to_rfc3339(),
        "updatedAt": row.updated_at.to_rfc3339(),
    }))
}

/// `(tenant, status)` of a project.
async fn project<C: ConnectionTrait>(db: &C, project_id: Uuid) -> Result<(Uuid, String), ApiError> {
    let r = db
        .query_one(stmt(
            "SELECT tenant_id, status FROM devplat_project_profiles WHERE application_id = $1",
            vec![project_id.into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?;
    Ok((r.try_get("", "tenant_id")?, r.try_get("", "status")?))
}

/// The CF Meet accounts of the workspace's ACTIVE members (the PRIVATE allow list).
pub async fn member_accounts<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
) -> Result<Vec<Uuid>, ApiError> {
    let rows = db
        .query_all(stmt(
            "SELECT user_id FROM devplat_workspace_members WHERE tenant_id = $1 AND status = 'ACTIVE' \
             ORDER BY created_at, user_id",
            vec![tenant_id.into()],
        ))
        .await?;
    rows.iter()
        .map(|r| r.try_get::<Uuid>("", "user_id").map_err(ApiError::from))
        .collect()
}

/// The Platform Access client profile of `row` (contract §2: `{name, redirectUris,
/// userScopes, botScopes, distribution, status, testAccountIds}`, the consent-page and
/// in-meeting fields `developerName` (the workspace display name), `logoUrl` and
/// `meetingSurface: {url, origins}` (omitted when the live manifest has none), and, for
/// PRIVATE apps, `allowedAccountIds`: the workspace members' CF Meet accounts).
/// Organizations owned by a member (`allowedOrganizationIds`) are not known to the
/// Developer Console and are left out.
pub async fn client_profile<C: ConnectionTrait>(db: &C, row: &AppRow) -> Result<Value, ApiError> {
    let live = manifest_of(db, row.app_id, row.live_version()).await?;
    let mut body = json!({
        "name": live["name"],
        "redirectUris": live["redirectUris"],
        "userScopes": live["userScopes"],
        "botScopes": live["botScopes"],
        "distribution": live["distribution"],
        "status": row.client_status(),
        "testAccountIds": row.test_account_ids,
    });
    let developer: Option<String> = db
        .query_one(stmt(
            "SELECT display_name FROM devplat_organizations WHERE tenant_id = $1",
            vec![row.tenant_id.into()],
        ))
        .await?
        .map(|r| r.try_get("", "display_name"))
        .transpose()?;
    if let Some(d) = developer {
        body["developerName"] = json!(d);
    }
    for key in ["logoUrl", "meetingSurface"] {
        if !live[key].is_null() {
            body[key] = live[key].clone();
        }
    }
    if live["distribution"] == "PRIVATE" {
        body["allowedAccountIds"] = json!(member_accounts(db, row.tenant_id).await?);
    }
    Ok(body)
}

/// Platform Access client refusals as Developer Platform errors.
fn client_refusal(status: u16, code: &str) -> ApiError {
    tracing::info!(status, %code, "platform access refused an oauth client change");
    ApiError(match (status, code) {
        (404, _) => ErrorCode::NotFound,
        (409, _) | (_, "INVALID_STATE") => ErrorCode::InvalidState,
        (_, "POLICY_VIOLATION") => ErrorCode::PolicyViolation,
        (400, _) | (422, _) => ErrorCode::ValidationError,
        _ => ErrorCode::ServiceUnavailable,
    })
}

/// Replaces the Platform Access client profile of `row` (in the change's transaction).
pub async fn sync<C: ConnectionTrait>(
    state: &AppState,
    db: &C,
    row: &AppRow,
    correlation: &cf_service_auth::CorrelationId,
) -> Result<(), ApiError> {
    let body = client_profile(db, row).await?;
    match state.oauth.put_client(row.app_id, &body, correlation).await {
        Ok(OAuthReply::Ok { .. }) => {}
        Ok(OAuthReply::Refused { status, code }) => return Err(client_refusal(status, &code)),
        Err(e) => {
            tracing::warn!(error = %e, "token authority unavailable");
            return Err(ApiError(ErrorCode::ServiceUnavailable));
        }
    }
    db.execute(stmt(
        "UPDATE devplat_apps SET synced_at = $2 WHERE app_id = $1",
        vec![row.app_id.into(), Utc::now().into()],
    ))
    .await?;
    Ok(())
}

/// Records one review-status transition (append-only history).
#[allow(clippy::too_many_arguments)]
pub async fn record_transition<C: ConnectionTrait>(
    db: &C,
    ev: crate::bridge_events::BridgeEvents,
    app_id: Uuid,
    tenant_id: Uuid,
    from: Option<&str>,
    to: &str,
    version: i32,
    actor_kind: &str,
    actor_ref: &str,
    reason: Option<&str>,
) -> Result<(), ApiError> {
    db.execute(stmt(
        "INSERT INTO devplat_app_transitions (id, app_id, tenant_id, from_status, to_status, app_version, \
           actor_kind, actor_ref, reason_code, occurred_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        vec![
            Uuid::new_v4().into(),
            app_id.into(),
            tenant_id.into(),
            from.map(str::to_string).into(),
            to.into(),
            version.into(),
            actor_kind.into(),
            actor_ref.into(),
            reason.map(str::to_string).into(),
            Utc::now().into(),
        ],
    ))
    .await?;
    // ADR 0068 A7: the app's review status (and its verification case) changed.
    crate::bridge_events::record(db, ev, crate::bridge_events::Source::App(app_id)).await?;
    Ok(())
}

async fn record<C: ConnectionTrait>(
    db: &C,
    caller: &Caller,
    row: &AppRow,
    action: &str,
    summary: Value,
) -> Result<(), ApiError> {
    activity::record(
        db,
        caller,
        Event {
            tenant_id: row.tenant_id,
            action,
            target_type: "APP",
            target_id: row.app_id.to_string(),
            summary,
        },
    )
    .await
}

/// The app of a project the caller may act on (`action`), loaded (`lock`) in `db`.
async fn authorized(
    state: &AppState,
    caller: &Caller,
    project_id: Uuid,
    action: Action,
) -> Result<Uuid, ApiError> {
    let (tenant_id, status) = project(&state.db, project_id).await?;
    authorize(state, caller, tenant_id, action).await?;
    if action == Action::AppsManage && status != "ACTIVE" {
        return Err(ApiError(ErrorCode::InvalidState));
    }
    Ok(tenant_id)
}

/// `GET /v1/projects/{projectId}/app`.
pub async fn get(state: &AppState, caller: &Caller, project_id: Uuid) -> Result<Value, ApiError> {
    authorized(state, caller, project_id, Action::AppsRead).await?;
    let row = load(&state.db, project_id, false)
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?;
    view(&state.db, &row).await
}

/// `PUT /v1/projects/{projectId}/app`: 201 registered, 200 changed or unchanged.
pub async fn put(
    state: &AppState,
    caller: &Caller,
    project_id: Uuid,
    body: &Value,
) -> Result<(StatusCode, Value), ApiError> {
    let tenant_id = authorized(state, caller, project_id, Action::AppsManage).await?;
    let input: manifest::ManifestInput =
        serde_json::from_value(body.clone()).map_err(|_| ApiError(ErrorCode::ValidationError))?;
    let manifest = manifest::normalize(input)?;
    let now = Utc::now();
    let txn = state.db.begin().await?;
    let created = txn
        .execute(stmt(
            "INSERT INTO devplat_apps (app_id, tenant_id, status, current_version, created_by, created_at, updated_at) \
             VALUES ($1, $2, 'DRAFT', 1, $3, $4, $4) ON CONFLICT (app_id) DO NOTHING",
            vec![
                project_id.into(),
                tenant_id.into(),
                caller.actor_ref().into(),
                now.into(),
            ],
        ))
        .await?
        .rows_affected()
        == 1;
    let mut row = load(&txn, project_id, true)
        .await?
        .ok_or(ApiError(ErrorCode::InternalError))?;
    if created {
        insert_version(&txn, project_id, 1, &manifest, caller, now).await?;
        record_transition(
            &txn,
            state.bridge_events,
            project_id,
            tenant_id,
            None,
            "DRAFT",
            1,
            caller.actor_kind(),
            &caller.actor_ref(),
            None,
        )
        .await?;
    } else {
        if matches!(row.status.as_str(), "SUBMITTED" | "IN_REVIEW" | "SUSPENDED") {
            return Err(ApiError(ErrorCode::InvalidState));
        }
        let current = manifest_of(&txn, project_id, row.current_version).await?;
        if current == manifest {
            txn.rollback().await?;
            return Ok((StatusCode::OK, view(&state.db, &row).await?));
        }
        let next = row.current_version + 1;
        insert_version(&txn, project_id, next, &manifest, caller, now).await?;
        // A rejected app becomes a draft again once it changes.
        let status = if row.status == "REJECTED" {
            "DRAFT"
        } else {
            row.status.as_str()
        };
        txn.execute(stmt(
            "UPDATE devplat_apps SET current_version = $2, status = $3, updated_at = $4, \
               version = version + 1 WHERE app_id = $1",
            vec![project_id.into(), next.into(), status.into(), now.into()],
        ))
        .await?;
        if status != row.status {
            record_transition(
                &txn,
                state.bridge_events,
                project_id,
                tenant_id,
                Some(&row.status),
                status,
                next,
                caller.actor_kind(),
                &caller.actor_ref(),
                None,
            )
            .await?;
        }
        row = load(&txn, project_id, true)
            .await?
            .ok_or(ApiError(ErrorCode::InternalError))?;
    }
    sync(state, &txn, &row, &caller.request.correlation_id).await?;
    record(
        &txn,
        caller,
        &row,
        if created {
            "app.created"
        } else {
            "app.updated"
        },
        json!({ "version": row.current_version, "status": row.status }),
    )
    .await?;
    let v = view(&txn, &row).await?;
    txn.commit().await?;
    Ok((
        if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        v,
    ))
}

async fn insert_version<C: ConnectionTrait>(
    db: &C,
    app_id: Uuid,
    version: i32,
    manifest: &Value,
    caller: &Caller,
    now: DateTime<Utc>,
) -> Result<(), ApiError> {
    db.execute(stmt(
        "INSERT INTO devplat_app_versions (app_id, version, manifest, created_by, created_at) \
         VALUES ($1, $2, $3, $4, $5)",
        vec![
            app_id.into(),
            version.into(),
            manifest.clone().into(),
            caller.actor_ref().into(),
            now.into(),
        ],
    ))
    .await?;
    Ok(())
}

/// The stored copy of a secret response (idempotent replay): never the secret.
pub fn redact_secret(v: &mut Value) {
    if let Some(o) = v.as_object_mut() {
        if o.remove("clientSecret").is_some() {
            o.insert("clientSecretRedacted".into(), Value::Bool(true));
        }
    }
}

/// `POST /v1/projects/{projectId}/app/secret`: 201 with the client secret, ONCE.
pub async fn rotate_secret(
    state: &AppState,
    caller: &Caller,
    project_id: Uuid,
) -> Result<(StatusCode, Value), ApiError> {
    authorized(state, caller, project_id, Action::AppsManage).await?;
    let row = load(&state.db, project_id, false)
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?;
    if row.status == "SUSPENDED" {
        return Err(ApiError(ErrorCode::InvalidState));
    }
    let issued = match state
        .oauth
        .issue_secret(project_id, &caller.request.correlation_id)
        .await
    {
        Ok(OAuthReply::Ok { body, .. }) => body,
        Ok(OAuthReply::Refused { status, code }) => return Err(client_refusal(status, &code)),
        Err(e) => {
            tracing::warn!(error = %e, "token authority unavailable");
            return Err(ApiError(ErrorCode::ServiceUnavailable));
        }
    };
    let secret = issued
        .get("clientSecret")
        .and_then(Value::as_str)
        .filter(|s| s.starts_with("cfcs_") && s.len() <= 512)
        .ok_or(ApiError(ErrorCode::ServiceUnavailable))?
        .to_string();
    let secret_id = issued
        .get("secretId")
        .and_then(Value::as_str)
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 64
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        })
        .map(str::to_string);
    let issued_at = issued
        .get("issuedAt")
        .and_then(Value::as_str)
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_else(Utc::now);
    let txn = state.db.begin().await?;
    txn.execute(stmt(
        "UPDATE devplat_apps SET secret_id = $2, secret_issued_at = $3, updated_at = $4, \
           version = version + 1 WHERE app_id = $1",
        vec![
            project_id.into(),
            secret_id.clone().into(),
            issued_at.into(),
            Utc::now().into(),
        ],
    ))
    .await?;
    record(
        &txn,
        caller,
        &row,
        "app.secret_rotated",
        json!({ "secretId": secret_id }),
    )
    .await?;
    txn.commit().await?;
    Ok((
        StatusCode::CREATED,
        json!({
            "clientId": project_id,
            "clientSecret": secret,
            "secretId": secret_id,
            "issuedAt": issued_at.to_rfc3339(),
        }),
    ))
}

/// `POST /v1/projects/{projectId}/app/submit`.
pub async fn submit(
    state: &AppState,
    caller: &Caller,
    project_id: Uuid,
) -> Result<Value, ApiError> {
    authorized(state, caller, project_id, Action::AppsManage).await?;
    let txn = state.db.begin().await?;
    let row = load(&txn, project_id, true)
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?;
    let submittable = match row.status.as_str() {
        "DRAFT" | "REJECTED" => true,
        "APPROVED" | "PUBLISHED" => row.pending_changes(),
        _ => false,
    };
    if !submittable {
        return Err(ApiError(ErrorCode::InvalidState));
    }
    let current = manifest_of(&txn, project_id, row.current_version).await?;
    if !manifest::reviewable(&current) {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    txn.execute(stmt(
        "UPDATE devplat_apps SET status = 'SUBMITTED', submitted_version = current_version, \
           decision_reason_code = NULL, updated_at = $2, version = version + 1 WHERE app_id = $1",
        vec![project_id.into(), Utc::now().into()],
    ))
    .await?;
    record_transition(
        &txn,
        state.bridge_events,
        project_id,
        row.tenant_id,
        Some(&row.status),
        "SUBMITTED",
        row.current_version,
        caller.actor_kind(),
        &caller.actor_ref(),
        None,
    )
    .await?;
    let row = load(&txn, project_id, true)
        .await?
        .ok_or(ApiError(ErrorCode::InternalError))?;
    sync(state, &txn, &row, &caller.request.correlation_id).await?;
    record(
        &txn,
        caller,
        &row,
        "app.submitted",
        json!({ "version": row.current_version }),
    )
    .await?;
    let v = view(&txn, &row).await?;
    txn.commit().await?;
    Ok(v)
}

/// A Platform Access grant as the developer sees it: no CF Meet account id.
fn grant_view(g: &Value) -> Option<Value> {
    let id = g.get("id").and_then(Value::as_str)?;
    Uuid::parse_str(id).ok()?;
    let text = |k: &str| {
        g.get(k)
            .filter(|v| v.is_string())
            .cloned()
            .unwrap_or(Value::Null)
    };
    let scopes = match g.get("scopes") {
        Some(Value::Array(a)) => {
            Value::Array(a.iter().filter(|s| s.is_string()).cloned().collect())
        }
        Some(Value::String(s)) => json!(s.split_whitespace().collect::<Vec<_>>()),
        _ => json!([]),
    };
    Some(json!({
        "id": id,
        "target": text("target"),
        "organizationId": text("organizationId"),
        "scopes": scopes,
        "status": text("status"),
        "createdAt": text("createdAt"),
        "revokedAt": text("revokedAt"),
    }))
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantsQuery {
    pub cursor: Option<String>,
}

/// `GET /v1/projects/{projectId}/app/grants`.
pub async fn grants(
    state: &AppState,
    caller: &Caller,
    project_id: Uuid,
    q: GrantsQuery,
) -> Result<Value, ApiError> {
    authorized(state, caller, project_id, Action::AppsRead).await?;
    load(&state.db, project_id, false)
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?;
    if q.cursor.as_deref().is_some_and(|c| {
        c.is_empty() || c.len() > 256 || !c.bytes().all(|b| (0x21..=0x7e).contains(&b))
    }) {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    let body = match state
        .oauth
        .client_grants(
            project_id,
            q.cursor.as_deref(),
            &caller.request.correlation_id,
        )
        .await
    {
        Ok(OAuthReply::Ok { body, .. }) => body,
        Ok(OAuthReply::Refused { status, code }) => return Err(client_refusal(status, &code)),
        Err(e) => {
            tracing::warn!(error = %e, "token authority unavailable");
            return Err(ApiError(ErrorCode::ServiceUnavailable));
        }
    };
    let items: Vec<Value> = body
        .get("items")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(grant_view).collect())
        .unwrap_or_default();
    let next = body
        .get("nextCursor")
        .filter(|c| c.is_string())
        .cloned()
        .unwrap_or(Value::Null);
    Ok(json!({ "items": items, "nextCursor": next }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PutTestAccounts {
    pub account_ids: Vec<Uuid>,
}

/// `PUT /v1/projects/{projectId}/app/test-accounts` (format checked only: Platform Access
/// and the Meeting Service decide who can authorize).
pub async fn put_test_accounts(
    state: &AppState,
    caller: &Caller,
    project_id: Uuid,
    body: PutTestAccounts,
) -> Result<Value, ApiError> {
    authorized(state, caller, project_id, Action::AppsManage).await?;
    let mut ids: Vec<Uuid> = Vec::new();
    for id in body.account_ids {
        if id.is_nil() {
            return Err(ApiError(ErrorCode::ValidationError));
        }
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    if ids.len() > manifest::MAX_TEST_ACCOUNTS {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    let txn = state.db.begin().await?;
    let row = load(&txn, project_id, true)
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?;
    if row.status == "SUSPENDED" {
        return Err(ApiError(ErrorCode::InvalidState));
    }
    txn.execute(stmt(
        "UPDATE devplat_apps SET test_account_ids = $2, updated_at = $3, version = version + 1 \
         WHERE app_id = $1",
        vec![project_id.into(), ids.clone().into(), Utc::now().into()],
    ))
    .await?;
    let row = load(&txn, project_id, true)
        .await?
        .ok_or(ApiError(ErrorCode::InternalError))?;
    sync(state, &txn, &row, &caller.request.correlation_id).await?;
    record(
        &txn,
        caller,
        &row,
        "app.test_accounts_updated",
        json!({ "count": ids.len() }),
    )
    .await?;
    let v = view(&txn, &row).await?;
    txn.commit().await?;
    Ok(v)
}

/// Re-syncs every non-suspended app of `tenant_id` (the PRIVATE allow list follows the
/// workspace membership). Best effort: a failure is logged, the next change re-syncs.
pub async fn resync_tenant(state: &AppState, tenant_id: Uuid) {
    if !state.build.apps {
        return;
    }
    let Ok(rows) = state
        .db
        .query_all(stmt(
            "SELECT app_id FROM devplat_apps WHERE tenant_id = $1 AND status <> 'SUSPENDED'",
            vec![tenant_id.into()],
        ))
        .await
    else {
        return;
    };
    let correlation = cf_service_auth::CorrelationId::generate();
    for r in rows {
        let Ok(app_id) = r.try_get::<Uuid>("", "app_id") else {
            continue;
        };
        let Ok(Some(row)) = load(&state.db, app_id, false).await else {
            continue;
        };
        if let Err(e) = sync(state, &state.db, &row, &correlation).await {
            tracing::warn!(code = e.0.as_str(), app = %app_id, "app client profile re-sync failed");
        }
    }
}

/// The manifest's strings at `key` (helper for the review and directory views).
pub fn manifest_strings(manifest: &Value, key: &str) -> Vec<String> {
    strings(&manifest[key])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(status: &str, current: i32, reviewed: Option<i32>, published: Option<i32>) -> AppRow {
        AppRow {
            app_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            status: status.into(),
            current_version: current,
            submitted_version: None,
            reviewed_version: reviewed,
            published_version: published,
            test_account_ids: vec![],
            secret_id: None,
            secret_issued_at: None,
            decision_reason_code: None,
            decided_at: None,
            published_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn the_live_version_is_the_reviewed_configuration() {
        let r = row("DRAFT", 3, None, None);
        assert_eq!((r.live_version(), r.client_status()), (3, "DRAFT"));
        let r = row("APPROVED", 3, Some(2), None);
        assert_eq!((r.live_version(), r.client_status()), (2, "APPROVED"));
        assert!(r.pending_changes());
        let r = row("SUBMITTED", 5, Some(4), Some(2));
        assert_eq!((r.live_version(), r.client_status()), (2, "PUBLISHED"));
        let r = row("SUSPENDED", 5, Some(4), Some(4));
        assert_eq!(r.client_status(), "SUSPENDED");
    }

    #[test]
    fn grants_never_show_account_ids() {
        let g = json!({ "id": Uuid::new_v4(), "target": "USER", "accountId": Uuid::new_v4(),
            "scopes": "profile:read meetings:write", "status": "ACTIVE" });
        let v = grant_view(&g).unwrap();
        assert!(v.get("accountId").is_none());
        assert_eq!(v["scopes"], json!(["profile:read", "meetings:write"]));
        assert!(grant_view(&json!({ "id": "nope" })).is_none());
    }
}
