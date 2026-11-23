//! The admin bridge's operator reads (ADR 0068, design §6.8): `/internal/v1/operator/*`.
//!
//! Reachable only with `platform-identity.administer` AND the admin bridge's strict HQ
//! operator attribution (`X-CF-Operator: hq:<staff>`, parsed by the middleware). Read-only.
//!
//! Platform Access is the identity authority, so these reads are deliberately narrow:
//! * never a credential secret, verifier, verifier algorithm or key version, client
//!   secret, token or refresh token: credentials carry their public id and last four only;
//! * never a principal's external reference (a partner's own user id, which may be an email
//!   address; plaintext telephone numbers are refused at registration): principals carry their opaque id, kind and status. A search may
//!   MATCH on the external reference (exact match only), it never returns it;
//! * never free text (service-account descriptions, credential labels) and never the
//!   audit `detail`;
//! * OAuth clients carry their scopes and the number of redirect URIs, not the URIs or
//!   test-account ids.
//!
//! Every list is keyset paginated (`limit` 1–100, default 50; `cursor` = `nextCursor`).

use axum::{
    extract::{Path, Query, State},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use cf_service_auth::InternalPermission;
use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, DbBackend, QueryResult, Statement, Value as DbValue};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::error::{AccessError, AccessResult, ErrorCode};
use super::middleware::InternalCaller;
use crate::{identity::PlatformError, state::AppState};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/operator/tenants", get(tenants))
        .route(
            "/operator/tenants/:tenant_id/applications",
            get(applications),
        )
        .route(
            "/operator/tenants/:tenant_id/environments",
            get(environments),
        )
        .route(
            "/operator/tenants/:tenant_id/service-accounts",
            get(service_accounts),
        )
        .route(
            "/operator/tenants/:tenant_id/scope-grants",
            get(scope_grants),
        )
        .route("/operator/tenants/:tenant_id/credentials", get(credentials))
        .route(
            "/operator/tenants/:tenant_id/principals",
            get(tenant_principals),
        )
        .route("/operator/tenants/:tenant_id/origins", get(origins))
        .route("/operator/principals", get(principals))
        .route("/operator/oauth-apps", get(oauth_apps))
        .route("/operator/oauth-apps/:app_id/grants", get(oauth_grants))
        .route("/operator/audit", get(audit))
}

pub const PAGE_DEFAULT: u64 = 50;
pub const PAGE_MAX: u64 = 100;
/// Longest audit window a single read may ask for.
pub const MAX_WINDOW_DAYS: i64 = 31;

/// Permission + the admin bridge's named HQ operator.
fn bridge(caller: &InternalCaller) -> AccessResult<()> {
    caller.require_permission(InternalPermission::PlatformIdentityAdminister)?;
    caller
        .bridge_operator()
        .map(|_| ())
        .ok_or(AccessError::Refused(ErrorCode::OperatorRequired))
}

fn invalid(message: &str) -> AccessError {
    AccessError::Operator(ErrorCode::ValidationError, message.to_string())
}

fn db_err(e: sea_orm::DbErr) -> AccessError {
    AccessError::operator(PlatformError::from(e))
}

pub fn page_limit(limit: Option<u64>) -> AccessResult<u64> {
    let l = limit.unwrap_or(PAGE_DEFAULT);
    if (1..=PAGE_MAX).contains(&l) {
        Ok(l)
    } else {
        Err(invalid("limit must be between 1 and 100"))
    }
}

fn uuid_cursor(cursor: &Option<String>) -> AccessResult<Option<Uuid>> {
    cursor
        .as_deref()
        .map(|c| Uuid::parse_str(c).map_err(|_| invalid("malformed cursor")))
        .transpose()
}

/// `%term%` for a case-insensitive LIKE with the metacharacters matched literally.
fn like(q: &Option<String>) -> AccessResult<Option<String>> {
    match q.as_deref().map(str::trim) {
        None | Some("") => Ok(None),
        Some(t) if t.chars().count() > 128 => Err(invalid("q must be at most 128 characters")),
        Some(t) => Ok(Some(format!(
            "%{}%",
            t.to_lowercase()
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        ))),
    }
}

