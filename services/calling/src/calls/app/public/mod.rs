//! Public-network calls (Part E §44–§48): the Calling-side orchestration of calls with
//! protected public parties, executed by Telephony.
//!
//! ```text
//! POST /v1/calls {targetContactReference} ─▶ Privacy Relay (PUBLIC_RELAY) ─▶ Routing Engine
//!   (APP_TO_PSTN) ─▶ one tx: Call, RTC + public CallLegs, attempts, public party, spend
//!   reservation, durable ORIGINATE command ─▶ media resource ─▶ DIALING
//!   ─▶ dispatcher: Contact Resolution Grant ─▶ Telephony originate (same command id)
//!   ─▶ Telephony events: RINGING ─▶ ANSWERED ─▶ BRIDGE (media join code ─▶ RTC attach)
//!   ─▶ BRIDGE_ESTABLISHED + LEG_CONNECTED + initiator connected ─▶ CONNECTED
//!   ─▶ hangup either side ─▶ TERMINATE / ENDING ─▶ ENDED ─▶ USAGE_FINALIZED settles spend
//! ```
//!
//! Calling never receives a telephone number, a provider identifier or a SIP reference:
//! the public party is an opaque ContactReference, the execution an opaque Telephony leg
//! id. UNKNOWN outcomes are never failures and never fail over (§48).

mod bridge;
mod callback;
mod dispatch;
mod events;
mod failover;
mod inbound;
mod outbound;

use std::sync::Arc;

use cf_service_auth::{ApplicationEnvironmentKind, DelegatedPlatformContext};
use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement};
use serde_json::Value;
use uuid::Uuid;

pub use callback::CreateCallbackRelay;
pub use events::EventDisposition;
pub use inbound::InboundOffer;
pub use outbound::CreatePublicCall;
pub(crate) use outbound::{enqueue_originate_in, live_telecom_allowed, OriginateSpec};

use super::AppCallService;
use crate::{
    calls::errors::{CallError, CallResult},
    config::ConfigError,
    models::calls,
    routing::engine::TelecomFlags,
    telephony::{media::PublicMediaGateway, TelephonyPort, UnconfiguredTelephony},
};

/// The public-network execution dependencies of application calling.
#[derive(Clone)]
pub struct PublicNetwork {
    pub telephony: Arc<dyn TelephonyPort>,
    pub media: Option<Arc<dyn PublicMediaGateway>>,
    pub flags: TelecomFlags,
}

impl Default for PublicNetwork {
    fn default() -> Self {
        Self {
            telephony: Arc::new(UnconfiguredTelephony),
            media: None,
            flags: TelecomFlags::default(),
        }
    }
}

/// Calling's telecom switches (`CALLING_PUBLIC_OUTBOUND_ENABLED`,
/// `CALLING_PUBLIC_INBOUND_ENABLED`, `CALLING_CALLBACK_RELAY_ENABLED`): all default OFF.
/// Turning one off stops new paid actions of that class only; application RTC is never
/// affected.
pub fn telecom_flags_from_lookup(
    get: &impl Fn(&str) -> Option<String>,
) -> Result<TelecomFlags, ConfigError> {
    let flag = |var: &'static str| -> Result<bool, ConfigError> {
        match get(var).map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            None | Some("") | Some("false") | Some("0") => Ok(false),
            Some("true") | Some("1") => Ok(true),
            Some(_) => Err(ConfigError::invalid(var, "must be true or false".into())),
        }
    };
    Ok(TelecomFlags {
        public_outbound: flag("CALLING_PUBLIC_OUTBOUND_ENABLED")?,
        public_inbound: flag("CALLING_PUBLIC_INBOUND_ENABLED")?,
        callback_relay: flag("CALLING_CALLBACK_RELAY_ENABLED")?,
    })
}

