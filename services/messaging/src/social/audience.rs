//! Status audiences: the remembered default, close friends, named lists, mutes and the
//! eligible contacts of the audience picker. All server-side per principal (every device
//! sees the same), all restricted to MUTUAL, unblocked contacts.
//!
//! A status's audience is RESOLVED once, at publish time, into a snapshot
//! ([`resolve`]); editing the default or a list afterwards never changes who may see an
//! earlier status.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{clean_text, eligible_contacts, not_found};
use crate::{
    conversations::service::rfc3339,
    db,
    error::{Error, ErrorCode, Result},
    http::middleware::Acting,
    identity::TrustedContext,
    state::AppState,
};

/// Principals one EXCEPT / ONLY choice or list may name.
pub const MAX_CHOSEN: usize = 1000;
/// Named lists per principal (close friends not counted).
pub const MAX_LISTS: i64 = 20;
pub const MAX_LIST_NAME: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AudienceMode {
    Contacts,
    ContactsExcept,
    Only,
    CloseFriends,
    Custom,
}

impl AudienceMode {
    pub fn as_str(self) -> &'static str {
        match self {
            AudienceMode::Contacts => "CONTACTS",
            AudienceMode::ContactsExcept => "CONTACTS_EXCEPT",
            AudienceMode::Only => "ONLY",
            AudienceMode::CloseFriends => "CLOSE_FRIENDS",
            AudienceMode::Custom => "CUSTOM",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "CONTACTS_EXCEPT" => AudienceMode::ContactsExcept,
            "ONLY" => AudienceMode::Only,
            "CLOSE_FRIENDS" => AudienceMode::CloseFriends,
            "CUSTOM" => AudienceMode::Custom,
            _ => AudienceMode::Contacts,
        }
    }
}

/// Who a status is for.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AudienceChoice {
    pub mode: AudienceMode,
    /// CONTACTS_EXCEPT: the excluded; ONLY: the included.
    #[serde(default)]
    pub principal_ids: Vec<Uuid>,
    /// CUSTOM: one of the caller's named lists.
    #[serde(default)]
    pub list_id: Option<Uuid>,
}

impl Default for AudienceChoice {
    fn default() -> Self {
        Self {
            mode: AudienceMode::Contacts,
            principal_ids: vec![],
            list_id: None,
        }
    }
}

/// A resolved audience, stored on each status.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub mode: String,
    pub list_id: Option<Uuid>,
    pub label: Option<String>,
    pub principals: Vec<Uuid>,
    pub excluded: i32,
    /// The latest `eligibleSince` among the selected contacts: a relationship that
    /// (re)starts later never sees this status.
    pub as_of: Option<DateTime<Utc>>,
}

#[derive(FromQueryResult)]
struct SettingsRow {
    mode: String,
    principals: Vec<Uuid>,
    list_id: Option<Uuid>,
    allow_sharing: bool,
}

/// The remembered default audience and sharing permission (CONTACTS, sharing allowed).
pub async fn preference<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    me: Uuid,
) -> Result<(AudienceChoice, bool)> {
    let row: Option<SettingsRow> = db::one(
        db,
        "SELECT mode, principals, list_id, allow_sharing FROM messaging_status_audience_settings \
          WHERE environment_id = $1 AND principal_id = $2",
        vec![environment_id.into(), me.into()],
    )
    .await?;
    Ok(match row {
        None => (AudienceChoice::default(), true),
        Some(r) => (
            AudienceChoice {
                mode: AudienceMode::parse(&r.mode),
                principal_ids: r.principals,
                list_id: r.list_id,
            },
            r.allow_sharing,
        ),
    })
}

#[derive(Debug, Clone, FromQueryResult)]
struct ListRow {
    id: Uuid,
    kind: String,
    name: String,
    members: Vec<Uuid>,
    updated_at: DateTime<Utc>,
}

async fn list<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    me: Uuid,
    id: Uuid,
) -> Result<ListRow> {
    db::one(
        db,
        "SELECT id, kind, name, members, updated_at FROM messaging_status_audience_lists \
          WHERE id = $1 AND environment_id = $2 AND principal_id = $3",
        vec![id.into(), environment_id.into(), me.into()],
    )
    .await?
    .ok_or_else(not_found)
}

async fn close_friends<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    me: Uuid,
) -> Result<Option<ListRow>> {
    Ok(db::one(
        db,
        "SELECT id, kind, name, members, updated_at FROM messaging_status_audience_lists \
          WHERE environment_id = $1 AND principal_id = $2 AND kind = 'CLOSE_FRIENDS'",
        vec![environment_id.into(), me.into()],
    )
    .await?)
}

