//! Community invitations, invite links, join requests and bans (ADR 0073 decision 6).
//!
//! * Who invites (direct invitations and links): any member of an `OPEN` or `APPROVAL`
//!   community, only admins of an `INVITE_ONLY` one.
//! * Joining: an admin's invitation, or any link of an `OPEN` / `INVITE_ONLY` community,
//!   admits at once; in an `APPROVAL` community a link, or a non-admin's invitation, creates a
//!   join request an admin approves.
//! * Link tokens (`mcl_` + 32 random bytes, base64url) are returned once; only their
//!   SHA-256 is stored. An unknown, expired, used-up or revoked link (or a deleted
//!   community) answers `404 NOT_FOUND` / `INVITE_LINK_UNAVAILABLE` alike.
//! * A ban removes the person from the community and its groups, closes their invitations
//!   and requests, and refuses them every later admission (community and linked groups).

use chrono::{DateTime, Duration, Utc};
use rand::RngCore;
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    admit, as_member, audit_action, forbidden, groups, join_in, leave_in, load_community,
    may_moderate, membership_of, not_found, notices, page_size, parse_cursor, preview, text,
    CommunityRow, MemberRow, PageQuery, MAX_MESSAGE_CHARS,
};
use crate::{
    conversations::service::rfc3339,
    db,
    error::{Error, ErrorCode, Result},
    http::middleware::Acting,
    identity::{PrincipalStatus, TrustedContext},
    limits::Window,
    state::AppState,
};

pub const TOKEN_PREFIX: &str = "mcl_";
pub const MIN_TTL_SECONDS: i64 = 60;
pub const MAX_TTL_SECONDS: i64 = 30 * 24 * 3600;
pub const DEFAULT_TTL_SECONDS: i64 = 7 * 24 * 3600;
pub const MAX_USES: i32 = 10_000;
pub const MAX_ACTIVE_LINKS: i64 = 50;
pub const INVITATION_TTL_DAYS: i64 = 14;
pub const MAX_INVITEES: usize = 50;
pub const INVITATIONS_PER_HOUR: i64 = 200;
pub const REQUESTS_PER_HOUR: i64 = 30;
pub const PREVIEWS_PER_MINUTE: i64 = 60;

/// Whether `m` may invite people (directly or by link) to `c`.
pub(crate) fn may_invite(c: &CommunityRow, m: &MemberRow) -> Result<()> {
    if m.is_admin() || c.privacy != "INVITE_ONLY" {
        Ok(())
    } else {
        Err(forbidden("NOT_COMMUNITY_ADMIN"))
    }
}

// ------------------------------------------------------------------------ join requests

#[derive(Debug, Clone, FromQueryResult)]
struct RequestRow {
    id: Uuid,
    community_id: Uuid,
    community_name: String,
    principal_id: Uuid,
    conversation_id: Option<Uuid>,
    group_title: Option<String>,
    state: String,
    message: Option<String>,
    created_at: DateTime<Utc>,
    decided_at: Option<DateTime<Utc>>,
    decided_by_principal_id: Option<Uuid>,
}

const REQUEST_SELECT: &str = "SELECT r.id, r.community_id, k.name AS community_name, r.principal_id, r.conversation_id, \
       c.title AS group_title, r.state, r.message, r.created_at, r.decided_at, r.decided_by_principal_id \
     FROM messaging_community_join_requests r JOIN messaging_communities k ON k.id = r.community_id \
     LEFT JOIN messaging_conversations c ON c.id = r.conversation_id";

fn request_json(r: &RequestRow) -> Value {
    json!({
        "requestId": r.id,
        "communityId": r.community_id,
        "communityName": r.community_name,
        "principalId": r.principal_id,
        "conversationId": r.conversation_id,
        "groupTitle": r.group_title,
        "state": r.state,
        "message": r.message,
        "createdAt": rfc3339(r.created_at),
        "decidedAt": r.decided_at.map(rfc3339),
        "decidedByPrincipalId": r.decided_by_principal_id,
    })
}

pub(crate) async fn request_view<C: ConnectionTrait>(db: &C, id: Uuid) -> Result<Value> {
    let r: RequestRow = db::one(
        db,
        &format!("{REQUEST_SELECT} WHERE r.id = $1"),
        vec![id.into()],
    )
    .await?
    .ok_or_else(not_found)?;
    Ok(request_json(&r))
}

/// The people told about a new request: the community's admins (and, for a group, the
/// group's admins), at most 20.
async fn deciders<C: ConnectionTrait>(
    db: &C,
    community: Uuid,
    conversation: Option<Uuid>,
) -> Result<Vec<Uuid>> {
    #[derive(FromQueryResult)]
    struct P {
        principal_id: Uuid,
    }
    let rows: Vec<P> = db::all(
        db,
        "SELECT principal_id FROM messaging_community_members WHERE community_id = $1 AND state = 'ACTIVE' \
           AND role IN ('OWNER', 'ADMIN') \
         UNION SELECT principal_id FROM messaging_conversation_members WHERE conversation_id = $2 \
           AND state = 'ACTIVE' AND role IN ('OWNER', 'ADMIN') LIMIT 20",
        vec![community.into(), conversation.into()],
    )
    .await?;
    Ok(rows.into_iter().map(|r| r.principal_id).collect())
}

