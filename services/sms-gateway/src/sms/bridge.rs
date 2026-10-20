//! Part V H §§106–112 — the SMS Bridge, transport side.
//!
//! Messaging owns the conversation and the canonical message; the SMS Gateway owns the
//! carrier transport. Through the bridge:
//!
//! * a **binding** ties a Messaging bridge (`bridge_ref`) to one number resource and one
//!   external party. The party's number is sealed here and never returned (Messaging
//!   receives only the masked form);
//! * an outbound **submission** is an ordinary send of the bound number to the bound
//!   party: every control runs exactly as for a customer send (sender capability and
//!   ownership via Numbering, opt-out, premium / allowed prefixes, spend and abuse limits,
//!   rate windows, provider) and refusals are recorded the same way;
//! * every inbound SMS of a bound party is **pushed** to Messaging (durable job, leased,
//!   retried, dead-lettered), in the same transaction that receives it.
//!
//! Callers are service-authenticated (`sms-bridge.submit`, Messaging only) and act inside
//! the signed delegated scope; Numbering re-checks that scope on every send.

use std::sync::Arc;

use async_trait::async_trait;
use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient},
    DelegatedPlatformContext, OutboundServiceRequest, ServiceId,
};
use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use uuid::Uuid;

use super::{
    e164,
    protection::{mask, Purpose},
    send::{self, SendOutcome, SendRequest},
    store, SmsError, SmsResult,
};
use crate::{
    db,
    domain::RequestContext,
    identity::{EnvironmentKind, TrustedContext},
    state::AppState,
};

/// Messaging's inbound route.
pub const INBOUND_PATH: &str = "/internal/v1/sms-bridge/inbound";
const LEASE_SECONDS: i64 = 60;
pub const MAX_DELIVERY_ATTEMPTS: i32 = 20;

/// The trusted context of a bridge call: the signed delegated scope (Messaging validated
/// it against Platform Access; Numbering re-validates number ownership in it on send).
pub fn bridge_context(d: &DelegatedPlatformContext) -> TrustedContext {
    use cf_service_auth::ApplicationEnvironmentKind as K;
    TrustedContext {
        tenant_id: d.tenant_id(),
        application_id: d.application_id(),
        environment_id: d.application_environment_id(),
        environment_kind: match d.application_environment_kind() {
            K::Sandbox => EnvironmentKind::Sandbox,
            K::Test => EnvironmentKind::Test,
            K::Production => EnvironmentKind::Production,
        },
        service_account_id: d.initiating_service_account_id(),
        credential_id: d.initiating_service_account_id(),
        first_party: false,
        live_telecom_allowed: false,
        full_authority: true,
        scopes: BTreeSet::new(),
    }
}

#[derive(Debug, Clone, FromQueryResult)]
pub struct BindingRow {
    pub id: Uuid,
    pub environment_id: Uuid,
    pub number_resource_id: Uuid,
    pub remote_sealed: String,
    pub remote_masked: String,
    pub bridge_ref: Uuid,
    pub status: String,
}

const BINDING_COLUMNS: &str =
    "id, environment_id, number_resource_id, remote_sealed, remote_masked, bridge_ref, status";