fn bounded(ids: &[Uuid]) -> Result<Vec<Uuid>> {
    if ids.len() > MAX_CHOSEN {
        return Err(Error::validation("At most 1000 people"));
    }
    let mut v = ids.to_vec();
    v.sort();
    v.dedup();
    Ok(v)
}

/// Resolves `choice` against the author's CURRENT eligible contacts into a snapshot.
pub async fn resolve(
    state: &AppState,
    ctx: &TrustedContext,
    me: Uuid,
    choice: &AudienceChoice,
) -> Result<Snapshot> {
    let env = ctx.scope.environment_id;
    let chosen = bounded(&choice.principal_ids)?;
    let contacts = eligible_contacts(state, ctx, me, None).await?;
    let (selected, excluded, label, list_id): (Vec<Uuid>, i32, String, Option<Uuid>) =
        match choice.mode {
            AudienceMode::Contacts => (
                contacts.keys().copied().collect(),
                0,
                "My contacts".into(),
                None,
            ),
            AudienceMode::ContactsExcept => {
                let out: HashSet<Uuid> = chosen.iter().copied().collect();
                let selected: Vec<Uuid> = contacts
                    .keys()
                    .copied()
                    .filter(|c| !out.contains(c))
                    .collect();
                let excluded = (contacts.len() - selected.len()) as i32;
                (
                    selected,
                    excluded,
                    "My contacts except\u{2026}".into(),
                    None,
                )
            }
            AudienceMode::Only => (
                chosen
                    .iter()
                    .copied()
                    .filter(|c| contacts.contains_key(c))
                    .collect(),
                0,
                "Only share with\u{2026}".into(),
                None,
            ),
            AudienceMode::CloseFriends => {
                let members = close_friends(&state.db, env, me)
                    .await?
                    .map(|l| l.members)
                    .unwrap_or_default();
                (
                    members
                        .into_iter()
                        .filter(|c| contacts.contains_key(c))
                        .collect(),
                    0,
                    "Close friends".into(),
                    None,
                )
            }
            AudienceMode::Custom => {
                let id = choice
                    .list_id
                    .ok_or_else(|| Error::validation("A CUSTOM audience needs listId"))?;
                let l = list(&state.db, env, me, id).await?;
                if l.kind != "CUSTOM" {
                    return Err(Error::validation("listId must be a named list"));
                }
                (
                    l.members
                        .into_iter()
                        .filter(|c| contacts.contains_key(c))
                        .collect(),
                    0,
                    l.name,
                    Some(l.id),
                )
            }
        };
    let mut principals = selected;
    principals.sort();
    principals.truncate(5000);
    let as_of = principals
        .iter()
        .filter_map(|p| contacts.get(p))
        .max()
        .copied();
    Ok(Snapshot {
        mode: choice.mode.as_str().into(),
        list_id,
        label: Some(label),
        principals,
        excluded,
        as_of,
    })
}

// --------------------------------------------------------------------- preferences API

fn preference_view(choice: &AudienceChoice, allow_sharing: bool) -> Value {
    json!({
        "mode": choice.mode.as_str(),
        "principalIds": choice.principal_ids,
        "listId": choice.list_id,
        "allowSharing": allow_sharing,
    })
}

