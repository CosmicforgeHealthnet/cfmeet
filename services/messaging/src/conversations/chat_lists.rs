//! A member's own chat lists (WhatsApp custom lists such as "Family" or "Work").
//!
//! | Route (under `/v1/messaging`) | Scope |
//! |---|---|
//! | `GET /chat-lists` | `messages.read` |
//! | `POST /chat-lists` | `messages.read` |
//! | `PATCH, DELETE /chat-lists/{listId}` | `messages.read` |
//! | `PUT, DELETE /chat-lists/{listId}/conversations/{conversationId}` | `messages.read` |
//!
//! Per principal (in the environment) and never visible to anyone else: at most
//! [`MAX_CHAT_LISTS`] lists, each with at most [`MAX_CHAT_LIST_ITEMS`] conversations; a
//! conversation may be in several lists. Only conversations the principal is an ACTIVE
//! member of can be added, and lists show only those (a conversation the principal left
//! drops out of every list without anything being deleted). Removing an item, or deleting
//! a list, never touches the conversation or its membership. The order of the lists is the
//! principal's own (`position`); items keep the order they were added in.

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, patch, put},
    Json, Router,
};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::service::rfc3339;
use crate::{
    db,
    error::{Error, ErrorCode, Result},
    http::{
        error::ApiError,
        middleware::{Acting, CustomerCaller},
    },
    identity::TrustedContext,
    messages::http::acting_with,
    state::AppState,
};

/// Most chat lists one principal may have.
pub const MAX_CHAT_LISTS: i64 = 20;
/// Most conversations one chat list may hold.
pub const MAX_CHAT_LIST_ITEMS: i64 = 500;
/// Longest chat list name (characters).
pub const MAX_CHAT_LIST_NAME: usize = 40;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/chat-lists", get(list_lists).post(create_list))
        .route(
            "/chat-lists/:list_id",
            patch(update_list).delete(delete_list),
        )
        .route(
            "/chat-lists/:list_id/conversations/:conversation_id",
            put(add_item).delete(remove_item),
        )
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateChatListRequest {
    pub name: String,
    #[serde(default)]
    pub conversation_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateChatListRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub position: Option<i64>,
}

async fn list_lists(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
) -> std::result::Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, super::http::MESSAGES_READ).await?;
    Ok(Json(list(&state, caller.context(), &acting).await?))
}

async fn create_list(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Json(body): Json<CreateChatListRequest>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, super::http::MESSAGES_READ).await?;
    Ok((
        StatusCode::CREATED,
        Json(create(&state, caller.context(), &acting, body).await?),
    ))
}

async fn update_list(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(list_id): Path<Uuid>,
    Json(body): Json<UpdateChatListRequest>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, super::http::MESSAGES_READ).await?;
    Ok(Json(
        update(&state, caller.context(), &acting, list_id, body).await?,
    ))
}

async fn delete_list(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(list_id): Path<Uuid>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, super::http::MESSAGES_READ).await?;
    delete(&state, caller.context(), &acting, list_id).await?;
    Ok(Json(json!({ "listId": list_id, "deleted": true })))
}

async fn add_item(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((list_id, conversation_id)): Path<(Uuid, Uuid)>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, super::http::MESSAGES_READ).await?;
    Ok(Json(
        set_item(
            &state,
            caller.context(),
            &acting,
            list_id,
            conversation_id,
            true,
        )
        .await?,
    ))
}

async fn remove_item(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((list_id, conversation_id)): Path<(Uuid, Uuid)>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, super::http::MESSAGES_READ).await?;
    Ok(Json(
        set_item(
            &state,
            caller.context(),
            &acting,
            list_id,
            conversation_id,
            false,
        )
        .await?,
    ))
}

#[derive(Debug, Clone, FromQueryResult)]
struct ListRow {
    id: Uuid,
    name: String,
    position: i32,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromQueryResult)]
struct ItemRow {
    list_id: Uuid,
    conversation_id: Uuid,
}

/// A trimmed name of 1-40 characters without control characters.
pub fn valid_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty()
        || name.chars().count() > MAX_CHAT_LIST_NAME
        || name.chars().any(char::is_control)
    {
        return Err(Error::validation(
            "name must be 1-40 characters without control characters",
        ));
    }
    Ok(name.to_string())
}

