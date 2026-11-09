//! Operator administration of the control plane (`/internal/v1/operator/**`).
//!
//! Reachable only with `developer-platform.administer` (registry-grantable to the
//! operator console alone) AND a named human operator (`x-cf-operator`). Every command
//! writes an OPERATOR activity event. This is where the explicit, audited decisions live
//! that customers can never make themselves: restricting / suspending an organization,
//! publishing plan versions, assigning plans, internal cost allocation, and registering a
//! FIRST_PARTY organization (the tenant's first-party classification is Platform Access's,
//! set by its own operator tooling; it is verified here, never inferred from a name).

use chrono::Utc;
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    activity::{self, Event},
    delegate_with, display_name, email, members, organizations,
    pagination::Page,
    stmt,
};
use crate::{
    domain::{
        rbac::{OrganizationState, Role},
        RequestContext,
    },
    http::error::{ApiError, ErrorCode},
    platform_access::DelegationCall,
    state::AppState,
};

/// `^[A-Za-z0-9][A-Za-z0-9._@-]{0,99}$` (an opaque operator handle).
pub fn valid_operator(value: &str) -> bool {
    let b = value.as_bytes();
    !b.is_empty()
        && b.len() <= 100
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || b"._@-".contains(c))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegisterFirstParty {
    pub tenant_id: Uuid,
    pub display_name: String,
    pub owner_email: String,
}

