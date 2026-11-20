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
        bridge_command::{self, BridgeCommand, Subject},
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
    /// ADR 0068 A4: SUSPENDED → the status the app had before (admin bridge only).
    LiftSuspension,
}

impl Command {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "review" => Command::Review,
            "approve" => Command::Approve,
            "reject" => Command::Reject,
            "publish" => Command::Publish,
            "suspend" => Command::Suspend,
            "lift-suspension" => Command::LiftSuspension,
            _ => return None,
        })
    }

    /// Whether only a named, non-member reviewer may run the command.
    pub fn needs_reviewer(&self) -> bool {
        !matches!(self, Command::Suspend | Command::LiftSuspension)
    }

    /// Commands served to the admin bridge only (the operator console never lifts a
    /// suspension: HQ decides it with two people).
    pub fn bridge_only(&self) -> bool {
        matches!(self, Command::LiftSuspension)
    }

    /// Commands answered with the standard A4 shape (`{status, version, resource}`)
    /// rather than HQ's verification case.
    pub fn standard_answer(&self) -> bool {
        matches!(self, Command::Suspend | Command::LiftSuspension)
    }

    /// Decisions HQ takes with two people (claiming a review is not one).
    pub fn two_person(&self) -> bool {
        !matches!(self, Command::Review)
    }

    /// The command's name in the bridge's operation record.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Command::Review => "app.review",
            Command::Approve => "app.approve",
            Command::Reject => "app.reject",
            Command::Publish => "app.publish",
            Command::Suspend => "app.suspend",
            Command::LiftSuspension => "app.lift-suspension",
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Decision {
    /// reject / suspend: a safe reason code.
    #[serde(default)]
    pub reason_code: Option<String>,
    /// Admin bridge decisions: the app row version the decision applies to.
    #[serde(default)]
    pub expected_version: Option<i64>,
}

/// `GET /internal/v1/operator/apps?status=`.
pub async fn operator_list(
    db: &DatabaseConnection,
    status: Option<&str>,
) -> Result<Value, ApiError> {
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
    let h: Result<Vec<Value>, _> = history
        .iter()
        .map(|r| r.try_get::<Value>("", "v"))
        .collect();
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

/// Runs an operator command on `app_id`. `bridge`: a decision relayed by the HQ admin
/// bridge ([`bridge_command`]): HQ's two-person approval replaces the named reviewers, the
/// change happens only at the expected version and is recorded in the same transaction.
#[allow(clippy::too_many_arguments)]
pub async fn operator_decide(
    state: &AppState,
    operator: &str,
    operator_user: Option<Uuid>,
    request: &RequestContext,
    app_id: Uuid,
    command: Command,
    d: Decision,
    bridge: Option<&BridgeCommand>,
) -> Result<Value, ApiError> {
    let current = load(&state.db, app_id, false)
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?;
    if let Some(b) = bridge {
        if let Some((st, v)) =
            bridge_command::prior(&state.db, b, command.as_str(), Subject::App, app_id).await?
        {
            if command.standard_answer() {
                return Ok(bridge_command::answer(&st, v, bridge_resource(&current)));
            }
            return view(&state.db, &current).await;
        }
        if let Some(user) = b.linked_user {
            if command.needs_reviewer()
                && membership(&state.db, current.tenant_id, user)
                    .await?
                    .is_some()
            {
                return Err(ApiError(ErrorCode::SelfApprovalForbidden));
            }
        }
    } else if command.bridge_only() {
        return Err(ApiError(ErrorCode::InternalPermissionDenied));
    } else if command.needs_reviewer() {
        // Named reviewers only (none named: no decision is possible).
        if !state.apps.reviewers.iter().any(|n| n == operator) {
            return Err(ApiError(ErrorCode::PolicyViolation));
        }
        let Some(user) = operator_user else {
            return Err(ApiError(ErrorCode::OperatorRequired));
        };
        if membership(&state.db, current.tenant_id, user)
            .await?
            .is_some()
        {
            return Err(ApiError(ErrorCode::SelfApprovalForbidden));
        }
    }
    let txn = state.db.begin().await?;
    if let Some(b) = bridge {
        bridge_command::lock_version(&txn, b, Subject::App, app_id).await?;
    }
    let row = load(&txn, app_id, true)
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?;
    let reviewer = format!("operator:{operator}");
    let now = Utc::now();
    // lift-suspension: back to the status the app had when it was suspended, with that
    // status's own decision reason (a rejection keeps its reason code).
    let lifted = if command == Command::LiftSuspension {
        Some(before_suspension(&txn, app_id).await?)
    } else {
        None
    };
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
        Command::LiftSuspension => {
            let c = reason(&d, "SUSPENSION_LIFTED")?;
            let (prior, prior_reason) = lifted.as_ref().ok_or(ApiError(ErrorCode::InternalError))?;
            (
                &["SUSPENDED"],
                prior.as_str(),
                ", decision_reason_code = $3",
                vec![prior_reason.clone().into()],
                Some(c),
                "app.suspension_lifted",
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
        Command::Suspend | Command::LiftSuspension => row.current_version,
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
    if let Some(b) = bridge {
        bridge_command::record(
            &txn,
            b,
            command.as_str(),
            Subject::App,
            app_id,
            row.tenant_id,
        )
        .await?;
        if command.standard_answer() {
            let version = app_version(&txn, app_id).await?;
            txn.commit().await?;
            return Ok(bridge_command::answer(
                &updated.status,
                Some(version),
                bridge_resource(&updated),
            ));
        }
    }
    txn.commit().await?;
    Ok(v)
}

/// The app as the admin bridge's A4 command answer shows it.
fn bridge_resource(row: &registration::AppRow) -> Value {
    json!({
        "appId": row.app_id,
        "organizationId": row.tenant_id,
        "state": row.status,
        "publishedVersion": row.published_version,
        "updatedAt": row.updated_at.to_rfc3339(),
    })
}

async fn app_version<C: ConnectionTrait>(db: &C, app_id: Uuid) -> Result<i64, ApiError> {
    Ok(db
        .query_one(stmt(
            "SELECT version FROM devplat_apps WHERE app_id = $1",
            vec![app_id.into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::InternalError))?
        .try_get("", "version")?)
}

/// `(status, decision reason)` the app had when it was last suspended (from the
/// append-only transition history).
async fn before_suspension<C: ConnectionTrait>(
    db: &C,
    app_id: Uuid,
) -> Result<(String, Option<String>), ApiError> {
    let row = db
        .query_one(stmt(
            "SELECT t.from_status, \
                    (SELECT p.reason_code FROM devplat_app_transitions p \
                      WHERE p.app_id = t.app_id AND p.to_status = t.from_status AND p.seq < t.seq \
                      ORDER BY p.seq DESC LIMIT 1) AS prior_reason \
             FROM devplat_app_transitions t WHERE t.app_id = $1 AND t.to_status = 'SUSPENDED' \
             ORDER BY t.seq DESC LIMIT 1",
            vec![app_id.into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::InvalidState))?;
    let from: Option<String> = row.try_get("", "from_status")?;
    let from = from
        .filter(|f| STATUSES.contains(&f.as_str()) && f != "SUSPENDED")
        .ok_or(ApiError(ErrorCode::InvalidState))?;
    let reason: Option<String> = row.try_get("", "prior_reason")?;
    // Only a rejection carries its decision reason; every other status has none.
    Ok((from.clone(), reason.filter(|_| from == "REJECTED")))
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
