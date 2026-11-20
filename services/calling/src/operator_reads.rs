//! The admin bridge's operator reads (ADR 0068, design §6.4): `/internal/v1/operator/*`.
//!
//! Only `cfmeet.admin-bridge` can hold `calling.administer`, and every request must name
//! the HQ staff member it reads for (`X-CF-Operator`, parsed once by the internal-auth
//! layer; refused otherwise with `INTERNAL_PERMISSION_DENIED`). Every read is scoped to ONE
//! Platform Access tenant (`tenantId`, required): no read crosses tenants (design §16),
//! except platform-wide retention holds and incidents, which are marked `PLATFORM`.
//!
//! Minimized by construction: ids, codes, states, counts, durations, sizes and times.
//! Never a telephone number, SIP URI, contact reference, display name, media URL, object
//! key, key material, free text or restricted incident evidence. Money is in minor units
//! with its ISO 4217 currency, per row, never summed.

use axum::{
    extract::{rejection::QueryRejection, Path, Query, State},
    http::{header, HeaderValue},
    response::{IntoResponse, Response},
    routing::get,
    Extension, Json, Router,
};
use cf_service_auth::{InternalPermission, OperatorAttribution};
use chrono::{DateTime, Duration, Utc};
use sea_orm::{DbBackend, FromQueryResult, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    enterprise::http::QueryParams, error::CallingError, internal_auth::middleware::InternalCaller,
    state::AppState,
};

const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 100;
const MAX_WINDOW_DAYS: i64 = 31;
/// Rows of a per-tenant configuration read (limits, routing, enterprise, spend).
const MAX_CONFIG_ROWS: usize = 100;
/// Participants listed on one call.
const MAX_PARTICIPANTS: i64 = 500;
const TERMINAL_STATES: &str = "'ENDED','DECLINED','BUSY','NO_ANSWER','FAILED','CANCELLED'";

/// The HQ staff member a bridge request reads for (set by the internal-auth layer only
/// for `cfmeet.admin-bridge` with valid attribution).
#[derive(Debug, Clone)]
pub struct BridgeOperator(pub OperatorAttribution);

pub fn internal_routes() -> Router<AppState> {
    Router::new()
        .route("/operator/calls", get(calls))
        .route("/operator/calls/:call_id", get(call))
        .route("/operator/live-calls", get(live_calls))
        .route("/operator/abuse-reports", get(abuse_reports))
        .route("/operator/blocks", get(blocks))
        .route("/operator/limits", get(limits))
        .route("/operator/routing-policies", get(routing_policies))
        .route("/operator/enterprise-policies", get(enterprise_policies))
        .route("/operator/spend-policies", get(spend_policies))
        .route("/operator/recordings", get(recordings))
        .route("/operator/voicemail", get(voicemail))
        .route("/operator/retention-holds", get(retention_holds))
        .route("/operator/incidents", get(incidents))
        .route("/operator/webhook-keys", get(webhook_keys))
        .route("/operator/audit", get(audit))
}

type OperatorExt = Option<Extension<BridgeOperator>>;

fn gate(caller: &InternalCaller, operator: &OperatorExt) -> Result<(), CallingError> {
    caller.require_permission(InternalPermission::CallingAdminister)?;
    if operator.is_none() {
        tracing::warn!(
            caller = %caller.context().authenticated_service(),
            "operator read without a named HQ operator"
        );
        return Err(CallingError::InternalPermissionDenied);
    }
    Ok(())
}

fn no_store(value: Value) -> Response {
    let mut r = Json(value).into_response();
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

fn stmt(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

#[derive(Debug, FromQueryResult)]
struct Row {
    doc: Value,
    k_at: Option<DateTime<Utc>>,
    k_id: Option<Uuid>,
}

async fn rows(
    state: &AppState,
    sql: &str,
    values: Vec<sea_orm::Value>,
) -> Result<Vec<Row>, CallingError> {
    Row::find_by_statement(stmt(sql, values))
        .all(&state.db)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "operator read");
            CallingError::Internal
        })
}

/// Common query of the operator reads.
struct Params {
    tenant: Uuid,
    limit: i64,
    after: Option<(DateTime<Utc>, Uuid)>,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
}

