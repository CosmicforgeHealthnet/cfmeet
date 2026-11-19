//! ADR 0065 — the app review workflow (the production-access pattern of
//! [`crate::control::production`]).
//!
//! ```text
//! DRAFT → SUBMITTED → IN_REVIEW → APPROVED | REJECTED → PUBLISHED, and SUSPENDED
//! ```
//!
//! * Operators (`developer-platform.administer` + named operator) decide through
//!   `/internal/v1/operator/apps/*` and the operator CLI.
//! * review / approve / reject / publish: only a reviewer named in
//!   `DEVELOPER_PLATFORM_APP_REVIEWERS` (none named: nobody), identified by the reviewing
//!   operator's CF Meet user id (`x-cf-operator-user-id`), who must NOT be a member of the
//!   app's workspace (no self-review).
//! * suspend: any administering operator (a safety action); Platform Access then refuses
//!   every authorization and revokes every grant.
//! * Every transition replaces the Platform Access client profile in the same transaction
//!   (refused or unreachable: nothing changes) and is audited (`devplat_app_transitions`
//!   and the workspace activity history).

use chrono::Utc;
use sea_orm::{ConnectionTrait, DatabaseConnection, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::registration::{self, load, record_transition, sync, view, STATUSES};
use crate::{
    control::{
        activity::{self, Event},
        membership, stmt,
    },
    domain::RequestContext,
    http::error::{ApiError, ErrorCode},
    state::AppState,
};

/// The operator commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Review,
    Approve,
    Reject,
    Publish,
    Suspend,
}

