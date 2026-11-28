//! Communities (ADR 0073): collections of related group conversations under one structure.
//!
//! * A community has a name, description, rules, a privacy setting (`OPEN`, `APPROVAL`,
//!   `INVITE_ONLY`), one OWNER, ADMINs and MEMBERs ([`MemberRow`]), and an Announcements
//!   group: an ordinary MANAGED `GROUP` conversation the server creates with the community,
//!   in which only its OWNER / ADMINs (the community owner and admins) post. Its group photo
//!   is the community picture and its pins are the pinned announcements.
//! * Its other groups ([`groups`]) are existing `GROUP` conversations, linked by one of
//!   their own admins or created inside the community. A conversation belongs to at most one
//!   community. Joining one goes through the ordinary admission
//!   ([`crate::membership::activate_from_authority`], `authority_source = 'COMMUNITY'`): a
//!   NEW history window, so nothing sent before the join is visible.
//! * Invitations, revocable invite links (`mcl_` tokens, SHA-256 stored), join requests and
//!   bans are in [`invites`]; notices for the Notification Service in [`notices`].
//! * Every community resource answers `404 NOT_FOUND` to anyone who is not an ACTIVE member;
//!   a member without the needed role gets `403 PRINCIPAL_NOT_AUTHORIZED`.
//!
//! Communities never read or require Organizations membership.

pub mod groups;
pub mod http;
pub mod invites;
pub mod notices;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    audit::{self, AuditEntry},
    conversations::{
        access,
        service::{self, load, request_context, rfc3339},
    },
    db,
    error::{Error, ErrorCode, Result},
    http::middleware::Acting,
    identity::TrustedContext,
    limits::{self, Window},
    membership,
    state::AppState,
};

pub const MAX_NAME_CHARS: usize = 100;
pub const MAX_DESCRIPTION_CHARS: usize = 2048;
pub const MAX_RULES_CHARS: usize = 4000;
pub const MAX_MESSAGE_CHARS: usize = 500;
/// Groups (the Announcements group included) per community.
pub const MAX_GROUPS: i64 = 100;
/// Communities one person may own.
pub const MAX_OWNED: i64 = 50;
pub const DEFAULT_PAGE: i64 = 50;
pub const MAX_PAGE: i64 = 100;
/// Communities created per person per hour.
pub const CREATIONS_PER_HOUR: i64 = 10;

/// Conversation states a linked group is still shown in.
pub(crate) const LIVE_STATES: &str = "('ACTIVE', 'LOCKED', 'CLOSED', 'ARCHIVED')";

#[derive(Debug, Clone, FromQueryResult)]
pub struct CommunityRow {
    pub id: Uuid,
    pub owner_principal_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub rules: Option<String>,
    pub privacy: String,
    pub members_can_add_groups: bool,
    pub announcements_conversation_id: Option<Uuid>,
    pub version: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const COLUMNS: &str =
    "id, owner_principal_id, name, description, rules, privacy, members_can_add_groups, \
     announcements_conversation_id, version, created_at, updated_at";

/// One community membership.
#[derive(Debug, Clone, FromQueryResult)]
pub struct MemberRow {
    pub principal_id: Uuid,
    pub role: String,
    pub state: String,
    pub joined_at: DateTime<Utc>,
}

impl MemberRow {
    pub fn is_active(&self) -> bool {
        self.state == "ACTIVE"
    }
    pub fn is_admin(&self) -> bool {
        self.is_active() && matches!(self.role.as_str(), "OWNER" | "ADMIN")
    }
    pub fn is_owner(&self) -> bool {
        self.is_active() && self.role == "OWNER"
    }
}

pub fn not_found() -> Error {
    Error::code(ErrorCode::NotFound)
}

pub(crate) fn forbidden(reason: &'static str) -> Error {
    Error::reason(ErrorCode::PrincipalNotAuthorized, reason)
}

pub(crate) fn require_admin(m: &MemberRow) -> Result<()> {
    if m.is_admin() {
        Ok(())
    } else {
        Err(forbidden("NOT_COMMUNITY_ADMIN"))
    }
}

pub(crate) fn require_owner(m: &MemberRow) -> Result<()> {
    if m.is_owner() {
        Ok(())
    } else {
        Err(forbidden("NOT_COMMUNITY_OWNER"))
    }
}

/// The ACTIVE community `id` of the caller's scope.
pub async fn load_community<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    id: Uuid,
    lock: bool,
) -> Result<Option<CommunityRow>> {
    Ok(db::one(
        db,
        &format!(
            "SELECT {COLUMNS} FROM messaging_communities WHERE id = $1 AND tenant_id = $2 AND application_id = $3 \
               AND environment_id = $4 AND state = 'ACTIVE'{}",
            if lock { " FOR UPDATE" } else { "" }
        ),
        vec![
            id.into(),
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            ctx.scope.environment_id.into(),
        ],
    )
    .await?)
}

/// `principal`'s membership row of `community` (any state).
pub async fn membership_of<C: ConnectionTrait>(
    db: &C,
    community: Uuid,
    principal: Uuid,
) -> Result<Option<MemberRow>> {
    Ok(db::one(
        db,
        "SELECT principal_id, role, state, joined_at FROM messaging_community_members \
          WHERE community_id = $1 AND principal_id = $2",
        vec![community.into(), principal.into()],
    )
    .await?)
}

/// The community and `me`'s ACTIVE membership; `NOT_FOUND` for anyone else.
pub async fn as_member<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    id: Uuid,
    me: Uuid,
    lock: bool,
) -> Result<(CommunityRow, MemberRow)> {
    let community = load_community(db, ctx, id, lock)
        .await?
        .ok_or_else(not_found)?;
    match membership_of(db, id, me).await? {
        Some(m) if m.is_active() => Ok((community, m)),
        _ => Err(not_found()),
    }
}

/// SQL condition "`principal` is an ACTIVE member of the ACTIVE community `community`"
/// (both SQL expressions), decided live. Events of COMMUNITY scope (ADR 0071 / 0073) use it
/// as their gate, so community membership is decided in this module only.
pub fn active_member_sql(community: &str, principal: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM messaging_community_members cm JOIN messaging_communities cc ON cc.id = cm.community_id            WHERE cm.community_id = {community} AND cm.principal_id = {principal} AND cm.state = 'ACTIVE'              AND cc.state = 'ACTIVE')"
    )
}

/// Whether `principal` is an ACTIVE member of the ACTIVE community `community`.
pub async fn is_active_member<C: ConnectionTrait>(
    db: &C,
    community: Uuid,
    principal: Uuid,
) -> Result<bool> {
    Ok(db::scalar_i64(
        db,
        &format!("SELECT count(*) WHERE {}", active_member_sql("$1", "$2")),
        vec![community.into(), principal.into()],
    )
    .await?
        > 0)
}