/// Opens (or returns the open) join request of `principal` for the community
/// (`conversation` `None`) or one of its groups, inside the caller's transaction.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn request_in<C: ConnectionTrait>(
    txn: &C,
    st: &AppState,
    ctx: &TrustedContext,
    community: &CommunityRow,
    principal: Uuid,
    conversation: Option<Uuid>,
    message: Option<String>,
    via: &'static str,
    now: DateTime<Utc>,
) -> Result<Uuid> {
    if super::banned(txn, community.id, principal).await? {
        return Err(Error::reason(
            ErrorCode::PrincipalNotAuthorized,
            "PRINCIPAL_BANNED",
        ));
    }
    #[derive(FromQueryResult)]
    struct Open {
        id: Uuid,
    }
    let open: Option<Open> = db::one(
        txn,
        "SELECT id FROM messaging_community_join_requests WHERE community_id = $1 AND principal_id = $2 \
           AND conversation_id IS NOT DISTINCT FROM $3 AND state = 'PENDING'",
        vec![community.id.into(), principal.into(), conversation.into()],
    )
    .await?;
    if let Some(o) = open {
        return Ok(o.id);
    }
    let id = Uuid::new_v4();
    db::exec(
        txn,
        "INSERT INTO messaging_community_join_requests (id, community_id, environment_id, principal_id, conversation_id, \
           state, message, via, created_at) VALUES ($1, $2, $3, $4, $5, 'PENDING', $6, $7, $8)",
        vec![
            id.into(),
            community.id.into(),
            ctx.scope.environment_id.into(),
            principal.into(),
            conversation.into(),
            message.into(),
            via.into(),
            now.into(),
        ],
    )
    .await?;
    for admin in deciders(txn, community.id, conversation).await? {
        if admin != principal {
            notices::enqueue(
                st,
                txn,
                ctx,
                notices::Kind::JoinRequested,
                id,
                community.id,
                admin,
                now,
            )
            .await?;
        }
    }
    audit_action(
        txn,
        ctx,
        principal,
        "COMMUNITY_JOIN_REQUESTED",
        community.id,
        json!({ "request_id": id, "conversation_id": conversation, "via": via }),
        now,
    )
    .await?;
    Ok(id)
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestQuery {
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub cursor: Option<String>,
}

/// `GET /v1/messaging/communities/{id}/join-requests` — the requests the caller may decide
/// (community admins: all; group admins: their groups'), oldest first.
pub async fn requests(
    state: &AppState,
    ctx: &TrustedContext,
    me: Uuid,
    id: Uuid,
    q: RequestQuery,
) -> Result<Value> {
    let (_, m) = as_member(&state.db, ctx, id, me, false).await?;
    let wanted = match q.state.as_deref() {
        None | Some("PENDING") => "PENDING",
        Some("APPROVED") => "APPROVED",
        Some("REJECTED") => "REJECTED",
        Some("CANCELLED") => "CANCELLED",
        Some(_) => {
            return Err(Error::validation(
                "state must be PENDING, APPROVED, REJECTED or CANCELLED",
            ))
        }
    };
    let group_admin_of = db::scalar_i64(
        &state.db,
        "SELECT count(*) FROM messaging_community_groups g JOIN messaging_conversation_members x \
           ON x.conversation_id = g.conversation_id AND x.principal_id = $2 AND x.state = 'ACTIVE' \
           AND x.role IN ('OWNER', 'ADMIN') WHERE g.community_id = $1",
        vec![id.into(), me.into()],
    )
    .await?;
    if !m.is_admin() && group_admin_of == 0 {
        return Err(forbidden("NOT_COMMUNITY_ADMIN"));
    }
    let limit = page_size(q.limit)?;
    let (t, after) = parse_cursor(q.cursor.as_deref())?.unwrap_or((super::far_past(), Uuid::nil()));
    let rows: Vec<RequestRow> = db::all(
        &state.db,
        &format!(
            "{REQUEST_SELECT} WHERE r.community_id = $1 AND r.state = $2 AND (r.created_at, r.id) > ($3, $4) \
               AND ($5 OR (r.conversation_id IS NOT NULL AND EXISTS (SELECT 1 FROM messaging_conversation_members x \
                 WHERE x.conversation_id = r.conversation_id AND x.principal_id = $6 AND x.state = 'ACTIVE' \
                   AND x.role IN ('OWNER', 'ADMIN')))) \
             ORDER BY r.created_at, r.id LIMIT $7"
        ),
        vec![
            id.into(),
            wanted.into(),
            t.into(),
            after.into(),
            m.is_admin().into(),
            me.into(),
            (limit + 1).into(),
        ],
    )
    .await?;
    let more = rows.len() as i64 > limit;
    let page: Vec<&RequestRow> = rows.iter().take(limit as usize).collect();
    let next = if more {
        page.last().map(|r| super::cursor(r.created_at, r.id))
    } else {
        None
    };
    Ok(
        json!({ "requests": page.iter().map(|r| request_json(r)).collect::<Vec<_>>(), "nextCursor": next }),
    )
}

/// `GET /v1/messaging/communities/join-requests` — my own requests (newest first).
pub async fn my_requests(
    state: &AppState,
    ctx: &TrustedContext,
    me: Uuid,
    q: PageQuery,
) -> Result<Value> {
    let limit = page_size(q.limit)?;
    let (t, after) = parse_cursor(q.cursor.as_deref())?
        .unwrap_or((super::far_future(), Uuid::from_u128(u128::MAX)));
    let rows: Vec<RequestRow> = db::all(
        &state.db,
        &format!(
            "{REQUEST_SELECT} WHERE r.environment_id = $1 AND r.principal_id = $2 AND k.state = 'ACTIVE' \
               AND k.tenant_id = $3 AND k.application_id = $4 AND (r.created_at, r.id) < ($5, $6) \
             ORDER BY r.created_at DESC, r.id DESC LIMIT $7"
        ),
        vec![
            ctx.scope.environment_id.into(),
            me.into(),
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            t.into(),
            after.into(),
            (limit + 1).into(),
        ],
    )
    .await?;
    let more = rows.len() as i64 > limit;
    let page: Vec<&RequestRow> = rows.iter().take(limit as usize).collect();
    let next = if more {
        page.last().map(|r| super::cursor(r.created_at, r.id))
    } else {
        None
    };
    Ok(
        json!({ "requests": page.iter().map(|r| request_json(r)).collect::<Vec<_>>(), "nextCursor": next }),
    )
}

#[derive(FromQueryResult)]
struct PendingRow {
    principal_id: Uuid,
    conversation_id: Option<Uuid>,
    state: String,
}

/// `POST /v1/messaging/communities/{id}/join-requests/{requestId}/approve` / `reject`.
pub async fn decide(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    request_id: Uuid,
    approve: bool,
) -> Result<Value> {
    let me = acting.principal_id;
    let ctx2 = ctx.clone();
    let st = state.clone();
    state
        .db
        .transaction::<_, (), Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let (c, m) = as_member(txn, &ctx2, id, me, true).await?;
                let r: PendingRow = db::one(
                    txn,
                    "SELECT principal_id, conversation_id, state FROM messaging_community_join_requests \
                      WHERE id = $1 AND community_id = $2 FOR UPDATE",
                    vec![request_id.into(), id.into()],
                )
                .await?
                .ok_or_else(not_found)?;
                if !groups::may_decide(txn, &m, r.conversation_id).await? {
                    return Err(forbidden("NOT_COMMUNITY_ADMIN"));
                }
                if r.state != "PENDING" {
                    return Err(Error::reason(ErrorCode::Conflict, "REQUEST_ALREADY_DECIDED"));
                }
                if approve {
                    match r.conversation_id {
                        None => {
                            join_in(txn, &st, &ctx2, &c, r.principal_id, "REQUEST", Some(me), now).await?;
                        }
                        Some(conversation) => {
                            // Only a current community member joins a community group.
                            membership_of(txn, id, r.principal_id)
                                .await?
                                .filter(MemberRow::is_active)
                                .ok_or(Error::reason(ErrorCode::Conflict, "NOT_A_COMMUNITY_MEMBER"))?;
                            admit(txn, &st, &ctx2, conversation, r.principal_id, false, false, now).await?;
                        }
                    }
                    notices::enqueue(
                        &st,
                        txn,
                        &ctx2,
                        notices::Kind::Approved,
                        request_id,
                        id,
                        r.principal_id,
                        now,
                    )
                    .await?;
                }
                db::exec(
                    txn,
                    "UPDATE messaging_community_join_requests SET state = $2, decided_at = $3, decided_by_principal_id = $4 \
                      WHERE id = $1",
                    vec![
                        request_id.into(),
                        (if approve { "APPROVED" } else { "REJECTED" }).into(),
                        now.into(),
                        me.into(),
                    ],
                )
                .await?;
                audit_action(
                    txn,
                    &ctx2,
                    me,
                    if approve { "COMMUNITY_REQUEST_APPROVED" } else { "COMMUNITY_REQUEST_REJECTED" },
                    id,
                    json!({ "request_id": request_id, "member_principal_id": r.principal_id }),
                    now,
                )
                .await
            })
        })
        .await
        .map_err(Error::from)?;
    request_view(&state.db, request_id).await
}

