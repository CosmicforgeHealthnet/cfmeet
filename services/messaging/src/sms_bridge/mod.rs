//! Part V H §§106–112 — the SMS Bridge, conversation side.
//!
//! Messaging owns the conversation and the canonical message; the SMS Gateway owns the
//! carrier transport (number, controls, carrier state, cost evidence). A bridge connects
//! one MANAGED conversation to one external party reachable by SMS:
//!
//! * **Never an automatic downgrade.** A PRIVATE_E2EE conversation is never bridged
//!   (`E2EE_CONVERSATION_NOT_BRIDGEABLE`, also a database trigger). Reaching an SMS party
//!   from private content means the user deliberately writes in a separate MANAGED
//!   conversation after acknowledging the current disclosure ([`DISCLOSURE_VERSION`]).
//! * **Outbound** MANAGED messages of members are queued (same transaction) and submitted
//!   by the worker; each runs every SMS control in the SMS Gateway. A refusal or carrier
//!   failure changes only the TRANSPORT state; the in-app message is unaffected.
//! * **Inbound** carrier messages become MANAGED messages with channel `SMS`, authored by
//!   the party's TELEPHONE_ENDPOINT principal — never injected into an E2EE conversation.
//! * Messaging stores the masked number only; the number stays sealed in the SMS Gateway.

pub mod http;

use std::sync::Arc;

use async_trait::async_trait;
use axum::http::Method;
use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient},
    DelegatedPlatformContext, OutboundServiceRequest, ServiceId,
};
use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    conversations::{access, service::ConversationRow},
    db,
    domain::SecurityProfile,
    error::{Error, ErrorCode, Result},
    http::middleware::Acting,
    identity::{PrincipalStatus, TrustedContext},
    messages::{self, managed, MessageRow},
    state::AppState,
};

/// The disclosure a bridge creator must acknowledge (version-pinned).
pub const DISCLOSURE_VERSION: &str = "sms-bridge-2026-10";
pub const DISCLOSURE_TEXT: &str = "Messages in this conversation are also sent and received as SMS. SMS is not \
end-to-end encrypted: the text is readable by CF Meet's servers, the SMS provider and the mobile carriers, \
and carrier charges may apply. Private end-to-end encrypted conversations are never bridged; nothing \
private is copied to SMS unless you write it here yourself.";

/// Statuses after which the carrier will not change a transport any more.
pub const TERMINAL: &[&str] = &["DELIVERED", "FAILED", "EXPIRED", "BLOCKED", "RECEIVED"];
const MAX_SUBMIT_ATTEMPTS: i32 = 12;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    /// The SMS Gateway refused (status, code, reason).
    Refused(u16, String, Option<String>),
    Unavailable,
    NotConfigured,
}