fn time(v: Option<&str>, name: &'static str) -> Result<Option<DateTime<Utc>>, CallingError> {
    v.map(|s| {
        DateTime::parse_from_rfc3339(s)
            .map(|t| t.with_timezone(&Utc))
            .map_err(|_| CallingError::Validation(name))
    })
    .transpose()
}

fn encode_cursor(at: DateTime<Utc>, id: Uuid) -> String {
    format!("{}_{}", at.timestamp_micros(), id.simple())
}

fn decode_cursor(cursor: &str) -> Option<(DateTime<Utc>, Uuid)> {
    let (micros, id) = cursor.split_once('_')?;
    Some((
        DateTime::from_timestamp_micros(micros.parse().ok()?)?,
        Uuid::parse_str(id).ok()?,
    ))
}

fn params(
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
    paged: bool,
    windowed: bool,
) -> Result<Params, CallingError> {
    let mut allowed = vec!["tenantId"];
    if paged {
        allowed.extend(["limit", "cursor"]);
    }
    if windowed {
        allowed.extend(["since", "until"]);
    }
    let q = QueryParams::parse(query, &allowed)?;
    let tenant = q
        .get("tenantId")
        .and_then(|t| Uuid::parse_str(t).ok())
        .ok_or(CallingError::Validation("tenantId must be a UUID."))?;
    let limit = match q.get("limit") {
        None => DEFAULT_LIMIT,
        Some(v) => v
            .parse::<i64>()
            .ok()
            .filter(|l| (1..=MAX_LIMIT).contains(l))
            .ok_or(CallingError::Validation(
                "limit must be an integer between 1 and 100.",
            ))?,
    };
    let after = q
        .get("cursor")
        .map(|c| decode_cursor(c).ok_or(CallingError::Validation("The cursor is invalid.")))
        .transpose()?;
    let since = time(q.get("since"), "since must be an RFC 3339 timestamp.")?;
    let until = time(q.get("until"), "until must be an RFC 3339 timestamp.")?;
    if let (Some(s), Some(u)) = (since, until) {
        if u <= s || u - s > Duration::days(MAX_WINDOW_DAYS) {
            return Err(CallingError::Validation(
                "until must be after since, at most 31 days later.",
            ));
        }
    }
    Ok(Params {
        tenant,
        limit,
        after,
        since,
        until,
    })
}

/// A keyset page over `base` (`SELECT … AS doc, <at> AS k_at, <id> AS k_id FROM … WHERE
/// <tenant = $1> …`), newest first unless `ascending`.
async fn page(
    state: &AppState,
    base: &str,
    mut values: Vec<sea_orm::Value>,
    at: &str,
    id: &str,
    p: &Params,
    ascending: bool,
) -> Result<Response, CallingError> {
    let mut sql = base.to_string();
    if let Some(s) = p.since {
        values.push(s.into());
        sql.push_str(&format!(" AND {at} >= ${}", values.len()));
    }
    if let Some(u) = p.until {
        values.push(u.into());
        sql.push_str(&format!(" AND {at} < ${}", values.len()));
    }
    let (cmp, dir) = if ascending {
        (">", "ASC")
    } else {
        ("<", "DESC")
    };
    if let Some((k_at, k_id)) = p.after {
        values.push(k_at.into());
        values.push(k_id.into());
        sql.push_str(&format!(
            " AND ({at}, {id}) {cmp} (${}, ${})",
            values.len() - 1,
            values.len()
        ));
    }
    values.push((p.limit + 1).into());
    sql.push_str(&format!(
        " ORDER BY {at} {dir}, {id} {dir} LIMIT ${}",
        values.len()
    ));
    let mut found = rows(state, &sql, values).await?;
    let next = if found.len() as i64 > p.limit {
        found.truncate(p.limit as usize);
        found
            .last()
            .and_then(|r| Some(encode_cursor(r.k_at?, r.k_id?)))
    } else {
        None
    };
    let items: Vec<Value> = found.into_iter().map(|r| r.doc).collect();
    Ok(no_store(json!({ "items": items, "nextCursor": next })))
}