/// How many of `principals` are ACTIVE members of the ACTIVE community `community`.
pub async fn active_members_among<C: ConnectionTrait>(
    db: &C,
    community: Uuid,
    principals: &[Uuid],
) -> Result<i64> {
    Ok(db::scalar_i64(
        db,
        "SELECT count(DISTINCT cm.principal_id) FROM messaging_community_members cm            JOIN messaging_communities cc ON cc.id = cm.community_id           WHERE cm.community_id = $1 AND cm.principal_id = ANY($2) AND cm.state = 'ACTIVE' AND cc.state = 'ACTIVE'",
        vec![community.into(), principals.to_vec().into()],
    )
    .await?)
}

#[derive(FromQueryResult)]
struct MemberCount {
    community_id: Uuid,
    members: i64,
}

/// ACTIVE members of each ACTIVE community of `communities` (absent: none, or not ACTIVE).
pub async fn active_member_counts<C: ConnectionTrait>(
    db: &C,
    communities: &[Uuid],
) -> Result<std::collections::HashMap<Uuid, i64>> {
    if communities.is_empty() {
        return Ok(Default::default());
    }
    let rows: Vec<MemberCount> = db::all(
        db,
        "SELECT cm.community_id, count(*) AS members FROM messaging_community_members cm            JOIN messaging_communities cc ON cc.id = cm.community_id           WHERE cm.community_id = ANY($1) AND cm.state = 'ACTIVE' AND cc.state = 'ACTIVE' GROUP BY cm.community_id",
        vec![communities.to_vec().into()],
    )
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.community_id, r.members))
        .collect())
}

/// Whether `principal` is banned from `community`.
pub async fn banned<C: ConnectionTrait>(db: &C, community: Uuid, principal: Uuid) -> Result<bool> {
    Ok(db::scalar_i64(
        db,
        "SELECT count(*) FROM messaging_community_bans WHERE community_id = $1 AND principal_id = $2",
        vec![community.into(), principal.into()],
    )
    .await?
        > 0)
}

/// Whether a community ban keeps `principal` out of `conversation` (a linked group): checked
/// by every admission, including the group's own links and invitations.
pub async fn banned_from_conversation<C: ConnectionTrait>(
    db: &C,
    conversation: Uuid,
    principal: Uuid,
) -> Result<bool> {
    Ok(db::scalar_i64(
        db,
        "SELECT count(*) FROM messaging_community_groups g JOIN messaging_community_bans b \
           ON b.community_id = g.community_id AND b.principal_id = $2 WHERE g.conversation_id = $1",
        vec![conversation.into(), principal.into()],
    )
    .await?
        > 0)
}

/// Whether `conversation` is the Announcements group of an ACTIVE community: its membership
/// follows the community (people join, leave and are removed through the community).
pub async fn is_announcements<C: ConnectionTrait>(db: &C, conversation: Uuid) -> Result<bool> {
    Ok(db::scalar_i64(
        db,
        "SELECT count(*) FROM messaging_communities WHERE announcements_conversation_id = $1 AND state = 'ACTIVE'",
        vec![conversation.into()],
    )
    .await?
        > 0)
}

pub(crate) async fn rate_limit(
    state: &AppState,
    what: &str,
    ctx: &TrustedContext,
    me: Uuid,
    window: Window,
    max: i64,
) -> Result<()> {
    let key = format!(
        "community-{what}:{}:{}",
        ctx.scope.environment_id.simple(),
        me.simple()
    );
    if let Some(retry) = limits::hit(&state.db, &key, window, max, state.now()).await? {
        return Err(Error::RateLimited {
            retry_after_seconds: retry,
        });
    }
    Ok(())
}

pub(crate) async fn audit_action<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    actor: Uuid,
    action: &'static str,
    community: Uuid,
    detail: Value,
    now: DateTime<Utc>,
) -> Result<()> {
    audit::record(
        db,
        AuditEntry {
            ctx,
            principal_id: Some(actor),
            action,
            target_kind: "COMMUNITY",
            target_id: community,
            detail,
        },
        &request_context(),
        now,
    )
    .await
}

/// Trimmed text of at most `max` characters (line breaks kept); empty is `None`.
pub(crate) fn text(value: Option<&str>, max: usize, what: &str) -> Result<Option<String>> {
    crate::social::clean_text(&value.map(str::to_owned), max, what)
}

pub(crate) fn privacy(value: &str) -> Result<&'static str> {
    match value {
        "OPEN" => Ok("OPEN"),
        "APPROVAL" => Ok("APPROVAL"),
        "INVITE_ONLY" => Ok("INVITE_ONLY"),
        _ => Err(Error::validation(
            "privacy must be OPEN, APPROVAL or INVITE_ONLY",
        )),
    }
}

// ---------------------------------------------------------------------------- cursors

pub(crate) fn cursor(t: DateTime<Utc>, id: Uuid) -> String {
    URL_SAFE_NO_PAD.encode(format!("{}|{id}", t.timestamp_micros()))
}

pub(crate) fn parse_cursor(c: Option<&str>) -> Result<Option<(DateTime<Utc>, Uuid)>> {
    let Some(c) = c else { return Ok(None) };
    let bad = || Error::validation("cursor is not valid");
    let raw = URL_SAFE_NO_PAD.decode(c).map_err(|_| bad())?;
    let raw = String::from_utf8(raw).map_err(|_| bad())?;
    let (t, id) = raw.split_once('|').ok_or_else(bad)?;
    let t = DateTime::from_timestamp_micros(t.parse().map_err(|_| bad())?).ok_or_else(bad)?;
    Ok(Some((t, Uuid::parse_str(id).map_err(|_| bad())?)))
}

/// Bounds of the keyset pages (inside PostgreSQL's timestamp range).
pub(crate) fn far_past() -> DateTime<Utc> {
    DateTime::from_timestamp(0, 0).unwrap_or_default()
}

pub(crate) fn far_future() -> DateTime<Utc> {
    DateTime::from_timestamp(253_402_300_799, 0).unwrap_or_default()
}