pub async fn get_preference(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
) -> Result<Value> {
    let (c, s) = preference(&state.db, ctx.scope.environment_id, acting.principal_id).await?;
    Ok(preference_view(&c, s))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreferenceRequest {
    pub mode: AudienceMode,
    #[serde(default)]
    pub principal_ids: Vec<Uuid>,
    #[serde(default)]
    pub list_id: Option<Uuid>,
    #[serde(default)]
    pub allow_sharing: Option<bool>,
}

/// Saves the default audience (future statuses only). Named principals are kept only when
/// they are eligible contacts now.
pub async fn put_preference(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    req: PreferenceRequest,
) -> Result<Value> {
    let env = ctx.scope.environment_id;
    let me = acting.principal_id;
    let chosen = bounded(&req.principal_ids)?;
    let principals = match req.mode {
        AudienceMode::ContactsExcept | AudienceMode::Only if !chosen.is_empty() => {
            let eligible = eligible_contacts(state, ctx, me, Some(&chosen)).await?;
            chosen
                .into_iter()
                .filter(|p| eligible.contains_key(p))
                .collect()
        }
        _ => vec![],
    };
    let list_id = match req.mode {
        AudienceMode::Custom => {
            let id = req
                .list_id
                .ok_or_else(|| Error::validation("A CUSTOM audience needs listId"))?;
            let l = list(&state.db, env, me, id).await?;
            if l.kind != "CUSTOM" {
                return Err(Error::validation("listId must be a named list"));
            }
            Some(id)
        }
        _ => None,
    };
    let (_, current_sharing) = preference(&state.db, env, me).await?;
    let sharing = req.allow_sharing.unwrap_or(current_sharing);
    db::exec(
        &state.db,
        "INSERT INTO messaging_status_audience_settings (environment_id, principal_id, mode, principals, list_id, allow_sharing, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT (environment_id, principal_id) DO UPDATE SET mode = EXCLUDED.mode, \
           principals = EXCLUDED.principals, list_id = EXCLUDED.list_id, allow_sharing = EXCLUDED.allow_sharing, updated_at = EXCLUDED.updated_at",
        vec![
            env.into(),
            me.into(),
            req.mode.as_str().into(),
            principals.clone().into(),
            list_id.into(),
            sharing.into(),
            state.now().into(),
        ],
    )
    .await?;
    Ok(preference_view(
        &AudienceChoice {
            mode: req.mode,
            principal_ids: principals,
            list_id,
        },
        sharing,
    ))
}

/// The audience picker: the caller's eligible (mutual, unblocked) contacts.
pub async fn eligible(state: &AppState, ctx: &TrustedContext, acting: &Acting) -> Result<Value> {
    let contacts = eligible_contacts(state, ctx, acting.principal_id, None).await?;
    let mut ids: Vec<Uuid> = contacts.into_keys().collect();
    ids.sort();
    Ok(
        json!({ "contacts": ids.into_iter().map(|p| json!({ "principalId": p })).collect::<Vec<_>>() }),
    )
}

// ----------------------------------------------------------------------------- lists

fn list_view(l: &ListRow) -> Value {
    json!({
        "listId": l.id,
        "kind": l.kind,
        "name": l.name,
        "memberPrincipalIds": l.members,
        "memberCount": l.members.len(),
        "updatedAt": rfc3339(l.updated_at),
    })
}

async fn eligible_members(
    state: &AppState,
    ctx: &TrustedContext,
    me: Uuid,
    ids: &[Uuid],
) -> Result<Vec<Uuid>> {
    let chosen = bounded(ids)?;
    if chosen.is_empty() {
        return Ok(chosen);
    }
    let eligible = eligible_contacts(state, ctx, me, Some(&chosen)).await?;
    Ok(chosen
        .into_iter()
        .filter(|p| eligible.contains_key(p))
        .collect())
}

pub async fn lists(state: &AppState, ctx: &TrustedContext, acting: &Acting) -> Result<Value> {
    let rows: Vec<ListRow> = db::all(
        &state.db,
        "SELECT id, kind, name, members, updated_at FROM messaging_status_audience_lists \
          WHERE environment_id = $1 AND principal_id = $2 ORDER BY kind, created_at",
        vec![ctx.scope.environment_id.into(), acting.principal_id.into()],
    )
    .await?;
    Ok(json!({ "lists": rows.iter().map(list_view).collect::<Vec<_>>() }))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub member_principal_ids: Option<Vec<Uuid>>,
}

pub async fn create_list(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    req: ListRequest,
) -> Result<Value> {
    let env = ctx.scope.environment_id;
    let me = acting.principal_id;
    let name = clean_text(&req.name, MAX_LIST_NAME, "A list name")?
        .ok_or_else(|| Error::validation("Give the list a name"))?;
    let n = db::scalar_i64(
        &state.db,
        "SELECT count(*) FROM messaging_status_audience_lists WHERE environment_id = $1 AND principal_id = $2 AND kind = 'CUSTOM'",
        vec![env.into(), me.into()],
    )
    .await?;
    if n >= MAX_LISTS {
        return Err(Error::reason(ErrorCode::QuotaExceeded, "TOO_MANY_LISTS"));
    }
    let members = eligible_members(
        state,
        ctx,
        me,
        &req.member_principal_ids.unwrap_or_default(),
    )
    .await?;
    let id = Uuid::new_v4();
    let now = state.now();
    db::exec(
        &state.db,
        "INSERT INTO messaging_status_audience_lists (id, environment_id, principal_id, kind, name, members, created_at, updated_at) \
         VALUES ($1, $2, $3, 'CUSTOM', $4, $5, $6, $6)",
        vec![id.into(), env.into(), me.into(), name.into(), members.into(), now.into()],
    )
    .await?;
    Ok(list_view(&list(&state.db, env, me, id).await?))
}

pub async fn update_list(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    req: ListRequest,
) -> Result<Value> {
    let env = ctx.scope.environment_id;
    let me = acting.principal_id;
    let current = list(&state.db, env, me, id).await?;
    if current.kind != "CUSTOM" {
        return Err(not_found());
    }
    let name = match &req.name {
        Some(_) => clean_text(&req.name, MAX_LIST_NAME, "A list name")?
            .ok_or_else(|| Error::validation("Give the list a name"))?,
        None => current.name,
    };
    let members = match &req.member_principal_ids {
        Some(m) => eligible_members(state, ctx, me, m).await?,
        None => current.members,
    };
    db::exec(
        &state.db,
        "UPDATE messaging_status_audience_lists SET name = $2, members = $3, updated_at = $4 WHERE id = $1",
        vec![id.into(), name.into(), members.into(), state.now().into()],
    )
    .await?;
    Ok(list_view(&list(&state.db, env, me, id).await?))
}

pub async fn delete_list(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
) -> Result<Value> {
    let env = ctx.scope.environment_id;
    let me = acting.principal_id;
    let current = list(&state.db, env, me, id).await?;
    if current.kind != "CUSTOM" {
        return Err(not_found());
    }
    db::exec(
        &state.db,
        "DELETE FROM messaging_status_audience_lists WHERE id = $1",
        vec![id.into()],
    )
    .await?;
    // A default that pointed at the list falls back to CONTACTS.
    db::exec(
        &state.db,
        "UPDATE messaging_status_audience_settings SET mode = 'CONTACTS', list_id = NULL, updated_at = $3 \
          WHERE environment_id = $1 AND principal_id = $2 AND list_id IS NOT DISTINCT FROM $4",
        vec![env.into(), me.into(), state.now().into(), Some(id).into()],
    )
    .await?;
    Ok(json!({ "listId": id, "deleted": true }))
}

pub async fn get_close_friends(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
) -> Result<Value> {
    let l = close_friends(&state.db, ctx.scope.environment_id, acting.principal_id).await?;
    Ok(json!({
        "memberPrincipalIds": l.as_ref().map(|l| l.members.clone()).unwrap_or_default(),
        "updatedAt": l.map(|l| rfc3339(l.updated_at)),
    }))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CloseFriendsRequest {
    pub member_principal_ids: Vec<Uuid>,
}

pub async fn put_close_friends(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    req: CloseFriendsRequest,
) -> Result<Value> {
    let env = ctx.scope.environment_id;
    let me = acting.principal_id;
    let members = eligible_members(state, ctx, me, &req.member_principal_ids).await?;
    let now = state.now();
    db::exec(
        &state.db,
        "INSERT INTO messaging_status_audience_lists (id, environment_id, principal_id, kind, name, members, created_at, updated_at) \
         VALUES ($1, $2, $3, 'CLOSE_FRIENDS', 'Close friends', $4, $5, $5) \
         ON CONFLICT (environment_id, principal_id) WHERE kind = 'CLOSE_FRIENDS' DO UPDATE SET members = EXCLUDED.members, updated_at = EXCLUDED.updated_at",
        vec![Uuid::new_v4().into(), env.into(), me.into(), members.clone().into(), now.into()],
    )
    .await?;
    Ok(json!({ "memberPrincipalIds": members, "updatedAt": rfc3339(now) }))
}

// ----------------------------------------------------------------------------- mutes

#[derive(FromQueryResult)]
struct MuteRow {
    muted_principal_id: Uuid,
}

pub async fn muted_set<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    me: Uuid,
) -> Result<HashSet<Uuid>> {
    Ok(db::all::<MuteRow, _>(
        db,
        "SELECT muted_principal_id FROM messaging_status_mutes WHERE environment_id = $1 AND principal_id = $2",
        vec![environment_id.into(), me.into()],
    )
    .await?
    .into_iter()
    .map(|m| m.muted_principal_id)
    .collect())
}

pub async fn mutes(state: &AppState, ctx: &TrustedContext, acting: &Acting) -> Result<Value> {
    let mut ids: Vec<Uuid> = muted_set(&state.db, ctx.scope.environment_id, acting.principal_id)
        .await?
        .into_iter()
        .collect();
    ids.sort();
    Ok(json!({ "mutedPrincipalIds": ids }))
}

/// Mute / unmute a contact's statuses (the other person is never told; chats and calls
/// are unaffected).
pub async fn set_mute(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    other: Uuid,
    on: bool,
) -> Result<Value> {
    let me = acting.principal_id;
    if other == me {
        return Err(Error::validation("You cannot mute yourself"));
    }
    if on {
        db::exec(
            &state.db,
            "INSERT INTO messaging_status_mutes (environment_id, principal_id, muted_principal_id, created_at) \
             VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
            vec![ctx.scope.environment_id.into(), me.into(), other.into(), state.now().into()],
        )
        .await?;
    } else {
        db::exec(
            &state.db,
            "DELETE FROM messaging_status_mutes WHERE environment_id = $1 AND principal_id = $2 AND muted_principal_id = $3",
            vec![ctx.scope.environment_id.into(), me.into(), other.into()],
        )
        .await?;
    }
    Ok(json!({ "principalId": other, "muted": on }))
}