/// A bounded per-tenant configuration list (`truncated` when more than 100 rows exist).
async fn config_list(
    state: &AppState,
    sql: &str,
    tenant: Uuid,
    key: &str,
) -> Result<Response, CallingError> {
    let mut found = rows(
        state,
        &format!("{sql} LIMIT {}", MAX_CONFIG_ROWS + 1),
        vec![tenant.into()],
    )
    .await?;
    let truncated = found.len() > MAX_CONFIG_ROWS;
    found.truncate(MAX_CONFIG_ROWS);
    let items: Vec<Value> = found.into_iter().map(|r| r.doc).collect();
    Ok(no_store(json!({ key: items, "truncated": truncated })))
}

// ---- Calls ---------------------------------------------------------------------------

const CALL_DOC: &str = "json_build_object(\
    'callId', c.id, 'applicationId', c.application_id, 'environmentId', c.environment_id, \
    'callKind', c.call_kind, 'state', c.current_state, 'origin', c.origin, \
    'participantCount', (SELECT count(*) FROM call_participants p WHERE p.call_id = c.id), \
    'terminalReason', c.terminal_reason, 'failureCategory', c.failure_category, \
    'requestedAt', c.requested_at, 'answeredAt', c.answered_at, 'connectedAt', c.connected_at, \
    'endedAt', c.ended_at, 'createdAt', c.created_at, 'version', c.version)";

/// `GET /operator/calls?tenantId&since&until&limit&cursor`: the tenant's calls, newest
/// first. No telephone number, SIP URI or display name.
async fn calls(
    State(state): State<AppState>,
    caller: InternalCaller,
    operator: OperatorExt,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    gate(&caller, &operator)?;
    let p = params(query, true, true)?;
    let base = format!(
        "SELECT {CALL_DOC} AS doc, c.created_at AS k_at, c.id AS k_id FROM calls c WHERE c.tenant_id = $1"
    );
    page(
        &state,
        &base,
        vec![p.tenant.into()],
        "c.created_at",
        "c.id",
        &p,
        false,
    )
    .await
}

/// `GET /operator/calls/{callId}?tenantId`: one call with its participants (principal ids,
/// kinds, roles and times only). Another tenant's call is 404.
async fn call(
    State(state): State<AppState>,
    caller: InternalCaller,
    operator: OperatorExt,
    Path(call_id): Path<String>,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    gate(&caller, &operator)?;
    let p = params(query, false, false)?;
    let call_id = Uuid::parse_str(&call_id).map_err(|_| CallingError::NotFound)?;
    let sql = format!(
        "SELECT ({CALL_DOC}::jsonb || jsonb_build_object('participants', COALESCE((\
           SELECT jsonb_agg(x.p ORDER BY x.at) FROM (\
             SELECT jsonb_build_object('principalId', p.principal_id, 'principalKind', p.principal_kind, \
               'role', p.role, 'isHost', p.is_host, 'joinedAt', p.created_at, 'leftAt', p.ended_at) AS p, \
               p.created_at AS at \
             FROM call_participants p WHERE p.call_id = c.id ORDER BY p.created_at LIMIT {MAX_PARTICIPANTS}) x), \
           '[]'::jsonb)))::json AS doc, c.created_at AS k_at, c.id AS k_id \
         FROM calls c WHERE c.tenant_id = $1 AND c.id = $2"
    );
    let found = rows(&state, &sql, vec![p.tenant.into(), call_id.into()]).await?;
    let row = found.into_iter().next().ok_or(CallingError::NotFound)?;
    Ok(no_store(row.doc))
}

/// `GET /operator/live-calls?tenantId&limit&cursor`: the tenant's calls in a non-terminal
/// state, newest first.
async fn live_calls(
    State(state): State<AppState>,
    caller: InternalCaller,
    operator: OperatorExt,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    gate(&caller, &operator)?;
    let p = params(query, true, false)?;
    let base = format!(
        "SELECT {CALL_DOC} AS doc, c.created_at AS k_at, c.id AS k_id FROM calls c \
         WHERE c.tenant_id = $1 AND c.current_state NOT IN ({TERMINAL_STATES})"
    );
    page(
        &state,
        &base,
        vec![p.tenant.into()],
        "c.created_at",
        "c.id",
        &p,
        false,
    )
    .await
}

// ---- Safety --------------------------------------------------------------------------