pub(crate) fn page_size(limit: Option<i64>) -> Result<i64> {
    match limit {
        None => Ok(DEFAULT_PAGE),
        Some(l) if (1..=MAX_PAGE).contains(&l) => Ok(l),
        Some(_) => Err(Error::validation(format!("limit must be 1-{MAX_PAGE}"))),
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PageQuery {
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub cursor: Option<String>,
}

// ------------------------------------------------------------------- conversations

/// Makes `principal` an ACTIVE member of the group `conversation` through the community
/// (a new history window). A group that no longer accepts members is skipped when
/// `best_effort` (the Announcements group of a join), refused otherwise.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn admit<C: ConnectionTrait>(
    txn: &C,
    st: &AppState,
    ctx: &TrustedContext,
    conversation: Uuid,
    principal: Uuid,
    admin: bool,
    best_effort: bool,
    now: DateTime<Utc>,
) -> Result<()> {
    let Some(conv) = load(txn, ctx, conversation, true).await? else {
        return if best_effort {
            Ok(())
        } else {
            Err(not_found())
        };
    };
    if best_effort && access::require_accepting(&conv, now).is_err() {
        return Ok(());
    }
    let request = request_context();
    let role = if admin { "ADMIN" } else { "MEMBER" };
    let existing = access::membership(txn, conv.id, principal, false).await?;
    if existing.as_ref().is_some_and(|m| m.is_active()) {
        if admin && existing.is_some_and(|m| m.role == "MEMBER") {
            membership::set_role_in(txn, ctx, &conv, principal, "ADMIN", &request, now).await?;
        }
        return Ok(());
    }
    membership::activate_from_authority(
        txn,
        st,
        ctx,
        &conv,
        principal,
        role,
        "COMMUNITY",
        &request,
        now,
    )
    .await?;
    Ok(())
}

/// Removes (or lets leave) `principal` from the group `conversation`. A person removed by
/// others keeps a group they own; a last owner with other members stays.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn depart<C: ConnectionTrait>(
    txn: &C,
    ctx: &TrustedContext,
    conversation: Uuid,
    principal: Uuid,
    leaving: bool,
    reason: &str,
    actor: Uuid,
    now: DateTime<Utc>,
) -> Result<()> {
    let Some(conv) = load(txn, ctx, conversation, true).await? else {
        return Ok(());
    };
    let Some(m) = access::membership(txn, conv.id, principal, false)
        .await?
        .filter(|m| m.is_active())
    else {
        return Ok(());
    };
    if !leaving && m.role == "OWNER" {
        return Ok(());
    }
    if leaving && m.role == "OWNER" {
        // remove_in hands ownership to the most senior member, or refuses for a last owner
        // with members left: check first (an error inside the transaction is final).
        let others = db::scalar_i64(
            txn,
            "SELECT count(*) FROM messaging_conversation_members WHERE conversation_id = $1 \
               AND principal_id <> $2 AND state = 'ACTIVE' AND role IN ('ADMIN', 'MODERATOR', 'MEMBER')",
            vec![conv.id.into(), principal.into()],
        )
        .await?;
        let any = db::scalar_i64(
            txn,
            "SELECT count(*) FROM messaging_conversation_members WHERE conversation_id = $1 \
               AND principal_id <> $2 AND state = 'ACTIVE'",
            vec![conv.id.into(), principal.into()],
        )
        .await?;
        if others == 0 && any > 0 {
            return Ok(());
        }
    }
    membership::remove_in(
        txn,
        ctx,
        &conv,
        principal,
        leaving,
        reason,
        Some(actor),
        &request_context(),
        now,
    )
    .await?;
    Ok(())
}

/// The group conversations of `community` `principal` is an ACTIVE member of.
pub(crate) async fn joined_groups<C: ConnectionTrait>(
    db: &C,
    community: Uuid,
    principal: Uuid,
) -> Result<Vec<Uuid>> {
    #[derive(FromQueryResult)]
    struct Row {
        conversation_id: Uuid,
    }
    let rows: Vec<Row> = db::all(
        db,
        &format!(
            "SELECT g.conversation_id FROM messaging_community_groups g \
               JOIN messaging_conversations c ON c.id = g.conversation_id AND c.lifecycle_state IN {LIVE_STATES} \
               JOIN messaging_conversation_members m ON m.conversation_id = g.conversation_id \
                    AND m.principal_id = $2 AND m.state = 'ACTIVE' \
              WHERE g.community_id = $1 ORDER BY g.announcements DESC, g.position, g.linked_at"
        ),
        vec![community.into(), principal.into()],
    )
    .await?;
    Ok(rows.into_iter().map(|r| r.conversation_id).collect())
}

/// Admits `principal` to `community` (a new membership or a re-admission), in the caller's
/// transaction holding the community row lock: the membership row and the Announcements
/// group. Banned principals are refused.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn join_in<C: ConnectionTrait>(
    txn: &C,
    st: &AppState,
    ctx: &TrustedContext,
    community: &CommunityRow,
    principal: Uuid,
    source: &'static str,
    added_by: Option<Uuid>,
    now: DateTime<Utc>,
) -> Result<bool> {
    if banned(txn, community.id, principal).await? {
        return Err(Error::reason(
            ErrorCode::PrincipalNotAuthorized,
            "PRINCIPAL_BANNED",
        ));
    }
    let existing = membership_of(txn, community.id, principal).await?;
    if existing.as_ref().is_some_and(MemberRow::is_active) {
        return Ok(false);
    }
    db::exec(
        txn,
        "INSERT INTO messaging_community_members (community_id, principal_id, environment_id, role, state, source, \
           added_by_principal_id, joined_at, updated_at) VALUES ($1, $2, $3, 'MEMBER', 'ACTIVE', $4, $5, $6, $6) \
         ON CONFLICT (community_id, principal_id) DO UPDATE SET role = 'MEMBER', state = 'ACTIVE', source = $4, \
           added_by_principal_id = $5, joined_at = $6, updated_at = $6",
        vec![
            community.id.into(),
            principal.into(),
            ctx.scope.environment_id.into(),
            source.into(),
            added_by.into(),
            now.into(),
        ],
    )
    .await?;
    // An open invitation or request is settled by the join.
    db::exec(
        txn,
        "UPDATE messaging_community_invitations SET state = 'ACCEPTED', answered_at = $3 \
          WHERE community_id = $1 AND principal_id = $2 AND state = 'PENDING'",
        vec![community.id.into(), principal.into(), now.into()],
    )
    .await?;
    if let Some(ann) = community.announcements_conversation_id {
        admit(txn, st, ctx, ann, principal, false, true, now).await?;
    }
    audit_action(
        txn,
        ctx,
        added_by.unwrap_or(principal),
        "COMMUNITY_MEMBER_JOINED",
        community.id,
        json!({ "member_principal_id": principal, "source": source }),
        now,
    )
    .await?;
    crate::metrics::inc("messaging_community_joins_total", &[("source", source)]);
    Ok(true)
}