/// `POST /v1/messaging/communities/join-requests/{requestId}/cancel` — the requester.
pub async fn cancel_request(
    state: &AppState,
    ctx: &TrustedContext,
    me: Uuid,
    request_id: Uuid,
) -> Result<Value> {
    let n = db::exec(
        &state.db,
        "UPDATE messaging_community_join_requests SET state = 'CANCELLED', decided_at = $4 \
          WHERE id = $1 AND principal_id = $2 AND environment_id = $3 AND state = 'PENDING'",
        vec![
            request_id.into(),
            me.into(),
            ctx.scope.environment_id.into(),
            state.now().into(),
        ],
    )
    .await?;
    if n == 0 {
        let mine = db::scalar_i64(
            &state.db,
            "SELECT count(*) FROM messaging_community_join_requests WHERE id = $1 AND principal_id = $2 AND environment_id = $3",
            vec![request_id.into(), me.into(), ctx.scope.environment_id.into()],
        )
        .await?;
        if mine == 0 {
            return Err(not_found());
        }
        return Err(Error::reason(
            ErrorCode::Conflict,
            "REQUEST_ALREADY_DECIDED",
        ));
    }
    request_view(&state.db, request_id).await
}

// ---------------------------------------------------------------------------- invitations

#[derive(Debug, Clone, FromQueryResult)]
struct InvitationRow {
    id: Uuid,
    community_id: Uuid,
    principal_id: Uuid,
    invited_by_principal_id: Uuid,
    by_admin: bool,
    state: String,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

const INVITATION_COLUMNS: &str =
    "id, community_id, principal_id, invited_by_principal_id, by_admin, state, \
     created_at, expires_at";

fn invitation_json(i: &InvitationRow, now: DateTime<Utc>) -> Value {
    let state = if i.state == "PENDING" && i.expires_at <= now {
        "EXPIRED"
    } else {
        i.state.as_str()
    };
    json!({
        "invitationId": i.id,
        "communityId": i.community_id,
        "principalId": i.principal_id,
        "invitedByPrincipalId": i.invited_by_principal_id,
        "state": state,
        "createdAt": rfc3339(i.created_at),
        "expiresAt": rfc3339(i.expires_at),
    })
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InviteRequest {
    pub principal_ids: Vec<Uuid>,
}

/// `POST /v1/messaging/communities/{id}/invitations` — invites people (skipping members,
/// banned people, open invitations and anyone Communications says may not be invited).
pub async fn invite(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    request: InviteRequest,
) -> Result<Value> {
    let me = acting.principal_id;
    let mut people = request.principal_ids.clone();
    people.sort();
    people.dedup();
    if people.is_empty() || people.len() > MAX_INVITEES {
        return Err(Error::validation(format!(
            "principalIds must list 1-{MAX_INVITEES} people"
        )));
    }
    let (c, m) = as_member(&state.db, ctx, id, me, false).await?;
    may_invite(&c, &m)?;
    super::rate_limit(state, "invite", ctx, me, Window::Hour, INVITATIONS_PER_HOUR).await?;
    // Eligibility outside the transaction (directory and Communications calls).
    let mut skipped: Vec<Value> = Vec::new();
    let mut eligible: Vec<Uuid> = Vec::new();
    for p in people {
        if p == me {
            skipped.push(json!({ "principalId": p, "reason": "SELF" }));
            continue;
        }
        let ok = match state.principals.resolve(&ctx.scope, p).await {
            Ok(s @ PrincipalStatus::Active { .. }) if s.is_messaging_capable() => {
                !matches!(s, PrincipalStatus::Active { ref kind } if kind == "BOT_OR_SERVICE")
            }
            Ok(_) => false,
            Err(e) => {
                tracing::warn!(error = %e, "principal directory unavailable");
                return Err(Error::code(ErrorCode::ServiceUnavailable));
            }
        };
        if !ok {
            skipped.push(json!({ "principalId": p, "reason": "NOT_AVAILABLE" }));
            continue;
        }
        match crate::communications::require(
            state,
            ctx,
            cf_communications_policy::Action::GroupInvitation,
            me,
            p,
        )
        .await
        {
            Ok(()) => eligible.push(p),
            Err(e)
                if matches!(
                    e,
                    Error::Code {
                        code: ErrorCode::ServiceUnavailable,
                        ..
                    }
                ) =>
            {
                return Err(e)
            }
            Err(_) => skipped.push(json!({ "principalId": p, "reason": "NOT_ALLOWED" })),
        }
    }
    let ctx2 = ctx.clone();
    let st = state.clone();
    let (created, more_skipped) = state
        .db
        .transaction::<_, (Vec<Uuid>, Vec<Value>), Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let (c, m) = as_member(txn, &ctx2, id, me, true).await?;
                may_invite(&c, &m)?;
                let mut created = Vec::new();
                let mut skipped = Vec::new();
                for p in eligible {
                    if membership_of(txn, id, p).await?.is_some_and(|x| x.is_active()) {
                        skipped.push(json!({ "principalId": p, "reason": "ALREADY_MEMBER" }));
                        continue;
                    }
                    if super::banned(txn, id, p).await? {
                        skipped.push(json!({ "principalId": p, "reason": "BANNED" }));
                        continue;
                    }
                    // An expired invitation no longer blocks a new one.
                    db::exec(
                        txn,
                        "UPDATE messaging_community_invitations SET state = 'REVOKED', answered_at = $3 \
                          WHERE community_id = $1 AND principal_id = $2 AND state = 'PENDING' AND expires_at <= $3",
                        vec![id.into(), p.into(), now.into()],
                    )
                    .await?;
                    let inv = Uuid::new_v4();
                    let n = db::exec(
                        txn,
                        "INSERT INTO messaging_community_invitations (id, community_id, environment_id, principal_id, \
                           invited_by_principal_id, by_admin, state, created_at, expires_at) \
                         VALUES ($1, $2, $3, $4, $5, $6, 'PENDING', $7, $8) ON CONFLICT DO NOTHING",
                        vec![
                            inv.into(),
                            id.into(),
                            ctx2.scope.environment_id.into(),
                            p.into(),
                            me.into(),
                            m.is_admin().into(),
                            now.into(),
                            (now + Duration::days(INVITATION_TTL_DAYS)).into(),
                        ],
                    )
                    .await?;
                    if n == 0 {
                        skipped.push(json!({ "principalId": p, "reason": "ALREADY_INVITED" }));
                        continue;
                    }
                    notices::enqueue(&st, txn, &ctx2, notices::Kind::Invited, inv, id, p, now).await?;
                    created.push(inv);
                }
                if !created.is_empty() {
                    audit_action(
                        txn,
                        &ctx2,
                        me,
                        "COMMUNITY_INVITATIONS_SENT",
                        id,
                        json!({ "count": created.len() }),
                        now,
                    )
                    .await?;
                }
                Ok((created, skipped))
            })
        })
        .await
        .map_err(Error::from)?;
    skipped.extend(more_skipped);
    let now = state.now();
    let mut invitations = Vec::new();
    for inv in created {
        if let Some(row) = invitation(&state.db, inv).await? {
            invitations.push(invitation_json(&row, now));
        }
    }
    Ok(json!({ "invitations": invitations, "skipped": skipped }))
}