/// Command kinds Calling sends to Telephony.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandKind {
    Originate,
    Terminate,
    Hold,
    Resume,
    Bridge,
    Unbridge,
    /// Part F: answer an internal enterprise SIP leg.
    Answer,
    /// Part F: play a built-in prompt on an internal enterprise SIP leg.
    Prompt,
    /// Part F: capture an internal enterprise SIP leg's audio at the edge.
    Capture,
}

impl CommandKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            CommandKind::Originate => "ORIGINATE",
            CommandKind::Terminate => "TERMINATE",
            CommandKind::Hold => "HOLD",
            CommandKind::Resume => "RESUME",
            CommandKind::Bridge => "BRIDGE",
            CommandKind::Unbridge => "UNBRIDGE",
            CommandKind::Answer => "ANSWER",
            CommandKind::Prompt => "PROMPT",
            CommandKind::Capture => "CAPTURE",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "ORIGINATE" => CommandKind::Originate,
            "TERMINATE" => CommandKind::Terminate,
            "HOLD" => CommandKind::Hold,
            "RESUME" => CommandKind::Resume,
            "BRIDGE" => CommandKind::Bridge,
            "UNBRIDGE" => CommandKind::Unbridge,
            "ANSWER" => CommandKind::Answer,
            "PROMPT" => CommandKind::Prompt,
            "CAPTURE" => CommandKind::Capture,
            _ => return None,
        })
    }
}

/// A row of `call_telephony_commands`.
#[derive(Debug, Clone, FromQueryResult)]
pub(crate) struct CommandRow {
    pub id: Uuid,
    pub call_id: Uuid,
    pub call_leg_id: Option<Uuid>,
    pub attempt_id: Option<Uuid>,
    pub bridge_id: Option<Uuid>,
    pub kind: String,
    pub idempotency_key: String,
    pub payload: Value,
    pub attempts: i32,
    pub created_at: DateTime<Utc>,
}

pub(crate) const COMMAND_COLUMNS: &str = "id, call_id, call_leg_id, attempt_id, bridge_id, kind, \
    idempotency_key, payload, attempts, created_at";

/// A protected public party of a Call.
#[derive(Debug, Clone, FromQueryResult)]
pub(crate) struct PartyRow {
    pub contact_reference: Uuid,
    pub call_leg_id: Option<Uuid>,
}