/// Ends `principal`'s membership (`LEFT` / `REMOVED`) and takes them out of the community's
/// groups (a group they own and others still use stays theirs).
pub(crate) async fn leave_in<C: ConnectionTrait>(
    txn: &C,
    ctx: &TrustedContext,
    community: &CommunityRow,
    principal: Uuid,
    leaving: bool,
    actor: Uuid,
    now: DateTime<Utc>,
) -> Result<()> {
    db::exec(
        txn,
        "UPDATE messaging_community_members SET state = $3, updated_at = $4 \
          WHERE community_id = $1 AND principal_id = $2 AND state = 'ACTIVE'",
        vec![
            community.id.into(),
            principal.into(),
            (if leaving { "LEFT" } else { "REMOVED" }).into(),
            now.into(),
        ],
    )
    .await?;
    // Their invite links stop admitting people.
    db::exec(
        txn,
        "UPDATE messaging_community_invite_links SET revoked_at = $3, revoked_by_principal_id = $4           WHERE community_id = $1 AND created_by_principal_id = $2 AND revoked_at IS NULL",
        vec![
            community.id.into(),
            principal.into(),
            now.into(),
            actor.into(),
        ],
    )
    .await?;
    let reason = if leaving {
        "LEFT_COMMUNITY"
    } else {
        "REMOVED_FROM_COMMUNITY"
    };
    for conversation in joined_groups(txn, community.id, principal).await? {
        depart(
            txn,
            ctx,
            conversation,
            principal,
            leaving,
            reason,
            actor,
            now,
        )
        .await?;
    }
    db::exec(
        txn,
        "UPDATE messaging_community_join_requests SET state = 'CANCELLED', decided_at = $3 \
          WHERE community_id = $1 AND principal_id = $2 AND state = 'PENDING' AND conversation_id IS NOT NULL",
        vec![community.id.into(), principal.into(), now.into()],
    )
    .await?;
    Ok(())
}

// ----------------------------------------------------------------------------- views

#[derive(FromQueryResult)]
struct Aggregates {
    member_count: i64,
    group_count: i64,
    picture_ref: Option<String>,
    pending_requests: i64,
}

/// The community as `me` sees it.
pub async fn view<C: ConnectionTrait>(db: &C, c: &CommunityRow, me: &MemberRow) -> Result<Value> {
    let a: Aggregates = db::one(
        db,
        &format!(
            "SELECT \
               (SELECT count(*) FROM messaging_community_members WHERE community_id = $1 AND state = 'ACTIVE') AS member_count, \
               (SELECT count(*) FROM messaging_community_groups g JOIN messaging_conversations c ON c.id = g.conversation_id \
                  WHERE g.community_id = $1 AND c.lifecycle_state IN {LIVE_STATES}) AS group_count, \
               (SELECT avatar_ref FROM messaging_conversations WHERE id = $3) AS picture_ref, \
               (SELECT count(*) FROM messaging_community_join_requests r WHERE r.community_id = $1 AND r.state = 'PENDING' \
                  AND ($4 OR (r.conversation_id IS NOT NULL AND EXISTS (SELECT 1 FROM messaging_conversation_members m \
                    WHERE m.conversation_id = r.conversation_id AND m.principal_id = $2 AND m.state = 'ACTIVE' \
                      AND m.role IN ('OWNER', 'ADMIN'))))) AS pending_requests"
        ),
        vec![
            c.id.into(),
            me.principal_id.into(),
            c.announcements_conversation_id.into(),
            me.is_admin().into(),
        ],
    )
    .await?
    .ok_or(Error::code(ErrorCode::InternalError))?;
    let joined = joined_groups(db, c.id, me.principal_id).await?;
    Ok(json!({
        "communityId": c.id,
        "name": c.name,
        "description": c.description,
        "rules": c.rules,
        "privacy": c.privacy,
        "membersCanAddGroups": c.members_can_add_groups,
        "ownerPrincipalId": c.owner_principal_id,
        "announcementsConversationId": c.announcements_conversation_id,
        "pictureRef": a.picture_ref,
        "memberCount": a.member_count,
        "groupCount": a.group_count,
        "myRole": me.role,
        "joinedAt": rfc3339(me.joined_at),
        "joinedConversationIds": joined,
        "pendingRequestCount": a.pending_requests,
        "version": c.version,
        "createdAt": rfc3339(c.created_at),
        "updatedAt": rfc3339(c.updated_at),
    }))
}

/// What someone holding an invitation or a link may see before joining.
pub async fn preview<C: ConnectionTrait>(db: &C, c: &CommunityRow, viewer: Uuid) -> Result<Value> {
    let members = db::scalar_i64(
        db,
        "SELECT count(*) FROM messaging_community_members WHERE community_id = $1 AND state = 'ACTIVE'",
        vec![c.id.into()],
    )
    .await?;
    let groups = db::scalar_i64(
        db,
        &format!(
            "SELECT count(*) FROM messaging_community_groups g JOIN messaging_conversations c ON c.id = g.conversation_id \
              WHERE g.community_id = $1 AND c.lifecycle_state IN {LIVE_STATES}"
        ),
        vec![c.id.into()],
    )
    .await?;
    let member = membership_of(db, c.id, viewer)
        .await?
        .is_some_and(|m| m.is_active());
    let pending = db::scalar_i64(
        db,
        "SELECT count(*) FROM messaging_community_join_requests WHERE community_id = $1 AND principal_id = $2 \
           AND state = 'PENDING' AND conversation_id IS NULL",
        vec![c.id.into(), viewer.into()],
    )
    .await?;
    Ok(json!({
        "communityId": c.id,
        "name": c.name,
        "description": c.description,
        "rules": c.rules,
        "privacy": c.privacy,
        "memberCount": members,
        "groupCount": groups,
        "member": member,
        "pendingRequest": pending > 0,
    }))
}

/// The community `id` as the ACTIVE member `me` sees it.
pub async fn get(state: &AppState, ctx: &TrustedContext, me: Uuid, id: Uuid) -> Result<Value> {
    let (c, m) = as_member(&state.db, ctx, id, me, false).await?;
    view(&state.db, &c, &m).await
}

#[derive(FromQueryResult)]
struct ListedRow {
    id: Uuid,
    joined_at: DateTime<Utc>,
}

