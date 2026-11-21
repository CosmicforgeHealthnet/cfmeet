//! Admin bridge reads added with the A4–A7 commands (ADR 0068, design §6.1–6.2): the
//! duplicate-account report and the test accounts (`account.administer`); conference
//! health, recordings, transcripts, webinars and their registrations, the chat migration,
//! email deliveries and the CRITICAL lane (`meeting.administer`); and the operations
//! record the bridge settles a command from.
//!
//! Minimal by construction: ids, codes, states, counts and times. Never a registrant's
//! address or name, a storage key, a provider reference, a reason text or content. The
//! test-account list shows each configured address masked, never its fixed code.

use axum::{
    extract::{OriginalUri, Path, Query, State},
    http::{request::Parts, StatusCode},
    response::Response,
};
use cf_service_auth::{InternalPermission, OperatorAttribution, RequestId};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    authenticate, error, limit_of, mask_email, not_found, ok, opt_rfc, page, path_uuid, rfc, rows,
    time_cursor, unavailable, uuid_cursor, validation, OperatorState, PageQuery,
};
use crate::{
    error::ErrorCode,
    services::{account_security as security, admin_operations as ledger},
};

pub const DUPLICATES: &str = "/internal/v1/operator/accounts/duplicates";
pub const TEST_ACCOUNTS: &str = "/internal/v1/operator/accounts/test-accounts";
pub const CONFERENCE_HEALTH: &str =
    "/internal/v1/operator/meetings/conferences/:conference_id/health";
pub const RECORDINGS: &str = "/internal/v1/operator/meetings/conferences/:conference_id/recordings";
pub const TRANSCRIPTS: &str =
    "/internal/v1/operator/meetings/conferences/:conference_id/transcripts";
pub const WEBINARS: &str = "/internal/v1/operator/meetings/webinars";
pub const WEBINAR_REGISTRATIONS: &str =
    "/internal/v1/operator/meetings/webinars/:space_id/registrations";
pub const CHAT_MIGRATION: &str = "/internal/v1/operator/meetings/chat-migration";
pub const EMAIL_DELIVERY: &str = "/internal/v1/operator/meetings/email-deliveries/:delivery_id";
pub const CRITICAL_LANE: &str = "/internal/v1/operator/meetings/critical-lane";
pub const OPERATION: &str = "/internal/v1/operator/operations/:idempotency_key";
pub const USER_EXPORTS: &str = "/internal/v1/operator/accounts/users/:user_id/exports";
pub const EXPORT: &str = "/internal/v1/operator/accounts/exports/:export_id";

macro_rules! auth {
    ($s:expr, $req:expr, $perm:expr) => {
        match authenticate(&$s, &$req, $perm).await {
            Ok(ctx) => ctx.request_id().as_str().to_string(),
            Err(r) => return *r,
        }
    };
}

macro_rules! try_r {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(r) => return *r,
        }
    };
}

async fn exists(s: &OperatorState, sql: &str, id: Uuid) -> Result<bool, sea_orm::DbErr> {
    Ok(s.app
        .db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            vec![id.into()],
        ))
        .await?
        .is_some())
}

// ---------------------------------------------------------------------------
// Accounts
// ---------------------------------------------------------------------------

/// `GET /internal/v1/operator/accounts/duplicates`: case-only duplicate accounts (account
/// ids grouped, at most 100 groups per field) and whether each rule is enforced.
pub(super) async fn duplicates(State(s): State<OperatorState>, req: Parts) -> Response {
    let rid = auth!(s, req, InternalPermission::AccountAdminister);
    match security::identity_report(&s.app.db).await {
        Ok(r) => ok(serde_json::to_value(r).unwrap_or(Value::Null)),
        Err(_) => unavailable(&rid),
    }
}

#[derive(FromQueryResult)]
struct TestAccountRow {
    email_lower: String,
    id: Uuid,
    status: String,
    role: String,
}

