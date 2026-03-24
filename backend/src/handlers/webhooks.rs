//! Webhook Handlers
//!
//! HTTP handlers for webhook management.

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
        CreateWebhookApiResponse, CreateWebhookRequest, CreateWebhookResponse,
        UpdateWebhookRequest, WebhookApiResponse, WebhookListResponse, WebhookResponse,
        MessageResponse,
    },
    error::{ApiError, ApiResult},
    middleware::AuthContext,
    models::webhooks::{self, Entity as Webhooks, WebhookEventType, WebhookStatus},
    state::AppState,
    utils::now_utc,
};

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

/// Create a new webhook
#[utoipa::path(
    post,
    path = "/api/v1/webhooks",
    tag = "Webhooks",
    request_body = CreateWebhookRequest,
    responses(
        (status = 201, description = "Webhook created successfully", body = CreateWebhookApiResponse),
        (status = 400, description = "Validation error"),
        (status = 401, description = "Unauthorized")
    ),
    security(
        ("bearer_auth" = [])
    )
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
        secret: Set(secret.clone()),
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
        created_at: format_datetime(webhook.created_at),
        updated_at: format_datetime(webhook.updated_at),
    };

    Ok((
        StatusCode::CREATED,
        Json(CreateWebhookApiResponse {
            success: true,
            data: response,
        }),
    ))
}

/// List all webhooks for the authenticated user
#[utoipa::path(
    get,
    path = "/api/v1/webhooks",
    tag = "Webhooks",
    responses(
        (status = 200, description = "List of webhooks", body = WebhookListResponse),
        (status = 401, description = "Unauthorized")
    ),
    security(
        ("bearer_auth" = [])
    )
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
            created_at: format_datetime(w.created_at),
            updated_at: format_datetime(w.updated_at),
        })
        .collect();

    Ok(Json(WebhookListResponse {
        success: true,
        data,
    }))
}

/// Update a webhook
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
        created_at: format_datetime(webhook.created_at),
        updated_at: format_datetime(webhook.updated_at),
    };

    Ok(Json(WebhookApiResponse {
        success: true,
        data: response,
    }))
}

/// Delete a webhook
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