/// Runs a page query: `sql` selects `v` (json) plus whatever `cursor_of` reads, and takes
/// `limit + 1` as its LAST parameter.
async fn page(
    s: &AppState,
    sql: &str,
    mut values: Vec<DbValue>,
    limit: u64,
    cursor_of: impl Fn(&QueryResult) -> Result<String, sea_orm::DbErr>,
) -> AccessResult<Json<Value>> {
    values.push(((limit + 1) as i64).into());
    let rows = s
        .platform
        .db()
        .query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            values,
        ))
        .await
        .map_err(db_err)?;
    let more = rows.len() as u64 > limit;
    let mut items = Vec::new();
    for r in rows.iter().take(limit as usize) {
        items.push(r.try_get::<Value>("", "v").map_err(db_err)?);
    }
    let next = if more {
        Some(cursor_of(&rows[limit as usize - 1]).map_err(db_err)?)
    } else {
        None
    };
    Ok(Json(json!({ "items": items, "nextCursor": next })))
}

fn id_cursor(r: &QueryResult) -> Result<String, sea_orm::DbErr> {
    Ok(r.try_get::<Uuid>("", "id")?.to_string())
}

async fn tenant_exists(s: &AppState, tenant_id: Uuid) -> AccessResult<()> {
    let row = s
        .platform
        .db()
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT 1 AS one FROM platform_tenants WHERE id = $1",
            [tenant_id.into()],
        ))
        .await
        .map_err(db_err)?;
    row.map(|_| ())
        .ok_or(AccessError::Refused(ErrorCode::NotFound))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageQuery {
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    limit: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchQuery {
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    limit: Option<u64>,
}

/// `GET /operator/tenants?q&cursor&limit` — tenants in id order; `q` matches the slug or
/// display name (case-insensitive substring).
async fn tenants(
    State(s): State<AppState>,
    caller: InternalCaller,
    Query(q): Query<SearchQuery>,
) -> AccessResult<impl IntoResponse> {
    bridge(&caller)?;
    let limit = page_limit(q.limit)?;
    page(
        &s,
        "SELECT id, json_build_object('id', id, 'slug', slug, 'displayName', display_name, \
           'classification', classification, 'status', status, 'billingCurrency', billing_currency, \
           'revision', revision, 'createdAt', created_at, 'updatedAt', updated_at) AS v \
         FROM platform_tenants \
         WHERE ($1::uuid IS NULL OR id > $1) \
           AND ($2::text IS NULL OR lower(slug) LIKE $2 ESCAPE '\\' OR lower(display_name) LIKE $2 ESCAPE '\\') \
         ORDER BY id LIMIT $3",
        vec![uuid_cursor(&q.cursor)?.into(), like(&q.q)?.into()],
        limit,
        id_cursor,
    )
    .await
}

/// `GET /operator/tenants/{tenantId}/applications`.
async fn applications(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path(tenant_id): Path<Uuid>,
    Query(q): Query<PageQuery>,
) -> AccessResult<impl IntoResponse> {
    bridge(&caller)?;
    let limit = page_limit(q.limit)?;
    tenant_exists(&s, tenant_id).await?;
    page(
        &s,
        "SELECT id, json_build_object('id', id, 'tenantId', tenant_id, 'slug', slug, \
           'displayName', display_name, 'status', status, 'revision', revision, \
           'createdAt', created_at, 'updatedAt', updated_at) AS v \
         FROM platform_applications WHERE tenant_id = $1 AND ($2::uuid IS NULL OR id > $2) \
         ORDER BY id LIMIT $3",
        vec![tenant_id.into(), uuid_cursor(&q.cursor)?.into()],
        limit,
        id_cursor,
    )
    .await
}

/// `GET /operator/tenants/{tenantId}/environments`.
async fn environments(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path(tenant_id): Path<Uuid>,
    Query(q): Query<PageQuery>,
) -> AccessResult<impl IntoResponse> {
    bridge(&caller)?;
    let limit = page_limit(q.limit)?;
    tenant_exists(&s, tenant_id).await?;
    page(
        &s,
        "SELECT id, json_build_object('id', id, 'applicationId', application_id, 'kind', kind, \
           'status', status, 'liveTelecomAllowed', live_telecom_allowed, 'revision', revision, \
           'createdAt', created_at, 'updatedAt', updated_at) AS v \
         FROM platform_environments WHERE tenant_id = $1 AND ($2::uuid IS NULL OR id > $2) \
         ORDER BY id LIMIT $3",
        vec![tenant_id.into(), uuid_cursor(&q.cursor)?.into()],
        limit,
        id_cursor,
    )
    .await
}

/// `GET /operator/tenants/{tenantId}/service-accounts` (no description: free text).
async fn service_accounts(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path(tenant_id): Path<Uuid>,
    Query(q): Query<PageQuery>,
) -> AccessResult<impl IntoResponse> {
    bridge(&caller)?;
    let limit = page_limit(q.limit)?;
    tenant_exists(&s, tenant_id).await?;
    page(
        &s,
        "SELECT id, json_build_object('id', id, 'applicationId', application_id, \
           'environmentId', environment_id, 'name', name, 'status', status, 'revokedAt', revoked_at, \
           'revision', revision, 'createdAt', created_at, 'updatedAt', updated_at) AS v \
         FROM platform_service_accounts WHERE tenant_id = $1 AND ($2::uuid IS NULL OR id > $2) \
         ORDER BY id LIMIT $3",
        vec![tenant_id.into(), uuid_cursor(&q.cursor)?.into()],
        limit,
        id_cursor,
    )
    .await
}

/// `GET /operator/tenants/{tenantId}/scope-grants` — application scope grants ordered by
/// (application, scope); `cursor` is `<applicationId>:<scope>` of the last row.
async fn scope_grants(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path(tenant_id): Path<Uuid>,
    Query(q): Query<PageQuery>,
) -> AccessResult<impl IntoResponse> {
    bridge(&caller)?;
    let limit = page_limit(q.limit)?;
    tenant_exists(&s, tenant_id).await?;
    let (after_app, after_scope) = match q.cursor.as_deref() {
        None => (None, None),
        Some(c) => {
            let (app, scope) = c
                .split_once(':')
                .ok_or_else(|| invalid("malformed cursor"))?;
            let app = Uuid::parse_str(app).map_err(|_| invalid("malformed cursor"))?;
            if scope.is_empty() || scope.len() > 100 {
                return Err(invalid("malformed cursor"));
            }
            (Some(app), Some(scope.to_string()))
        }
    };
    page(
        &s,
        "SELECT application_id, scope, json_build_object('applicationId', application_id, \
           'scope', scope, 'allowedEnvironmentKinds', allowed_environment_kinds, \
           'grantBasis', grant_basis, 'grantedAt', granted_at) AS v \
         FROM platform_application_scopes WHERE tenant_id = $1 \
           AND ($2::uuid IS NULL OR (application_id, scope) > ($2, $3::text)) \
         ORDER BY application_id, scope LIMIT $4",
        vec![tenant_id.into(), after_app.into(), after_scope.into()],
        limit,
        |r| {
            Ok(format!(
                "{}:{}",
                r.try_get::<Uuid>("", "application_id")?,
                r.try_get::<String>("", "scope")?
            ))
        },
    )
    .await
}

/// `GET /operator/tenants/{tenantId}/credentials` — API keys: public id, last four,
/// scopes, state and times. Never the verifier, its algorithm or key version, or a label.
/// `version` (`revision` + suspension changes) is the bridge commands' expected version.
/// `delivery` (null for a key the partner created themselves) is the reference of an
/// owner-delivered key issued or rotated through the bridge: channel, ref, state
/// (`PENDING` / `REVEALED` / `EXPIRED`), expiry and reveal time — never the secret.
async fn credentials(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path(tenant_id): Path<Uuid>,
    Query(q): Query<PageQuery>,
) -> AccessResult<impl IntoResponse> {
    bridge(&caller)?;
    let limit = page_limit(q.limit)?;
    tenant_exists(&s, tenant_id).await?;
    page(
        &s,
        "SELECT c.id, json_build_object('id', c.id, 'publicId', c.public_id, 'lastFour', c.last_four, \
           'applicationId', c.application_id, 'environmentId', c.environment_id, \
           'serviceAccountId', c.service_account_id, 'status', c.status, \
           'scopes', ARRAY(SELECT cs.scope FROM platform_api_credential_scopes cs \
                           WHERE cs.credential_id = c.id ORDER BY cs.scope), \
           'notBefore', c.not_before, 'expiresAt', c.expires_at, 'revokedAt', c.revoked_at, \
           'supersededAt', c.superseded_at, 'suspendedAt', su.suspended_at, \
           'suspensionReason', su.reason_code, 'lastUsedAt', u.last_authenticated_at, \
           'revision', c.revision, 'version', c.revision + COALESCE(x.changes, 0), \
           'createdAt', c.created_at, \
           'delivery', CASE WHEN d.id IS NULL THEN NULL ELSE json_build_object( \
             'delivery', 'OWNER_DELIVERED', 'deliveryChannel', d.channel, 'deliveryRef', d.id, \
             'deliveryState', d.state, 'expiresAt', d.expires_at, \
             'revealedAt', d.revealed_at) END) AS v \
         FROM platform_api_credentials c \
         LEFT JOIN platform_credential_deliveries d ON d.credential_id = c.id \
         LEFT JOIN platform_credential_suspensions su ON su.credential_id = c.id \
         LEFT JOIN platform_credential_suspension_changes x ON x.credential_id = c.id \
         LEFT JOIN platform_credential_usage u ON u.credential_id = c.id \
         WHERE c.tenant_id = $1 AND ($2::uuid IS NULL OR c.id > $2) \
         ORDER BY c.id LIMIT $3",
        vec![tenant_id.into(), uuid_cursor(&q.cursor)?.into()],
        limit,
        id_cursor,
    )
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrincipalQuery {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    limit: Option<u64>,
}

const PRINCIPAL_KINDS: &[&str] = &[
    "NATIVE_CF_USER",
    "EXTERNAL_APPLICATION_PRINCIPAL",
    "ENTERPRISE_MEMBER",
    "SERVICE_ACCOUNT",
    "TELEPHONE_ENDPOINT",
    "TEMPORARY_EXTERNAL_PARTICIPANT",
    "BOT_OR_SERVICE",
];

/// An exact search term: a principal id or an external reference (never returned).
fn exact(q: &Option<String>) -> AccessResult<Option<String>> {
    match q.as_deref().map(str::trim) {
        None | Some("") => Ok(None),
        Some(t) if t.chars().count() > 256 => Err(invalid("q must be at most 256 characters")),
        Some(t) => Ok(Some(t.to_string())),
    }
}

const PRINCIPAL_JSON: &str = "json_build_object('id', id, 'tenantId', tenant_id, \
    'applicationId', application_id, 'environmentId', environment_id, 'kind', kind, \
    'linkedServiceAccountId', linked_service_account_id, 'status', status, \
    'tombstonedAt', tombstoned_at, 'revision', revision, 'createdAt', created_at, \
    'updatedAt', updated_at)";

/// `GET /operator/tenants/{tenantId}/principals?kind&q&cursor&limit` — a tenant's
/// principals (partner users: `kind=EXTERNAL_APPLICATION_PRINCIPAL`); `q` matches the
/// principal id or the external reference EXACTLY and is never echoed back.
async fn tenant_principals(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path(tenant_id): Path<Uuid>,
    Query(q): Query<PrincipalQuery>,
) -> AccessResult<impl IntoResponse> {
    bridge(&caller)?;
    let limit = page_limit(q.limit)?;
    if q.kind
        .as_deref()
        .is_some_and(|k| !PRINCIPAL_KINDS.contains(&k))
    {
        return Err(invalid("unknown principal kind"));
    }
    tenant_exists(&s, tenant_id).await?;
    page(
        &s,
        &format!(
            "SELECT id, {PRINCIPAL_JSON} AS v FROM platform_principals \
             WHERE tenant_id = $1 AND ($2::text IS NULL OR kind = $2) \
               AND ($3::text IS NULL OR id::text = $3 OR external_reference = $3) \
               AND ($4::uuid IS NULL OR id > $4) \
             ORDER BY id LIMIT $5"
        ),
        vec![
            tenant_id.into(),
            q.kind.clone().into(),
            exact(&q.q)?.into(),
            uuid_cursor(&q.cursor)?.into(),
        ],
        limit,
        id_cursor,
    )
    .await
}

/// `GET /operator/principals?q&cursor&limit` — identity records across tenants. `q` is
/// required (a principal id or an exact external reference, which may be an email
/// address); it only matches and is never echoed back.
async fn principals(
    State(s): State<AppState>,
    caller: InternalCaller,
    Query(q): Query<SearchQuery>,
) -> AccessResult<impl IntoResponse> {
    bridge(&caller)?;
    let limit = page_limit(q.limit)?;
    let term = exact(&q.q)?.ok_or_else(|| invalid("q is required"))?;
    page(
        &s,
        &format!(
            "SELECT id, {PRINCIPAL_JSON} AS v FROM platform_principals \
             WHERE (id::text = $1 OR external_reference = $1) AND ($2::uuid IS NULL OR id > $2) \
             ORDER BY id LIMIT $3"
        ),
        vec![term.into(), uuid_cursor(&q.cursor)?.into()],
        limit,
        id_cursor,
    )
    .await
}

/// `GET /operator/tenants/{tenantId}/origins` — allowed websites per environment (an
/// environment without origins answers an empty list).
async fn origins(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path(tenant_id): Path<Uuid>,
    Query(q): Query<PageQuery>,
) -> AccessResult<impl IntoResponse> {
    bridge(&caller)?;
    let limit = page_limit(q.limit)?;
    tenant_exists(&s, tenant_id).await?;
    page(
        &s,
        "SELECT e.id, json_build_object('environmentId', e.id, 'applicationId', e.application_id, \
           'environmentKind', e.kind, 'origins', COALESCE(o.origins, '{}'::text[]), \
           'revision', o.revision, 'updatedAt', o.updated_at) AS v \
         FROM platform_environments e \
         LEFT JOIN platform_environment_origins o ON o.environment_id = e.id \
         WHERE e.tenant_id = $1 AND ($2::uuid IS NULL OR e.id > $2) \
         ORDER BY e.id LIMIT $3",
        vec![tenant_id.into(), uuid_cursor(&q.cursor)?.into()],
        limit,
        id_cursor,
    )
    .await
}

/// `GET /operator/oauth-apps?q&cursor&limit` — OAuth clients (CF Meet Apps): name, scopes,
/// distribution and status; the number of redirect URIs, never the URIs or test accounts.
async fn oauth_apps(
    State(s): State<AppState>,
    caller: InternalCaller,
    Query(q): Query<SearchQuery>,
) -> AccessResult<impl IntoResponse> {
    bridge(&caller)?;
    let limit = page_limit(q.limit)?;
    page(
        &s,
        "SELECT application_id AS id, json_build_object('applicationId', application_id, \
           'tenantId', tenant_id, 'name', name, 'distribution', distribution, 'status', status, \
           'userScopes', user_scopes, 'botScopes', bot_scopes, \
           'redirectUriCount', cardinality(redirect_uris), 'revision', revision, \
           'createdAt', created_at, 'updatedAt', updated_at) AS v \
         FROM platform_oauth_clients \
         WHERE ($1::uuid IS NULL OR application_id > $1) \
           AND ($2::text IS NULL OR lower(name) LIKE $2 ESCAPE '\\') \
         ORDER BY application_id LIMIT $3",
        vec![uuid_cursor(&q.cursor)?.into(), like(&q.q)?.into()],
        limit,
        id_cursor,
    )
    .await
}

/// `GET /operator/oauth-apps/{appId}/grants?cursor&limit` — every grant of an app, any
/// status, in id order.
async fn oauth_grants(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path(app_id): Path<Uuid>,
    Query(q): Query<PageQuery>,
) -> AccessResult<impl IntoResponse> {
    bridge(&caller)?;
    let limit = page_limit(q.limit)?;
    let exists = s
        .platform
        .db()
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT 1 AS one FROM platform_oauth_clients WHERE application_id = $1",
            [app_id.into()],
        ))
        .await
        .map_err(db_err)?;
    if exists.is_none() {
        return Err(AccessError::Refused(ErrorCode::NotFound));
    }
    page(
        &s,
        "SELECT id, json_build_object('id', id, 'target', target, 'accountId', account_id, \
           'organizationId', organization_id, 'installedByAccountId', installed_by_account_id, \
           'environmentId', environment_id, 'principalId', principal_id, 'scopes', scopes, \
           'status', status, 'revokedAt', revoked_at, 'revokeReason', revoke_reason, \
           'createdAt', created_at, 'updatedAt', updated_at) AS v \
         FROM platform_oauth_grants WHERE app_id = $1 AND ($2::uuid IS NULL OR id > $2) \
         ORDER BY id LIMIT $3",
        vec![app_id.into(), uuid_cursor(&q.cursor)?.into()],
        limit,
        id_cursor,
    )
    .await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuditParams {
    #[serde(default)]
    since: Option<String>,
    #[serde(default)]
    until: Option<String>,
    #[serde(default)]
    tenant_id: Option<Uuid>,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    limit: Option<u64>,
}

type Window = (Option<DateTime<Utc>>, Option<DateTime<Utc>>);

/// Validates an audit window: RFC 3339 bounds, `until` after `since`, at most 31 days.
pub fn window(since: Option<&str>, until: Option<&str>) -> AccessResult<Window> {
    let parse = |v: Option<&str>, name: &str| -> AccessResult<Option<DateTime<Utc>>> {
        v.map(|t| {
            DateTime::parse_from_rfc3339(t)
                .map(|d| d.with_timezone(&Utc))
                .map_err(|_| invalid(&format!("{name} must be an RFC 3339 timestamp")))
        })
        .transpose()
    };
    let (s, u) = (parse(since, "since")?, parse(until, "until")?);
    if let (Some(s), Some(u)) = (s, u) {
        if u <= s || u - s > Duration::days(MAX_WINDOW_DAYS) {
            return Err(invalid("until must be after since, at most 31 days later"));
        }
    }
    Ok((s, u))
}

/// Opaque audit cursor: `<occurredAt micros>_<id>` (same form as `GET /admin/audit`).
pub fn parse_audit_cursor(value: &str) -> Option<(DateTime<Utc>, Uuid)> {
    let (micros, id) = value.split_once('_')?;
    Some((
        DateTime::<Utc>::from_timestamp_micros(micros.parse().ok()?)?,
        Uuid::parse_str(id).ok()?,
    ))
}

/// `GET /operator/audit?since&until&tenantId&cursor&limit` — the operator audit trail,
/// newest first, without the row `detail`.
async fn audit(
    State(s): State<AppState>,
    caller: InternalCaller,
    Query(q): Query<AuditParams>,
) -> AccessResult<impl IntoResponse> {
    bridge(&caller)?;
    let limit = page_limit(q.limit)?;
    let (since, until) = window(q.since.as_deref(), q.until.as_deref())?;
    let (before_at, before_id) = match q.cursor.as_deref() {
        None => (None, None),
        Some(c) => {
            let (a, i) = parse_audit_cursor(c).ok_or_else(|| invalid("malformed cursor"))?;
            (Some(a), Some(i))
        }
    };
    page(
        &s,
        "SELECT id, occurred_at, json_build_object('id', id, 'actor', actor, \
           'correlationId', correlation_id, 'action', action, 'targetKind', target_kind, \
           'targetId', target_id, 'tenantId', tenant_id, 'applicationId', application_id, \
           'environmentId', environment_id, 'occurredAt', occurred_at) AS v \
         FROM platform_access_audit \
         WHERE ($1::uuid IS NULL OR tenant_id = $1) \
           AND ($2::timestamptz IS NULL OR occurred_at >= $2) \
           AND ($3::timestamptz IS NULL OR occurred_at < $3) \
           AND ($4::timestamptz IS NULL OR (occurred_at, id) < ($4, $5::uuid)) \
         ORDER BY occurred_at DESC, id DESC LIMIT $6",
        vec![
            q.tenant_id.into(),
            since.into(),
            until.into(),
            before_at.into(),
            before_id.into(),
        ],
        limit,
        |r| {
            let at = r.try_get::<DateTime<Utc>>("", "occurred_at")?;
            let id = r.try_get::<Uuid>("", "id")?;
            Ok(format!("{}_{}", at.timestamp_micros(), id.simple()))
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_windows_and_cursors_are_validated() {
        assert_eq!(page_limit(None).unwrap(), PAGE_DEFAULT);
        assert!(page_limit(Some(0)).is_err() && page_limit(Some(101)).is_err());
        assert!(window(Some("2026-11-01T00:00:00Z"), Some("2026-11-20T00:00:00Z")).is_ok());
        assert!(window(Some("2026-10-01T00:00:00Z"), Some("2026-11-20T00:00:00Z")).is_err());
        assert!(window(Some("2026-11-20T00:00:00Z"), Some("2026-11-01T00:00:00Z")).is_err());
        assert!(window(Some("yesterday"), None).is_err());
        assert!(parse_audit_cursor("x").is_none());
        let id = Uuid::new_v4();
        assert_eq!(
            parse_audit_cursor(&format!("1790000000123456_{}", id.simple())).map(|c| c.1),
            Some(id)
        );
        assert_eq!(
            like(&Some("50%_a\\".into())).unwrap().as_deref(),
            Some("%50\\%\\_a\\\\%")
        );
        assert!(exact(&Some(" ".into())).unwrap().is_none());
    }
}