/// `GET /operator/abuse-reports?tenantId&limit&cursor`: the tenant's OPEN reports, oldest
/// first (the review queue). Bounded codes and opaque ids only, never free text.
async fn abuse_reports(
    State(state): State<AppState>,
    caller: InternalCaller,
    operator: OperatorExt,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    gate(&caller, &operator)?;
    let p = params(query, true, false)?;
    let base = "SELECT json_build_object(\
        'abuseReportId', r.id, 'applicationId', r.application_id, 'environmentId', r.environment_id, \
        'callId', r.call_id, 'reportingPrincipalId', r.reporting_principal_id, \
        'reportedPrincipalId', r.reported_principal_id, 'category', r.category, \
        'commentCode', r.comment_code, 'status', r.status, 'createdAt', r.created_at, \
        'reviewedAt', r.reviewed_at, 'version', r.version) AS doc, r.created_at AS k_at, r.id AS k_id \
        FROM calling_abuse_reports r WHERE r.tenant_id = $1 AND r.status = 'OPEN'";
    page(
        &state,
        base,
        vec![p.tenant.into()],
        "r.created_at",
        "r.id",
        &p,
        true,
    )
    .await
}

/// `GET /operator/blocks?tenantId&limit&cursor`: the tenant's block rules, newest first. A
/// blocked contact is reported only as present (`hasContactReference`), never named.
async fn blocks(
    State(state): State<AppState>,
    caller: InternalCaller,
    operator: OperatorExt,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    gate(&caller, &operator)?;
    let p = params(query, true, false)?;
    let base = "SELECT json_build_object(\
        'blockRuleId', b.id, 'applicationId', b.application_id, 'environmentId', b.environment_id, \
        'blockerKind', b.blocker_kind, 'blockerPrincipalId', b.blocker_principal_id, \
        'blockedKind', b.blocked_kind, 'blockedPrincipalId', b.blocked_principal_id, \
        'hasContactReference', b.blocked_contact_reference IS NOT NULL, \
        'reasonCode', b.reason_code, 'status', b.status, 'createdAt', b.created_at, \
        'releasedAt', b.released_at, 'version', b.version) AS doc, b.created_at AS k_at, b.id AS k_id \
        FROM calling_block_rules b WHERE b.tenant_id = $1";
    page(
        &state,
        base,
        vec![p.tenant.into()],
        "b.created_at",
        "b.id",
        &p,
        false,
    )
    .await
}

// ---- Configuration (per tenant, bounded) ----------------------------------------------

/// `GET /operator/limits?tenantId`: the tenant's limit policy and its applications'
/// policies (at most 100, `truncated` beyond). Spend limits carry the tenant's billing
/// currency. Unknown tenant: 404.
async fn limits(
    State(state): State<AppState>,
    caller: InternalCaller,
    operator: OperatorExt,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    gate(&caller, &operator)?;
    let p = params(query, false, false)?;
    let sql = format!(
        "SELECT json_build_object(\
           'tenantId', t.id, 'currency', t.billing_currency, \
           'tenant', (SELECT json_build_object(\
               'requestsPerMinute', l.requests_per_minute, \
               'maxConcurrentCommunications', l.max_concurrent_communications, \
               'telecomSpendDailyLimitMinor', l.telecom_spend_daily_limit_minor, \
               'telecomSpendMonthlyLimitMinor', l.telecom_spend_monthly_limit_minor, \
               'updatedAt', l.updated_at) FROM platform_tenant_limits l WHERE l.tenant_id = t.id), \
           'applications', COALESCE((SELECT json_agg(s.x) FROM (\
               SELECT json_build_object('applicationId', a.application_id, \
                 'requestsPerMinute', a.requests_per_minute, \
                 'maxConcurrentCommunications', a.max_concurrent_communications, \
                 'telecomSpendDailyLimitMinor', a.telecom_spend_daily_limit_minor, \
                 'telecomSpendMonthlyLimitMinor', a.telecom_spend_monthly_limit_minor, \
                 'updatedAt', a.updated_at) AS x \
               FROM platform_application_limits a WHERE a.tenant_id = t.id \
               ORDER BY a.application_id LIMIT {}) s), '[]'::json)) AS doc, \
           NULL::timestamptz AS k_at, t.id AS k_id \
         FROM platform_tenants t WHERE t.id = $1",
        MAX_CONFIG_ROWS + 1
    );
    let found = rows(&state, &sql, vec![p.tenant.into()]).await?;
    let mut doc = found.into_iter().next().ok_or(CallingError::NotFound)?.doc;
    let truncated = doc["applications"]
        .as_array()
        .is_some_and(|a| a.len() > MAX_CONFIG_ROWS);
    if let Some(a) = doc["applications"].as_array_mut() {
        a.truncate(MAX_CONFIG_ROWS);
    }
    doc["truncated"] = json!(truncated);
    Ok(no_store(doc))
}