pub fn binding_view(b: &BindingRow) -> Value {
    json!({
        "bindingId": b.id,
        "bridgeRef": b.bridge_ref,
        "numberResourceId": b.number_resource_id,
        "remote": { "masked": b.remote_masked },
        "status": b.status,
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateBinding {
    pub bridge_ref: Uuid,
    pub number_resource_id: Uuid,
    pub to: String,
}

async fn load_binding<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    bridge_ref: Uuid,
) -> SmsResult<Option<BindingRow>> {
    Ok(db::one(
        db,
        &format!("SELECT {BINDING_COLUMNS} FROM sms_bridge_bindings WHERE environment_id = $1 AND bridge_ref = $2"),
        vec![ctx.environment_id.into(), bridge_ref.into()],
    )
    .await?)
}

/// Idempotent per bridge reference; one ACTIVE binding per (number, party).
pub async fn create_binding(
    state: &AppState,
    ctx: &TrustedContext,
    req: CreateBinding,
) -> SmsResult<(bool, BindingRow)> {
    if let Some(existing) = load_binding(&state.db, ctx, req.bridge_ref).await? {
        return Ok((false, existing));
    }
    let to = zeroize::Zeroizing::new(
        e164::normalize(&req.to).ok_or(SmsError::Validation("to must be an E.164 number."))?,
    );
    if !e164::allowed(&to, &state.controls.allowed_prefixes) {
        return Err(SmsError::Validation(
            "to is outside the allowed destinations.",
        ));
    }
    let p = &state.protection;
    let id = Uuid::new_v4();
    let now = state.now();
    let row: Option<BindingRow> = db::one(
        &state.db,
        &format!(
            "INSERT INTO sms_bridge_bindings (id, tenant_id, application_id, environment_id, environment_kind, \
               service_account_id, number_resource_id, remote_sealed, remote_fingerprint, remote_masked, bridge_ref, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) ON CONFLICT DO NOTHING RETURNING {BINDING_COLUMNS}"
        ),
        vec![
            id.into(),
            ctx.tenant_id.into(),
            ctx.application_id.into(),
            ctx.environment_id.into(),
            ctx.environment_kind.as_str().into(),
            ctx.service_account_id.into(),
            req.number_resource_id.into(),
            p.seal(Purpose::Remote, id, &to).into(),
            p.number_fingerprint(&to).into(),
            mask(&to).into(),
            req.bridge_ref.into(),
            now.into(),
        ],
    )
    .await?;
    match row {
        Some(r) => {
            crate::metrics::inc("sms_bridge_bindings_total", &[("outcome", "created")]);
            Ok((true, r))
        }
        // A concurrent duplicate, or the party already bridged on this number.
        None => match load_binding(&state.db, ctx, req.bridge_ref).await? {
            Some(r) => Ok((false, r)),
            None => Err(SmsError::Conflict("PARTY_ALREADY_BRIDGED")),
        },
    }
}

pub async fn close_binding(
    state: &AppState,
    ctx: &TrustedContext,
    bridge_ref: Uuid,
) -> SmsResult<BindingRow> {
    db::exec(
        &state.db,
        "UPDATE sms_bridge_bindings SET status = 'CLOSED', closed_at = $3 \
         WHERE environment_id = $1 AND bridge_ref = $2 AND status = 'ACTIVE'",
        vec![
            ctx.environment_id.into(),
            bridge_ref.into(),
            state.now().into(),
        ],
    )
    .await?;
    load_binding(&state.db, ctx, bridge_ref)
        .await?
        .ok_or(SmsError::NotFound)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Submission {
    pub bridge_ref: Uuid,
    /// Messaging's canonical message id (the idempotency of this transport).
    pub message_ref: Uuid,
    pub text: String,
}

/// One outbound SMS of a bridged conversation message (all customer-send controls).
pub async fn submit(
    state: &AppState,
    ctx: &TrustedContext,
    req: Submission,
    rc: &RequestContext,
) -> SmsResult<SendOutcome> {
    let binding = load_binding(&state.db, ctx, req.bridge_ref)
        .await?
        .ok_or(SmsError::NotFound)?;
    if binding.status != "ACTIVE" {
        return Err(SmsError::Conflict("BRIDGE_CLOSED"));
    }
    let to = zeroize::Zeroizing::new(
        state
            .protection
            .open(Purpose::Remote, binding.id, &binding.remote_sealed)
            .map_err(|_| SmsError::Conflict("BINDING_UNAVAILABLE"))?,
    );
    let request = SendRequest {
        from_number: binding.number_resource_id,
        to: Some(to.to_string()),
        thread_id: None,
        text: req.text,
        principal_id: None,
        validity_seconds: None,
    };
    let key = format!("bridge:{}", req.message_ref.simple());
    let outcome = send::send(state, ctx, &key, request, rc).await;
    crate::metrics::inc(
        "sms_bridge_submissions_total",
        &[(
            "outcome",
            match &outcome {
                Ok(SendOutcome::Accepted(_)) => "accepted",
                Ok(SendOutcome::Replayed(_)) => "replayed",
                Err(SmsError::Blocked { .. }) => "blocked",
                Err(SmsError::RateLimited { .. }) => "rate_limited",
                Err(_) => "error",
            },
        )],
    );
    outcome
}

/// Transport state of one bridged message (separate from any in-app state).
pub async fn transport_status(
    state: &AppState,
    ctx: &TrustedContext,
    sms_message_id: Uuid,
) -> SmsResult<Value> {
    let row = store::load(&state.db, ctx.environment_id, sms_message_id)
        .await?
        .ok_or(SmsError::NotFound)?;
    Ok(transport_view(&store::view(&row, &state.protection)))
}

/// What Messaging may hold about a transport: state, codes, segments, cost evidence —
/// never the content or a number.
pub fn transport_view(v: &store::MessageView) -> Value {
    let full = serde_json::to_value(v).unwrap_or(Value::Null);
    json!({
        "smsMessageId": full["messageId"],
        "direction": full["direction"],
        "status": full["status"],
        "segmentCount": full["segmentCount"],
        "failureCode": full["failureCode"],
        "blockedReason": full["blockedReason"],
        "cost": full["cost"],
    })
}

/// Called inside the transaction that RECEIVES an inbound SMS: a bound party's message is
/// queued for Messaging (atomically with the receipt).
pub async fn enqueue_inbound<C: ConnectionTrait>(
    txn: &C,
    environment_id: Uuid,
    number_resource_id: Uuid,
    remote_fingerprint: &str,
    transport_id: Uuid,
    now: DateTime<Utc>,
) -> SmsResult<bool> {
    let n = db::exec(
        txn,
        "INSERT INTO sms_bridge_deliveries (id, binding_id, transport_id, next_attempt_at, created_at) \
         SELECT $1, b.id, $2, $3, $3 FROM sms_bridge_bindings b \
          WHERE b.environment_id = $4 AND b.number_resource_id = $5 AND b.remote_fingerprint = $6 AND b.status = 'ACTIVE' \
         ON CONFLICT (transport_id) DO NOTHING",
        vec![
            Uuid::new_v4().into(),
            transport_id.into(),
            now.into(),
            environment_id.into(),
            number_resource_id.into(),
            remote_fingerprint.into(),
        ],
    )
    .await?;
    Ok(n == 1)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushFailure {
    Unavailable,
    Rejected,
    NotConfigured,
}

#[async_trait]
pub trait BridgeInbound: Send + Sync {
    async fn deliver(
        &self,
        delegated: &DelegatedPlatformContext,
        body: &Value,
    ) -> Result<(), PushFailure>;
}

pub struct UnconfiguredBridgeInbound;

#[async_trait]
impl BridgeInbound for UnconfiguredBridgeInbound {
    async fn deliver(&self, _: &DelegatedPlatformContext, _: &Value) -> Result<(), PushFailure> {
        Err(PushFailure::NotConfigured)
    }
}

/// Signed internal HTTP to Messaging (`messaging-sms-bridge.deliver`).
pub struct HttpBridgeInbound {
    client: Arc<InternalServiceClient>,
}

impl HttpBridgeInbound {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl BridgeInbound for HttpBridgeInbound {
    async fn deliver(
        &self,
        delegated: &DelegatedPlatformContext,
        body: &Value,
    ) -> Result<(), PushFailure> {
        let request = OutboundServiceRequest::new_operation(ServiceId::messaging())
            .with_delegated_context(delegated.clone());
        match self.client.post_json(&request, INBOUND_PATH, body).await {
            Ok(_) => Ok(()),
            Err(InternalCallError::UnknownTarget(_)) => Err(PushFailure::NotConfigured),
            Err(InternalCallError::Status { status, .. }) if status < 500 && status != 429 => {
                Err(PushFailure::Rejected)
            }
            Err(_) => Err(PushFailure::Unavailable),
        }
    }
}

#[derive(Debug, FromQueryResult)]
struct DueDelivery {
    id: Uuid,
    attempt_count: i32,
    transport_id: Uuid,
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    environment_kind: String,
    service_account_id: Uuid,
    bridge_ref: Uuid,
    content_sealed: Option<String>,
    received_at: Option<DateTime<Utc>>,
    segment_count: i16,
}

/// Delivers due inbound pushes (multi-instance safe). Returns how many were delivered.
pub async fn deliver_due(
    state: &AppState,
    inbound: &dyn BridgeInbound,
    limit: u64,
) -> SmsResult<usize> {
    let now = state.now();
    let due: Vec<DueDelivery> = db::all(
        &state.db,
        "WITH claimed AS ( \
           UPDATE sms_bridge_deliveries SET lease_owner = $1, lease_expires_at = $2 \
            WHERE id IN (SELECT id FROM sms_bridge_deliveries WHERE status = 'PENDING' AND next_attempt_at <= $3 \
                           AND (lease_expires_at IS NULL OR lease_expires_at <= $3) \
                         ORDER BY created_at LIMIT $4 FOR UPDATE SKIP LOCKED) \
           RETURNING id, attempt_count, transport_id, binding_id) \
         SELECT c.id, c.attempt_count, c.transport_id, b.tenant_id, b.application_id, b.environment_id, \
                b.environment_kind, b.service_account_id, b.bridge_ref, t.content_sealed, t.received_at, \
                t.segment_count \
           FROM claimed c JOIN sms_bridge_bindings b ON b.id = c.binding_id JOIN sms_transports t ON t.id = c.transport_id",
        vec![
            state.instance.clone().into(),
            (now + Duration::seconds(LEASE_SECONDS)).into(),
            now.into(),
            (limit as i64).into(),
        ],
    )
    .await?;
    let mut delivered = 0;
    for d in due {
        let kind = match EnvironmentKind::parse(&d.environment_kind) {
            Some(EnvironmentKind::Sandbox) => cf_service_auth::ApplicationEnvironmentKind::Sandbox,
            Some(EnvironmentKind::Test) => cf_service_auth::ApplicationEnvironmentKind::Test,
            Some(EnvironmentKind::Production) => {
                cf_service_auth::ApplicationEnvironmentKind::Production
            }
            None => continue,
        };
        let text = d.content_sealed.as_deref().and_then(|s| {
            state
                .protection
                .open(Purpose::Content, d.transport_id, s)
                .ok()
        });
        let outcome = match text {
            // The content was purged by retention before delivery: never deliverable.
            None => Err(PushFailure::Rejected),
            Some(text) => {
                let body = json!({
                    "bridgeRef": d.bridge_ref,
                    "smsMessageId": d.transport_id,
                    "text": text,
                    "segmentCount": d.segment_count,
                    "receivedAt": d.received_at.map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Micros, true)),
                });
                let delegated = DelegatedPlatformContext::new(
                    d.tenant_id,
                    d.application_id,
                    d.environment_id,
                    kind,
                    d.service_account_id,
                );
                inbound.deliver(&delegated, &body).await
            }
        };
        let now = state.now();
        match outcome {
            Ok(()) => {
                db::exec(
                    &state.db,
                    "UPDATE sms_bridge_deliveries SET status = 'DELIVERED', delivered_at = $2, attempt_count = attempt_count + 1, \
                       last_error = NULL, lease_owner = NULL, lease_expires_at = NULL WHERE id = $1 AND status = 'PENDING'",
                    vec![d.id.into(), now.into()],
                )
                .await?;
                crate::metrics::inc("sms_bridge_inbound_total", &[("outcome", "delivered")]);
                delivered += 1;
            }
            Err(failure) => {
                let attempts = d.attempt_count + 1;
                let dead = failure == PushFailure::Rejected || attempts >= MAX_DELIVERY_ATTEMPTS;
                let backoff = Duration::seconds((1i64 << attempts.clamp(0, 9)).min(300));
                db::exec(
                    &state.db,
                    "UPDATE sms_bridge_deliveries SET attempt_count = $2, last_error = $3, next_attempt_at = $4, \
                       status = CASE WHEN $5 THEN 'DEAD_LETTER' ELSE status END, lease_owner = NULL, lease_expires_at = NULL \
                     WHERE id = $1 AND status = 'PENDING'",
                    vec![
                        d.id.into(),
                        attempts.into(),
                        match failure {
                            PushFailure::Unavailable => "UNAVAILABLE",
                            PushFailure::Rejected => "REJECTED",
                            PushFailure::NotConfigured => "NOT_CONFIGURED",
                        }
                        .into(),
                        (now + backoff).into(),
                        dead.into(),
                    ],
                )
                .await?;
                crate::metrics::inc(
                    "sms_bridge_inbound_total",
                    &[("outcome", if dead { "dead_letter" } else { "retry" })],
                );
            }
        }
    }
    Ok(delivered)
}