pub(crate) fn stmt(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

pub(crate) async fn exec<C: ConnectionTrait>(
    db: &C,
    sql: &str,
    values: Vec<sea_orm::Value>,
) -> CallResult<u64> {
    Ok(db.execute(stmt(sql, values)).await?.rows_affected())
}

pub(crate) async fn parties<C: ConnectionTrait>(
    db: &C,
    call_id: Uuid,
) -> CallResult<Vec<PartyRow>> {
    Ok(PartyRow::find_by_statement(stmt(
        "SELECT contact_reference, call_leg_id FROM call_public_parties \
         WHERE call_id = $1 ORDER BY created_at, id",
        vec![call_id.into()],
    ))
    .all(db)
    .await?)
}

/// Whether the Call dials a protected public party (APP_TO_PSTN: no app ringing).
pub(crate) async fn is_public_outbound<C: ConnectionTrait>(
    db: &C,
    call_id: Uuid,
) -> CallResult<bool> {
    let row = db
        .query_one(stmt(
            "SELECT EXISTS (SELECT 1 FROM call_public_parties WHERE call_id = $1 AND role = 'CALLEE')",
            vec![call_id.into()],
        ))
        .await?;
    Ok(row
        .map(|r| r.try_get_by_index::<bool>(0))
        .transpose()?
        .unwrap_or(false))
}

/// Whether the Call has protected public parties (it is executed by Telephony).
pub(crate) async fn is_public<C: ConnectionTrait>(db: &C, call_id: Uuid) -> CallResult<bool> {
    let row = db
        .query_one(stmt(
            "SELECT EXISTS (SELECT 1 FROM call_public_parties WHERE call_id = $1)",
            vec![call_id.into()],
        ))
        .await?;
    Ok(row
        .map(|r| r.try_get_by_index::<bool>(0))
        .transpose()?
        .unwrap_or(false))
}

/// Backoff of a command retry (bounded).
pub(crate) fn retry_delay(attempts: i32) -> Duration {
    Duration::milliseconds((500_i64 << attempts.clamp(0, 6)).min(30_000))
}

/// Writes a durable command to Telephony (inside the caller's transaction). The payload
/// never contains a grant secret (the table refuses it). Returns the command id; an
/// existing command for the same idempotency key is returned instead.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn enqueue_in<C: ConnectionTrait>(
    txn: &C,
    call: &calls::Model,
    kind: CommandKind,
    call_leg_id: Option<Uuid>,
    attempt_id: Option<Uuid>,
    bridge_id: Option<Uuid>,
    idempotency_key: &str,
    payload: Value,
    due: DateTime<Utc>,
) -> CallResult<Uuid> {
    let id = payload["commandId"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .unwrap_or_else(Uuid::new_v4);
    let row = txn
        .query_one(stmt(
            "INSERT INTO call_telephony_commands (id, call_id, tenant_id, application_id, environment_id, \
               call_leg_id, attempt_id, bridge_id, kind, idempotency_key, payload, status, attempts, \
               next_attempt_at, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, 'PENDING', 0, $12, $12, $12) \
             ON CONFLICT DO NOTHING RETURNING id",
            vec![
                id.into(),
                call.id.into(),
                call.tenant_id.into(),
                call.application_id.into(),
                call.environment_id.into(),
                call_leg_id.into(),
                attempt_id.into(),
                bridge_id.into(),
                kind.as_str().into(),
                idempotency_key.into(),
                payload.into(),
                due.into(),
            ],
        ))
        .await?;
    match row {
        Some(r) => Ok(r.try_get_by_index::<Uuid>(0)?),
        None => {
            let existing = txn
                .query_one(stmt(
                    "SELECT id FROM call_telephony_commands WHERE idempotency_key = $1 \
                     OR (call_leg_id = $2 AND kind = $3 AND kind IN ('ORIGINATE', 'TERMINATE'))",
                    vec![
                        idempotency_key.into(),
                        call_leg_id.into(),
                        kind.as_str().into(),
                    ],
                ))
                .await?
                .ok_or(CallError::Conflict(
                    "a conflicting telephony command exists",
                ))?;
            Ok(existing.try_get_by_index::<Uuid>(0)?)
        }
    }
}

impl AppCallService {
    /// Uses `public` for public-network execution (default: unconfigured — no public
    /// route exists; application RTC is unaffected).
    pub fn with_public_network(mut self, public: PublicNetwork) -> Self {
        self.public = public;
        self
    }

    pub fn public_network(&self) -> &PublicNetwork {
        &self.public
    }

    /// The delegated platform context of a Call's own scope (Calling-issued commands on
    /// the customer's behalf: the Call's creating service account, never a credential).
    pub(crate) async fn delegated_for(
        &self,
        call: &calls::Model,
    ) -> CallResult<DelegatedPlatformContext> {
        let row = self
            .db()
            .query_one(stmt(
                "SELECT kind FROM platform_environments WHERE id = $1 AND application_id = $2",
                vec![call.environment_id.into(), call.application_id.into()],
            ))
            .await?
            .ok_or(CallError::NotFound)?;
        let kind: String = row.try_get_by_index(0)?;
        let kind = match kind.as_str() {
            "PRODUCTION" => ApplicationEnvironmentKind::Production,
            "TEST" => ApplicationEnvironmentKind::Test,
            _ => ApplicationEnvironmentKind::Sandbox,
        };
        Ok(DelegatedPlatformContext::new(
            call.tenant_id,
            call.application_id,
            call.environment_id,
            kind,
            call.created_by_service_account_id,
        ))
    }
}
