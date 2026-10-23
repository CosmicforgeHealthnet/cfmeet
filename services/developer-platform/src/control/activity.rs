//! Immutable tenant-scoped activity history (Section 50 §6.5).
//!
//! Every human / operator / machine control-plane mutation writes ONE row in the SAME
//! transaction as the change (the row exists iff the change committed); denied attempts
//! by members of the tenant are recorded too. Rows carry the actor reference, action,
//! target, request / correlation ids, outcome and a SAFE change summary: opaque ids,
//! bounded codes and display metadata only — never a credential, webhook secret, email
//! address, contact or E2EE material (the database refuses such keys; the table is
//! append-only by trigger).

use chrono::Utc;
use sea_orm::ConnectionTrait;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::{pagination::Page, stmt, Caller};
use crate::{
    domain::rbac::Action,
    http::error::{ApiError, ErrorCode},
};

/// Summary keys that may never be recorded (defence in depth; the table also refuses).
const FORBIDDEN_KEYS: &[&str] = &[
    "secret",
    "signingSecret",
    "credential",
    "password",
    "token",
    "apiKey",
    "verifier",
    "privateKey",
    "phoneNumber",
    "email",
    "contact",
];

fn safe_summary(summary: Value) -> Value {
    match summary {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(k, _)| !FORBIDDEN_KEYS.iter().any(|f| k.eq_ignore_ascii_case(f)))
                .collect::<Map<_, _>>(),
        ),
        _ => json!({}),
    }
}

/// An activity event to record.
pub struct Event<'a> {
    pub tenant_id: Uuid,
    pub action: &'a str,
    pub target_type: &'a str,
    pub target_id: String,
    pub summary: Value,
}

/// Records a SUCCEEDED event of `caller` in `db` (the mutation's transaction).
pub async fn record<C: ConnectionTrait>(
    db: &C,
    caller: &Caller,
    event: Event<'_>,
) -> Result<(), ApiError> {
    insert(
        db,
        event.tenant_id,
        caller.actor_kind(),
        &caller.actor_ref(),
        event.action,
        event.target_type,
        &event.target_id,
        "SUCCEEDED",
        caller.request.request_id.as_str(),
        caller.request.correlation_id.as_str(),
        event.summary,
    )
    .await
}

/// Records an operator event.
#[allow(clippy::too_many_arguments)]
pub async fn record_operator<C: ConnectionTrait>(
    db: &C,
    operator: &str,
    request_id: &str,
    correlation_id: &str,
    event: Event<'_>,
) -> Result<(), ApiError> {
    insert(
        db,
        event.tenant_id,
        "OPERATOR",
        &format!("operator:{operator}"),
        event.action,
        event.target_type,
        &event.target_id,
        "SUCCEEDED",
        request_id,
        correlation_id,
        event.summary,
    )
    .await
}

fn action_name(action: Action) -> &'static str {
    match action {
        Action::OrganizationRead => "organization.read",
        Action::OrganizationUpdate => "organization.update",
        Action::OrganizationClose => "organization.close",
        Action::MembersRead => "member.read",
        Action::MembersManage => "member.manage",
        Action::OwnershipTransfer => "organization.ownership_transfer",
        Action::ProjectsRead => "project.read",
        Action::ProjectsManage => "project.manage",
        Action::EnvironmentsManage => "environment.manage",
        Action::WebhooksRead => "webhook.read",
        Action::WebhooksManage => "webhook.manage",
        Action::UsageRead => "usage.read",
        Action::UsageExport => "usage.export",
        Action::LimitsRead => "limits.read",
        Action::ActivityRead => "activity.read",
        Action::CommercialRead => "commercial.read",
        Action::BillingAccountManage => "billing_account.manage",
        Action::ProductionAccessRequest => "production_access.request",
        Action::CredentialsManage => "credential.manage",
        Action::ServiceAccountsRead => "service_account.read",
        Action::ServiceAccountsManage => "service_account.manage",
        Action::ServiceAccountsSuspend => "service_account.suspend",
    }
}

/// Records a DENIED attempt (best effort, outside any transaction).
pub async fn record_denied<C: ConnectionTrait>(
    db: &C,
    caller: &Caller,
    tenant_id: Uuid,
    action: Action,
    code: ErrorCode,
) {
    if let Err(e) = insert(
        db,
        tenant_id,
        caller.actor_kind(),
        &caller.actor_ref(),
        action_name(action),
        "ORGANIZATION",
        &tenant_id.to_string(),
        "DENIED",
        caller.request.request_id.as_str(),
        caller.request.correlation_id.as_str(),
        json!({ "reason": code.as_str() }),
    )
    .await
    {
        tracing::warn!(error = ?e, "denied activity not recorded");
    }
}

#[allow(clippy::too_many_arguments)]
async fn insert<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
    actor_kind: &str,
    actor_ref: &str,
    action: &str,
    target_type: &str,
    target_id: &str,
    outcome: &str,
    request_id: &str,
    correlation_id: &str,
    summary: Value,
) -> Result<(), ApiError> {
    db.execute(stmt(
        "INSERT INTO devplat_activity_events \
             (id, tenant_id, actor_kind, actor_ref, action, target_type, target_id, outcome, \
              request_id, correlation_id, summary, occurred_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        vec![
            Uuid::new_v4().into(),
            tenant_id.into(),
            actor_kind.into(),
            actor_ref.chars().take(100).collect::<String>().into(),
            action.into(),
            target_type.into(),
            target_id.chars().take(64).collect::<String>().into(),
            outcome.into(),
            request_id.into(),
            correlation_id.into(),
            safe_summary(summary).into(),
            Utc::now().into(),
        ],
    ))
    .await?;
    Ok(())
}

/// One page of a tenant's activity, newest first (cursor: the event sequence).
pub async fn list<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
    page: &Page,
) -> Result<Value, ApiError> {
    let before = page.cursor_seq().unwrap_or(i64::MAX);
    let rows = db
        .query_all(stmt(
            "SELECT seq, jsonb_build_object( \
                 'id', id, 'organizationId', tenant_id, 'actorKind', actor_kind, \
                 'actorRef', actor_ref, 'action', action, 'targetType', target_type, \
                 'targetId', target_id, 'outcome', outcome, 'requestId', request_id, \
                 'correlationId', correlation_id, 'summary', summary, 'occurredAt', occurred_at) AS v \
             FROM devplat_activity_events WHERE tenant_id = $1 AND seq < $2 \
             ORDER BY seq DESC LIMIT $3",
            vec![tenant_id.into(), before.into(), (page.limit as i64 + 1).into()],
        ))
        .await?;
    let mut items = Vec::new();
    let mut last = None;
    for r in rows.iter().take(page.limit) {
        items.push(r.try_get::<Value>("", "v")?);
        last = Some(r.try_get::<i64>("", "seq")?);
    }
    let next = (rows.len() > page.limit)
        .then(|| last.map(Page::seq_cursor))
        .flatten();
    Ok(json!({ "items": items, "nextCursor": next }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_never_carry_secret_or_contact_keys() {
        let s = safe_summary(json!({
            "displayName": "Acme", "secret": "s", "Email": "a@b", "token": "t", "role": "ADMIN"
        }));
        assert_eq!(s, json!({ "displayName": "Acme", "role": "ADMIN" }));
        assert_eq!(safe_summary(json!([1, 2])), json!({}));
    }
}