/// The SMS Gateway's bridge API (port; HTTP in production, fakes in tests).
#[async_trait]
pub trait SmsTransport: Send + Sync {
    async fn call(
        &self,
        delegated: &DelegatedPlatformContext,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> std::result::Result<(u16, Value), TransportError>;
}

pub struct UnconfiguredSmsTransport;

#[async_trait]
impl SmsTransport for UnconfiguredSmsTransport {
    async fn call(
        &self,
        _: &DelegatedPlatformContext,
        _: Method,
        _: &str,
        _: Option<&Value>,
    ) -> std::result::Result<(u16, Value), TransportError> {
        Err(TransportError::NotConfigured)
    }
}

/// Signed internal HTTP (`sms-bridge.submit`).
pub struct HttpSmsTransport {
    client: Arc<InternalServiceClient>,
}

impl HttpSmsTransport {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl SmsTransport for HttpSmsTransport {
    async fn call(
        &self,
        delegated: &DelegatedPlatformContext,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> std::result::Result<(u16, Value), TransportError> {
        let request = OutboundServiceRequest::new_operation(ServiceId::sms_gateway())
            .with_delegated_context(delegated.clone());
        match self
            .client
            .exchange_json(&request, method, path, body)
            .await
        {
            Ok(r) => {
                let v: Value = if r.body().is_empty() {
                    Value::Null
                } else {
                    r.json().unwrap_or(Value::Null)
                };
                let status = r.status();
                if (200..300).contains(&status) {
                    Ok((status, v))
                } else if status >= 500 || status == 429 {
                    Err(TransportError::Unavailable)
                } else {
                    Err(TransportError::Refused(
                        status,
                        v["error"]["code"].as_str().unwrap_or("UNKNOWN").to_string(),
                        v["error"]["reason"].as_str().map(str::to_string),
                    ))
                }
            }
            Err(InternalCallError::UnknownTarget(_)) => Err(TransportError::NotConfigured),
            Err(_) => Err(TransportError::Unavailable),
        }
    }
}

fn delegated(ctx: &TrustedContext) -> DelegatedPlatformContext {
    ctx.scope.delegated()
}

#[derive(Debug, Clone, FromQueryResult)]
pub struct BridgeRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub conversation_id: Uuid,
    pub number_resource_id: Uuid,
    pub external_principal_id: Uuid,
    pub remote_masked: String,
    pub status: String,
    pub disclosure_version: String,
    pub created_by_principal_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
}

const BRIDGE_COLUMNS: &str = "id, tenant_id, application_id, environment_id, conversation_id, number_resource_id, \
     external_principal_id, remote_masked, status, disclosure_version, created_by_principal_id, created_at, closed_at";

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

pub fn view(b: &BridgeRow) -> Value {
    json!({
        "bridgeId": b.id,
        "conversationId": b.conversation_id,
        "channel": "SMS",
        "numberResourceId": b.number_resource_id,
        "externalPrincipalId": b.external_principal_id,
        "remote": { "masked": b.remote_masked },
        "status": b.status,
        "securityProfile": "MANAGED",
        "encryption": { "endToEnd": false, "readableInTransit": true },
        "disclosureVersion": b.disclosure_version,
        "createdAt": rfc3339(b.created_at),
        "closedAt": b.closed_at.map(rfc3339),
    })
}

pub fn disclosure() -> Value {
    json!({ "version": DISCLOSURE_VERSION, "text": DISCLOSURE_TEXT, "securityProfile": "MANAGED",
            "encryption": { "endToEnd": false, "readableInTransit": true } })
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateBridge {
    pub number_resource_id: Uuid,
    /// The party's number: passed to the SMS Gateway once, never stored here.
    pub to: String,
    pub external_principal_id: Uuid,
    pub disclosure_version: String,
    pub sms_disclosure_acknowledged: bool,
}

async fn active_bridge<C: ConnectionTrait>(
    db: &C,
    conversation_id: Uuid,
) -> Result<Option<BridgeRow>> {
    Ok(db::one(
        db,
        &format!("SELECT {BRIDGE_COLUMNS} FROM messaging_sms_bridges WHERE conversation_id = $1 AND status = 'ACTIVE'"),
        vec![conversation_id.into()],
    )
    .await?)
}

async fn bridge_in_scope<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    id: Uuid,
) -> Result<Option<BridgeRow>> {
    Ok(db::one(
        db,
        &format!("SELECT {BRIDGE_COLUMNS} FROM messaging_sms_bridges WHERE id = $1 AND environment_id = $2"),
        vec![id.into(), ctx.scope.environment_id.into()],
    )
    .await?)
}

/// The conversation, with the acting principal an ACTIVE administrator of it.
async fn administered(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation_id: Uuid,
) -> Result<ConversationRow> {
    let (conversation, member) =
        access::as_member(&state.db, ctx, conversation_id, acting.principal_id).await?;
    if !member.is_admin() {
        return Err(Error::reason(
            ErrorCode::PrincipalNotAuthorized,
            "NOT_ADMIN",
        ));
    }
    Ok(conversation)
}

/// `POST /v1/conversations/{id}/sms-bridges`.
pub async fn create(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation_id: Uuid,
    request: CreateBridge,
) -> Result<Value> {
    crate::operations::require_open(
        &state.db,
        crate::operations::KillSwitch::SmsBridge,
        ctx.scope.environment_id,
    )
    .await?;
    let conversation = administered(state, ctx, acting, conversation_id).await?;
    if conversation.profile() != SecurityProfile::Managed {
        crate::metrics::inc("messaging_sms_bridge_refusals_total", &[("reason", "e2ee")]);
        return Err(Error::reason(
            ErrorCode::ContentRejected,
            "E2EE_CONVERSATION_NOT_BRIDGEABLE",
        ));
    }
    // §28 / §108: the context's policy decides whether its conversations may be bridged.
    let effective =
        crate::conversations::policy::current(&state.db, ctx.scope.environment_id).await?;
    let eligible = crate::conversations::ContextClass::parse(&conversation.context_class)
        .and_then(|c| effective.contexts.get(&c))
        .is_some_and(|c| c.sms_bridge_eligible);
    if !eligible {
        crate::metrics::inc(
            "messaging_sms_bridge_refusals_total",
            &[("reason", "policy")],
        );
        return Err(Error::reason(
            ErrorCode::FeatureDisabled,
            "SMS_BRIDGE_NOT_ELIGIBLE",
        ));
    }
    if !request.sms_disclosure_acknowledged || request.disclosure_version != DISCLOSURE_VERSION {
        return Err(Error::reason(
            ErrorCode::ContentRejected,
            "SMS_DISCLOSURE_REQUIRED",
        ));
    }
    access::require_accepting(&conversation, state.now())?;
    match state
        .principals
        .resolve(&ctx.scope, request.external_principal_id)
        .await
    {
        Ok(PrincipalStatus::Active { kind }) if kind == "TELEPHONE_ENDPOINT" => {}
        Ok(_) => {
            return Err(Error::reason(
                ErrorCode::PrincipalNotAuthorized,
                "EXTERNAL_PARTY_NOT_A_TELEPHONE_ENDPOINT",
            ))
        }
        Err(e) => {
            tracing::warn!(error = %e, "principal directory unavailable");
            return Err(Error::code(ErrorCode::ServiceUnavailable));
        }
    }
    if active_bridge(&state.db, conversation.id).await?.is_some() {
        return Err(Error::reason(ErrorCode::Conflict, "BRIDGE_EXISTS"));
    }
    let id = Uuid::new_v4();
    let binding = state
        .sms
        .call(
            &delegated(ctx),
            Method::POST,
            "/internal/v1/sms-bridge/bindings",
            Some(&json!({ "bridgeRef": id, "numberResourceId": request.number_resource_id, "to": request.to })),
        )
        .await
        .map_err(|e| transport_error(&e))?
        .1;
    let masked = binding["remote"]["masked"]
        .as_str()
        .unwrap_or("***")
        .to_string();
    let now = state.now();
    let row: BridgeRow = db::one(
        &state.db,
        &format!(
            "INSERT INTO messaging_sms_bridges (id, tenant_id, application_id, environment_id, conversation_id, \
               number_resource_id, external_principal_id, remote_masked, disclosure_version, created_by_principal_id, created_at, \
               environment_kind, service_account_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) RETURNING {BRIDGE_COLUMNS}"
        ),
        vec![
            id.into(),
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            ctx.scope.environment_id.into(),
            conversation.id.into(),
            request.number_resource_id.into(),
            request.external_principal_id.into(),
            masked.into(),
            DISCLOSURE_VERSION.into(),
            acting.principal_id.into(),
            now.into(),
            ctx.scope.environment_kind.as_str().into(),
            ctx.scope.service_account_id.into(),
        ],
    )
    .await
    .map_err(|e| {
        if e.to_string().contains("uq_messaging_sms_bridges_active") {
            Error::reason(ErrorCode::Conflict, "BRIDGE_EXISTS")
        } else {
            Error::from(e)
        }
    })?
    .ok_or(Error::code(ErrorCode::InternalError))?;
    crate::audit::record(
        &state.db,
        crate::audit::AuditEntry {
            ctx,
            principal_id: Some(acting.principal_id),
            action: "SMS_BRIDGE_CREATED",
            target_kind: "CONVERSATION",
            target_id: conversation.id,
            detail: json!({ "bridge_id": row.id, "disclosure_version": DISCLOSURE_VERSION }),
        },
        &crate::conversations::service::request_context(),
        now,
    )
    .await?;
    crate::metrics::inc("messaging_sms_bridges_total", &[("outcome", "created")]);
    Ok(view(&row))
}

pub async fn list(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation_id: Uuid,
) -> Result<Value> {
    access::as_member(&state.db, ctx, conversation_id, acting.principal_id).await?;
    let rows: Vec<BridgeRow> = db::all(
        &state.db,
        &format!("SELECT {BRIDGE_COLUMNS} FROM messaging_sms_bridges WHERE conversation_id = $1 ORDER BY created_at"),
        vec![conversation_id.into()],
    )
    .await?;
    Ok(json!({ "bridges": rows.iter().map(view).collect::<Vec<_>>() }))
}

pub async fn close(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation_id: Uuid,
    bridge_id: Uuid,
) -> Result<Value> {
    administered(state, ctx, acting, conversation_id).await?;
    let b = bridge_in_scope(&state.db, ctx, bridge_id)
        .await?
        .filter(|b| b.conversation_id == conversation_id)
        .ok_or(Error::code(ErrorCode::NotFound))?;
    if b.status == "ACTIVE" {
        // Close the carrier side first: no further SMS may reach the conversation.
        match state
            .sms
            .call(
                &delegated(ctx),
                Method::POST,
                &format!("/internal/v1/sms-bridge/bindings/{}/close", b.id),
                None,
            )
            .await
        {
            Ok(_) | Err(TransportError::Refused(404, _, _)) => {}
            Err(e) => return Err(transport_error(&e)),
        }
        db::exec(
            &state.db,
            "UPDATE messaging_sms_bridges SET status = 'CLOSED', closed_at = $2 WHERE id = $1 AND status = 'ACTIVE'",
            vec![b.id.into(), state.now().into()],
        )
        .await?;
    }
    Ok(view(
        &bridge_in_scope(&state.db, ctx, bridge_id)
            .await?
            .ok_or(Error::code(ErrorCode::NotFound))?,
    ))
}

fn transport_error(e: &TransportError) -> Error {
    match e {
        TransportError::NotConfigured => {
            Error::reason(ErrorCode::FeatureDisabled, "SMS_BRIDGE_NOT_CONFIGURED")
        }
        TransportError::Unavailable => Error::code(ErrorCode::ServiceUnavailable),
        TransportError::Refused(_, code, reason) => {
            let known = ["PARTY_ALREADY_BRIDGED", "BRIDGE_CLOSED", "VALIDATION_ERROR"];
            let r = reason.as_deref().unwrap_or(code.as_str());
            Error::reason(
                ErrorCode::ContentRejected,
                known
                    .iter()
                    .find(|k| **k == r)
                    .copied()
                    .unwrap_or("SMS_GATEWAY_REFUSED"),
            )
        }
    }
}

/// Called in the transaction that creates a MANAGED message: a member's message of a
/// bridged conversation is queued for SMS (never the party's own inbound messages).
pub async fn enqueue_outbound<C: ConnectionTrait>(
    txn: &C,
    conversation: &ConversationRow,
    row: &MessageRow,
    now: DateTime<Utc>,
) -> Result<bool> {
    if conversation.profile() != SecurityProfile::Managed {
        return Ok(false);
    }
    let n = db::exec(
        txn,
        "INSERT INTO messaging_sms_transports (message_id, bridge_id, direction, status, next_attempt_at, created_at, updated_at) \
         SELECT $1, b.id, 'OUTBOUND', 'QUEUED', $2, $2, $2 FROM messaging_sms_bridges b \
          WHERE b.conversation_id = $3 AND b.status = 'ACTIVE' AND b.external_principal_id <> $4 \
         ON CONFLICT DO NOTHING",
        vec![row.id.into(), now.into(), conversation.id.into(), row.sender_principal_id.into()],
    )
    .await?;
    Ok(n == 1)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InboundSms {
    pub bridge_ref: Uuid,
    pub sms_message_id: Uuid,
    pub text: String,
    #[serde(default)]
    pub segment_count: Option<i32>,
    #[serde(default)]
    pub received_at: Option<DateTime<Utc>>,
}

/// `POST /internal/v1/sms-bridge/inbound` (`messaging-sms-bridge.deliver`): a carrier
/// message of the bridged party becomes a MANAGED `SMS` message. Idempotent per SMS id.
pub async fn accept_inbound(
    state: &AppState,
    ctx: &TrustedContext,
    request: InboundSms,
) -> Result<Value> {
    let bridge = bridge_in_scope(&state.db, ctx, request.bridge_ref)
        .await?
        .ok_or(Error::code(ErrorCode::NotFound))?;
    if bridge.status != "ACTIVE" {
        return Err(Error::reason(
            ErrorCode::ConversationNotActive,
            "BRIDGE_CLOSED",
        ));
    }
    let conversation =
        crate::conversations::service::load(&state.db, ctx, bridge.conversation_id, false)
            .await?
            .ok_or(Error::code(ErrorCode::NotFound))?;
    if conversation.profile() != SecurityProfile::Managed {
        // Unreachable by construction (trigger); kept as the last line of defence.
        return Err(Error::reason(
            ErrorCode::ContentRejected,
            "SMS_INTO_E2EE_REFUSED",
        ));
    }
    access::require_accepting(&conversation, state.now())?;
    let content = managed::admit(request.text)?;
    let st = state.clone();
    let ctx2 = ctx.clone();
    let rctx = crate::conversations::service::request_context();
    let row = state
        .db
        .transaction::<_, MessageRow, Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let ring = st.managed_keys.as_deref().ok_or(Error::code(ErrorCode::FeatureDisabled))?;
                db::exec(
                    txn,
                    "SELECT 1 FROM messaging_conversation_sequences WHERE conversation_id = $1 FOR UPDATE",
                    vec![conversation.id.into()],
                )
                .await?;
                // Idempotent: the SMS id is the client message id of the party.
                if let Some(existing) =
                    messages::load_by_client_id(txn, conversation.id, bridge.external_principal_id, request.sms_message_id).await?
                {
                    return Ok(existing);
                }
                let row = managed::insert_channel(
                    txn,
                    ring,
                    &st.db,
                    &ctx2,
                    &conversation,
                    bridge.external_principal_id,
                    request.sms_message_id,
                    &content,
                    "SMS",
                    now,
                )
                .await?;
                db::exec(
                    txn,
                    "INSERT INTO messaging_sms_transports (message_id, bridge_id, direction, sms_message_id, status, segment_count, created_at, updated_at) \
                     VALUES ($1, $2, 'INBOUND', $3, 'RECEIVED', $4, $5, $5)",
                    vec![row.id.into(), bridge.id.into(), request.sms_message_id.into(), request.segment_count.into(), now.into()],
                )
                .await?;
                messages::record_created_notify(txn, &ctx2, &row, &rctx, now, true).await?;
                Ok(row)
            })
        })
        .await
        .map_err(Error::from)?;
    crate::metrics::inc("messaging_sms_bridge_inbound_total", &[]);
    state.wake_outbox();
    Ok(
        json!({ "messageId": row.id, "sequence": row.sequence, "channel": "SMS", "securityProfile": "MANAGED" }),
    )
}