async fn invitation<C: ConnectionTrait>(db: &C, id: Uuid) -> Result<Option<InvitationRow>> {
    Ok(db::one(
        db,
        &format!("SELECT {INVITATION_COLUMNS} FROM messaging_community_invitations WHERE id = $1"),
        vec![id.into()],
    )
    .await?)
}

/// `GET /v1/messaging/communities/{id}/invitations` — open invitations (admins: all;
/// other inviting members: their own).
pub async fn invitations(
    state: &AppState,
    ctx: &TrustedContext,
    me: Uuid,
    id: Uuid,
) -> Result<Value> {
    let (c, m) = as_member(&state.db, ctx, id, me, false).await?;
    may_invite(&c, &m)?;
    let now = state.now();
    let rows: Vec<InvitationRow> = db::all(
        &state.db,
        &format!(
            "SELECT {INVITATION_COLUMNS} FROM messaging_community_invitations WHERE community_id = $1 \
               AND state = 'PENDING' AND expires_at > $2 AND ($3 OR invited_by_principal_id = $4) \
             ORDER BY created_at DESC, id LIMIT 200"
        ),
        vec![id.into(), now.into(), m.is_admin().into(), me.into()],
    )
    .await?;
    Ok(json!({ "invitations": rows.iter().map(|r| invitation_json(r, now)).collect::<Vec<_>>() }))
}