/// Serializes one principal's list changes (caps and names are checked, then written).
async fn lock_owner<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    principal: Uuid,
) -> Result<()> {
    db::exec(
        db,
        "SELECT pg_advisory_xact_lock(hashtextextended('messaging-chat-lists:' || $1::text || ':' || $2::text, 0))",
        vec![ctx.scope.environment_id.into(), principal.into()],
    )
    .await?;
    Ok(())
}

async fn owned<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    principal: Uuid,
    list_id: Uuid,
) -> Result<ListRow> {
    db::one::<ListRow, _>(
        db,
        "SELECT id, name, position, created_at, updated_at FROM messaging_chat_lists \
          WHERE id = $1 AND environment_id = $2 AND principal_id = $3",
        vec![
            list_id.into(),
            ctx.scope.environment_id.into(),
            principal.into(),
        ],
    )
    .await?
    .ok_or(Error::code(ErrorCode::NotFound))
}

async fn name_taken<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    principal: Uuid,
    name: &str,
    except: Option<Uuid>,
) -> Result<bool> {
    Ok(db::scalar_i64(
        db,
        "SELECT count(*) FROM messaging_chat_lists WHERE environment_id = $1 AND principal_id = $2 \
           AND lower(name) = lower($3) AND ($4::uuid IS NULL OR id <> $4)",
        vec![
            ctx.scope.environment_id.into(),
            principal.into(),
            name.into(),
            except.into(),
        ],
    )
    .await?
        > 0)
}

/// The conversation ids of `lists` that `principal` is still an ACTIVE member of.
async fn items_of<C: ConnectionTrait>(
    db: &C,
    principal: Uuid,
    lists: &[Uuid],
) -> Result<Vec<ItemRow>> {
    if lists.is_empty() {
        return Ok(Vec::new());
    }
    Ok(db::all(
        db,
        "SELECT i.list_id, i.conversation_id FROM messaging_chat_list_items i \
           JOIN messaging_conversation_members m ON m.conversation_id = i.conversation_id \
            AND m.principal_id = $2 AND m.state = 'ACTIVE' \
          WHERE i.list_id = ANY($1) ORDER BY i.added_at, i.conversation_id",
        vec![lists.to_vec().into(), principal.into()],
    )
    .await?)
}

fn view(row: &ListRow, items: &[ItemRow]) -> Value {
    json!({
        "listId": row.id,
        "name": row.name,
        "position": row.position,
        "conversationIds": items.iter().filter(|i| i.list_id == row.id).map(|i| i.conversation_id).collect::<Vec<_>>(),
        "createdAt": rfc3339(row.created_at),
        "updatedAt": rfc3339(row.updated_at),
    })
}

async fn one_view<C: ConnectionTrait>(db: &C, principal: Uuid, row: &ListRow) -> Result<Value> {
    let items = items_of(db, principal, &[row.id]).await?;
    Ok(view(row, &items))
}

async fn rows<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    principal: Uuid,
) -> Result<Vec<ListRow>> {
    Ok(db::all(
        db,
        "SELECT id, name, position, created_at, updated_at FROM messaging_chat_lists \
          WHERE environment_id = $1 AND principal_id = $2 ORDER BY position, created_at, id",
        vec![ctx.scope.environment_id.into(), principal.into()],
    )
    .await?)
}

/// `GET /v1/messaging/chat-lists`: the acting principal's lists, in its order.
pub async fn list(state: &AppState, ctx: &TrustedContext, acting: &Acting) -> Result<Value> {
    let lists = rows(&state.db, ctx, acting.principal_id).await?;
    let ids: Vec<Uuid> = lists.iter().map(|l| l.id).collect();
    let items = items_of(&state.db, acting.principal_id, &ids).await?;
    Ok(json!({
        "chatLists": lists.iter().map(|l| view(l, &items)).collect::<Vec<_>>(),
        "maxChatLists": MAX_CHAT_LISTS,
        "maxConversationsPerList": MAX_CHAT_LIST_ITEMS,
    }))
}

/// Every conversation must be one the principal is an ACTIVE member of (else 404, so a
/// list never probes which conversations exist).
async fn require_member_of_all<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    principal: Uuid,
    conversations: &[Uuid],
) -> Result<()> {
    if conversations.is_empty() {
        return Ok(());
    }
    let found = db::scalar_i64(
        db,
        "SELECT count(DISTINCT c.id) FROM messaging_conversations c \
           JOIN messaging_conversation_members m ON m.conversation_id = c.id \
            AND m.principal_id = $2 AND m.state = 'ACTIVE' \
          WHERE c.id = ANY($1) AND c.tenant_id = $3 AND c.application_id = $4 AND c.environment_id = $5",
        vec![
            conversations.to_vec().into(),
            principal.into(),
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            ctx.scope.environment_id.into(),
        ],
    )
    .await?;
    if found != conversations.len() as i64 {
        return Err(Error::code(ErrorCode::NotFound));
    }
    Ok(())
}

