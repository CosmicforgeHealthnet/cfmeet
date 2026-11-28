//! The groups of a community (ADR 0073 decision 3-4).
//!
//! * Listing shows every live group to members: title, description and photo reference;
//!   member counts only to the group's own members (a member list is never shown here).
//! * A group is linked by one of ITS admins who may also add groups to the community
//!   (a community admin, or any member when `membersCanAddGroups`), or created inside the
//!   community (a MANAGED group the creator owns). Unlinking (a community admin or the
//!   group's own admins) never touches the conversation.
//! * Joining an `OPEN` group admits the member at once; a `RESTRICTED` group takes a join
//!   request its admins (or a community admin) decide. Either way the admission is the
//!   ordinary one with a NEW history window: nothing earlier becomes visible.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    admit, as_member, audit_action, forbidden, invites, not_found, text, CommunityRow, MemberRow,
    LIVE_STATES, MAX_GROUPS, MAX_MESSAGE_CHARS,
};
use crate::{
    conversations::{
        access,
        service::{self, load, rfc3339},
    },
    db,
    error::{Error, ErrorCode, Result},
    http::middleware::Acting,
    identity::TrustedContext,
    state::AppState,
};

#[derive(Debug, Clone, FromQueryResult)]
struct GroupRow {
    conversation_id: Uuid,
    position: i32,
    access: String,
    announcements: bool,
    linked_at: DateTime<Utc>,
    title: Option<String>,
    description: Option<String>,
    avatar_ref: Option<String>,
    security_profile: String,
    my_role: Option<String>,
    member_count: i64,
    pending_request: bool,
}

const GROUP_SELECT: &str = "SELECT g.conversation_id, g.position, g.access, g.announcements, g.linked_at, \
       c.title, c.description, c.avatar_ref, c.security_profile, m.role AS my_role, \
       (SELECT count(*) FROM messaging_conversation_members x WHERE x.conversation_id = g.conversation_id \
          AND x.state = 'ACTIVE') AS member_count, \
       EXISTS (SELECT 1 FROM messaging_community_join_requests r WHERE r.community_id = g.community_id \
          AND r.conversation_id = g.conversation_id AND r.principal_id = $2 AND r.state = 'PENDING') AS pending_request \
     FROM messaging_community_groups g JOIN messaging_conversations c ON c.id = g.conversation_id \
     LEFT JOIN messaging_conversation_members m ON m.conversation_id = g.conversation_id \
          AND m.principal_id = $2 AND m.state = 'ACTIVE'";

fn group_view(g: &GroupRow, community_admin: bool) -> Value {
    let joined = g.my_role.is_some();
    let group_admin = matches!(g.my_role.as_deref(), Some("OWNER" | "ADMIN"));
    json!({
        "conversationId": g.conversation_id,
        "title": g.title,
        "description": g.description,
        "avatarRef": g.avatar_ref,
        "securityProfile": g.security_profile,
        "access": g.access,
        "position": g.position,
        "announcements": g.announcements,
        "joined": joined,
        "memberCount": if joined { Some(g.member_count) } else { None },
        "myRole": g.my_role,
        "canManage": community_admin || group_admin,
        "pendingRequest": g.pending_request,
        "linkedAt": rfc3339(g.linked_at),
    })
}

async fn rows<C: ConnectionTrait>(db: &C, community: Uuid, me: Uuid) -> Result<Vec<GroupRow>> {
    Ok(db::all(
        db,
        &format!(
            "{GROUP_SELECT} WHERE g.community_id = $1 AND c.lifecycle_state IN {LIVE_STATES} \
             ORDER BY g.announcements DESC, g.position, g.linked_at, g.conversation_id"
        ),
        vec![community.into(), me.into()],
    )
    .await?)
}

async fn row<C: ConnectionTrait>(
    db: &C,
    community: Uuid,
    me: Uuid,
    conversation: Uuid,
) -> Result<GroupRow> {
    db::one(
        db,
        &format!(
            "{GROUP_SELECT} WHERE g.community_id = $1 AND g.conversation_id = $3 AND c.lifecycle_state IN {LIVE_STATES}"
        ),
        vec![community.into(), me.into(), conversation.into()],
    )
    .await?
    .ok_or_else(not_found)
}