/// `GET /operator/routing-policies?tenantId`: each environment's typed routing policy
/// (summary flags, route classes, failover bound, compliance countries). Destination and
/// duplicate-risk sub-policies are not relayed.
async fn routing_policies(
    State(state): State<AppState>,
    caller: InternalCaller,
    operator: OperatorExt,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    gate(&caller, &operator)?;
    let p = params(query, false, false)?;
    let sql = "SELECT json_build_object(\
        'environmentId', r.environment_id, 'applicationId', r.application_id, \
        'revision', r.revision, 'updatedAt', r.updated_at, \
        'allowedRouteClasses', COALESCE(r.policy->'allowedRouteClasses', '[]'::jsonb), \
        'appFirst', r.policy->'appFirst', \
        'publicCallingEnabled', r.policy->'publicCallingEnabled', \
        'callbackRelayEnabled', r.policy->'callbackRelayEnabled', \
        'requireVerifiedContacts', r.policy->'requireVerifiedContacts', \
        'maxFailoverAttempts', r.policy->'maxFailoverAttempts', \
        'complianceRequiredCountries', COALESCE(r.policy->'complianceRequiredCountries', '[]'::jsonb)) AS doc, \
        r.updated_at AS k_at, r.environment_id AS k_id \
        FROM call_routing_policies r WHERE r.tenant_id = $1 ORDER BY r.environment_id";
    config_list(&state, sql, p.tenant, "environments").await
}

/// `GET /operator/enterprise-policies?tenantId`: enterprise calling policies (feature
/// switches, bounds and the operator-granted administrative capabilities).
async fn enterprise_policies(
    State(state): State<AppState>,
    caller: InternalCaller,
    operator: OperatorExt,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    gate(&caller, &operator)?;
    let p = params(query, false, false)?;
    let sql = "SELECT json_build_object(\
        'policyId', e.id, 'environmentId', e.environment_id, 'applicationId', e.application_id, \
        'name', e.name, 'isDefault', e.is_default, 'status', e.status, \
        'internalCallingEnabled', e.internal_calling_enabled, \
        'externalPublicCallingAllowed', e.external_public_calling_allowed, \
        'publicDestinationCountries', COALESCE(to_json(e.public_destination_countries), '[]'::json), \
        'forwardingAllowed', e.forwarding_allowed, 'publicForwardingAllowed', e.public_forwarding_allowed, \
        'voicemailAllowed', e.voicemail_allowed, 'transferAllowed', e.transfer_allowed, \
        'adminCapabilities', COALESCE(to_json(e.admin_capabilities), '[]'::json), \
        'version', e.version, 'updatedAt', e.updated_at, 'disabledAt', e.disabled_at) AS doc, \
        e.updated_at AS k_at, e.id AS k_id \
        FROM enterprise_call_policies e WHERE e.tenant_id = $1 ORDER BY e.environment_id, e.id";
    config_list(&state, sql, p.tenant, "policies").await
}

/// `GET /operator/spend-policies?tenantId`: telecom spend / velocity policies per
/// environment, each amount in minor units of its own currency.
async fn spend_policies(
    State(state): State<AppState>,
    caller: InternalCaller,
    operator: OperatorExt,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    gate(&caller, &operator)?;
    let p = params(query, false, false)?;
    let sql = "SELECT json_build_object(\
        'environmentId', s.environment_id, 'applicationId', s.application_id, \
        'currency', s.currency, 'enabled', s.enabled, \
        'legAllowanceMinor', s.leg_allowance_minor, 'maxLegSeconds', s.max_leg_seconds, \
        'dailyLimitMinor', s.daily_limit_minor, 'monthlyLimitMinor', s.monthly_limit_minor, \
        'maxConcurrentLegs', s.max_concurrent_legs, \
        'maxOriginationsPerMinute', s.max_originations_per_minute, \
        'revision', s.revision, 'updatedAt', s.updated_at) AS doc, \
        s.updated_at AS k_at, s.environment_id AS k_id \
        FROM call_spend_policies s WHERE s.tenant_id = $1 ORDER BY s.environment_id";
    config_list(&state, sql, p.tenant, "environments").await
}

