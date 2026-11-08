//! Legacy meeting webhook handlers — DEPRECATED, never delivered.
//!
//! Blueprint v1.0 Section 16 (PLT-003 / DATA-007). These routes store registrations,
//! but no code reads the `webhooks` table to deliver anything, and none will be added:
//! meeting events for integrators go through the Developer Platform webhook pipeline
//! (`/v1/environments/{environmentId}/webhooks`, signed delivery, SSRF policy, replay).
//! Personal accounts have no Developer Platform environment, so delivering these rows
//! through that pipeline would need a second, invented ownership model.
//!
//! Until the routes are removed (D-9: measured zero consumers), every answer says so:
//! `delivery: NOT_DELIVERED` on each item, a `notice` on each wrapper, and the
//! `middleware::deprecation::legacy_webhooks` layer (`Deprecation`, `Sunset`,
//! `Link rel="deprecation"`) on every response, errors included. See
//! `docs/api/legacy-meeting-webhooks-deprecation.md`.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Extension, Json,
};
use rand::Rng;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};
use uuid::Uuid;
use validator::Validate;

use crate::{
    dto::{
        CreateWebhookApiResponse, CreateWebhookRequest, CreateWebhookResponse, MessageResponse,
        UpdateWebhookRequest, WebhookApiResponse, WebhookListResponse, WebhookResponse,
    },
    error::{ApiError, ApiResult},
    middleware::AuthContext,
    models::webhooks::{self, Entity as Webhooks, WebhookEventType, WebhookStatus},
    services::legacy_credentials,
    state::AppState,
    utils::now_utc,
};

/// Item marker: legacy meeting webhooks are never delivered.
pub const DELIVERY_NOT_DELIVERED: &str = "NOT_DELIVERED";

/// Wrapper notice on every legacy webhook response.
pub const DEPRECATION_NOTICE: &str = "Deprecated: legacy meeting webhooks are stored but never \
     delivered; no event will be sent to this endpoint. Use Developer Platform webhooks \
     (/v1/environments/{environmentId}/webhooks) for signed meeting event delivery.";

