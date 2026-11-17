//! Unified email delivery: provider webhooks and internal operations.
//!
//! - `POST /api/v1/email/provider-events/resend` — Resend delivery events (Svix-signed:
//!   `svix-id`, `svix-timestamp`, `svix-signature`, secret `RESEND_WEBHOOK_SECRET`). Public
//!   (the provider calls it), refused with 401 without a valid signature, 404 when no secret
//!   is configured. Duplicate event ids are acknowledged and ignored. The provider account
//!   has this one webhook: an event for a message this ledger did not send (one the
//!   Notification Service sent) is handed on, byte for byte, to the Notification Service's
//!   own Svix-verified callback when the relay is configured (`CF_NOTIFICATIONS_BASE_URL`);
//!   503 when that hop fails, so the provider retries.
//! - `POST /internal/operations/email/acceptance` — enqueue one controlled acceptance
//!   email to the given address (operations; `X-Internal-Token`).
//! - `GET /internal/operations/email/deliveries/{id}` — PII-free delivery status
//!   (`X-Internal-Token`).
//! - `POST /internal/operations/email/developer-access-decision` — notify the applicant of
//!   a Developer Platform production-access decision (operator console, after the audited
//!   decision; `X-Internal-Token`). Fixed template: no caller-supplied text reaches the
//!   email beyond a safe reason code.
//! - `POST /internal/operations/email/fallback-drill` — Notification Service N12: one
//!   controlled acceptance email through the CRITICAL lane's legacy fallback path (sealed
//!   to and opened with the Account fallback key; `X-Internal-Token`).
//! - `GET /internal/operations/notifications/critical` — PII-free CRITICAL lane state
//!   (counts, oldest open age, leased-uncompleted decision ids; `X-Internal-Token`).
//! - `POST /internal/operations/notifications/critical/reroute` — the operator's
//!   lease-fenced reroute of CRITICAL decisions to the legacy path (break-glass under
//!   `critical_fallback = FORCED`; `X-Internal-Token`).

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
        critical_notifications as critical,
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
    if let (Some(relay), Some(message_id)) = (
        state.notifications.as_ref(),
        payload.pointer("/data/email_id").and_then(Value::as_str),
    ) {
        match email_delivery::owns_message(&state.db, "resend", message_id).await {
            Ok(true) => {}
            Ok(false) => match relay.forward_resend_event(&headers, &body).await {
                // The Notification Service applied (or already had) the event; record it
                // here too (no delivery of this ledger matches, so nothing changes).
                Ok(status) if (200..300).contains(&status) => {
                    inc(&OM.email_webhook_forwarded);
                    let _ = email_delivery::apply_provider_event(&state.db, "resend", id, &payload)
                        .await;
                    return Json(json!({"received": true, "forwarded": true})).into_response();
                }
                // The service has no provider callback configured: this ledger keeps the
                // event, exactly as before.
                Ok(404) => {}
                _ => {
                    inc(&OM.email_webhook_forward_failures);
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(json!({"error": "SERVICE_UNAVAILABLE"})),
                    )
                        .into_response();
                }
            },
            Err(_) => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({"error": "SERVICE_UNAVAILABLE"})),
                )
                    .into_response()
            }
        }
    }
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

// ---------------------------------------------------------------------------
// Notification Service N12: the CRITICAL lane's legacy fallback (operations)
// ---------------------------------------------------------------------------

/// The fallback drill (`email-acceptance confirm=fallback`, Document 4 §10.4 readiness
/// invariants 1, 2 and 4): one controlled `OPERATIONS_ACCEPTANCE` email sealed to the
/// Account fallback key, opened and queued in the legacy ledger — the exact path an AUTO /
/// FORCED failover takes. Poll `GET /internal/operations/email/deliveries/{id}` for
/// `ACCEPTED` (and `DELIVERED` with the Resend webhook). 503 `FALLBACK_KEY_UNAVAILABLE`
/// when no fallback key is loaded.
pub async fn fallback_drill(
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
    let subject = format!("CF Meet fallback email drill {reference}");
    let template = notification_email(
        "CF Meet operations",
        &subject,
        &format!(
            "This is a controlled drill of the CF Meet emergency email fallback (reference {reference}). It travelled the path a verification code takes when the Notification Service cannot send it. No action is needed."
        ),
    );
    let email = email_delivery::NewEmail {
        category: category::OPERATIONS_ACCEPTANCE,
        idempotency_key: Some(format!("acceptance:fallback:{reference}")),
        to: req.to.clone(),
        to_name: None,
        subject,
        html: template.html,
        text: template.text,
        expires_at: Some(Utc::now() + Duration::hours(1)),
    };
    match critical::fallback_drill(&state.db, critical::installed(), email).await {
        Ok(Some(e)) => {
            inc(&OM.critical_fallback_drills);
            (
                StatusCode::ACCEPTED,
                Json(json!({"deliveryId": e.id, "created": e.created, "path": "LEGACY_FALLBACK"})),
            )
                .into_response()
        }
        Ok(None) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "FALLBACK_KEY_UNAVAILABLE"})),
        )
            .into_response(),
        Err(e) => e.into_response(),
    }
}

/// PII-free state of the CRITICAL lane (decision ids and counts only).
pub async fn critical_lane_status(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !internal_token_ok(&headers) {
        return not_found();
    }
    match critical::lane_status(&state.db).await {
        Ok(s) => Json(s).into_response(),
        Err(e) => crate::error::ApiError::from(e).into_response(),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CriticalRerouteRequest {
    pub decision_ids: Vec<Uuid>,
}

/// The operator's reroute of CRITICAL decisions to the legacy path (Document 4 §9.8,
/// break-glass): never-leased decisions always; a leased, lapsed, uncompleted decision
/// only while its category is `critical_fallback = FORCED` (recorded
/// `REROUTED_POSSIBLE_DUPLICATE`). A live lease answers `WAIT_FOR_LEASE`, a completed
/// decision `REFUSED`.
pub async fn critical_reroute(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CriticalRerouteRequest>,
) -> Response {
    if !internal_token_ok(&headers) {
        return not_found();
    }
    if req.decision_ids.is_empty() || req.decision_ids.len() > critical::MAX_BATCH {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "VALIDATION_ERROR"})),
        )
            .into_response();
    }
    let mut results = Vec::with_capacity(req.decision_ids.len());
    for id in req.decision_ids {
        match critical::operator_reroute(&state.db, critical::installed(), id, Utc::now()).await {
            Ok(outcome) => {
                let mut r = json!({"decisionId": id, "outcome": outcome.as_str()});
                if let critical::Failover::Sent {
                    reason,
                    delivery_id,
                } = outcome
                {
                    r["reason"] = json!(reason.as_str());
                    r["deliveryId"] = json!(delivery_id);
                }
                results.push(r);
            }
            Err(e) => return crate::error::ApiError::from(e).into_response(),
        }
    }
    Json(json!({"results": results})).into_response()
}