/// `GET /internal/v1/operator/accounts/test-accounts?limit&cursor`: the fixed-code review
/// accounts configured in this deployment (`AUTH_TEST_ACCOUNTS`), in address order,
/// masked; the account they belong to (if registered) and its state. Never the code.
/// `cursor` is the position of the next entry.
pub(super) async fn test_accounts(
    State(s): State<OperatorState>,
    Query(q): Query<PageQuery>,
    req: Parts,
) -> Response {
    let rid = auth!(s, req, InternalPermission::AccountAdminister);
    let limit = try_r!(limit_of(&q, &rid)) as usize;
    let start = match q.cursor.as_deref() {
        None => 0usize,
        Some(c) => match c.parse::<usize>() {
            Ok(n) if n <= 100_000 => n,
            _ => return validation("cursor is invalid", &rid),
        },
    };
    let all = s.app.auth_policy.test_accounts.addresses();
    let slice: Vec<&str> = all.iter().skip(start).take(limit).copied().collect();
    let lowered: Vec<String> = slice.iter().map(|a| a.to_lowercase()).collect();
    let found: Vec<TestAccountRow> = if lowered.is_empty() {
        vec![]
    } else {
        match rows(
            &s,
            "SELECT lower(email) AS email_lower, id, status::text AS status, role::text AS role \
               FROM users WHERE lower(email) = ANY(string_to_array($1, chr(31)))",
            vec![lowered.join("\u{1f}").into()],
        )
        .await
        {
            Ok(r) => r,
            Err(_) => return unavailable(&rid),
        }
    };
    let items = slice
        .iter()
        .zip(lowered.iter())
        .map(|(address, lower)| {
            let account = found.iter().find(|r| &r.email_lower == lower);
            json!({
                "emailMasked": mask_email(address),
                "registered": account.is_some(),
                "userId": account.map(|a| a.id),
                "accountStatus": account.map(|a| ledger::state_code(&a.status)),
                "role": account.map(|a| ledger::state_code(&a.role)),
            })
        })
        .collect();
    let next = (start + limit < all.len()).then(|| (start + limit).to_string());
    page(items, next)
}

// ---------------------------------------------------------------------------
// Meetings
// ---------------------------------------------------------------------------

/// `GET /internal/v1/operator/meetings/conferences/{conferenceId}/health`: the conference's
/// health as the operations view evaluates it (states, counts, sanitized provider region
/// and node labels).
pub(super) async fn conference_health(
    State(s): State<OperatorState>,
    Path(id): Path<String>,
    req: Parts,
) -> Response {
    let rid = auth!(s, req, InternalPermission::MeetingAdminister);
    let id = try_r!(path_uuid(&id, &rid));
    match s.app.meeting.operations.evaluate(id).await {
        Ok(h) => ok(serde_json::to_value(h).unwrap_or(Value::Null)),
        Err(e) if e.code() == Some(ErrorCode::ConferenceNotFound) => not_found(&rid),
        Err(crate::error::ApiError::NotFound(_)) => not_found(&rid),
        Err(_) => unavailable(&rid),
    }
}

#[derive(FromQueryResult)]
struct ArtifactRow {
    id: Uuid,
    meeting_space_id: Uuid,
    status: String,
    size_bytes: Option<i64>,
    created_at: DateTime<Utc>,
    available_at: Option<DateTime<Utc>>,
    retention_at: Option<DateTime<Utc>>,
    delete_requested_at: Option<DateTime<Utc>>,
    deleted_at: Option<DateTime<Utc>>,
    version: i32,
}

async fn artifacts(s: OperatorState, id: String, q: PageQuery, req: Parts, kind: &str) -> Response {
    let rid = auth!(s, req, InternalPermission::MeetingAdminister);
    let id = try_r!(path_uuid(&id, &rid));
    let limit = try_r!(limit_of(&q, &rid));
    let after = try_r!(uuid_cursor(&q, &rid));
    match exists(
        &s,
        "SELECT 1 AS one FROM conference_sessions WHERE id = $1 AND origin::text <> 'LEGACY'",
        id,
    )
    .await
    {
        Ok(true) => {}
        Ok(false) => return not_found(&rid),
        Err(_) => return unavailable(&rid),
    }
    let found: Vec<ArtifactRow> = match rows(
        &s,
        "SELECT id, meeting_space_id, status, size_bytes, created_at, available_at, retention_at, \
                delete_requested_at, deleted_at, version \
           FROM meeting_artifacts WHERE conference_id = $1 AND artifact_type = $2 \
            AND ($3::uuid IS NULL OR id > $3) ORDER BY id LIMIT $4",
        vec![id.into(), kind.into(), after.into(), (limit + 1).into()],
    )
    .await
    {
        Ok(r) => r,
        Err(_) => return unavailable(&rid),
    };
    let next = (found.len() as i64 > limit).then(|| found[limit as usize - 1].id.to_string());
    page(
        found
            .iter()
            .take(limit as usize)
            .map(|r| {
                json!({
                    "artifactId": r.id,
                    "conferenceId": id,
                    "spaceId": r.meeting_space_id,
                    "artifactStatus": r.status,
                    "sizeBytes": r.size_bytes,
                    "createdAt": rfc(r.created_at),
                    "availableAt": opt_rfc(r.available_at),
                    "retentionAt": opt_rfc(r.retention_at),
                    "deleteRequestedAt": opt_rfc(r.delete_requested_at),
                    "deletedAt": opt_rfc(r.deleted_at),
                    "version": r.version,
                })
            })
            .collect(),
        next,
    )
}