/// `DELETE /v1/messaging/communities/{id}/invitations/{invitationId}` — an admin or the
/// inviter.
pub async fn revoke_invitation(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    invitation_id: Uuid,
) -> Result<Value> {
    let me = acting.principal_id;
    let (_, m) = as_member(&state.db, ctx, id, me, false).await?;
    let row = invitation(&state.db, invitation_id)
        .await?
        .filter(|i| i.community_id == id)
        .ok_or_else(not_found)?;
    if !m.is_admin() && row.invited_by_principal_id != me {
        return Err(forbidden("NOT_COMMUNITY_ADMIN"));
    }
    let now = state.now();
    db::exec(
        &state.db,
        "UPDATE messaging_community_invitations SET state = 'REVOKED', answered_at = $2 WHERE id = $1 AND state = 'PENDING'",
        vec![invitation_id.into(), now.into()],
    )
    .await?;
    audit_action(
        &state.db,
        ctx,
        me,
        "COMMUNITY_INVITATION_REVOKED",
        id,
        json!({ "invitation_id": invitation_id }),
        now,
    )
    .await?;
    let row = invitation(&state.db, invitation_id)
        .await?
        .ok_or_else(not_found)?;
    Ok(invitation_json(&row, now))
}

/// `GET /v1/messaging/communities/invitations` — my open invitations with a preview.
pub async fn my_invitations(state: &AppState, ctx: &TrustedContext, me: Uuid) -> Result<Value> {
    let now = state.now();
    let rows: Vec<InvitationRow> = db::all(
        &state.db,
        &format!(
            "SELECT i.{} FROM messaging_community_invitations i JOIN messaging_communities k ON k.id = i.community_id \
              WHERE i.environment_id = $1 AND i.principal_id = $2 AND i.state = 'PENDING' AND i.expires_at > $3 \
                AND k.state = 'ACTIVE' AND k.tenant_id = $4 AND k.application_id = $5 \
              ORDER BY i.created_at DESC, i.id LIMIT 200",
            INVITATION_COLUMNS.replace(", ", ", i.")
        ),
        vec![
            ctx.scope.environment_id.into(),
            me.into(),
            now.into(),
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
        ],
    )
    .await?;
    let mut out = Vec::new();
    for r in &rows {
        if let Some(c) = load_community(&state.db, ctx, r.community_id, false).await? {
            let mut v = invitation_json(r, now);
            v["community"] = preview(&state.db, &c, me).await?;
            out.push(v);
        }
    }
    Ok(json!({ "invitations": out }))
}

/// `POST /v1/messaging/communities/invitations/{invitationId}/accept` / `decline`.
pub async fn answer(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    invitation_id: Uuid,
    accept: bool,
) -> Result<(u16, Value)> {
    let me = acting.principal_id;
    let ctx2 = ctx.clone();
    let st = state.clone();
    let outcome = state
        .db
        .transaction::<_, (Uuid, Option<Uuid>, bool), Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let inv: InvitationRow = db::one(
                    txn,
                    &format!(
                        "SELECT {INVITATION_COLUMNS} FROM messaging_community_invitations WHERE id = $1 \
                           AND principal_id = $2 AND environment_id = $3 FOR UPDATE"
                    ),
                    vec![invitation_id.into(), me.into(), ctx2.scope.environment_id.into()],
                )
                .await?
                .ok_or_else(not_found)?;
                let c = load_community(txn, &ctx2, inv.community_id, true)
                    .await?
                    .ok_or_else(not_found)?;
                if inv.state != "PENDING" || inv.expires_at <= now {
                    return Err(Error::reason(ErrorCode::Conflict, "INVITATION_NOT_OPEN"));
                }
                if !accept {
                    db::exec(
                        txn,
                        "UPDATE messaging_community_invitations SET state = 'DECLINED', answered_at = $2 WHERE id = $1",
                        vec![invitation_id.into(), now.into()],
                    )
                    .await?;
                    return Ok((c.id, None, false));
                }
                if membership_of(txn, c.id, me).await?.is_some_and(|x| x.is_active()) {
                    db::exec(
                        txn,
                        "UPDATE messaging_community_invitations SET state = 'ACCEPTED', answered_at = $2 WHERE id = $1",
                        vec![invitation_id.into(), now.into()],
                    )
                    .await?;
                    return Ok((c.id, None, true));
                }
                // An admin's invitation is approval; a member's in an APPROVAL community asks.
                if c.privacy == "APPROVAL" && !inv.by_admin {
                    let request =
                        request_in(txn, &st, &ctx2, &c, me, None, None, "INVITATION", now).await?;
                    db::exec(
                        txn,
                        "UPDATE messaging_community_invitations SET state = 'ACCEPTED', answered_at = $2 WHERE id = $1",
                        vec![invitation_id.into(), now.into()],
                    )
                    .await?;
                    return Ok((c.id, Some(request), false));
                }
                join_in(txn, &st, &ctx2, &c, me, "INVITATION", Some(inv.invited_by_principal_id), now)
                    .await?;
                Ok((c.id, None, true))
            })
        })
        .await
        .map_err(Error::from)?;
    let now = state.now();
    match outcome {
        (_, None, false) => {
            let row = invitation(&state.db, invitation_id)
                .await?
                .ok_or_else(not_found)?;
            Ok((200, invitation_json(&row, now)))
        }
        (_, Some(request), _) => Ok((
            202,
            json!({ "outcome": "REQUESTED", "request": request_view(&state.db, request).await? }),
        )),
        (community, None, true) => Ok((
            200,
            json!({ "outcome": "JOINED", "community": super::get(state, ctx, me, community).await? }),
        )),
    }
}