/// `GET /v1/messaging/communities/{id}/groups` — any member.
pub async fn list(state: &AppState, ctx: &TrustedContext, me: Uuid, id: Uuid) -> Result<Value> {
    let (_, m) = as_member(&state.db, ctx, id, me, false).await?;
    let groups = rows(&state.db, id, me).await?;
    Ok(json!({ "groups": groups.iter().map(|g| group_view(g, m.is_admin())).collect::<Vec<_>>() }))
}

fn access_value(value: Option<&str>) -> Result<&'static str> {
    match value {
        None | Some("OPEN") => Ok("OPEN"),
        Some("RESTRICTED") => Ok("RESTRICTED"),
        Some(_) => Err(Error::validation("access must be OPEN or RESTRICTED")),
    }
}

fn may_add_groups(c: &CommunityRow, m: &MemberRow) -> Result<()> {
    if m.is_admin() || c.members_can_add_groups {
        Ok(())
    } else {
        Err(forbidden("NOT_COMMUNITY_ADMIN"))
    }
}

/// Whether `me` administers the group conversation (its OWNER / ADMIN).
async fn group_admin<C: ConnectionTrait>(db: &C, conversation: Uuid, me: Uuid) -> Result<bool> {
    Ok(access::membership(db, conversation, me, false)
        .await?
        .is_some_and(|m| m.is_admin()))
}

/// Links `conversation` inside the caller's transaction (community row locked).
async fn link_in<C: ConnectionTrait>(
    txn: &C,
    ctx: &TrustedContext,
    community: &CommunityRow,
    conversation: Uuid,
    access: &str,
    me: Uuid,
    now: DateTime<Utc>,
) -> Result<()> {
    let count = db::scalar_i64(
        txn,
        "SELECT count(*) FROM messaging_community_groups WHERE community_id = $1",
        vec![community.id.into()],
    )
    .await?;
    if count >= MAX_GROUPS {
        return Err(Error::reason(ErrorCode::Conflict, "TOO_MANY_GROUPS"));
    }
    let taken = db::scalar_i64(
        txn,
        "SELECT count(*) FROM messaging_community_groups WHERE conversation_id = $1",
        vec![conversation.into()],
    )
    .await?;
    if taken > 0 {
        return Err(Error::reason(
            ErrorCode::Conflict,
            "GROUP_ALREADY_IN_A_COMMUNITY",
        ));
    }
    let position = db::scalar_i64(
        txn,
        "SELECT COALESCE(max(position), 0) + 1 FROM messaging_community_groups WHERE community_id = $1",
        vec![community.id.into()],
    )
    .await?;
    db::exec(
        txn,
        "INSERT INTO messaging_community_groups (community_id, conversation_id, position, access, announcements, \
           linked_by_principal_id, linked_at) VALUES ($1, $2, $3, $4, false, $5, $6)",
        vec![
            community.id.into(),
            conversation.into(),
            (position as i32).into(),
            access.into(),
            me.into(),
            now.into(),
        ],
    )
    .await?;
    audit_action(
        txn,
        ctx,
        me,
        "COMMUNITY_GROUP_LINKED",
        community.id,
        json!({ "conversation_id": conversation, "access": access }),
        now,
    )
    .await
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkRequest {
    pub conversation_id: Uuid,
    #[serde(default)]
    pub access: Option<String>,
}

/// `POST /v1/messaging/communities/{id}/groups/link` — an admin of the group who may add
/// groups to the community. Nothing about the group's members or content changes.
pub async fn link(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    request: LinkRequest,
) -> Result<Value> {
    let me = acting.principal_id;
    let access = access_value(request.access.as_deref())?;
    let conversation = request.conversation_id;
    let ctx2 = ctx.clone();
    let st = state.clone();
    let admin = state
        .db
        .transaction::<_, bool, Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let (c, m) = as_member(txn, &ctx2, id, me, true).await?;
                may_add_groups(&c, &m)?;
                // The group: in scope, a GROUP, accepting, and the caller one of its admins
                // (anything else is indistinguishable from a group that does not exist).
                let conv = load(txn, &ctx2, conversation, true)
                    .await?
                    .ok_or_else(not_found)?;
                let mine = access::membership(txn, conv.id, me, false).await?;
                match mine {
                    Some(x) if x.is_admin() => {}
                    Some(x) if x.is_active() => {
                        return Err(forbidden("NOT_GROUP_ADMIN"));
                    }
                    _ => return Err(not_found()),
                }
                if conv.context_class != "GROUP" {
                    return Err(Error::reason(ErrorCode::InvalidTransition, "GROUPS_ONLY"));
                }
                access::require_accepting(&conv, now)?;
                link_in(txn, &ctx2, &c, conv.id, access, me, now).await?;
                Ok(m.is_admin())
            })
        })
        .await
        .map_err(Error::from)?;
    let g = row(&state.db, id, me, conversation).await?;
    Ok(group_view(&g, admin))
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateGroupRequest {
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub access: Option<String>,
}