/// Generate a secure random webhook secret
fn generate_webhook_secret() -> String {
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::thread_rng();
    (0..32)
        .map(|_| {
            let idx = rng.gen_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}

/// Parse event type string to enum
fn parse_event_type(event_type: &str) -> Result<WebhookEventType, ApiError> {
    match event_type {
        "meeting_start" => Ok(WebhookEventType::MeetingStart),
        "meeting_end" => Ok(WebhookEventType::MeetingEnd),
        "participant_join" => Ok(WebhookEventType::ParticipantJoin),
        "participant_leave" => Ok(WebhookEventType::ParticipantLeave),
        _ => Err(ApiError::BadRequest(format!(
            "Invalid event type: {}. Valid types: meeting_start, meeting_end, participant_join, participant_leave",
            event_type
        ))),
    }
}

/// Parse status string to enum
fn parse_status(status: &str) -> Result<WebhookStatus, ApiError> {
    match status {
        "active" => Ok(WebhookStatus::Active),
        "inactive" => Ok(WebhookStatus::Inactive),
        _ => Err(ApiError::BadRequest(format!(
            "Invalid status: {}. Valid statuses: active, inactive",
            status
        ))),
    }
}

/// Format event type enum to string
fn format_event_type(event_type: &WebhookEventType) -> String {
    match event_type {
        WebhookEventType::MeetingStart => "meeting_start".to_string(),
        WebhookEventType::MeetingEnd => "meeting_end".to_string(),
        WebhookEventType::ParticipantJoin => "participant_join".to_string(),
        WebhookEventType::ParticipantLeave => "participant_leave".to_string(),
    }
}

/// Format status enum to string
fn format_status(status: &WebhookStatus) -> String {
    match status {
        WebhookStatus::Active => "active".to_string(),
        WebhookStatus::Inactive => "inactive".to_string(),
    }
}

/// Format datetime to ISO 8601 UTC string
fn format_datetime(dt: chrono::NaiveDateTime) -> String {
    format!("{}Z", dt.format("%Y-%m-%dT%H:%M:%S"))
}

/// Register a legacy meeting webhook (deprecated: never delivered)
///
/// DEPRECATED (Blueprint v1.0 Section 16, PLT-003). Stores a registration for one event type
/// (`meeting_start`, `meeting_end`, `participant_join` or `participant_leave`), owned by the
/// authenticated user. **No event is ever delivered to the endpoint**: the response says so
/// (`delivery: NOT_DELIVERED`, `notice`) and carries `Deprecation`, `Sunset` and
/// `Link rel="deprecation"` headers. Use Developer Platform webhooks
/// (`/v1/environments/{environmentId}/webhooks`) for signed meeting event delivery. The
/// generated secret is returned only here: the service keeps only a one-way fingerprint of
/// it, so it can never be shown again.
#[utoipa::path(
    post,
    path = "/api/v1/webhooks",
    tag = "Webhooks",
    request_body = CreateWebhookRequest,
    responses(
        (status = 201, description = "Registration stored; nothing will be delivered (deprecated)", body = CreateWebhookApiResponse),
        (status = 400, description = "Validation error"),
        (status = 401, description = "Unauthorized")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
#[deprecated(
    note = "legacy meeting webhooks are never delivered; use Developer Platform webhooks (PLT-003)"
)]
pub async fn create_webhook(
    State(state): State<AppState>,
    Extension(auth_context): Extension<AuthContext>,
    Json(payload): Json<CreateWebhookRequest>,
) -> ApiResult<(StatusCode, Json<CreateWebhookApiResponse>)> {
    payload
        .validate()
        .map_err(|e| ApiError::ValidationError(e.to_string()))?;

    let event_type = parse_event_type(&payload.event_type)?;
    let secret = generate_webhook_secret();
    let now = now_utc();

    let webhook = webhooks::ActiveModel {
        id: Set(Uuid::new_v4()),
        user_id: Set(auth_context.user_id),
        event_type: Set(event_type.clone()),
        endpoint_url: Set(payload.endpoint_url.clone()),
        // DATA-003: legacy webhooks are never delivered (DATA-007), so the secret is never
        // needed again: keep only a fingerprint, never the secret itself.
        secret: Set(legacy_credentials::WEBHOOK_SECRET_PLACEHOLDER.to_owned()),
        secret_sha256: Set(Some(legacy_credentials::webhook_secret_fingerprint(
            &secret,
        ))),
        status: Set(WebhookStatus::Active),
        created_at: Set(now),
        updated_at: Set(now),
    };

    let webhook = webhook.insert(&state.db).await?;

    let response = CreateWebhookResponse {
        id: webhook.id,
        user_id: webhook.user_id,
        event_type: format_event_type(&webhook.event_type),
        endpoint_url: webhook.endpoint_url,
        secret,
        status: format_status(&webhook.status),
        delivery: DELIVERY_NOT_DELIVERED.to_string(),
        created_at: format_datetime(webhook.created_at),
        updated_at: format_datetime(webhook.updated_at),
    };

    Ok((
        StatusCode::CREATED,
        Json(CreateWebhookApiResponse {
            success: true,
            notice: DEPRECATION_NOTICE.to_string(),
            data: response,
        }),
    ))
}

/// List legacy meeting webhooks (deprecated: never delivered)
///
/// DEPRECATED (PLT-003). Returns the authenticated user's stored registrations, active and
/// inactive, newest first, each marked `delivery: NOT_DELIVERED`. Secrets are not included. Not
/// paginated. Nothing registered here is ever delivered; use Developer Platform webhooks.
#[utoipa::path(
    get,
    path = "/api/v1/webhooks",
    tag = "Webhooks",
    responses(
        (status = 200, description = "Stored registrations (none is delivered)", body = WebhookListResponse),
        (status = 401, description = "Unauthorized")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
#[deprecated(
    note = "legacy meeting webhooks are never delivered; use Developer Platform webhooks (PLT-003)"
)]
pub async fn list_webhooks(
    State(state): State<AppState>,
    Extension(auth_context): Extension<AuthContext>,
) -> ApiResult<Json<WebhookListResponse>> {
    let webhooks_list = Webhooks::find()
        .filter(webhooks::Column::UserId.eq(auth_context.user_id))
        .order_by_desc(webhooks::Column::CreatedAt)
        .all(&state.db)
        .await?;

    let data: Vec<WebhookResponse> = webhooks_list
        .into_iter()
        .map(|w| WebhookResponse {
            id: w.id,
            user_id: w.user_id,
            event_type: format_event_type(&w.event_type),
            endpoint_url: w.endpoint_url,
            status: format_status(&w.status),
            delivery: DELIVERY_NOT_DELIVERED.to_string(),
            created_at: format_datetime(w.created_at),
            updated_at: format_datetime(w.updated_at),
        })
        .collect();

    Ok(Json(WebhookListResponse {
        success: true,
        notice: DEPRECATION_NOTICE.to_string(),
        data,
    }))
}