#[derive(Debug, FromQueryResult)]
pub struct TransportRow {
    pub message_id: Uuid,
    pub bridge_id: Uuid,
    pub direction: String,
    pub sms_message_id: Option<Uuid>,
    pub status: String,
    pub blocked_reason: Option<String>,
    pub failure_code: Option<String>,
    pub segment_count: Option<i32>,
    pub attempt_count: i32,
    pub updated_at: DateTime<Utc>,
}

const TRANSPORT_COLUMNS: &str =
    "message_id, bridge_id, direction, sms_message_id, status, blocked_reason, failure_code, \
     segment_count, attempt_count, updated_at";

pub fn transport_view(t: &TransportRow) -> Value {
    json!({
        "messageId": t.message_id,
        "bridgeId": t.bridge_id,
        "channel": "SMS",
        "direction": t.direction,
        "transportStatus": t.status,
        "blockedReason": t.blocked_reason,
        "failureCode": t.failure_code,
        "segmentCount": t.segment_count,
        "updatedAt": rfc3339(t.updated_at),
        // In-app delivery / read state is reported by receipts, never by the carrier.
        "inAppStateIndependent": true,
    })
}

/// `GET /v1/conversations/{id}/messages/{messageId}/sms-transport` (members).
pub async fn message_transport(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation_id: Uuid,
    message_id: Uuid,
) -> Result<Value> {
    access::as_member(&state.db, ctx, conversation_id, acting.principal_id).await?;
    let t: TransportRow = db::one(
        &state.db,
        &format!(
            "SELECT {TRANSPORT_COLUMNS} FROM messaging_sms_transports t JOIN messaging_messages m ON m.id = t.message_id \
             WHERE t.message_id = $1 AND m.conversation_id = $2"
        ),
        vec![message_id.into(), conversation_id.into()],
    )
    .await?
    .ok_or(Error::code(ErrorCode::NotFound))?;
    Ok(transport_view(&t))
}