/// `GET /v1/messaging/communities` — my communities, most recently joined first.
pub async fn list(state: &AppState, ctx: &TrustedContext, me: Uuid, q: PageQuery) -> Result<Value> {
    let limit = page_size(q.limit)?;
    let after = parse_cursor(q.cursor.as_deref())?;
    let (t, id) = after.unwrap_or((far_future(), Uuid::from_u128(u128::MAX)));
    let rows: Vec<ListedRow> = db::all(
        &state.db,
        "SELECT c.id, m.joined_at FROM messaging_community_members m JOIN messaging_communities c ON c.id = m.community_id \
          WHERE m.environment_id = $1 AND m.principal_id = $2 AND m.state = 'ACTIVE' AND c.state = 'ACTIVE' \
            AND c.tenant_id = $3 AND c.application_id = $4 AND (m.joined_at, c.id) < ($5, $6) \
          ORDER BY m.joined_at DESC, c.id DESC LIMIT $7",
        vec![
            ctx.scope.environment_id.into(),
            me.into(),
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            t.into(),
            id.into(),
            (limit + 1).into(),
        ],
    )
    .await?;
    let more = rows.len() as i64 > limit;
    let mut out = Vec::new();
    for r in rows.iter().take(limit as usize) {
        if let Ok((c, m)) = as_member(&state.db, ctx, r.id, me, false).await {
            out.push(view(&state.db, &c, &m).await?);
        }
    }
    let next = if more {
        rows.get(limit as usize - 1)
            .map(|r| cursor(r.joined_at, r.id))
    } else {
        None
    };
    Ok(json!({ "communities": out, "nextCursor": next }))
}

// --------------------------------------------------------------------------- creation

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateRequest {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub rules: Option<String>,
    #[serde(default)]
    pub privacy: Option<String>,
    #[serde(default)]
    pub members_can_add_groups: Option<bool>,
}

/// `POST /v1/messaging/communities` — the creator owns the community and its Announcements
/// group. `(status, view)`.
pub async fn create(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    request: CreateRequest,
    key: Option<String>,
) -> Result<(u16, Value)> {
    let me = acting.principal_id;
    let name = text(Some(&request.name), MAX_NAME_CHARS, "name")?
        .ok_or_else(|| Error::validation("name is required"))?;
    let description = text(
        request.description.as_deref(),
        MAX_DESCRIPTION_CHARS,
        "description",
    )?;
    let rules = text(request.rules.as_deref(), MAX_RULES_CHARS, "rules")?;
    let privacy = privacy(request.privacy.as_deref().unwrap_or("APPROVAL"))?;
    let now = state.now();
    let idem = key.map(|k| {
        crate::social::Idempotent::new(
            "community.create",
            k,
            me,
            &serde_json::to_value(&request).unwrap_or(Value::Null),
        )
    });
    if let Some(i) = &idem {
        if let Some(v) =
            crate::social::replayed(&state.db, ctx.scope.environment_id, i, now).await?
        {
            return replay(state, ctx, acting, v).await;
        }
    }
    rate_limit(state, "create", ctx, me, Window::Hour, CREATIONS_PER_HOUR).await?;
    let owned = db::scalar_i64(
        &state.db,
        "SELECT count(*) FROM messaging_communities WHERE environment_id = $1 AND owner_principal_id = $2 AND state = 'ACTIVE'",
        vec![ctx.scope.environment_id.into(), me.into()],
    )
    .await?;
    if owned >= MAX_OWNED {
        return Err(Error::reason(
            ErrorCode::QuotaExceeded,
            "TOO_MANY_COMMUNITIES",
        ));
    }
    let id = Uuid::new_v4();
    let ctx2 = ctx.clone();
    let members_can_add_groups = request.members_can_add_groups.unwrap_or(false);
    let replay_of = state
        .db
        .transaction::<_, Option<Value>, Error>(move |txn| {
            Box::pin(async move {
                if let Some(i) = &idem {
                    if let Some(v) =
                        crate::social::claim(txn, ctx2.scope.environment_id, i, now).await?
                    {
                        return Ok(Some(v));
                    }
                }
                db::exec(
                    txn,
                    "INSERT INTO messaging_communities (id, tenant_id, application_id, environment_id, owner_principal_id, name, \
                       description, rules, privacy, members_can_add_groups, created_by_principal_id, created_at, updated_at) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $5, $11, $11)",
                    vec![
                        id.into(),
                        ctx2.scope.tenant_id.into(),
                        ctx2.scope.application_id.into(),
                        ctx2.scope.environment_id.into(),
                        me.into(),
                        name.into(),
                        description.into(),
                        rules.into(),
                        privacy.into(),
                        members_can_add_groups.into(),
                        now.into(),
                    ],
                )
                .await?;
                db::exec(
                    txn,
                    "INSERT INTO messaging_community_members (community_id, principal_id, environment_id, role, state, source, \
                       added_by_principal_id, joined_at, updated_at) VALUES ($1, $2, $3, 'OWNER', 'ACTIVE', 'CREATOR', $2, $4, $4)",
                    vec![id.into(), me.into(), ctx2.scope.environment_id.into(), now.into()],
                )
                .await?;
                audit_action(txn, &ctx2, me, "COMMUNITY_CREATED", id, json!({}), now).await?;
                if let Some(i) = &idem {
                    crate::social::remember(
                        txn,
                        ctx2.scope.environment_id,
                        i,
                        &json!({ "communityId": id }),
                    )
                    .await?;
                }
                Ok(None)
            })
        })
        .await
        .map_err(Error::from)?;
    if let Some(v) = replay_of {
        return replay(state, ctx, acting, v).await;
    }
    crate::metrics::inc("messaging_communities_created_total", &[]);
    ensure_announcements(state, ctx, acting, id).await?;
    Ok((201, get(state, ctx, me, id).await?))
}