/// `GET …/meetings/conferences/{conferenceId}/recordings?limit&cursor`.
pub(super) async fn recordings(
    State(s): State<OperatorState>,
    Path(id): Path<String>,
    Query(q): Query<PageQuery>,
    req: Parts,
) -> Response {
    artifacts(s, id, q, req, "RECORDING").await
}

/// `GET …/meetings/conferences/{conferenceId}/transcripts?limit&cursor`.
pub(super) async fn transcripts(
    State(s): State<OperatorState>,
    Path(id): Path<String>,
    Query(q): Query<PageQuery>,
    req: Parts,
) -> Response {
    artifacts(s, id, q, req, "TRANSCRIPT").await
}

#[derive(FromQueryResult)]
struct WebinarRow {
    id: Uuid,
    meeting_code: String,
    meeting_type: String,
    status: String,
    owner_user_id: Option<Uuid>,
    organization_id: Option<Uuid>,
    created_at: DateTime<Utc>,
    version: i32,
    registered: i64,
    cancelled: i64,
}

/// `GET /internal/v1/operator/meetings/webinars?limit&cursor`: webinar and town-hall
/// meeting spaces with their registration counts (no title or description).
pub(super) async fn webinars(
    State(s): State<OperatorState>,
    Query(q): Query<PageQuery>,
    req: Parts,
) -> Response {
    let rid = auth!(s, req, InternalPermission::MeetingAdminister);
    let limit = try_r!(limit_of(&q, &rid));
    let after = try_r!(uuid_cursor(&q, &rid));
    let found: Vec<WebinarRow> = match rows(
        &s,
        "SELECT m.id, m.meeting_code, m.meeting_type::text AS meeting_type, m.status::text AS status, \
                m.owner_user_id, m.organization_id, m.created_at, m.version, \
                (SELECT count(*) FROM webinar_registrations r WHERE r.meeting_space_id = m.id \
                   AND r.status = 'REGISTERED') AS registered, \
                (SELECT count(*) FROM webinar_registrations r WHERE r.meeting_space_id = m.id \
                   AND r.status = 'CANCELLED') AS cancelled \
           FROM meeting_spaces m WHERE m.meeting_type::text IN ('WEBINAR', 'TOWN_HALL') \
            AND ($1::uuid IS NULL OR m.id > $1) ORDER BY m.id LIMIT $2",
        vec![after.into(), (limit + 1).into()],
    )
    .await
    {
        Ok(r) => r,
        Err(_) => return unavailable(&rid),
    };
    let next = (found.len() as i64 > limit).then(|| found[limit as usize - 1].id.to_string());
    page(
        found
            .iter()
            .take(limit as usize)
            .map(|r| {
                json!({
                    "spaceId": r.id,
                    "meetingCode": r.meeting_code,
                    "meetingType": r.meeting_type,
                    "status": r.status,
                    "ownerUserId": r.owner_user_id,
                    "organizationId": r.organization_id,
                    "registrationsActive": r.registered,
                    "registrationsCancelled": r.cancelled,
                    "createdAt": rfc(r.created_at),
                    "version": r.version,
                })
            })
            .collect(),
        next,
    )
}

#[derive(FromQueryResult)]
struct RegistrationRow {
    id: Uuid,
    conference_id: Option<Uuid>,
    status: String,
    user_id: Option<Uuid>,
    created_at: DateTime<Utc>,
    cancelled_at: Option<DateTime<Utc>>,
}