/// `POST /v1/messaging/chat-lists`: a new list at the end, optionally with conversations.
pub async fn create(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    request: CreateChatListRequest,
) -> Result<Value> {
    let name = valid_name(&request.name)?;
    let mut conversations = request.conversation_ids;
    conversations.sort();
    conversations.dedup();
    if conversations.len() as i64 > MAX_CHAT_LIST_ITEMS {
        return Err(Error::reason(ErrorCode::Conflict, "CHAT_LIST_FULL"));
    }
    let principal = acting.principal_id;
    let now = state.now();
    let txn = state.db.begin().await.map_err(Error::from)?;
    lock_owner(&txn, ctx, principal).await?;
    let count = db::scalar_i64(
        &txn,
        "SELECT count(*) FROM messaging_chat_lists WHERE environment_id = $1 AND principal_id = $2",
        vec![ctx.scope.environment_id.into(), principal.into()],
    )
    .await?;
    if count >= MAX_CHAT_LISTS {
        return Err(Error::reason(
            ErrorCode::Conflict,
            "CHAT_LIST_LIMIT_REACHED",
        ));
    }
    if name_taken(&txn, ctx, principal, &name, None).await? {
        return Err(Error::reason(ErrorCode::Conflict, "CHAT_LIST_NAME_TAKEN"));
    }
    require_member_of_all(&txn, ctx, principal, &conversations).await?;
    let id = Uuid::new_v4();
    db::exec(
        &txn,
        "INSERT INTO messaging_chat_lists (id, environment_id, principal_id, name, position, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, \
                 (SELECT COALESCE(max(position) + 1, 0) FROM messaging_chat_lists WHERE environment_id = $2 AND principal_id = $3), \
                 $5, $5)",
        vec![
            id.into(),
            ctx.scope.environment_id.into(),
            principal.into(),
            name.into(),
            now.into(),
        ],
    )
    .await?;
    if !conversations.is_empty() {
        db::exec(
            &txn,
            "INSERT INTO messaging_chat_list_items (list_id, conversation_id, added_at) \
             SELECT $1, c, $3 FROM unnest($2::uuid[]) AS c",
            vec![id.into(), conversations.into(), now.into()],
        )
        .await?;
    }
    let row = owned(&txn, ctx, principal, id).await?;
    let out = one_view(&txn, principal, &row).await?;
    txn.commit().await.map_err(Error::from)?;
    crate::metrics::inc("messaging_chat_list_changes_total", &[]);
    Ok(out)
}

/// `PATCH /v1/messaging/chat-lists/{listId}`: rename and / or move the list to `position`
/// (0-based; beyond the end moves it last). The other lists keep their relative order.
pub async fn update(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    list_id: Uuid,
    request: UpdateChatListRequest,
) -> Result<Value> {
    if request.name.is_none() && request.position.is_none() {
        return Err(Error::validation(
            "nothing to change: name or position is required",
        ));
    }
    let name = request.name.as_deref().map(valid_name).transpose()?;
    if request.position.is_some_and(|p| p < 0) {
        return Err(Error::validation("position must be 0 or greater"));
    }
    let principal = acting.principal_id;
    let now = state.now();
    let txn = state.db.begin().await.map_err(Error::from)?;
    lock_owner(&txn, ctx, principal).await?;
    owned(&txn, ctx, principal, list_id).await?;
    if let Some(name) = &name {
        if name_taken(&txn, ctx, principal, name, Some(list_id)).await? {
            return Err(Error::reason(ErrorCode::Conflict, "CHAT_LIST_NAME_TAKEN"));
        }
        db::exec(
            &txn,
            "UPDATE messaging_chat_lists SET name = $2, updated_at = $3 WHERE id = $1",
            vec![list_id.into(), name.clone().into(), now.into()],
        )
        .await?;
    }
    if let Some(target) = request.position {
        let mut order: Vec<Uuid> = rows(&txn, ctx, principal)
            .await?
            .into_iter()
            .map(|l| l.id)
            .filter(|id| *id != list_id)
            .collect();
        let at = (target as usize).min(order.len());
        order.insert(at, list_id);
        // Renumbered 0..n in one statement (positions stay dense and unique).
        db::exec(
            &txn,
            "UPDATE messaging_chat_lists l SET position = o.ord - 1, \
                    updated_at = CASE WHEN l.id = $2 THEN $3 ELSE l.updated_at END \
               FROM unnest($1::uuid[]) WITH ORDINALITY AS o(id, ord) WHERE l.id = o.id",
            vec![order.into(), list_id.into(), now.into()],
        )
        .await?;
    }
    let row = owned(&txn, ctx, principal, list_id).await?;
    let out = one_view(&txn, principal, &row).await?;
    txn.commit().await.map_err(Error::from)?;
    crate::metrics::inc("messaging_chat_list_changes_total", &[]);
    Ok(out)
}