async fn replay(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    stored: Value,
) -> Result<(u16, Value)> {
    let id = stored["communityId"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or(Error::code(ErrorCode::InternalError))?;
    ensure_announcements(state, ctx, acting, id).await?;
    Ok((200, get(state, ctx, acting.principal_id, id).await?))
}

/// Creates the Announcements group of community `id` when it has none yet (only its owner
/// can: they own the group). A deterministic idempotency key makes a retry after a partial
/// failure reuse the same conversation.
pub async fn ensure_announcements(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
) -> Result<()> {
    let Some(c) = load_community(&state.db, ctx, id, false).await? else {
        return Ok(());
    };
    if c.announcements_conversation_id.is_some() || c.owner_principal_id != acting.principal_id {
        return Ok(());
    }
    let title: String = c.name.chars().take(120).collect();
    let request: service::CreateRequest = serde_json::from_value(json!({
        "contextClass": "GROUP",
        "securityProfile": "MANAGED",
        "managedDisclosureAcknowledged": true,
        "title": title,
    }))
    .map_err(|_| Error::code(ErrorCode::InternalError))?;
    let created = service::create(
        state,
        ctx,
        acting,
        request,
        &format!("community-announcements:{}", id.simple()),
    )
    .await?;
    let conversation = created.body["conversationId"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or(Error::code(ErrorCode::InternalError))?;
    // The current state (a replayed creation answers the body stored when it was created).
    let conv = load(&state.db, ctx, conversation, false)
        .await?
        .ok_or(Error::code(ErrorCode::InternalError))?;
    if !conv.announcement_only {
        let patch: service::PatchRequest = serde_json::from_value(json!({
            "expectedVersion": conv.version,
            "announcementOnly": true,
        }))
        .map_err(|_| Error::code(ErrorCode::InternalError))?;
        service::patch(state, ctx, Some(acting), conversation, patch).await?;
    }
    let ctx2 = ctx.clone();
    let st = state.clone();
    let owner = acting.principal_id;
    state
        .db
        .transaction::<_, (), Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let Some(c) = load_community(txn, &ctx2, id, true).await? else {
                    return Ok(());
                };
                if c.announcements_conversation_id.is_some() {
                    return Ok(());
                }
                db::exec(
                    txn,
                    "UPDATE messaging_communities SET announcements_conversation_id = $2, updated_at = $3 WHERE id = $1",
                    vec![id.into(), conversation.into(), now.into()],
                )
                .await?;
                db::exec(
                    txn,
                    "INSERT INTO messaging_community_groups (community_id, conversation_id, position, access, announcements, \
                       linked_by_principal_id, linked_at) VALUES ($1, $2, 0, 'OPEN', true, $3, $4) ON CONFLICT DO NOTHING",
                    vec![id.into(), conversation.into(), owner.into(), now.into()],
                )
                .await?;
                // Members and admins who joined meanwhile.
                #[derive(FromQueryResult)]
                struct M {
                    principal_id: Uuid,
                    role: String,
                }
                let members: Vec<M> = db::all(
                    txn,
                    "SELECT principal_id, role FROM messaging_community_members WHERE community_id = $1 \
                       AND state = 'ACTIVE' AND principal_id <> $2 ORDER BY joined_at LIMIT 5000",
                    vec![id.into(), owner.into()],
                )
                .await?;
                for m in members {
                    admit(txn, &st, &ctx2, conversation, m.principal_id, m.role == "ADMIN", true, now).await?;
                }
                Ok(())
            })
        })
        .await
        .map_err(Error::from)
}

// ----------------------------------------------------------------------------- update

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateRequest {
    #[serde(default)]
    pub expected_version: Option<i32>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, deserialize_with = "present")]
    pub description: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    pub rules: Option<Option<String>>,
    #[serde(default)]
    pub privacy: Option<String>,
    #[serde(default)]
    pub members_can_add_groups: Option<bool>,
}

fn present<'de, D, T>(d: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

/// `PATCH /v1/messaging/communities/{id}` — a community admin.
pub async fn update(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    request: UpdateRequest,
) -> Result<Value> {
    let me = acting.principal_id;
    if request.name.is_none()
        && request.description.is_none()
        && request.rules.is_none()
        && request.privacy.is_none()
        && request.members_can_add_groups.is_none()
    {
        return Err(Error::validation(
            "nothing to change: name, description, rules, privacy or membersCanAddGroups is required",
        ));
    }
    let name = match &request.name {
        Some(n) => Some(
            text(Some(n), MAX_NAME_CHARS, "name")?
                .ok_or_else(|| Error::validation("name cannot be empty"))?,
        ),
        None => None,
    };
    let description = match &request.description {
        Some(d) => Some(text(d.as_deref(), MAX_DESCRIPTION_CHARS, "description")?),
        None => None,
    };
    let rules = match &request.rules {
        Some(r) => Some(text(r.as_deref(), MAX_RULES_CHARS, "rules")?),
        None => None,
    };
    let privacy = request.privacy.as_deref().map(privacy).transpose()?;
    let ctx2 = ctx.clone();
    let st = state.clone();
    let renamed = name.clone();
    let announcements = state
        .db
        .transaction::<_, Option<Uuid>, Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let (c, m) = as_member(txn, &ctx2, id, me, true).await?;
                require_admin(&m)?;
                if request
                    .expected_version
                    .is_some_and(|v| v != c.version)
                {
                    return Err(Error::code(ErrorCode::VersionConflict));
                }
                db::exec(
                    txn,
                    "UPDATE messaging_communities SET name = COALESCE($2, name), \
                       description = CASE WHEN $3 THEN $4 ELSE description END, \
                       rules = CASE WHEN $5 THEN $6 ELSE rules END, privacy = COALESCE($7, privacy), \
                       members_can_add_groups = COALESCE($8, members_can_add_groups), version = version + 1, updated_at = $9 \
                     WHERE id = $1",
                    vec![
                        id.into(),
                        name.into(),
                        description.is_some().into(),
                        description.flatten().into(),
                        rules.is_some().into(),
                        rules.flatten().into(),
                        privacy.map(str::to_owned).into(),
                        request.members_can_add_groups.into(),
                        now.into(),
                    ],
                )
                .await?;
                if privacy == Some("INVITE_ONLY") {
                    // Only admins invite now: links of anyone else stop working.
                    db::exec(
                        txn,
                        "UPDATE messaging_community_invite_links l SET revoked_at = $2, revoked_by_principal_id = $3                           WHERE l.community_id = $1 AND l.revoked_at IS NULL AND NOT EXISTS (                             SELECT 1 FROM messaging_community_members m WHERE m.community_id = l.community_id                                AND m.principal_id = l.created_by_principal_id AND m.state = 'ACTIVE'                                AND m.role IN ('OWNER', 'ADMIN'))",
                        vec![id.into(), now.into(), me.into()],
                    )
                    .await?;
                }
                audit_action(txn, &ctx2, me, "COMMUNITY_UPDATED", id, json!({}), now).await?;
                Ok(c.announcements_conversation_id)
            })
        })
        .await
        .map_err(Error::from)?;
    // The Announcements group carries the community's name (best effort: its admins may
    // also rename it).
    if let (Some(n), Some(conversation)) = (renamed, announcements) {
        if let Ok(Some(conv)) = load(&state.db, ctx, conversation, false).await {
            if let Ok(patch) = serde_json::from_value::<service::PatchRequest>(json!({
                "expectedVersion": conv.version,
                "title": n.chars().take(120).collect::<String>(),
            })) {
                if let Err(e) = service::patch(state, ctx, Some(acting), conversation, patch).await
                {
                    tracing::debug!(error = %e, "announcements group not renamed");
                }
            }
        }
    }
    get(state, ctx, me, id).await
}