impl Command {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "review" => Command::Review,
            "approve" => Command::Approve,
            "reject" => Command::Reject,
            "publish" => Command::Publish,
            "suspend" => Command::Suspend,
            _ => return None,
        })
    }

    /// Whether only a named, non-member reviewer may run the command.
    pub fn needs_reviewer(&self) -> bool {
        !matches!(self, Command::Suspend)
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Decision {
    /// reject / suspend: a safe reason code.
    #[serde(default)]
    pub reason_code: Option<String>,
}

/// `GET /internal/v1/operator/apps?status=`.
pub async fn operator_list(db: &DatabaseConnection, status: Option<&str>) -> Result<Value, ApiError> {
    if status.is_some_and(|s| !STATUSES.contains(&s)) {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    let rows = db
        .query_all(stmt(
            "SELECT app_id FROM devplat_apps WHERE ($1::text IS NULL OR status = $1) \
             ORDER BY updated_at, app_id LIMIT 200",
            vec![status.map(str::to_string).into()],
        ))
        .await?;
    let mut items = Vec::new();
    for r in rows {
        let id: Uuid = r.try_get("", "app_id")?;
        if let Some(row) = load(db, id, false).await? {
            items.push(view(db, &row).await?);
        }
    }
    Ok(json!({ "items": items, "nextCursor": null }))
}

/// `GET /internal/v1/operator/apps/{appId}`: the app, the manifest under review and the
/// review history.
pub async fn operator_get(db: &DatabaseConnection, app_id: Uuid) -> Result<Value, ApiError> {
    let row = load(db, app_id, false)
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?;
    let mut v = view(db, &row).await?;
    v["submittedManifest"] = match row.submitted_version {
        Some(s) => registration::manifest_of(db, app_id, s).await?,
        None => Value::Null,
    };
    let developer: Option<String> = db
        .query_one(stmt(
            "SELECT display_name FROM devplat_organizations WHERE tenant_id = $1",
            vec![row.tenant_id.into()],
        ))
        .await?
        .map(|r| r.try_get("", "display_name"))
        .transpose()?;
    v["developerName"] = json!(developer);
    let history = db
        .query_all(stmt(
            "SELECT jsonb_build_object('fromStatus', from_status, 'toStatus', to_status, \
               'version', app_version, 'actorKind', actor_kind, 'reasonCode', reason_code, \
               'occurredAt', occurred_at) AS v FROM devplat_app_transitions WHERE app_id = $1 ORDER BY seq",
            vec![app_id.into()],
        ))
        .await?;
    let h: Result<Vec<Value>, _> = history.iter().map(|r| r.try_get::<Value>("", "v")).collect();
    v["history"] = Value::Array(h?);
    Ok(v)
}

fn reason(d: &Decision, default: &str) -> Result<String, ApiError> {
    let r = d.reason_code.clone().unwrap_or_else(|| default.to_string());
    if crate::control::operator::valid_reason(&r) {
        Ok(r)
    } else {
        Err(ApiError(ErrorCode::ValidationError))
    }
}

/// Runs an operator command on `app_id`.
pub async fn operator_decide(
    state: &AppState,
    operator: &str,
    operator_user: Option<Uuid>,
    request: &RequestContext,
    app_id: Uuid,
    command: Command,
    d: Decision,
) -> Result<Value, ApiError> {
    let current = load(&state.db, app_id, false)
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?;
    if command.needs_reviewer() {
        // Named reviewers only (none named: no decision is possible).
        if !state.apps.reviewers.iter().any(|n| n == operator) {
            return Err(ApiError(ErrorCode::PolicyViolation));
        }
        let Some(user) = operator_user else {
            return Err(ApiError(ErrorCode::OperatorRequired));
        };
        if membership(&state.db, current.tenant_id, user).await?.is_some() {
            return Err(ApiError(ErrorCode::SelfApprovalForbidden));
        }
    }
    let txn = state.db.begin().await?;
    let row = load(&txn, app_id, true)
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?;
    let reviewer = format!("operator:{operator}");
    let now = Utc::now();
    let (from, to, set, values, code, action): (
        &[&str],
        &str,
        &str,
        Vec<sea_orm::Value>,
        Option<String>,
        &str,
    ) = match command {
        Command::Review => (
            &["SUBMITTED"],
            "IN_REVIEW",
            ", reviewer_ref = $3",
            vec![reviewer.clone().into()],
            None,
            "app.in_review",
        ),
        Command::Approve => (
            &["IN_REVIEW"],
            "APPROVED",
            ", reviewer_ref = $3, reviewed_version = submitted_version, decision_reason_code = NULL, \
               decided_at = $2",
            vec![reviewer.clone().into()],
            None,
            "app.approved",
        ),
        Command::Reject => {
            let c = reason(&d, "NOT_ELIGIBLE")?;
            (
                &["SUBMITTED", "IN_REVIEW"],
                "REJECTED",
                ", reviewer_ref = $3, decision_reason_code = $4, decided_at = $2",
                vec![reviewer.clone().into(), c.clone().into()],
                Some(c),
                "app.rejected",
            )
        }
        Command::Publish => (
            &["APPROVED"],
            "PUBLISHED",
            ", reviewer_ref = $3, published_version = reviewed_version, published_at = $2",
            vec![reviewer.clone().into()],
            None,
            "app.published",
        ),
        Command::Suspend => {
            let c = reason(&d, "OPERATOR_SUSPENDED")?;
            (
                &["DRAFT", "SUBMITTED", "IN_REVIEW", "APPROVED", "REJECTED", "PUBLISHED"],
                "SUSPENDED",
                ", decision_reason_code = $3, decided_at = $2",
                vec![c.clone().into()],
                Some(c),
                "app.suspended",
            )
        }
    };
    if !from.contains(&row.status.as_str()) {
        return Err(ApiError(ErrorCode::InvalidState));
    }
    let mut all: Vec<sea_orm::Value> = vec![app_id.into(), now.into()];
    all.extend(values);
    txn.execute(stmt(
        &format!(
            "UPDATE devplat_apps SET status = '{to}', updated_at = $2, version = version + 1{set} \
             WHERE app_id = $1"
        ),
        all,
    ))
    .await?;
    let version = match command {
        Command::Review | Command::Approve | Command::Reject => {
            row.submitted_version.unwrap_or(row.current_version)
        }
        Command::Publish => row.reviewed_version.unwrap_or(row.current_version),
        Command::Suspend => row.current_version,
    };
    record_transition(
        &txn,
        app_id,
        row.tenant_id,
        Some(&row.status),
        to,
        version,
        "OPERATOR",
        &reviewer,
        code.as_deref(),
    )
    .await?;
    activity::record_operator(
        &txn,
        operator,
        request.request_id.as_str(),
        request.correlation_id.as_str(),
        Event {
            tenant_id: row.tenant_id,
            action,
            target_type: "APP",
            target_id: app_id.to_string(),
            summary: json!({ "version": version, "reasonCode": code }),
        },
    )
    .await?;
    let updated = load(&txn, app_id, true)
        .await?
        .ok_or(ApiError(ErrorCode::InternalError))?;
    // Platform Access first: a suspension withdraws every grant before it is reported.
    sync(state, &txn, &updated, &request.correlation_id).await?;
    let v = view(&txn, &updated).await?;
    txn.commit().await?;
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_explicit() {
        assert_eq!(Command::parse("publish"), Some(Command::Publish));
        assert_eq!(Command::parse("auto-approve"), None);
        assert!(Command::Approve.needs_reviewer() && !Command::Suspend.needs_reviewer());
    }
}