/// Registers the organization of an existing FIRST_PARTY Platform Access tenant: state
/// INVITED with an OWNER invitation (token returned once); the owner's acceptance
/// activates it.
pub async fn register_first_party(
    state: &AppState,
    operator: &str,
    request: &RequestContext,
    body: RegisterFirstParty,
) -> Result<Value, ApiError> {
    let name = display_name(&body.display_name)?;
    let owner = email(&body.owner_email)?;
    let tenant = delegate_with(
        state,
        &request.correlation_id,
        DelegationCall::GetTenant {
            tenant_id: body.tenant_id,
        },
    )
    .await?;
    if tenant.get("classification").and_then(Value::as_str) != Some("FIRST_PARTY") {
        return Err(ApiError(ErrorCode::PolicyViolation));
    }
    let now = Utc::now();
    let txn = state.db.begin().await?;
    let inserted = txn
        .execute(stmt(
            "INSERT INTO devplat_organizations \
                 (tenant_id, display_name, state, state_reason, created_by, created_at, updated_at) \
             VALUES ($1, $2, 'INVITED', 'AWAITING_OWNER', $3, $4, $4) ON CONFLICT DO NOTHING",
            vec![
                body.tenant_id.into(),
                name.clone().into(),
                format!("operator:{operator}").into(),
                now.into(),
            ],
        ))
        .await?
        .rows_affected();
    if inserted != 1 {
        return Err(ApiError(ErrorCode::InvalidState));
    }
    let (invitation, token) = members::insert_invitation(
        &txn,
        body.tenant_id,
        &owner,
        Role::Owner,
        &format!("operator:{operator}"),
    )
    .await?;
    activity::record_operator(
        &txn,
        operator,
        request.request_id.as_str(),
        request.correlation_id.as_str(),
        Event {
            tenant_id: body.tenant_id,
            action: "organization.first_party_registered",
            target_type: "ORGANIZATION",
            target_id: body.tenant_id.to_string(),
            summary: json!({ "displayName": name, "classification": "FIRST_PARTY",
                             "ownerInvitationId": invitation["id"] }),
        },
    )
    .await?;
    let org = organizations::view(&txn, body.tenant_id, None).await?;
    txn.commit().await?;
    let mut invitation = invitation;
    invitation["token"] = Value::from(token);
    Ok(json!({ "organization": org, "ownerInvitation": invitation }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetState {
    pub state: String,
    pub reason: String,
}

pub fn valid_reason(reason: &str) -> bool {
    let b = reason.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && b[0].is_ascii_uppercase()
        && b.iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == b'_')
}

/// A read model of a tenant for operators (organization + members count + project count).
pub async fn organization_overview<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
) -> Result<Value, ApiError> {
    let mut org = organizations::view(db, tenant_id, None).await?;
    let counts = db
        .query_one(stmt(
            "SELECT (SELECT count(*) FROM devplat_workspace_members WHERE tenant_id = $1 AND status = 'ACTIVE') AS members, \
                    (SELECT count(*) FROM devplat_project_profiles WHERE tenant_id = $1) AS projects",
            vec![tenant_id.into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::InternalError))?;
    org["activeMembers"] = Value::from(counts.try_get::<i64>("", "members")?);
    org["projects"] = Value::from(counts.try_get::<i64>("", "projects")?);
    Ok(org)
}

/// Section 15 (PLT-009): records an operator command that is not about one organization
/// (`devplat_operator_audit`), in `db` — the command's transaction where it has one.
/// Tenant-scoped commands keep recording OPERATOR activity events instead.
pub async fn audit_command<C: ConnectionTrait>(
    db: &C,
    operator: &str,
    request: &RequestContext,
    action: &str,
    target_type: &str,
    target_id: &str,
    summary: Value,
) -> Result<(), ApiError> {
    db.execute(stmt(
        "INSERT INTO devplat_operator_audit \
             (id, operator, action, target_type, target_id, request_id, correlation_id, summary, occurred_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        vec![
            Uuid::new_v4().into(),
            operator.into(),
            action.into(),
            target_type.into(),
            target_id.chars().take(64).collect::<String>().into(),
            request.request_id.as_str().into(),
            request.correlation_id.as_str().into(),
            summary.into(),
            Utc::now().into(),
        ],
    ))
    .await?;
    tracing::info!(operator, action, target_type, "operator command audited");
    Ok(())
}

/// Escapes `LIKE` wildcards so an operator's search text matches literally.
fn like_literal(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('%');
    for c in text.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

/// `GET /internal/v1/operator/organizations` filters.
#[derive(Debug, Default, Clone)]
pub struct OrganizationSearch {
    /// Case-insensitive display-name substring, or an exact organization (tenant) id.
    pub query: Option<String>,
    pub state: Option<OrganizationState>,
}

/// Section 15 (PLT-009): operator list / search of organizations, newest first
/// (keyset cursor over `(created_at, tenant_id)`).
pub async fn list_organizations<C: ConnectionTrait>(
    db: &C,
    search: &OrganizationSearch,
    page: &Page,
) -> Result<Value, ApiError> {
    let query = search
        .query
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty());
    if query.is_some_and(|q| q.chars().count() > 120 || q.chars().any(char::is_control))
        || (page.has_cursor() && page.cursor_key().is_none())
    {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    let exact = query.and_then(|q| Uuid::parse_str(q).ok());
    let pattern = query.filter(|_| exact.is_none()).map(like_literal);
    let (after_at, after_id) = page
        .cursor_key()
        .map(|(a, i)| (Some(a), Some(i)))
        .unwrap_or((None, None));
    let rows = db
        .query_all(stmt(
            &format!(
                "SELECT o.created_at AS k_at, o.tenant_id AS k_id, {ORG_VIEW} AS v, o.created_by, \
                        o.platform_name_sync_pending \
                 FROM devplat_organizations o \
                 WHERE ($1::uuid IS NULL OR o.tenant_id = $1) \
                   AND ($2::text IS NULL OR o.display_name ILIKE $2 ESCAPE '\\') \
                   AND ($3::text IS NULL OR o.state = $3) \
                   AND ($4::timestamptz IS NULL OR (o.created_at, o.tenant_id) < ($4, $5::uuid)) \
                 ORDER BY o.created_at DESC, o.tenant_id DESC LIMIT $6",
                ORG_VIEW = organizations::ORG_VIEW
            ),
            vec![
                exact.into(),
                pattern.into(),
                search.state.map(|s| s.as_str().to_string()).into(),
                after_at.into(),
                after_id.into(),
                (page.limit as i64 + 1).into(),
            ],
        ))
        .await?;
    let mut items = Vec::new();
    let mut last = None;
    for r in rows.iter().take(page.limit) {
        let mut v: Value = r.try_get("", "v")?;
        v["myRole"] = Value::Null;
        v["createdBy"] = Value::from(r.try_get::<String>("", "created_by")?);
        v["platformNameSyncPending"] =
            Value::from(r.try_get::<bool>("", "platform_name_sync_pending")?);
        items.push(v);
        last = Some((r.try_get("", "k_at")?, r.try_get("", "k_id")?));
    }
    let next = (rows.len() > page.limit)
        .then(|| last.map(|(at, id)| Page::key_cursor(at, id)))
        .flatten();
    Ok(json!({ "items": items, "nextCursor": next }))
}

/// `GET /internal/v1/operator/activity` filters.
#[derive(Debug, Default, Clone)]
pub struct OperatorActivityFilter {
    pub organization_id: Option<Uuid>,
    pub operator: Option<String>,
    /// Exact action or a family prefix ending in `.`.
    pub action: Option<String>,
}

/// Section 15 (PLT-009): every operator command, newest first — the OPERATOR events of
/// organization activity (`scope = ORGANIZATION`) and the platform-level commands
/// (`scope = PLATFORM`) — keyset-paginated over `(occurred_at, id)`.
pub async fn operator_activity<C: ConnectionTrait>(
    db: &C,
    filter: &OperatorActivityFilter,
    page: &Page,
) -> Result<Value, ApiError> {
    activity::ActivityFilter {
        project_id: None,
        action: filter.action.clone(),
    }
    .validate()?;
    if filter
        .operator
        .as_deref()
        .is_some_and(|o| !valid_operator(o))
        || (page.has_cursor() && page.cursor_key().is_none())
    {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    let (exact, prefix) = match filter.action.as_deref() {
        Some(a) if a.ends_with('.') => (None, Some(format!("{a}%"))),
        Some(a) => (Some(a.to_string()), None),
        None => (None, None),
    };
    let (before_at, before_id) = page
        .cursor_key()
        .map(|(a, i)| (Some(a), Some(i)))
        .unwrap_or((None, None));
    let rows = db
        .query_all(stmt(
            "SELECT id, occurred_at, v FROM ( \
                 SELECT id, occurred_at, tenant_id, substr(actor_ref, 10) AS operator, action, \
                        jsonb_build_object('id', id, 'scope', 'ORGANIZATION', 'organizationId', tenant_id, \
                            'operator', substr(actor_ref, 10), 'action', action, 'targetType', target_type, \
                            'targetId', target_id, 'outcome', outcome, 'requestId', request_id, \
                            'correlationId', correlation_id, 'summary', summary, 'occurredAt', occurred_at) AS v \
                 FROM devplat_activity_events WHERE actor_kind = 'OPERATOR' \
                 UNION ALL \
                 SELECT id, occurred_at, NULL::uuid, operator, action, \
                        jsonb_build_object('id', id, 'scope', 'PLATFORM', 'organizationId', NULL, \
                            'operator', operator, 'action', action, 'targetType', target_type, \
                            'targetId', target_id, 'outcome', 'SUCCEEDED', 'requestId', request_id, \
                            'correlationId', correlation_id, 'summary', summary, 'occurredAt', occurred_at) \
                 FROM devplat_operator_audit) x \
             WHERE ($1::uuid IS NULL OR tenant_id = $1) \
               AND ($2::text IS NULL OR operator = $2) \
               AND ($3::text IS NULL OR action = $3) AND ($4::text IS NULL OR action LIKE $4) \
               AND ($5::timestamptz IS NULL OR (occurred_at, id) < ($5, $6::uuid)) \
             ORDER BY occurred_at DESC, id DESC LIMIT $7",
            vec![
                filter.organization_id.into(),
                filter.operator.clone().into(),
                exact.into(),
                prefix.into(),
                before_at.into(),
                before_id.into(),
                (page.limit as i64 + 1).into(),
            ],
        ))
        .await?;
    let mut items = Vec::new();
    let mut last = None;
    for r in rows.iter().take(page.limit) {
        items.push(r.try_get::<Value>("", "v")?);
        last = Some((r.try_get("", "occurred_at")?, r.try_get("", "id")?));
    }
    let next = (rows.len() > page.limit)
        .then(|| last.map(|(at, id)| Page::key_cursor(at, id)))
        .flatten();
    Ok(json!({ "items": items, "nextCursor": next }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_text_matches_literally() {
        assert_eq!(like_literal("acme"), "%acme%");
        assert_eq!(like_literal("50%_off\\"), "%50\\%\\_off\\\\%");
    }

    #[test]
    fn operator_handles_and_reasons_are_bounded_codes() {
        assert!(valid_operator("ops.alice@cf"));
        assert!(!valid_operator(""));
        assert!(!valid_operator("has space"));
        assert!(valid_reason("FRAUD_REVIEW"));
        assert!(!valid_reason("fraud review"));
        assert!(!valid_reason(&"A".repeat(65)));
    }
}