// --------------------------------------------------------------------------- invite links

#[derive(Debug, Clone, FromQueryResult)]
struct LinkRow {
    id: Uuid,
    community_id: Uuid,
    max_uses: Option<i32>,
    use_count: i32,
    expires_at: DateTime<Utc>,
    created_by_principal_id: Uuid,
    created_at: DateTime<Utc>,
    revoked_at: Option<DateTime<Utc>>,
}

const LINK_COLUMNS: &str =
    "id, community_id, max_uses, use_count, expires_at, created_by_principal_id, \
     created_at, revoked_at";

fn digest(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn new_token() -> String {
    let mut b = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut b);
    format!("{TOKEN_PREFIX}{}", crate::messages::b64(&b))
}

fn link_status(l: &LinkRow, now: DateTime<Utc>) -> &'static str {
    if l.revoked_at.is_some() {
        "REVOKED"
    } else if l.expires_at <= now {
        "EXPIRED"
    } else if l.max_uses.is_some_and(|m| l.use_count >= m) {
        "EXHAUSTED"
    } else {
        "ACTIVE"
    }
}

fn link_json(l: &LinkRow, now: DateTime<Utc>) -> Value {
    json!({
        "inviteLinkId": l.id,
        "communityId": l.community_id,
        "maxUses": l.max_uses,
        "useCount": l.use_count,
        "expiresAt": rfc3339(l.expires_at),
        "createdByPrincipalId": l.created_by_principal_id,
        "createdAt": rfc3339(l.created_at),
        "revokedAt": l.revoked_at.map(rfc3339),
        "status": link_status(l, now),
    })
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateLinkRequest {
    #[serde(default)]
    pub expires_in_seconds: Option<i64>,
    #[serde(default)]
    pub max_uses: Option<i32>,
}

/// `POST /v1/messaging/communities/{id}/invite-links` — the token is in this response only.
pub async fn create_link(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    request: CreateLinkRequest,
) -> Result<Value> {
    let me = acting.principal_id;
    let ttl = request.expires_in_seconds.unwrap_or(DEFAULT_TTL_SECONDS);
    if !(MIN_TTL_SECONDS..=MAX_TTL_SECONDS).contains(&ttl) {
        return Err(Error::validation(format!(
            "expiresInSeconds must be {MIN_TTL_SECONDS}-{MAX_TTL_SECONDS}"
        )));
    }
    if request
        .max_uses
        .is_some_and(|m| !(1..=MAX_USES).contains(&m))
    {
        return Err(Error::validation(format!("maxUses must be 1-{MAX_USES}")));
    }
    let (c, m) = as_member(&state.db, ctx, id, me, false).await?;
    may_invite(&c, &m)?;
    let now = state.now();
    let open = db::scalar_i64(
        &state.db,
        "SELECT count(*) FROM messaging_community_invite_links WHERE community_id = $1 AND revoked_at IS NULL \
           AND expires_at > $2",
        vec![id.into(), now.into()],
    )
    .await?;
    if open >= MAX_ACTIVE_LINKS {
        return Err(Error::reason(ErrorCode::Conflict, "TOO_MANY_INVITE_LINKS"));
    }
    let token = new_token();
    let link = Uuid::new_v4();
    db::exec(
        &state.db,
        "INSERT INTO messaging_community_invite_links (id, community_id, environment_id, token_sha256, max_uses, \
           expires_at, created_by_principal_id, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        vec![
            link.into(),
            id.into(),
            ctx.scope.environment_id.into(),
            digest(&token).into(),
            request.max_uses.into(),
            (now + Duration::seconds(ttl)).into(),
            me.into(),
            now.into(),
        ],
    )
    .await?;
    audit_action(
        &state.db,
        ctx,
        me,
        "COMMUNITY_INVITE_LINK_CREATED",
        id,
        json!({ "invite_link_id": link }),
        now,
    )
    .await?;
    let row = link_row(&state.db, id, link).await?;
    let mut v = link_json(&row, now);
    v["token"] = json!(token);
    Ok(v)
}