/// `POST /v1/messaging/communities/{id}/groups` — a new MANAGED group the caller owns,
/// linked to the community. `Idempotency-Key` makes a retry return the same group.
pub async fn create(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    request: CreateGroupRequest,
    key: Option<String>,
) -> Result<(u16, Value)> {
    let me = acting.principal_id;
    let access = access_value(request.access.as_deref())?;
    let title = text(Some(&request.title), 120, "title")?
        .ok_or_else(|| Error::validation("title is required"))?;
    let description = text(
        request.description.as_deref(),
        service::MAX_DESCRIPTION_CHARS,
        "description",
    )?;
    {
        let (c, m) = as_member(&state.db, ctx, id, me, false).await?;
        may_add_groups(&c, &m)?;
        let count = db::scalar_i64(
            &state.db,
            "SELECT count(*) FROM messaging_community_groups WHERE community_id = $1",
            vec![id.into()],
        )
        .await?;
        if count >= MAX_GROUPS {
            return Err(Error::reason(ErrorCode::Conflict, "TOO_MANY_GROUPS"));
        }
    }
    let key = match key {
        Some(k) => format!("community-group:{}:{k}", id.simple()),
        None => format!("community-group:{}", Uuid::new_v4().simple()),
    };
    let mut body = json!({
        "contextClass": "GROUP",
        "securityProfile": "MANAGED",
        "managedDisclosureAcknowledged": true,
        "title": title,
    });
    if let Some(d) = description {
        body["description"] = json!(d);
    }
    let req: service::CreateRequest =
        serde_json::from_value(body).map_err(|_| Error::code(ErrorCode::InternalError))?;
    let key: String = key.chars().take(128).collect();
    let created = service::create(state, ctx, acting, req, &key).await?;
    let conversation = created.body["conversationId"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or(Error::code(ErrorCode::InternalError))?;
    let ctx2 = ctx.clone();
    let st = state.clone();
    let (status, admin) = state
        .db
        .transaction::<_, (u16, bool), Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let (c, m) = as_member(txn, &ctx2, id, me, true).await?;
                let linked: Option<Uuid> = db::one::<LinkedRow, _>(
                    txn,
                    "SELECT community_id FROM messaging_community_groups WHERE conversation_id = $1",
                    vec![conversation.into()],
                )
                .await?
                .map(|r| r.community_id);
                match linked {
                    Some(other) if other == id => Ok((200, m.is_admin())),
                    Some(_) => Err(Error::reason(ErrorCode::Conflict, "GROUP_ALREADY_IN_A_COMMUNITY")),
                    None => {
                        may_add_groups(&c, &m)?;
                        link_in(txn, &ctx2, &c, conversation, access, me, now).await?;
                        Ok((201, m.is_admin()))
                    }
                }
            })
        })
        .await
        .map_err(Error::from)?;
    let g = row(&state.db, id, me, conversation).await?;
    Ok((status, group_view(&g, admin)))
}

