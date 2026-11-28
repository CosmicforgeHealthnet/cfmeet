//! Reports, verification applications and the moderation history (ADR 0072).
//!
//! The owner side only: an owner applies for verification and sees the answer; anyone who
//! can see a channel reports it or one of its posts. Every DECISION (approve, reject or
//! revoke a verification; restrict, suspend, ban or reinstate a channel; take a post down;
//! resolve a report) comes from HQ through the admin bridge
//! (`crate::operator_commands::channels`). No customer route can set a badge or a state.

use chrono::{DateTime, Duration, Utc};
use sea_orm::FromQueryResult;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{audit, forbidden, limit, load, Role};
use crate::{
    conversations::service::rfc3339,
    db,
    error::{Error, ErrorCode, Result},
    http::middleware::Acting,
    identity::TrustedContext,
    limits::Window,
    social::{clean_text, not_found},
    state::AppState,
};

pub const REPORT_CATEGORIES: &[&str] = &[
    "SPAM",
    "SCAM",
    "HARASSMENT",
    "HATE",
    "VIOLENCE",
    "SEXUAL_CONTENT",
    "CHILD_SAFETY",
    "SELF_HARM",
    "IMPERSONATION",
    "MISINFORMATION",
    "COPYRIGHT",
    "ILLEGAL_CONTENT",
    "OTHER",
];

/// Why a channel asks for the badge.
pub const VERIFICATION_BASES: &[&str] = &[
    "PUBLIC_FIGURE",
    "BRAND",
    "ORGANIZATION",
    "MEDIA",
    "GOVERNMENT",
    "CREATOR",
    "OTHER",
];
pub const REPORTS_PER_HOUR: i64 = 20;
/// After a rejection the owner waits this long before applying again.
pub const REAPPLY_AFTER_DAYS: i64 = 30;
/// Followers a channel needs before it can apply.
pub const MIN_FOLLOWERS_TO_APPLY: i64 = 1;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReportRequest {
    /// One of [`REPORT_CATEGORIES`].
    pub category: String,
    /// A post of the channel (absent: the channel itself).
    #[serde(default)]
    pub post_id: Option<Uuid>,
    /// Optional words for the reviewers (at most 500 characters; kept by Messaging, never
    /// relayed to HQ's console).
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(FromQueryResult)]
struct ReportRow {
    id: Uuid,
    status: String,
    created_at: DateTime<Utc>,
}