/// `GET …/meetings/webinars/{spaceId}/registrations?limit&cursor`: ids, states and times
/// only — never a registrant's address, name, organization or custom fields.
pub(super) async fn webinar_registrations(
    State(s): State<OperatorState>,
    Path(id): Path<String>,
    Query(q): Query<PageQuery>,
    req: Parts,
) -> Response {
    let rid = auth!(s, req, InternalPermission::MeetingAdminister);
    let id = try_r!(path_uuid(&id, &rid));
    let limit = try_r!(limit_of(&q, &rid));
    let after = try_r!(uuid_cursor(&q, &rid));
    match exists(&s, "SELECT 1 AS one FROM meeting_spaces WHERE id = $1", id).await {
        Ok(true) => {}
        Ok(false) => return not_found(&rid),
        Err(_) => return unavailable(&rid),
    }
    let found: Vec<RegistrationRow> = match rows(
        &s,
        "SELECT id, conference_id, status, user_id, created_at, cancelled_at \
           FROM webinar_registrations WHERE meeting_space_id = $1 \
            AND ($2::uuid IS NULL OR id > $2) ORDER BY id LIMIT $3",
        vec![id.into(), after.into(), (limit + 1).into()],
    )
    .await
    {
        Ok(r) => r,
        Err(_) => return unavailable(&rid),
    };
    let next = (found.len() as i64 > limit).then(|| found[limit as usize - 1].id.to_string());
    page(
        found
            .iter()
            .take(limit as usize)
            .map(|r| {
                json!({
                    "registrationId": r.id,
                    "spaceId": id,
                    "conferenceId": r.conference_id,
                    "status": r.status,
                    "userId": r.user_id,
                    "createdAt": rfc(r.created_at),
                    "cancelledAt": opt_rfc(r.cancelled_at),
                })
            })
            .collect(),
        next,
    )
}

/// `GET /internal/v1/operator/meetings/chat-migration`: the meeting-chat migration state,
/// version and gates (counts and the latest parity run; no actor or reason text).
pub(super) async fn chat_migration(State(s): State<OperatorState>, req: Parts) -> Response {
    let rid = auth!(s, req, InternalPermission::MeetingAdminister);
    let Some(unified) = s.app.meeting.core.unified.as_ref() else {
        return error(
            StatusCode::NOT_FOUND,
            "FEATURE_DISABLED",
            "Unified Messaging is not enabled in this deployment.",
            &rid,
        );
    };
    let v = match unified.migration_status().await {
        Ok(v) => v,
        Err(_) => return unavailable(&rid),
    };
    ok(json!({
        "state": v.state.as_str(),
        "version": v.version,
        "changedAt": v.changed_at,
        "pendingMirror": v.pending_mirror,
        "pendingRollbackCopies": v.pending_rollback_copies,
        "pendingSessionRevocations": v.pending_session_revocations,
        "linkedConferences": v.linked_conferences,
        "latestParityRun": v.latest_parity_run.map(|p| json!({
            "parityRunId": p.id,
            "status": p.status,
            "startedAt": p.started_at,
            "finishedAt": p.finished_at,
            "conferencesChecked": p.conferences_checked,
            "messagesChecked": p.messages_checked,
            "mismatches": p.mismatches,
        })),
    }))
}

/// `GET /internal/v1/operator/meetings/email-deliveries/{deliveryId}`: one delivery's
/// PII-free status (category, state, provider, attempts, error class, times).
pub(super) async fn email_delivery(
    State(s): State<OperatorState>,
    Path(id): Path<String>,
    req: Parts,
) -> Response {
    let rid = auth!(s, req, InternalPermission::MeetingAdminister);
    let id = try_r!(path_uuid(&id, &rid));
    match crate::services::email_delivery::status(&s.app.db, id).await {
        Ok(Some(d)) => ok(serde_json::to_value(d).unwrap_or(Value::Null)),
        Ok(None) => not_found(&rid),
        Err(_) => unavailable(&rid),
    }
}