// ---- Media metadata ------------------------------------------------------------------

/// `GET /operator/recordings?tenantId&since&until&limit&cursor`: recording metadata only,
/// newest first. Never an object key or media URL.
async fn recordings(
    State(state): State<AppState>,
    caller: InternalCaller,
    operator: OperatorExt,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    gate(&caller, &operator)?;
    let p = params(query, true, true)?;
    let base = "SELECT json_build_object(\
        'recordingId', r.id, 'callId', r.call_id, 'environmentId', r.environment_id, \
        'state', r.state, 'captureSource', r.capture_source, 'durationMs', r.duration_ms, \
        'sizeBytes', r.size_bytes, 'retentionUntil', r.retention_until, \
        'createdAt', r.created_at, 'deletedAt', r.deleted_at) AS doc, \
        r.created_at AS k_at, r.id AS k_id FROM call_recordings r WHERE r.tenant_id = $1";
    page(
        &state,
        base,
        vec![p.tenant.into()],
        "r.created_at",
        "r.id",
        &p,
        false,
    )
    .await
}

/// `GET /operator/voicemail?tenantId&since&until&limit&cursor`: voicemail metadata only,
/// newest first. Never an object key, media URL or caller identity.
async fn voicemail(
    State(state): State<AppState>,
    caller: InternalCaller,
    operator: OperatorExt,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    gate(&caller, &operator)?;
    let p = params(query, true, true)?;
    let base = "SELECT json_build_object(\
        'messageId', v.id, 'callId', v.call_id, 'mailboxId', v.mailbox_id, \
        'environmentId', v.environment_id, 'state', v.state, 'readState', v.read_state, \
        'durationMs', v.duration_ms, 'sizeBytes', v.size_bytes, \
        'retentionUntil', v.retention_until, 'receivedAt', v.received_at, \
        'deletedAt', v.deleted_at) AS doc, \
        v.created_at AS k_at, v.id AS k_id FROM voicemail_messages v WHERE v.tenant_id = $1";
    page(
        &state,
        base,
        vec![p.tenant.into()],
        "v.created_at",
        "v.id",
        &p,
        false,
    )
    .await
}

// ---- Operations ----------------------------------------------------------------------

/// `GET /operator/retention-holds?tenantId&limit&cursor`: the tenant's holds and the
/// platform-wide ones (`scope` PLATFORM), newest first. The placing operator is not
/// relayed.
async fn retention_holds(
    State(state): State<AppState>,
    caller: InternalCaller,
    operator: OperatorExt,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    gate(&caller, &operator)?;
    let p = params(query, true, false)?;
    let base = "SELECT json_build_object(\
        'holdId', h.id, 'scope', CASE WHEN h.tenant_id IS NULL THEN 'PLATFORM' ELSE 'TENANT' END, \
        'dataClass', h.data_class, 'resourceId', h.resource_id, \
        'environmentId', h.environment_id, 'reasonCode', h.reason_code, \
        'active', h.released_at IS NULL, 'createdAt', h.created_at, \
        'releasedAt', h.released_at) AS doc, h.created_at AS k_at, h.id AS k_id \
        FROM calling_retention_holds h WHERE (h.tenant_id = $1 OR h.tenant_id IS NULL)";
    page(
        &state,
        base,
        vec![p.tenant.into()],
        "h.created_at",
        "h.id",
        &p,
        false,
    )
    .await
}