/// Report a channel or one of its posts (`201`; the same open report again answers it
/// with `200`).
pub async fn report(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    req: ReportRequest,
) -> Result<(u16, Value)> {
    let me = acting.principal_id;
    let row = load(&state.db, ctx, me, id).await?;
    if row.role(me) == Role::Owner {
        return Err(Error::reason(ErrorCode::Conflict, "OWN_CHANNEL"));
    }
    if !REPORT_CATEGORIES.contains(&req.category.as_str()) {
        return Err(Error::validation("category is not a report category"));
    }
    let note = clean_text(&req.note, 500, "A note")?;
    if let Some(p) = req.post_id {
        let n = db::scalar_i64(
            &state.db,
            "SELECT count(*) FROM messaging_channel_posts WHERE id = $1 AND channel_id = $2",
            vec![p.into(), id.into()],
        )
        .await?;
        if n == 0 || !row.posts_readable(me) {
            return Err(not_found());
        }
    }
    let view = |r: &ReportRow| {
        json!({
            "reportId": r.id,
            "channelId": id,
            "postId": req.post_id,
            "category": req.category,
            "status": r.status,
            "createdAt": rfc3339(r.created_at),
        })
    };
    let open: Option<ReportRow> = db::one(
        &state.db,
        "SELECT id, status, created_at FROM messaging_channel_reports WHERE channel_id = $1 \
           AND coalesce(post_id, '00000000-0000-0000-0000-000000000000'::uuid) = coalesce($2::uuid, '00000000-0000-0000-0000-000000000000'::uuid) \
           AND reporter_principal_id = $3 AND status = 'OPEN'",
        vec![id.into(), req.post_id.into(), me.into()],
    )
    .await?;
    if let Some(r) = open {
        return Ok((200, view(&r)));
    }
    limit(state, ctx, "report", me, Window::Hour, REPORTS_PER_HOUR).await?;
    let report = Uuid::new_v4();
    let now = state.now();
    db::exec(
        &state.db,
        "INSERT INTO messaging_channel_reports (id, tenant_id, application_id, environment_id, channel_id, post_id, \
           reporter_principal_id, category, note, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
         ON CONFLICT DO NOTHING",
        vec![
            report.into(),
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            ctx.scope.environment_id.into(),
            id.into(),
            req.post_id.into(),
            me.into(),
            req.category.clone().into(),
            note.into(),
            now.into(),
        ],
    )
    .await?;
    crate::metrics::inc("messaging_channel_reports_total", &[]);
    let r: ReportRow = db::one(
        &state.db,
        "SELECT id, status, created_at FROM messaging_channel_reports WHERE channel_id = $1 \
           AND coalesce(post_id, '00000000-0000-0000-0000-000000000000'::uuid) = coalesce($2::uuid, '00000000-0000-0000-0000-000000000000'::uuid) \
           AND reporter_principal_id = $3 AND status = 'OPEN'",
        vec![id.into(), req.post_id.into(), me.into()],
    )
    .await?
    .ok_or(Error::code(ErrorCode::InternalError))?;
    Ok((201, view(&r)))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VerificationRequest {
    /// One of [`VERIFICATION_BASES`].
    pub basis: String,
}

#[derive(FromQueryResult)]
struct ApplicationRow {
    id: Uuid,
    basis: String,
    state: String,
    decision_reason: Option<String>,
    decided_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
}

fn application_view(a: &ApplicationRow) -> Value {
    json!({
        "applicationId": a.id,
        "basis": a.basis,
        "state": a.state,
        "decisionReason": a.decision_reason,
        "decidedAt": a.decided_at.map(rfc3339),
        "createdAt": rfc3339(a.created_at),
    })
}

async fn applications(state: &AppState, channel: Uuid) -> Result<Vec<ApplicationRow>> {
    Ok(db::all(
        &state.db,
        "SELECT id, basis, state, decision_reason, decided_at, created_at FROM messaging_channel_verifications \
          WHERE channel_id = $1 ORDER BY created_at DESC LIMIT 20",
        vec![channel.into()],
    )
    .await?)
}

/// The badge and the channel's applications (owner and admins).
pub async fn verification(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
) -> Result<Value> {
    let me = acting.principal_id;
    let row = load(&state.db, ctx, me, id).await?;
    if !row.staff(me) {
        return Err(forbidden(
            "NOT_CHANNEL_ADMIN",
            "Only the channel's owner and admins can do this",
        ));
    }
    let apps = applications(state, id).await?;
    Ok(json!({
        "channelId": id,
        "verified": row.verified_at.is_some(),
        "verifiedAt": row.verified_at.map(rfc3339),
        "applications": apps.iter().map(application_view).collect::<Vec<_>>(),
    }))
}

/// The owner applies for the badge (`201`). HQ reviews it; nothing here verifies.
pub async fn apply(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    req: VerificationRequest,
) -> Result<(u16, Value)> {
    let me = acting.principal_id;
    let row = load(&state.db, ctx, me, id).await?;
    if row.role(me) != Role::Owner {
        return Err(forbidden(
            "NOT_CHANNEL_OWNER",
            "Only the channel's owner can do this",
        ));
    }
    if !VERIFICATION_BASES.contains(&req.basis.as_str()) {
        return Err(Error::validation("basis is not a verification basis"));
    }
    let refuse = |reason: &'static str, message: &str| Error::Code {
        code: ErrorCode::Conflict,
        reason: Some(reason),
        message: Some(message.into()),
    };
    if row.verified_at.is_some() {
        return Err(refuse(
            "ALREADY_VERIFIED",
            "This channel is already verified",
        ));
    }
    if row.moderation_state != "ACTIVE" {
        return Err(refuse(
            "CHANNEL_NOT_ACTIVE",
            "A restricted or suspended channel can't apply",
        ));
    }
    if row.private() || row.handle.is_none() {
        return Err(refuse(
            "PUBLIC_HANDLE_REQUIRED",
            "Make the channel public and give it a username first",
        ));
    }
    if row.followers < MIN_FOLLOWERS_TO_APPLY {
        return Err(refuse(
            "TOO_FEW_FOLLOWERS",
            "The channel needs followers first",
        ));
    }
    let now = state.now();
    let apps = applications(state, id).await?;
    if apps.iter().any(|a| a.state == "PENDING") {
        return Err(refuse(
            "APPLICATION_PENDING",
            "An application is already being reviewed",
        ));
    }
    if apps.iter().any(|a| {
        a.state == "REJECTED"
            && a.decided_at
                .is_some_and(|d| d > now - Duration::days(REAPPLY_AFTER_DAYS))
    }) {
        return Err(refuse(
            "REAPPLY_TOO_SOON",
            "You can apply again 30 days after a rejection",
        ));
    }
    let app = Uuid::new_v4();
    let n = db::exec(
        &state.db,
        "INSERT INTO messaging_channel_verifications (id, tenant_id, application_id, environment_id, channel_id, \
           applicant_principal_id, basis, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) ON CONFLICT DO NOTHING",
        vec![
            app.into(),
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            ctx.scope.environment_id.into(),
            id.into(),
            me.into(),
            req.basis.clone().into(),
            now.into(),
        ],
    )
    .await?;
    if n == 0 {
        return Err(refuse(
            "APPLICATION_PENDING",
            "An application is already being reviewed",
        ));
    }
    audit(
        &state.db,
        id,
        me,
        "VERIFICATION_REQUESTED",
        None,
        Some(app),
        now,
    )
    .await?;
    let apps = applications(state, id).await?;
    let a = apps
        .iter()
        .find(|a| a.id == app)
        .ok_or(Error::code(ErrorCode::InternalError))?;
    Ok((201, application_view(a)))
}

#[derive(FromQueryResult)]
struct ActionRow {
    id: Uuid,
    action: String,
    resulting_state: Option<String>,
    reason: String,
    post_id: Option<Uuid>,
    created_at: DateTime<Utc>,
}

/// What HQ decided about the channel (owner and admins): actions, states, reasons and
/// times. Which HQ staff member acted is not shown.
pub async fn moderation(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
) -> Result<Value> {
    let me = acting.principal_id;
    let row = load(&state.db, ctx, me, id).await?;
    if !row.staff(me) {
        return Err(forbidden(
            "NOT_CHANNEL_ADMIN",
            "Only the channel's owner and admins can do this",
        ));
    }
    let rows: Vec<ActionRow> = db::all(
        &state.db,
        "SELECT id, action, resulting_state, reason, post_id, created_at FROM messaging_channel_moderation_actions \
          WHERE channel_id = $1 ORDER BY created_at DESC LIMIT 100",
        vec![id.into()],
    )
    .await?;
    Ok(json!({
        "channelId": id,
        "moderationState": row.moderation_state,
        "verified": row.verified_at.is_some(),
        "actions": rows.iter().map(|a| json!({
            "actionId": a.id,
            "action": a.action,
            "resultingState": a.resulting_state,
            "reason": a.reason,
            "postId": a.post_id,
            "createdAt": rfc3339(a.created_at),
        })).collect::<Vec<_>>(),
    }))
}