async fn link_row<C: ConnectionTrait>(db: &C, community: Uuid, id: Uuid) -> Result<LinkRow> {
    db::one(
        db,
        &format!("SELECT {LINK_COLUMNS} FROM messaging_community_invite_links WHERE community_id = $1 AND id = $2"),
        vec![community.into(), id.into()],
    )
    .await?
    .ok_or_else(not_found)
}

/// `GET /v1/messaging/communities/{id}/invite-links` — admins see every link, other
/// inviting members their own; never a token.
pub async fn links(state: &AppState, ctx: &TrustedContext, me: Uuid, id: Uuid) -> Result<Value> {
    let (c, m) = as_member(&state.db, ctx, id, me, false).await?;
    may_invite(&c, &m)?;
    let rows: Vec<LinkRow> = db::all(
        &state.db,
        &format!(
            "SELECT {LINK_COLUMNS} FROM messaging_community_invite_links WHERE community_id = $1 \
               AND ($2 OR created_by_principal_id = $3) ORDER BY created_at DESC, id LIMIT 200"
        ),
        vec![id.into(), m.is_admin().into(), me.into()],
    )
    .await?;
    let now = state.now();
    Ok(json!({ "inviteLinks": rows.iter().map(|r| link_json(r, now)).collect::<Vec<_>>() }))
}

/// `POST /v1/messaging/communities/{id}/invite-links/{linkId}/revoke` — final; an admin or
/// the link's creator.
pub async fn revoke_link(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    link_id: Uuid,
) -> Result<Value> {
    let me = acting.principal_id;
    let (_, m) = as_member(&state.db, ctx, id, me, false).await?;
    let row = link_row(&state.db, id, link_id).await?;
    if !m.is_admin() && row.created_by_principal_id != me {
        return Err(forbidden("NOT_COMMUNITY_ADMIN"));
    }
    let now = state.now();
    let n = db::exec(
        &state.db,
        "UPDATE messaging_community_invite_links SET revoked_at = $3, revoked_by_principal_id = $4 \
          WHERE community_id = $1 AND id = $2 AND revoked_at IS NULL",
        vec![id.into(), link_id.into(), now.into(), me.into()],
    )
    .await?;
    if n == 1 {
        audit_action(
            &state.db,
            ctx,
            me,
            "COMMUNITY_INVITE_LINK_REVOKED",
            id,
            json!({ "invite_link_id": link_id }),
            now,
        )
        .await?;
    }
    Ok(link_json(&link_row(&state.db, id, link_id).await?, now))
}

fn unavailable() -> Error {
    Error::reason(ErrorCode::NotFound, "INVITE_LINK_UNAVAILABLE")
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TokenRequest {
    pub token: String,
    #[serde(default)]
    pub message: Option<String>,
}

/// The ACTIVE link of `token` and its community (locked with `lock`).
async fn by_token<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    token: &str,
    lock: bool,
    now: DateTime<Utc>,
) -> Result<(LinkRow, CommunityRow)> {
    if !token.starts_with(TOKEN_PREFIX) || token.len() > 80 {
        return Err(unavailable());
    }
    let link: LinkRow = db::one(
        db,
        &format!(
            "SELECT {LINK_COLUMNS} FROM messaging_community_invite_links WHERE token_sha256 = $1 \
               AND environment_id = $2{}",
            if lock { " FOR UPDATE" } else { "" }
        ),
        vec![digest(token).into(), ctx.scope.environment_id.into()],
    )
    .await?
    .ok_or_else(unavailable)?;
    if link_status(&link, now) != "ACTIVE" {
        return Err(unavailable());
    }
    let c = load_community(db, ctx, link.community_id, lock)
        .await?
        .ok_or_else(unavailable)?;
    Ok((link, c))
}

/// `POST /v1/messaging/communities/invite-links/preview` `{ token }`.
pub async fn preview_link(
    state: &AppState,
    ctx: &TrustedContext,
    me: Uuid,
    request: TokenRequest,
) -> Result<Value> {
    super::rate_limit(
        state,
        "preview",
        ctx,
        me,
        Window::Minute,
        PREVIEWS_PER_MINUTE,
    )
    .await?;
    let (_, c) = by_token(&state.db, ctx, &request.token, false, state.now()).await?;
    if super::banned(&state.db, c.id, me).await? {
        return Err(unavailable());
    }
    preview(&state.db, &c, me).await
}

/// `POST /v1/messaging/communities/invite-links/join` `{ token, message? }`.
pub async fn join_link(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    request: TokenRequest,
) -> Result<(u16, Value)> {
    let me = acting.principal_id;
    if acting.kind == "BOT_OR_SERVICE" {
        return Err(Error::reason(
            ErrorCode::PrincipalNotAuthorized,
            "BOT_CANNOT_JOIN_BY_LINK",
        ));
    }
    let message = text(request.message.as_deref(), MAX_MESSAGE_CHARS, "message")?;
    super::rate_limit(state, "join", ctx, me, Window::Hour, REQUESTS_PER_HOUR).await?;
    let ctx2 = ctx.clone();
    let st = state.clone();
    let token = request.token;
    let outcome = state
        .db
        .transaction::<_, (u16, Uuid, Option<Uuid>), Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let (link, c) = by_token(txn, &ctx2, &token, true, now).await?;
                if membership_of(txn, c.id, me).await?.is_some_and(|x| x.is_active()) {
                    return Ok((200, c.id, None));
                }
                if c.privacy == "APPROVAL" {
                    let request = request_in(txn, &st, &ctx2, &c, me, None, message, "LINK", now).await?;
                    return Ok((202, c.id, Some(request)));
                }
                join_in(txn, &st, &ctx2, &c, me, "LINK", Some(link.created_by_principal_id), now).await?;
                db::exec(
                    txn,
                    "UPDATE messaging_community_invite_links SET use_count = use_count + 1 WHERE id = $1",
                    vec![link.id.into()],
                )
                .await?;
                Ok((201, c.id, None))
            })
        })
        .await
        .map_err(Error::from)?;
    match outcome {
        (202, _, Some(request)) => Ok((
            202,
            json!({ "outcome": "REQUESTED", "request": request_view(&state.db, request).await? }),
        )),
        (status, community, _) => Ok((
            status,
            json!({
                "outcome": if status == 201 { "JOINED" } else { "ALREADY_MEMBER" },
                "community": super::get(state, ctx, me, community).await?,
            }),
        )),
    }
}