/// `GET /operator/incidents?tenantId&since&until&limit&cursor`: the tenant's incidents and
/// the platform-wide ones (`scope` PLATFORM), newest first by first sighting. Restricted
/// evidence and dedupe keys are never relayed.
async fn incidents(
    State(state): State<AppState>,
    caller: InternalCaller,
    operator: OperatorExt,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    gate(&caller, &operator)?;
    let p = params(query, true, true)?;
    let base = "SELECT json_build_object(\
        'incidentId', i.id, 'scope', CASE WHEN i.tenant_id IS NULL THEN 'PLATFORM' ELSE 'TENANT' END, \
        'category', i.category, 'severity', i.severity, 'status', i.status, \
        'callId', i.call_id, 'resourceType', i.resource_type, 'resourceId', i.resource_id, \
        'occurrences', i.occurrences, 'firstSeenAt', i.first_seen_at, \
        'lastSeenAt', i.last_seen_at, 'resolvedAt', i.resolved_at) AS doc, \
        i.first_seen_at AS k_at, i.id AS k_id \
        FROM calling_incidents i WHERE (i.tenant_id = $1 OR i.tenant_id IS NULL)";
    page(
        &state,
        base,
        vec![p.tenant.into()],
        "i.first_seen_at",
        "i.id",
        &p,
        false,
    )
    .await
}

/// `GET /operator/webhook-keys?tenantId`: the tenant's webhook subscriptions per
/// signing-secret sealing key id (counts only). Never a secret or sealed value.
async fn webhook_keys(
    State(state): State<AppState>,
    caller: InternalCaller,
    operator: OperatorExt,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    gate(&caller, &operator)?;
    let p = params(query, true, false)?;
    let sql = "SELECT json_build_object(\
        'keyId', s.secret_key_id, 'subscriptions', count(*), \
        'activeSubscriptions', count(*) FILTER (WHERE s.deleted_at IS NULL AND s.status = 'ACTIVE'), \
        'lastUpdatedAt', max(s.updated_at)) AS doc, NULL::timestamptz AS k_at, NULL::uuid AS k_id \
        FROM webhook_subscriptions s WHERE s.tenant_id = $1 \
        GROUP BY s.secret_key_id ORDER BY s.secret_key_id NULLS FIRST LIMIT 100";
    let found = rows(&state, sql, vec![p.tenant.into()]).await?;
    let items: Vec<Value> = found.into_iter().map(|r| r.doc).collect();
    Ok(no_store(json!({ "items": items, "nextCursor": null })))
}

/// `GET /operator/audit?tenantId&since&until&limit&cursor`: the operator audit of the
/// tenant, newest first. The `detail` document is never relayed.
async fn audit(
    State(state): State<AppState>,
    caller: InternalCaller,
    operator: OperatorExt,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    gate(&caller, &operator)?;
    let p = params(query, true, true)?;
    let base = "SELECT json_build_object(\
        'eventId', a.id, 'actor', a.actor, 'action', a.action, 'targetKind', a.target_kind, \
        'targetId', a.target_id, 'applicationId', a.application_id, \
        'environmentId', a.environment_id, 'occurredAt', a.occurred_at) AS doc, \
        a.occurred_at AS k_at, a.id AS k_id FROM platform_operator_audit a WHERE a.tenant_id = $1";
    page(
        &state,
        base,
        vec![p.tenant.into()],
        "a.occurred_at",
        "a.id",
        &p,
        false,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_round_trip_and_reject_garbage() {
        let at = DateTime::from_timestamp_micros(1_795_000_000_123_456).unwrap();
        let id = Uuid::new_v4();
        assert_eq!(decode_cursor(&encode_cursor(at, id)), Some((at, id)));
        for bad in [
            "",
            "x_y",
            "123",
            "123_notauuid",
            "abc_0b7e7f0e2b9c4c8e9d7a1f2e3d4c5b6a",
        ] {
            assert!(decode_cursor(bad).is_none(), "{bad}");
        }
    }

    // Bounds that the bridge's closed schemas rely on.
    const _: () = assert!(MAX_CONFIG_ROWS == 100 && MAX_PARTICIPANTS <= 500 && MAX_LIMIT == 100);

    #[test]
    fn terminal_states_are_the_call_state_machine_ones() {
        for s in [
            "ENDED",
            "DECLINED",
            "BUSY",
            "NO_ANSWER",
            "FAILED",
            "CANCELLED",
        ] {
            assert!(TERMINAL_STATES.contains(&format!("'{s}'")), "{s}");
        }
    }
}