#[derive(FromQueryResult)]
struct LinkedRow {
    community_id: Uuid,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateGroupRequest {
    pub access: String,
}

/// The community, the caller's membership and whether they may manage `conversation`
/// (a community admin or the group's admin), inside a transaction. NOT_FOUND when the group
/// is not one of the community's.
async fn managed<C: ConnectionTrait>(
    txn: &C,
    ctx: &TrustedContext,
    id: Uuid,
    me: Uuid,
    conversation: Uuid,
) -> Result<(CommunityRow, MemberRow, bool)> {
    let (c, m) = as_member(txn, ctx, id, me, true).await?;
    let linked = db::scalar_i64(
        txn,
        "SELECT count(*) FROM messaging_community_groups WHERE community_id = $1 AND conversation_id = $2",
        vec![id.into(), conversation.into()],
    )
    .await?;
    if linked == 0 {
        return Err(not_found());
    }
    let announcements = c.announcements_conversation_id == Some(conversation);
    if !(m.is_admin() || group_admin(txn, conversation, me).await?) {
        return Err(forbidden("NOT_GROUP_ADMIN"));
    }
    Ok((c, m, announcements))
}

/// `PATCH /v1/messaging/communities/{id}/groups/{conversationId}` `{ access }`.
pub async fn update(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    conversation: Uuid,
    request: UpdateGroupRequest,
) -> Result<Value> {
    let me = acting.principal_id;
    let access = access_value(Some(&request.access))?;
    let ctx2 = ctx.clone();
    let st = state.clone();
    let admin = state
        .db
        .transaction::<_, bool, Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let (_, m, announcements) = managed(txn, &ctx2, id, me, conversation).await?;
                if announcements && access != "OPEN" {
                    return Err(Error::reason(ErrorCode::Conflict, "ANNOUNCEMENTS_ALWAYS_OPEN"));
                }
                db::exec(
                    txn,
                    "UPDATE messaging_community_groups SET access = $3 WHERE community_id = $1 AND conversation_id = $2",
                    vec![id.into(), conversation.into(), access.into()],
                )
                .await?;
                audit_action(
                    txn,
                    &ctx2,
                    me,
                    "COMMUNITY_GROUP_UPDATED",
                    id,
                    json!({ "conversation_id": conversation, "access": access }),
                    now,
                )
                .await?;
                Ok(m.is_admin())
            })
        })
        .await
        .map_err(Error::from)?;
    let g = row(&state.db, id, me, conversation).await?;
    Ok(group_view(&g, admin))
}

/// `DELETE /v1/messaging/communities/{id}/groups/{conversationId}` — unlink (the group keeps
/// its members and content). The Announcements group cannot be unlinked.
pub async fn unlink(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    conversation: Uuid,
) -> Result<Value> {
    let me = acting.principal_id;
    let ctx2 = ctx.clone();
    let st = state.clone();
    state
        .db
        .transaction::<_, (), Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let (_, _, announcements) = managed(txn, &ctx2, id, me, conversation).await?;
                if announcements {
                    return Err(Error::reason(ErrorCode::Conflict, "ANNOUNCEMENTS_CANNOT_BE_UNLINKED"));
                }
                db::exec(
                    txn,
                    "DELETE FROM messaging_community_groups WHERE community_id = $1 AND conversation_id = $2",
                    vec![id.into(), conversation.into()],
                )
                .await?;
                db::exec(
                    txn,
                    "UPDATE messaging_community_join_requests SET state = 'CANCELLED', decided_at = $3 \
                      WHERE community_id = $1 AND conversation_id = $2 AND state = 'PENDING'",
                    vec![id.into(), conversation.into(), now.into()],
                )
                .await?;
                audit_action(
                    txn,
                    &ctx2,
                    me,
                    "COMMUNITY_GROUP_UNLINKED",
                    id,
                    json!({ "conversation_id": conversation }),
                    now,
                )
                .await
            })
        })
        .await
        .map_err(Error::from)?;
    Ok(json!({ "conversationId": conversation, "unlinked": true }))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OrderRequest {
    pub conversation_ids: Vec<Uuid>,
}