#[derive(Debug, FromQueryResult)]
struct DueTransport {
    message_id: Uuid,
    bridge_id: Uuid,
    direction: String,
    sms_message_id: Option<Uuid>,
    status: String,
    attempt_count: i32,
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    environment_kind: String,
    service_account_id: Uuid,
    conversation_id: Uuid,
}

fn backoff(attempts: i32) -> Duration {
    Duration::seconds((1i64 << attempts.clamp(0, 9)).min(300))
}

/// One pass: submit QUEUED transports, refresh non-terminal ones. Multi-instance safe
/// (row locks, SKIP LOCKED). Returns how many transports changed.
pub async fn run_due(state: &AppState, limit: i64) -> Result<usize> {
    let mut changed = 0;
    let mut gave_up = Vec::new();
    for _ in 0..limit {
        let txn = state.db.begin().await?;
        let now = state.now();
        let due: Option<DueTransport> = db::one(
            &txn,
            "SELECT t.message_id, t.bridge_id, t.direction, t.sms_message_id, t.status, t.attempt_count, \
                    b.tenant_id, b.application_id, b.environment_id, b.environment_kind, b.service_account_id, b.conversation_id \
               FROM messaging_sms_transports t JOIN messaging_sms_bridges b ON b.id = t.bridge_id \
              WHERE t.next_attempt_at IS NOT NULL AND t.next_attempt_at <= $1 \
              ORDER BY t.next_attempt_at LIMIT 1 FOR UPDATE OF t SKIP LOCKED",
            vec![now.into()],
        )
        .await?;
        let Some(d) = due else {
            txn.rollback().await?;
            break;
        };
        // Kill switch: the transport waits (durably queued), nothing is lost.
        if crate::operations::engaged(
            &txn,
            crate::operations::KillSwitch::SmsBridge,
            Some(d.environment_id),
        )
        .await?
        {
            db::exec(
                &txn,
                "UPDATE messaging_sms_transports SET next_attempt_at = $2 WHERE message_id = $1",
                vec![d.message_id.into(), (now + Duration::seconds(60)).into()],
            )
            .await?;
            txn.commit().await?;
            continue;
        }
        let scope = crate::identity::OwnerScope {
            tenant_id: d.tenant_id,
            application_id: d.application_id,
            environment_id: d.environment_id,
            environment_kind: crate::identity::EnvironmentKind::parse(&d.environment_kind)
                .unwrap_or(crate::identity::EnvironmentKind::Sandbox),
            service_account_id: d.service_account_id,
        }
        .delegated();
        let result = if d.status == "QUEUED" {
            let row = messages::load_message(&txn, d.conversation_id, d.message_id, false).await?;
            let text = match row.as_ref().filter(|r| r.is_active()) {
                Some(r) => managed::text_of(state, r, managed::DecryptPurpose::SmsDelivery).await,
                None => None,
            };
            match text {
                // Deleted / moderated before it was sent: nothing goes to the carrier.
                None => Ok(json!({ "status": "FAILED", "failureCode": "MESSAGE_WITHDRAWN" })),
                Some(text) => state
                    .sms
                    .call(
                        &scope,
                        Method::POST,
                        "/internal/v1/sms-bridge/messages",
                        Some(&json!({ "bridgeRef": d.bridge_id, "messageRef": d.message_id, "text": text })),
                    )
                    .await
                    .map(|(_, v)| v),
            }
        } else {
            match d.sms_message_id {
                Some(id) => state
                    .sms
                    .call(
                        &scope,
                        Method::GET,
                        &format!("/internal/v1/sms-bridge/messages/{id}"),
                        None,
                    )
                    .await
                    .map(|(_, v)| v),
                None => Ok(json!({ "status": d.status })),
            }
        };
        let attempts = d.attempt_count + 1;
        match result {
            Ok(v) => {
                let status = v["status"].as_str().unwrap_or("UNKNOWN").to_string();
                let terminal = TERMINAL.contains(&status.as_str()) || attempts >= 200;
                db::exec(
                    &txn,
                    "UPDATE messaging_sms_transports SET status = $2, sms_message_id = COALESCE($3, sms_message_id), \
                       failure_code = $4, blocked_reason = $5, segment_count = COALESCE($6, segment_count), \
                       attempt_count = $7, next_attempt_at = $8, updated_at = $9 WHERE message_id = $1",
                    vec![
                        d.message_id.into(),
                        status.clone().into(),
                        v["smsMessageId"].as_str().and_then(|s| Uuid::parse_str(s).ok()).into(),
                        v["failureCode"].as_str().map(str::to_string).into(),
                        v["blockedReason"].as_str().map(str::to_string).into(),
                        v["segmentCount"].as_i64().map(|n| n as i32).into(),
                        attempts.into(),
                        (!terminal).then(|| now + backoff(attempts.min(6))).into(),
                        now.into(),
                    ],
                )
                .await?;
                // Segments are metered once the gateway accepted the submission.
                if let (Some(segments), false) = (
                    v["segmentCount"].as_i64(),
                    matches!(status.as_str(), "FAILED" | "BLOCKED" | "UNKNOWN"),
                ) {
                    crate::usage::record(
                        &txn,
                        crate::usage::Owner {
                            tenant_id: d.tenant_id,
                            application_id: d.application_id,
                            environment_id: d.environment_id,
                        },
                        crate::usage::SMS_BRIDGED,
                        segments,
                        d.message_id,
                        Some("MANAGED"),
                        &format!("sms-outbound:{}", d.message_id),
                        now,
                    )
                    .await?;
                }
            }
            Err(TransportError::Refused(_, code, reason)) => {
                // A transport control refused it (opt-out, spend, capability …): recorded
                // as BLOCKED with the reason; the in-app message stays as it is.
                db::exec(
                    &txn,
                    "UPDATE messaging_sms_transports SET status = 'BLOCKED', blocked_reason = $2, attempt_count = $3, \
                       next_attempt_at = NULL, updated_at = $4 WHERE message_id = $1",
                    vec![d.message_id.into(), reason.unwrap_or(code).chars().take(64).collect::<String>().into(), attempts.into(), now.into()],
                )
                .await?;
            }
            Err(_) => {
                let give_up = attempts >= MAX_SUBMIT_ATTEMPTS && d.status == "QUEUED";
                db::exec(
                    &txn,
                    "UPDATE messaging_sms_transports SET attempt_count = $2, next_attempt_at = $3, \
                       status = CASE WHEN $4 THEN 'FAILED' ELSE status END, \
                       failure_code = CASE WHEN $4 THEN 'TRANSPORT_UNAVAILABLE' ELSE failure_code END, updated_at = $5 \
                     WHERE message_id = $1",
                    vec![
                        d.message_id.into(),
                        attempts.into(),
                        (!give_up).then(|| now + backoff(attempts)).into(),
                        give_up.into(),
                        now.into(),
                    ],
                )
                .await?;
                if give_up {
                    gave_up.push(d.message_id);
                }
            }
        }
        // §118 / Appendix D: a carrier transport state change is a domain event (ids and
        // codes only; never the text or a telephone number).
        #[derive(FromQueryResult)]
        struct Now {
            status: String,
            failure_code: Option<String>,
            blocked_reason: Option<String>,
        }
        if let Some(after) = db::one::<Now, _>(
            &txn,
            "SELECT status, failure_code, blocked_reason FROM messaging_sms_transports WHERE message_id = $1",
            vec![d.message_id.into()],
        )
        .await?
        .filter(|a| a.status != d.status)
        {
            crate::events::outbox::record(
                &txn,
                "messaging.message.sms-transport-changed",
                "SMS_TRANSPORT",
                d.message_id,
                crate::events::outbox::EventScope {
                    tenant_id: d.tenant_id,
                    application_id: d.application_id,
                    environment_id: d.environment_id,
                },
                json!({ "message_id": d.message_id, "conversation_id": d.conversation_id, "bridge_id": d.bridge_id,
                        "direction": d.direction, "previous_status": d.status, "status": after.status,
                        "failure_code": after.failure_code, "blocked_reason": after.blocked_reason }),
                now,
            )
            .await?;
        }
        txn.commit().await?;
        changed += 1;
    }
    for id in gave_up {
        crate::operations::raise(
            &state.db,
            crate::operations::IncidentCategory::SmsTransportFailed,
            &id.to_string(),
            state.now(),
        )
        .await;
    }
    Ok(changed)
}

pub async fn run(state: AppState, interval: std::time::Duration) {
    loop {
        if let Err(e) = run_due(&state, 100).await {
            tracing::warn!(error = %e, "sms bridge transport pass failed");
        }
        tokio::time::sleep(interval).await;
    }
}