/// `GET /internal/v1/operator/meetings/critical-lane`: the Account CRITICAL lane (counts,
/// the fallback key's readiness and the leased-uncompleted decision ids).
pub(super) async fn critical_lane(State(s): State<OperatorState>, req: Parts) -> Response {
    let rid = auth!(s, req, InternalPermission::MeetingAdminister);
    match crate::services::critical_notifications::lane_status(&s.app.db).await {
        Ok(l) => ok(serde_json::to_value(l).unwrap_or(Value::Null)),
        Err(_) => unavailable(&rid),
    }
}

// ---------------------------------------------------------------------------
// A7: owner-delivered personal-data exports (references only)
// ---------------------------------------------------------------------------

/// `GET /internal/v1/operator/accounts/users/{userId}/exports?limit&cursor`: the account's
/// exports, newest first (`cursor` = `<micros>.<exportId>`): state, delivery channel,
/// masked recipient, size, SHA-256 and times. Never the document, a token, a link or the
/// address.
pub(super) async fn user_exports(
    State(s): State<OperatorState>,
    Path(id): Path<String>,
    Query(q): Query<PageQuery>,
    req: Parts,
) -> Response {
    let rid = auth!(s, req, InternalPermission::AccountAdminister);
    let id = try_r!(path_uuid(&id, &rid));
    let limit = try_r!(limit_of(&q, &rid));
    let after = try_r!(time_cursor(&q, &rid));
    match exists(&s, "SELECT 1 AS one FROM users WHERE id = $1", id).await {
        Ok(true) => {}
        Ok(false) => return not_found(&rid),
        Err(_) => return unavailable(&rid),
    }
    let found = match crate::services::account_export::list(&s.app.db, id, after, limit).await {
        Ok(f) => f,
        Err(_) => return unavailable(&rid),
    };
    let next = (found.len() as i64 > limit).then(|| {
        let (at, eid, _) = &found[limit as usize - 1];
        format!("{}.{}", at.timestamp_micros(), eid)
    });
    page(
        found
            .into_iter()
            .take(limit as usize)
            .map(|(_, _, v)| v)
            .collect(),
        next,
    )
}

/// `GET /internal/v1/operator/accounts/exports/{exportId}`: one export's state (the same
/// reference the command answered).
pub(super) async fn export(
    State(s): State<OperatorState>,
    Path(id): Path<String>,
    req: Parts,
) -> Response {
    let rid = auth!(s, req, InternalPermission::AccountAdminister);
    let id = try_r!(path_uuid(&id, &rid));
    match crate::services::account_export::resource(&s.app.db, id).await {
        Ok(Some(v)) => ok(v),
        Ok(None) => not_found(&rid),
        Err(_) => unavailable(&rid),
    }
}

/// `GET /internal/v1/operator/operations/{idempotencyKey}`: the bridge's settlement read.
/// Only `cfmeet.admin-bridge` with operator attribution and one of the bridge permissions
/// gets an answer; anyone else, and an unknown key, gets `404`.
pub(super) async fn operation(
    State(s): State<OperatorState>,
    Path(key): Path<String>,
    req: Parts,
) -> Response {
    let path = req
        .extensions
        .get::<OriginalUri>()
        .map(|u| u.0.path().to_owned())
        .unwrap_or_else(|| req.uri.path().to_owned());
    let ctx = match s
        .verifier
        .verify_request(&req.method, &path, &req.headers)
        .await
    {
        Ok(ctx) => ctx,
        Err(_) => return not_found(RequestId::generate().as_str()),
    };
    let rid = ctx.request_id().as_str().to_string();
    let bridge = matches!(
        OperatorAttribution::from_headers(ctx.authenticated_service(), &req.headers),
        Ok(Some(_))
    );
    let permitted = ctx
        .require_permission(InternalPermission::AccountAdminister)
        .is_ok()
        || ctx
            .require_permission(InternalPermission::MeetingAdminister)
            .is_ok();
    if !bridge || !permitted {
        return not_found(&rid);
    }
    let Ok(key) = Uuid::parse_str(&key) else {
        return not_found(&rid);
    };
    match ledger::find(&s.app.db, key).await {
        Ok(Some(r)) => ok(json!({
            "idempotency_key": key,
            "outcome": "APPLIED",
            "source_status": r.resulting_state,
            "source_version": r.resulting_version.map(|v| v.to_string()),
        })),
        Ok(None) => not_found(&rid),
        Err(_) => unavailable(&rid),
    }
}