/// Update a legacy meeting webhook (deprecated: never delivered)
///
/// DEPRECATED (PLT-003). Changes the endpoint URL and/or status (`active` or `inactive`) of a
/// stored registration owned by the authenticated user; omitted fields are left unchanged. Even
/// `active` registrations are never delivered (`delivery: NOT_DELIVERED`). Returns 404 if the
/// webhook does not exist or belongs to another user.
#[utoipa::path(
    patch,
    path = "/api/v1/webhooks/{id}",
    tag = "Webhooks",
    params(
        ("id" = Uuid, Path, description = "Webhook UUID")
    ),
    request_body = UpdateWebhookRequest,
    responses(
        (status = 200, description = "Webhook updated successfully", body = WebhookApiResponse),
        (status = 400, description = "Validation error"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Webhook not found")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
#[deprecated(
    note = "legacy meeting webhooks are never delivered; use Developer Platform webhooks (PLT-003)"
)]
pub async fn update_webhook(
    State(state): State<AppState>,
    Extension(auth_context): Extension<AuthContext>,
    Path(id): Path<Uuid>,
    Json(payload): Json<UpdateWebhookRequest>,
) -> ApiResult<Json<WebhookApiResponse>> {
    payload
        .validate()
        .map_err(|e| ApiError::ValidationError(e.to_string()))?;

    let webhook = Webhooks::find_by_id(id)
        .filter(webhooks::Column::UserId.eq(auth_context.user_id))
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("Webhook not found".to_string()))?;

    let mut webhook: webhooks::ActiveModel = webhook.into();

    if let Some(endpoint_url) = payload.endpoint_url {
        webhook.endpoint_url = Set(endpoint_url);
    }

    if let Some(status) = payload.status {
        webhook.status = Set(parse_status(&status)?);
    }

    webhook.updated_at = Set(now_utc());
    let webhook = webhook.update(&state.db).await?;

    let response = WebhookResponse {
        id: webhook.id,
        user_id: webhook.user_id,
        event_type: format_event_type(&webhook.event_type),
        endpoint_url: webhook.endpoint_url,
        status: format_status(&webhook.status),
        delivery: DELIVERY_NOT_DELIVERED.to_string(),
        created_at: format_datetime(webhook.created_at),
        updated_at: format_datetime(webhook.updated_at),
    };

    Ok(Json(WebhookApiResponse {
        success: true,
        notice: DEPRECATION_NOTICE.to_string(),
        data: response,
    }))
}

/// Delete a legacy meeting webhook (deprecated)
///
/// DEPRECATED (PLT-003). Permanently deletes a stored registration owned by the authenticated
/// user (nothing was ever delivered to it). Returns 404 if the webhook does not exist or belongs
/// to another user.
#[utoipa::path(
    delete,
    path = "/api/v1/webhooks/{id}",
    tag = "Webhooks",
    params(
        ("id" = Uuid, Path, description = "Webhook UUID")
    ),
    responses(
        (status = 200, description = "Webhook deleted successfully", body = MessageResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Webhook not found")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
#[deprecated(
    note = "legacy meeting webhooks are never delivered; use Developer Platform webhooks (PLT-003)"
)]
pub async fn delete_webhook(
    State(state): State<AppState>,
    Extension(auth_context): Extension<AuthContext>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<MessageResponse>> {
    let webhook = Webhooks::find_by_id(id)
        .filter(webhooks::Column::UserId.eq(auth_context.user_id))
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("Webhook not found".to_string()))?;

    Webhooks::delete_by_id(webhook.id).exec(&state.db).await?;

    Ok(Json(MessageResponse {
        message: "Webhook deleted successfully".to_string(),
    }))
}