/// `DELETE /v1/messaging/communities/{id}` — the owner. Groups are unlinked (they keep
/// working on their own); the Announcements group is deleted.
pub async fn delete(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
) -> Result<Value> {
    let me = acting.principal_id;
    let ctx2 = ctx.clone();
    let st = state.clone();
    let announcements = state
        .db
        .transaction::<_, Option<Uuid>, Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let (c, m) = as_member(txn, &ctx2, id, me, true).await?;
                require_owner(&m)?;
                db::exec(
                    txn,
                    "UPDATE messaging_communities SET state = 'DELETED', deleted_at = $2, updated_at = $2, \
                       version = version + 1 WHERE id = $1",
                    vec![id.into(), now.into()],
                )
                .await?;
                db::exec(
                    txn,
                    "DELETE FROM messaging_community_groups WHERE community_id = $1",
                    vec![id.into()],
                )
                .await?;
                db::exec(
                    txn,
                    "UPDATE messaging_community_invite_links SET revoked_at = $2, revoked_by_principal_id = $3 \
                      WHERE community_id = $1 AND revoked_at IS NULL",
                    vec![id.into(), now.into(), me.into()],
                )
                .await?;
                db::exec(
                    txn,
                    "UPDATE messaging_community_invitations SET state = 'REVOKED', answered_at = $2 \
                      WHERE community_id = $1 AND state = 'PENDING'",
                    vec![id.into(), now.into()],
                )
                .await?;
                db::exec(
                    txn,
                    "UPDATE messaging_community_join_requests SET state = 'CANCELLED', decided_at = $2 \
                      WHERE community_id = $1 AND state = 'PENDING'",
                    vec![id.into(), now.into()],
                )
                .await?;
                audit_action(txn, &ctx2, me, "COMMUNITY_DELETED", id, json!({}), now).await?;
                Ok(c.announcements_conversation_id)
            })
        })
        .await
        .map_err(Error::from)?;
    if let Some(conversation) = announcements {
        if let Ok(Some(conv)) = load(&state.db, ctx, conversation, false).await {
            if conv.owner_principal_id == me && conv.lifecycle_state != "DELETED" {
                if let Ok(patch) = serde_json::from_value::<service::PatchRequest>(json!({
                    "expectedVersion": conv.version,
                    "lifecycleState": "DELETED",
                    "reason": "COMMUNITY_DELETED",
                })) {
                    if let Err(e) =
                        service::patch(state, ctx, Some(acting), conversation, patch).await
                    {
                        tracing::warn!(error = %e, "announcements group of a deleted community not deleted");
                    }
                }
            }
        }
    }
    crate::metrics::inc("messaging_communities_deleted_total", &[]);
    Ok(json!({ "communityId": id, "deleted": true }))
}

// ------------------------------------------------------------------------- membership

/// `POST /v1/messaging/communities/{id}/leave` — the owner hands the community over first.
pub async fn leave(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
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
                if m.is_owner() {
                    return Err(Error::reason(
                        ErrorCode::Conflict,
                        "OWNER_MUST_TRANSFER_OR_DELETE",
                    ));
                }
                leave_in(txn, &ctx2, &c, me, true, me, now).await?;
                audit_action(txn, &ctx2, me, "COMMUNITY_MEMBER_LEFT", id, json!({}), now).await?;
                Ok(())
            })
        })
        .await
        .map_err(Error::from)?;
    Ok(json!({ "communityId": id, "left": true }))
}

#[derive(FromQueryResult)]
struct MemberListRow {
    principal_id: Uuid,
    role: String,
    joined_at: DateTime<Utc>,
}

/// `GET /v1/messaging/communities/{id}/members` — any member; oldest first.
pub async fn members(
    state: &AppState,
    ctx: &TrustedContext,
    me: Uuid,
    id: Uuid,
    q: PageQuery,
) -> Result<Value> {
    as_member(&state.db, ctx, id, me, false).await?;
    let limit = page_size(q.limit)?;
    let (t, after) = parse_cursor(q.cursor.as_deref())?.unwrap_or((far_past(), Uuid::nil()));
    let rows: Vec<MemberListRow> = db::all(
        &state.db,
        "SELECT principal_id, role, joined_at FROM messaging_community_members \
          WHERE community_id = $1 AND state = 'ACTIVE' AND (joined_at, principal_id) > ($2, $3) \
          ORDER BY joined_at, principal_id LIMIT $4",
        vec![id.into(), t.into(), after.into(), (limit + 1).into()],
    )
    .await?;
    let more = rows.len() as i64 > limit;
    let page: Vec<&MemberListRow> = rows.iter().take(limit as usize).collect();
    let next = if more {
        page.last().map(|r| cursor(r.joined_at, r.principal_id))
    } else {
        None
    };
    Ok(json!({
        "members": page.iter().map(|r| json!({
            "principalId": r.principal_id,
            "role": r.role,
            "joinedAt": rfc3339(r.joined_at),
        })).collect::<Vec<_>>(),
        "nextCursor": next,
    }))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RoleRequest {
    pub role: String,
}

/// `PUT /v1/messaging/communities/{id}/members/{principalId}/role` — admins promote; only
/// the owner demotes another admin (an admin may step down).
pub async fn set_role(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    target: Uuid,
    request: RoleRequest,
) -> Result<Value> {
    let me = acting.principal_id;
    let role: &'static str = match request.role.as_str() {
        "ADMIN" => "ADMIN",
        "MEMBER" => "MEMBER",
        _ => return Err(Error::validation("role must be ADMIN or MEMBER")),
    };
    let ctx2 = ctx.clone();
    let st = state.clone();
    state
        .db
        .transaction::<_, (), Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let (c, m) = as_member(txn, &ctx2, id, me, true).await?;
                let t = membership_of(txn, id, target)
                    .await?
                    .filter(MemberRow::is_active)
                    .ok_or_else(not_found)?;
                if t.role == "OWNER" {
                    return Err(Error::reason(ErrorCode::Conflict, "OWNER_ROLE_FIXED"));
                }
                let stepping_down = target == me && role == "MEMBER";
                if !stepping_down {
                    require_admin(&m)?;
                    if t.role == "ADMIN" && role == "MEMBER" {
                        require_owner(&m)?;
                    }
                }
                if t.role == role {
                    return Ok(());
                }
                db::exec(
                    txn,
                    "UPDATE messaging_community_members SET role = $3, updated_at = $4 WHERE community_id = $1 AND principal_id = $2",
                    vec![id.into(), target.into(), role.into(), now.into()],
                )
                .await?;
                if let Some(ann) = c.announcements_conversation_id {
                    if let Some(conv) = load(txn, &ctx2, ann, true).await? {
                        if access::membership(txn, ann, target, false)
                            .await?
                            .is_some_and(|x| x.is_active() && x.role != "OWNER")
                        {
                            membership::set_role_in(txn, &ctx2, &conv, target, role, &request_context(), now)
                                .await?;
                        }
                    }
                }
                audit_action(
                    txn,
                    &ctx2,
                    me,
                    "COMMUNITY_ROLE_CHANGED",
                    id,
                    json!({ "member_principal_id": target, "role": role }),
                    now,
                )
                .await?;
                Ok(())
            })
        })
        .await
        .map_err(Error::from)?;
    let t = membership_of(&state.db, id, target)
        .await?
        .ok_or_else(not_found)?;
    Ok(json!({ "principalId": target, "role": t.role, "joinedAt": rfc3339(t.joined_at) }))
}

