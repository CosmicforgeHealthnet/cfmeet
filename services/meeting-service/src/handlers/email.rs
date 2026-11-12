//! Unified email delivery: provider webhooks and internal operations.
//!
//! - `POST /api/v1/email/provider-events/resend` — Resend delivery events (Svix-signed:
//!   `svix-id`, `svix-timestamp`, `svix-signature`, secret `RESEND_WEBHOOK_SECRET`). Public
//!   (the provider calls it), refused with 401 without a valid signature, 404 when no secret
//!   is configured. Duplicate event ids are acknowledged and ignored.
//! - `POST /internal/operations/email/acceptance` — enqueue one controlled acceptance
//!   email to the given address (operations; `X-Internal-Token`).
//! - `GET /internal/operations/email/deliveries/{id}` — PII-free delivery status
//!   (`X-Internal-Token`).
//! - `POST /internal/operations/email/developer-access-decision` — notify the applicant of
//!   a Developer Platform production-access decision (operator console, after the audited
//!   decision; `X-Internal-Token`). Fixed template: no caller-supplied text reaches the
//!   email beyond a safe reason code.

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::{Duration, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    handlers::internal::internal_token_ok,
    services::{
        email_delivery::{self, category, WebhookError},
        ops_metrics::{inc, OPS_METRICS as OM},
    },
    state::AppState,
    templates::email::notification_email,
};

fn header<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({"error": "NOT_FOUND"}))).into_response()
}

/// Resend delivery events.
pub async fn resend_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(secret) = state.email_webhook_secret.as_deref() else {
        return not_found();
    };
    if body.len() > 256 * 1024 {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({"error": "PAYLOAD_TOO_LARGE"})),
        )
            .into_response();
    }
    let id = header(&headers, "svix-id");
    if email_delivery::verify_svix(
        secret,
        id,
        header(&headers, "svix-timestamp"),
        header(&headers, "svix-signature"),
        &body,
        Utc::now(),
    )
    .is_err()
    {
        inc(&OM.email_webhook_rejected);
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "INVALID_SIGNATURE"})),
        )
            .into_response();
    }
    let Ok(payload) = serde_json::from_slice::<Value>(&body) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "MALFORMED_EVENT"})),
        )
            .into_response();
    };
    match email_delivery::apply_provider_event(&state.db, "resend", id, &payload).await {
        Ok(applied) => Json(json!({"received": true, "duplicate": !applied})).into_response(),
        Err(WebhookError::Malformed) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "MALFORMED_EVENT"})),
        )
            .into_response(),
        Err(WebhookError::Unauthorized) => (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "INVALID_SIGNATURE"})),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptanceEmailRequest {
    pub to: String,
    /// Correlation reference printed in the email (no secrets).
    pub reference: String,
}

/// Enqueue one controlled acceptance email (operations).
pub async fn acceptance_email(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<AcceptanceEmailRequest>,
) -> Response {
    if !internal_token_ok(&headers) {
        return not_found();
    }
    let reference: String = req
        .reference
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(64)
        .collect();
    if reference.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "VALIDATION_ERROR"})),
        )
            .into_response();
    }
    let subject = format!("CF Meet email delivery check {reference}");
    let template = notification_email(
        "CF Meet operations",
        &subject,
        &format!(
            "This is a controlled delivery check of the CF Meet transactional email service (reference {reference}). No action is needed."
        ),
    );
    match state
        .email_service
        .enqueue_template(
            category::OPERATIONS_ACCEPTANCE,
            Some(format!("acceptance:{reference}")),
            &req.to,
            None,
            &subject,
            template,
            Some(Utc::now() + Duration::hours(1)),
        )
        .await
    {
        Ok(e) => (
            StatusCode::ACCEPTED,
            Json(json!({"deliveryId": e.id, "created": e.created})),
        )
            .into_response(),
        Err(e) => e.into_response(),
    }
}

/// PII-free delivery status (operations).
pub async fn delivery_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    if !internal_token_ok(&headers) {
        return not_found();
    }
    match email_delivery::status(&state.db, id).await {
        Ok(Some(s)) => Json(s).into_response(),
        Ok(None) => not_found(),
        Err(e) => e.into_response(),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeveloperAccessDecisionNotice {
    /// The applicant (`requestedBy` of the production-access request: a CF Meet user id).
    pub user_id: Uuid,
    pub request_id: Uuid,
    /// `APPROVED` or `REJECTED`.
    pub decision: String,
    /// Safe reason code of a rejection (upper snake case).
    #[serde(default)]
    pub reason_code: Option<String>,
}

/// Production-access decision notice to the applicant (Developer Platform, Section 6).
pub async fn developer_access_decision(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<DeveloperAccessDecisionNotice>,
) -> Response {
    use sea_orm::EntityTrait;
    if !internal_token_ok(&headers) {
        return not_found();
    }
    let reason_ok = |r: &str| {
        !r.is_empty()
            && r.len() <= 64
            && r.bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
    };
    let (subject, message) = match (req.decision.as_str(), req.reason_code.as_deref()) {
        ("APPROVED", None) => (
            "Your CF Meet production access was approved",
            "Your request for production access on the CF Meet Developer Platform was approved. \
             Activate production access from the Developer Console; the approval expires if it is not activated in time."
                .to_string(),
        ),
        ("REJECTED", Some(code)) if reason_ok(code) => (
            "Your CF Meet production access request was not approved",
            format!(
                "Your request for production access on the CF Meet Developer Platform was not approved (reason: {code}). \
                 You can review the request in the Developer Console and submit a new one."
            ),
        ),
        _ => {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": "VALIDATION_ERROR"})))
                .into_response()
        }
    };
    let user = match crate::models::users::Entity::find_by_id(req.user_id)
        .one(&state.db)
        .await
    {
        Ok(Some(u)) => u,
        Ok(None) => return not_found(),
        Err(e) => return crate::error::ApiError::from(e).into_response(),
    };
    let template = notification_email(&user.username, subject, &message);
    match state
        .email_service
        .enqueue_template(
            category::DEVELOPER_PLATFORM,
            Some(format!(
                "devplat-access:{}:{}",
                req.request_id, req.decision
            )),
            &user.email,
            Some(&user.username),
            subject,
            template,
            Some(Utc::now() + Duration::days(3)),
        )
        .await
    {
        Ok(e) => (
            StatusCode::ACCEPTED,
            Json(json!({"deliveryId": e.id, "created": e.created})),
        )
            .into_response(),
        Err(e) => e.into_response(),
    }
}