/// `PUT /v1/messaging/communities/{id}/groups/order` — a community admin; the listed groups
/// come first in that order (the Announcements group always leads).
pub async fn reorder(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    request: OrderRequest,
) -> Result<Value> {
    let me = acting.principal_id;
    if request.conversation_ids.len() as i64 > MAX_GROUPS {
        return Err(Error::validation(format!(
            "conversationIds can list at most {MAX_GROUPS} groups"
        )));
    }
    let mut seen = std::collections::HashSet::new();
    if !request.conversation_ids.iter().all(|c| seen.insert(*c)) {
        return Err(Error::validation("conversationIds lists a group twice"));
    }
    let ctx2 = ctx.clone();
    let st = state.clone();
    state
        .db
        .transaction::<_, (), Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let (_, m) = as_member(txn, &ctx2, id, me, true).await?;
                super::require_admin(&m)?;
                for (i, conversation) in request.conversation_ids.iter().enumerate() {
                    let n = db::exec(
                        txn,
                        "UPDATE messaging_community_groups SET position = $3 WHERE community_id = $1 AND conversation_id = $2 \
                           AND NOT announcements",
                        vec![id.into(), (*conversation).into(), (i as i32 + 1).into()],
                    )
                    .await?;
                    if n == 0 {
                        let ann = db::scalar_i64(
                            txn,
                            "SELECT count(*) FROM messaging_community_groups WHERE community_id = $1 AND conversation_id = $2",
                            vec![id.into(), (*conversation).into()],
                        )
                        .await?;
                        if ann == 0 {
                            return Err(Error::validation("conversationIds lists a group of another community"));
                        }
                    }
                }
                // Groups not listed keep their relative order after the listed ones.
                db::exec(
                    txn,
                    "UPDATE messaging_community_groups g SET position = o.rn + $3 FROM ( \
                       SELECT conversation_id, row_number() OVER (ORDER BY position, linked_at) AS rn \
                         FROM messaging_community_groups WHERE community_id = $1 AND NOT announcements \
                           AND NOT (conversation_id = ANY($2))) o \
                     WHERE g.community_id = $1 AND g.conversation_id = o.conversation_id",
                    vec![
                        id.into(),
                        request.conversation_ids.clone().into(),
                        (request.conversation_ids.len() as i32).into(),
                    ],
                )
                .await?;
                audit_action(txn, &ctx2, me, "COMMUNITY_GROUPS_REORDERED", id, json!({}), now).await
            })
        })
        .await
        .map_err(Error::from)?;
    list(state, ctx, me, id).await
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JoinGroupRequest {
    #[serde(default)]
    pub message: Option<String>,
}

/// `POST /v1/messaging/communities/{id}/groups/{conversationId}/join` — a member joins an
/// OPEN group (or any group, as a community admin) at once, else asks to join.
pub async fn join(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    conversation: Uuid,
    request: JoinGroupRequest,
) -> Result<(u16, Value)> {
    let me = acting.principal_id;
    let message = text(request.message.as_deref(), MAX_MESSAGE_CHARS, "message")?;
    super::rate_limit(
        state,
        "group-join",
        ctx,
        me,
        crate::limits::Window::Hour,
        invites::REQUESTS_PER_HOUR,
    )
    .await?;
    let ctx2 = ctx.clone();
    let st = state.clone();
    let outcome = state
        .db
        .transaction::<_, (u16, Option<Uuid>, bool), Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let (c, m) = as_member(txn, &ctx2, id, me, true).await?;
                let g = row(txn, id, me, conversation).await?;
                if g.my_role.is_some() {
                    return Ok((200, None, m.is_admin()));
                }
                if g.access == "OPEN" || m.is_admin() {
                    admit(txn, &st, &ctx2, conversation, me, false, false, now).await?;
                    audit_action(
                        txn,
                        &ctx2,
                        me,
                        "COMMUNITY_GROUP_JOINED",
                        id,
                        json!({ "conversation_id": conversation }),
                        now,
                    )
                    .await?;
                    return Ok((201, None, m.is_admin()));
                }
                let request = invites::request_in(
                    txn,
                    &st,
                    &ctx2,
                    &c,
                    me,
                    Some(conversation),
                    message,
                    "GROUP",
                    now,
                )
                .await?;
                Ok((202, Some(request), m.is_admin()))
            })
        })
        .await
        .map_err(Error::from)?;
    match outcome {
        (202, Some(request), _) => Ok((
            202,
            json!({ "outcome": "REQUESTED", "request": invites::request_view(&state.db, request).await? }),
        )),
        (status, _, admin) => {
            let g = row(&state.db, id, me, conversation).await?;
            Ok((
                status,
                json!({
                    "outcome": if status == 201 { "JOINED" } else { "ALREADY_MEMBER" },
                    "group": group_view(&g, admin),
                }),
            ))
        }
    }
}

/// Whether `me` may decide a request for `conversation` (`None`: membership of the
/// community): community admins always; a group's own admins for that group.
pub(crate) async fn may_decide<C: ConnectionTrait>(
    db: &C,
    m: &MemberRow,
    conversation: Option<Uuid>,
) -> Result<bool> {
    if m.is_admin() {
        return Ok(true);
    }
    match conversation {
        Some(c) => group_admin(db, c, m.principal_id).await,
        None => Ok(false),
    }
}