/// Whether `actor` may remove or ban `target`: admins remove MEMBERs, only the owner removes
/// admins, nobody removes the owner.
pub(crate) fn may_moderate(actor: &MemberRow, target_role: Option<&str>) -> Result<()> {
    require_admin(actor)?;
    match target_role {
        Some("OWNER") => Err(Error::reason(
            ErrorCode::Conflict,
            "OWNER_CANNOT_BE_REMOVED",
        )),
        Some("ADMIN") if !actor.is_owner() => Err(forbidden("NOT_COMMUNITY_OWNER")),
        _ => Ok(()),
    }
}

/// `DELETE /v1/messaging/communities/{id}/members/{principalId}`.
pub async fn remove(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    target: Uuid,
) -> Result<Value> {
    let me = acting.principal_id;
    if target == me {
        return Err(Error::validation("use leave to leave a community"));
    }
    let ctx2 = ctx.clone();
    let st = state.clone();
    state
        .db
        .transaction::<_, (), Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let (c, m) = as_member(txn, &ctx2, id, me, true).await?;
                let t = membership_of(txn, id, target)
                    .await?
                    .filter(MemberRow::is_active)
                    .ok_or_else(not_found)?;
                may_moderate(&m, Some(&t.role))?;
                leave_in(txn, &ctx2, &c, target, false, me, now).await?;
                audit_action(
                    txn,
                    &ctx2,
                    me,
                    "COMMUNITY_MEMBER_REMOVED",
                    id,
                    json!({ "member_principal_id": target }),
                    now,
                )
                .await?;
                Ok(())
            })
        })
        .await
        .map_err(Error::from)?;
    Ok(json!({ "principalId": target, "state": "REMOVED" }))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OwnerRequest {
    pub principal_id: Uuid,
}

/// `POST /v1/messaging/communities/{id}/owner` — the owner hands over to an ACTIVE member
/// (the previous owner becomes an admin), with the Announcements group.
pub async fn transfer(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    request: OwnerRequest,
) -> Result<Value> {
    let me = acting.principal_id;
    let to = request.principal_id;
    if to == me {
        return Err(Error::validation("you already own this community"));
    }
    let ctx2 = ctx.clone();
    let st = state.clone();
    state
        .db
        .transaction::<_, (), Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let (c, m) = as_member(txn, &ctx2, id, me, true).await?;
                require_owner(&m)?;
                membership_of(txn, id, to)
                    .await?
                    .filter(MemberRow::is_active)
                    .ok_or_else(not_found)?;
                db::exec(
                    txn,
                    "UPDATE messaging_community_members SET role = 'ADMIN', updated_at = $3 WHERE community_id = $1 AND principal_id = $2",
                    vec![id.into(), me.into(), now.into()],
                )
                .await?;
                db::exec(
                    txn,
                    "UPDATE messaging_community_members SET role = 'OWNER', updated_at = $3 WHERE community_id = $1 AND principal_id = $2",
                    vec![id.into(), to.into(), now.into()],
                )
                .await?;
                db::exec(
                    txn,
                    "UPDATE messaging_communities SET owner_principal_id = $2, version = version + 1, updated_at = $3 WHERE id = $1",
                    vec![id.into(), to.into(), now.into()],
                )
                .await?;
                if let Some(ann) = c.announcements_conversation_id {
                    if let Some(conv) = load(txn, &ctx2, ann, true).await? {
                        let from_owner = access::membership(txn, ann, me, false)
                            .await?
                            .is_some_and(|x| x.is_active() && x.role == "OWNER");
                        if !access::membership(txn, ann, to, false)
                            .await?
                            .is_some_and(|x| x.is_active())
                        {
                            admit(txn, &st, &ctx2, ann, to, true, true, now).await?;
                        }
                        let to_active = access::membership(txn, ann, to, false)
                            .await?
                            .is_some_and(|x| x.is_active());
                        if from_owner && to_active {
                            membership::transfer_in(txn, &ctx2, &conv, me, to, &request_context(), now)
                                .await?;
                        }
                    }
                }
                audit_action(
                    txn,
                    &ctx2,
                    me,
                    "COMMUNITY_OWNERSHIP_TRANSFERRED",
                    id,
                    json!({ "to_principal_id": to }),
                    now,
                )
                .await?;
                Ok(())
            })
        })
        .await
        .map_err(Error::from)?;
    get(state, ctx, me, id).await
}

// ------------------------------------------------------------------------ preferences

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Preferences {
    pub chat_list_mode: String,
}

/// `GET /v1/messaging/communities/preferences`.
pub async fn preferences(state: &AppState, ctx: &TrustedContext, me: Uuid) -> Result<Value> {
    #[derive(FromQueryResult)]
    struct Row {
        chat_list_mode: String,
    }
    let row: Option<Row> = db::one(
        &state.db,
        "SELECT chat_list_mode FROM messaging_community_preferences WHERE environment_id = $1 AND principal_id = $2",
        vec![ctx.scope.environment_id.into(), me.into()],
    )
    .await?;
    Ok(
        json!({ "chatListMode": row.map(|r| r.chat_list_mode).unwrap_or_else(|| "TOGETHER".into()) }),
    )
}

/// `PUT /v1/messaging/communities/preferences`.
pub async fn set_preferences(
    state: &AppState,
    ctx: &TrustedContext,
    me: Uuid,
    request: Preferences,
) -> Result<Value> {
    let mode = match request.chat_list_mode.as_str() {
        "TOGETHER" => "TOGETHER",
        "SEPARATE" => "SEPARATE",
        _ => {
            return Err(Error::validation(
                "chatListMode must be TOGETHER or SEPARATE",
            ))
        }
    };
    db::exec(
        &state.db,
        "INSERT INTO messaging_community_preferences (environment_id, principal_id, chat_list_mode, updated_at) \
         VALUES ($1, $2, $3, $4) ON CONFLICT (environment_id, principal_id) DO UPDATE SET chat_list_mode = $3, updated_at = $4",
        vec![
            ctx.scope.environment_id.into(),
            me.into(),
            mode.into(),
            state.now().into(),
        ],
    )
    .await?;
    Ok(json!({ "chatListMode": mode }))
}