// ----------------------------------------------------------------------------------- bans

#[derive(Debug, Clone, FromQueryResult)]
struct BanRow {
    principal_id: Uuid,
    banned_by_principal_id: Uuid,
    created_at: DateTime<Utc>,
}

fn ban_json(b: &BanRow) -> Value {
    json!({
        "principalId": b.principal_id,
        "bannedAt": rfc3339(b.created_at),
        "bannedByPrincipalId": b.banned_by_principal_id,
    })
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BanRequest {
    pub principal_id: Uuid,
}

/// `POST /v1/messaging/communities/{id}/bans` — an admin (only the owner bans an admin).
pub async fn ban(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    request: BanRequest,
) -> Result<Value> {
    let me = acting.principal_id;
    let target = request.principal_id;
    if target == me {
        return Err(Error::validation("you cannot ban yourself"));
    }
    let ctx2 = ctx.clone();
    let st = state.clone();
    state
        .db
        .transaction::<_, (), Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let (c, m) = as_member(txn, &ctx2, id, me, true).await?;
                let t = membership_of(txn, id, target).await?.filter(MemberRow::is_active);
                may_moderate(&m, t.as_ref().map(|x| x.role.as_str()))?;
                db::exec(
                    txn,
                    "INSERT INTO messaging_community_bans (community_id, principal_id, banned_by_principal_id, created_at) \
                     VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
                    vec![id.into(), target.into(), me.into(), now.into()],
                )
                .await?;
                if t.is_some() {
                    leave_in(txn, &ctx2, &c, target, false, me, now).await?;
                }
                db::exec(
                    txn,
                    "UPDATE messaging_community_invitations SET state = 'REVOKED', answered_at = $3 \
                      WHERE community_id = $1 AND principal_id = $2 AND state = 'PENDING'",
                    vec![id.into(), target.into(), now.into()],
                )
                .await?;
                db::exec(
                    txn,
                    "UPDATE messaging_community_join_requests SET state = 'REJECTED', decided_at = $3, decided_by_principal_id = $4 \
                      WHERE community_id = $1 AND principal_id = $2 AND state = 'PENDING'",
                    vec![id.into(), target.into(), now.into(), me.into()],
                )
                .await?;
                audit_action(
                    txn,
                    &ctx2,
                    me,
                    "COMMUNITY_MEMBER_BANNED",
                    id,
                    json!({ "member_principal_id": target }),
                    now,
                )
                .await
            })
        })
        .await
        .map_err(Error::from)?;
    let row: BanRow = db::one(
        &state.db,
        "SELECT principal_id, banned_by_principal_id, created_at FROM messaging_community_bans \
          WHERE community_id = $1 AND principal_id = $2",
        vec![id.into(), target.into()],
    )
    .await?
    .ok_or(Error::code(ErrorCode::InternalError))?;
    Ok(ban_json(&row))
}

/// `GET /v1/messaging/communities/{id}/bans` — admins.
pub async fn bans(state: &AppState, ctx: &TrustedContext, me: Uuid, id: Uuid) -> Result<Value> {
    let (_, m) = as_member(&state.db, ctx, id, me, false).await?;
    super::require_admin(&m)?;
    let rows: Vec<BanRow> = db::all(
        &state.db,
        "SELECT principal_id, banned_by_principal_id, created_at FROM messaging_community_bans \
          WHERE community_id = $1 ORDER BY created_at DESC, principal_id LIMIT 500",
        vec![id.into()],
    )
    .await?;
    Ok(json!({ "bans": rows.iter().map(ban_json).collect::<Vec<_>>() }))
}

/// `DELETE /v1/messaging/communities/{id}/bans/{principalId}` — admins. The person may be
/// invited again (nothing restores their membership).
pub async fn unban(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    target: Uuid,
) -> Result<Value> {
    let me = acting.principal_id;
    let (_, m) = as_member(&state.db, ctx, id, me, false).await?;
    super::require_admin(&m)?;
    let n = db::exec(
        &state.db,
        "DELETE FROM messaging_community_bans WHERE community_id = $1 AND principal_id = $2",
        vec![id.into(), target.into()],
    )
    .await?;
    if n == 0 {
        return Err(not_found());
    }
    audit_action(
        &state.db,
        ctx,
        me,
        "COMMUNITY_MEMBER_UNBANNED",
        id,
        json!({ "member_principal_id": target }),
        state.now(),
    )
    .await?;
    Ok(json!({ "principalId": target, "lifted": true }))
}
