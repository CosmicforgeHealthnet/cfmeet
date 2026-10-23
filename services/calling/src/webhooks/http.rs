//! Customer webhook API (`/v1/webhooks…`, `/v1/webhook-deliveries…`; Part G §68).
//!
//! Scope comes only from the authenticated credential: a subscription of another tenant,
//! application or environment is 404. Reads need `webhooks.read`, changes
//! `webhooks.manage`, and every subscribed event type needs its own read scope
//! (e.g. `calls.read` for `call.*`). Mutations of an existing subscription carry
//! `expectedVersion`. The signing secret is returned only by create / rotate-secret.
//!
//! Part VI §53 webhook migration: while `CALLING_WEBHOOK_MUTATIONS_FROZEN` is set, every
//! mutation (create, update, delete, rotate-secret, replay) answers 503
//! `SERVICE_UNAVAILABLE` with `CF-Webhook-Migration: frozen` and `Retry-After` (reads and
//! deliveries continue); with `CALLING_WEBHOOK_AUTHORITY=retired` every route answers 503
//! `SERVICE_UNAVAILABLE` with `CF-Webhook-Migration: moved` (the Developer Platform owns
//! customer webhooks). The scope checks run first: an unauthorized caller learns nothing.

use axum::{
    extract::{rejection::JsonRejection, rejection::QueryRejection, Path, Query, State},
    http::StatusCode,
    response::Response,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;

use crate::{
    calls::{
        app::http::{parse_body, respond, uuid_path},
        command::CallScope,
    },
    enterprise::{cursor, http::QueryParams},
    error::CallingError,
    platform::{context::PlatformAuthContext, scopes::names},
    state::AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/webhooks", post(create).get(list))
        .route(
            "/v1/webhooks/:webhook_id",
            get(get_one).patch(update).delete(delete),
        )
        .route("/v1/webhooks/:webhook_id/rotate-secret", post(rotate))
        .route("/v1/webhooks/:webhook_id/deliveries", get(deliveries))
        .route("/v1/webhook-deliveries/:delivery_id/replay", post(replay))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateBody {
    url: String,
    event_types: Vec<String>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateBody {
    expected_version: i64,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    event_types: Option<Vec<String>>,
    #[serde(default, with = "double_option")]
    description: Option<Option<String>>,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VersionBody {
    expected_version: i64,
}

/// `null` clears, absence keeps.
mod double_option {
    use serde::{Deserialize, Deserializer};

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<Option<String>>, D::Error> {
        Ok(Some(Option::<String>::deserialize(d)?))
    }
}

/// A keyset position (creation time, id).
type Cursor = Option<(chrono::DateTime<chrono::Utc>, uuid::Uuid)>;

fn page(q: &QueryParams) -> Result<(u64, Cursor), CallingError> {
    let limit = match q.get("limit") {
        None => 50,
        Some(v) => match v.parse::<u64>() {
            Ok(l) if (1..=100).contains(&l) => l,
            _ => {
                return Err(CallingError::Validation(
                    "limit must be an integer between 1 and 100.",
                ))
            }
        },
    };
    let cursor = q
        .get("cursor")
        .map(|c| cursor::decode(c).map_err(|_| CallingError::Validation("cursor is invalid.")))
        .transpose()?;
    Ok((limit, cursor))
}

async fn create(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    body: Result<Json<CreateBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::WEBHOOKS_MANAGE)?;
    if let Some(refused) = state.webhooks.migration_refusal(true) {
        return Ok(refused);
    }
    let body = parse_body(body)?;
    let created = state
        .webhooks
        .create(&ctx, &body.url, &body.event_types, body.description)
        .await?;
    Ok(respond(StatusCode::CREATED, created, false))
}

async fn list(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::WEBHOOKS_READ)?;
    if let Some(refused) = state.webhooks.migration_refusal(false) {
        return Ok(refused);
    }
    let q = QueryParams::parse(query, &["limit", "cursor"])?;
    let (limit, cursor) = page(&q)?;
    let list = state
        .webhooks
        .list(CallScope::of_platform(&ctx), limit, cursor)
        .await?;
    Ok(respond(StatusCode::OK, list, false))
}

async fn get_one(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::WEBHOOKS_READ)?;
    if let Some(refused) = state.webhooks.migration_refusal(false) {
        return Ok(refused);
    }
    let view = state
        .webhooks
        .get(CallScope::of_platform(&ctx), uuid_path(&id)?)
        .await?;
    Ok(respond(StatusCode::OK, view, false))
}

async fn update(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
    body: Result<Json<UpdateBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::WEBHOOKS_MANAGE)?;
    if let Some(refused) = state.webhooks.migration_refusal(true) {
        return Ok(refused);
    }
    let id = uuid_path(&id)?;
    let b = parse_body(body)?;
    // Part H §72: a suspended scope may only DISABLE a subscription here — never move
    // it to another endpoint, change its event types or re-enable it.
    if ctx.is_safety_only()
        && (b.url.is_some()
            || b.event_types.is_some()
            || b.description.is_some()
            || b.status.as_deref() != Some("DISABLED"))
    {
        return Err(CallingError::AuthenticationFailed);
    }
    let view = state
        .webhooks
        .update(
            &ctx,
            id,
            b.expected_version,
            super::WebhookChange {
                url: b.url,
                event_types: b.event_types,
                description: b.description,
                status: b.status,
            },
        )
        .await?;
    Ok(respond(StatusCode::OK, view, false))
}

async fn delete(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::WEBHOOKS_MANAGE)?;
    if let Some(refused) = state.webhooks.migration_refusal(true) {
        return Ok(refused);
    }
    let id = uuid_path(&id)?;
    let q = QueryParams::parse(query, &["expectedVersion"])?;
    let version = q
        .get("expectedVersion")
        .and_then(|v| v.parse::<i64>().ok())
        .ok_or(CallingError::Validation("expectedVersion is required."))?;
    let view = state.webhooks.delete(&ctx, id, version).await?;
    Ok(respond(StatusCode::OK, view, false))
}

async fn rotate(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
    body: Result<Json<VersionBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::WEBHOOKS_MANAGE)?;
    if let Some(refused) = state.webhooks.migration_refusal(true) {
        return Ok(refused);
    }
    let id = uuid_path(&id)?;
    let b = parse_body(body)?;
    let rotated = state
        .webhooks
        .rotate_secret(&ctx, id, b.expected_version)
        .await?;
    Ok(respond(StatusCode::OK, rotated, false))
}

async fn deliveries(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::WEBHOOKS_READ)?;
    if let Some(refused) = state.webhooks.migration_refusal(false) {
        return Ok(refused);
    }
    let id = uuid_path(&id)?;
    let q = QueryParams::parse(query, &["limit", "cursor", "status"])?;
    let (limit, cursor) = page(&q)?;
    let list = state
        .webhooks
        .deliveries(
            CallScope::of_platform(&ctx),
            id,
            q.get("status"),
            limit,
            cursor,
        )
        .await?;
    Ok(respond(StatusCode::OK, list, false))
}

async fn replay(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(id): Path<String>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::WEBHOOKS_MANAGE)?;
    if let Some(refused) = state.webhooks.migration_refusal(true) {
        return Ok(refused);
    }
    let view = state.webhooks.replay(&ctx, uuid_path(&id)?).await?;
    Ok(respond(StatusCode::CREATED, view, false))
}