/// `DELETE /v1/messaging/chat-lists/{listId}`: the list and its items (never the
/// conversations).
pub async fn delete(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    list_id: Uuid,
) -> Result<()> {
    let principal = acting.principal_id;
    let txn = state.db.begin().await.map_err(Error::from)?;
    lock_owner(&txn, ctx, principal).await?;
    owned(&txn, ctx, principal, list_id).await?;
    db::exec(
        &txn,
        "DELETE FROM messaging_chat_lists WHERE id = $1",
        vec![list_id.into()],
    )
    .await?;
    // Positions stay dense.
    db::exec(
        &txn,
        "UPDATE messaging_chat_lists l SET position = o.ord - 1 FROM ( \
             SELECT id, row_number() OVER (ORDER BY position, created_at, id) AS ord FROM messaging_chat_lists \
              WHERE environment_id = $1 AND principal_id = $2) o \
          WHERE l.id = o.id AND l.position <> o.ord - 1",
        vec![ctx.scope.environment_id.into(), principal.into()],
    )
    .await?;
    txn.commit().await.map_err(Error::from)?;
    crate::metrics::inc("messaging_chat_list_changes_total", &[]);
    Ok(())
}

/// `PUT` (add: true) / `DELETE` (add: false)
/// `/v1/messaging/chat-lists/{listId}/conversations/{conversationId}` (idempotent).
/// Adding needs an ACTIVE membership; removing never touches the conversation.
pub async fn set_item(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    list_id: Uuid,
    conversation_id: Uuid,
    add: bool,
) -> Result<Value> {
    let principal = acting.principal_id;
    let now = state.now();
    let txn = state.db.begin().await.map_err(Error::from)?;
    lock_owner(&txn, ctx, principal).await?;
    let row = owned(&txn, ctx, principal, list_id).await?;
    if add {
        require_member_of_all(&txn, ctx, principal, &[conversation_id]).await?;
        let present = db::scalar_i64(
            &txn,
            "SELECT count(*) FROM messaging_chat_list_items WHERE list_id = $1 AND conversation_id = $2",
            vec![list_id.into(), conversation_id.into()],
        )
        .await?;
        if present == 0 {
            let size = db::scalar_i64(
                &txn,
                "SELECT count(*) FROM messaging_chat_list_items WHERE list_id = $1",
                vec![list_id.into()],
            )
            .await?;
            if size >= MAX_CHAT_LIST_ITEMS {
                return Err(Error::reason(ErrorCode::Conflict, "CHAT_LIST_FULL"));
            }
            db::exec(
                &txn,
                "INSERT INTO messaging_chat_list_items (list_id, conversation_id, added_at) VALUES ($1, $2, $3)",
                vec![list_id.into(), conversation_id.into(), now.into()],
            )
            .await?;
        }
    } else {
        db::exec(
            &txn,
            "DELETE FROM messaging_chat_list_items WHERE list_id = $1 AND conversation_id = $2",
            vec![list_id.into(), conversation_id.into()],
        )
        .await?;
    }
    let out = one_view(&txn, principal, &row).await?;
    txn.commit().await.map_err(Error::from)?;
    crate::metrics::inc("messaging_chat_list_changes_total", &[]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_names_are_bounded_and_trimmed() {
        assert_eq!(valid_name("  Family ").unwrap(), "Family");
        assert_eq!(valid_name(&"é".repeat(40)).unwrap().chars().count(), 40);
        for bad in ["", "   ", "a\u{0007}b", &"x".repeat(41)] {
            assert!(valid_name(bad).is_err(), "{bad:?}");
        }
    }
}
