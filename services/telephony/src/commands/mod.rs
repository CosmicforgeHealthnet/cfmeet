//! Durable Telephony commands (Part E §34, §165–§168).
//!
//! Intake validates and PERSISTS a command (and, for ORIGINATE, its leg) in one
//! transaction and answers 202; provider-facing execution happens later in
//! [`executor`]. Same command id / same idempotency key → the same row → at most one
//! provider effect; a different body under the same key is IDEMPOTENCY_CONFLICT.

pub mod executor;
pub mod originate;

use cf_service_auth::DelegatedPlatformContext;
use chrono::Duration;
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use self::originate::OriginateCommand;
use crate::{
    db,
    domain::{CommandKind, EndReason, LegStatus, NormalizedEvent, PublicRouteClass},
    error::{is_unique_violation, ErrorCode, TelephonyError, TelephonyResult},
    legs::{self, Change, LegRow},
    service::TelephonyService,
};

/// The authenticated requester of a command.
#[derive(Debug, Clone)]
pub struct Requester {
    pub service: String,
    pub request_id: String,
    pub correlation_id: String,
    pub delegated: Option<DelegatedPlatformContext>,
}

#[derive(Debug, Clone, FromQueryResult)]
pub(crate) struct CommandRow {
    pub id: Uuid,
    pub kind: String,
    pub telephony_leg_id: Option<Uuid>,
    pub bridge_id: Option<Uuid>,
    pub request_digest: String,
    pub status: String,
    pub attempts: i32,
    pub max_attempts: i32,
    pub transmitted_at: Option<chrono::DateTime<chrono::Utc>>,
    pub grant_id: Option<Uuid>,
    pub grant_secret: Option<String>,
    pub grant_expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub payload: Value,
    pub correlation_id: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// The Numbering CALLER_IDENTITY grant for a DCN origination (scrubbed at transmit).
    pub dcn_identity_grant: Option<String>,
}

pub(crate) const COMMAND_COLUMNS: &str = "id, kind, telephony_leg_id, bridge_id, request_digest, \
    status, attempts, max_attempts, transmitted_at, grant_id, grant_secret, grant_expires_at, payload, \
    correlation_id, created_at, dcn_identity_grant";

/// Body of TERMINATE / HOLD / RESUME.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ControlCommand {
    pub schema_version: String,
    pub command_id: Uuid,
    pub idempotency_key: String,
    pub correlation_id: String,
    #[serde(default)]
    pub reason: Option<String>,
    /// Part F: the BUILT-IN prompt of a PROMPT command (Calling's vocabulary).
    #[serde(default)]
    pub prompt: Option<String>,
    /// Part F: what a CAPTURE command records (Calling's capture id, purpose, bound).
    #[serde(default)]
    pub capture: Option<CaptureSpec>,
}

/// Part F: the body of a CAPTURE command. Never a file name, path, key or URL — the
/// spool name is Telephony's own, derived from the capture id.
#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaptureSpec {
    pub capture_id: Uuid,
    pub purpose: String,
    pub max_duration_seconds: i32,
    pub beep: bool,
}

impl ControlCommand {
    fn validate(&self) -> TelephonyResult<()> {
        if self.schema_version != originate::SCHEMA_VERSION {
            return Err(TelephonyError::Validation("unsupported schemaVersion"));
        }
        let ok = |s: &str, max: usize| {
            !s.is_empty()
                && s.len() <= max
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b':'))
        };
        if !ok(&self.idempotency_key, 128) || !ok(&self.correlation_id, 64) {
            return Err(TelephonyError::Validation(
                "invalid idempotencyKey or correlationId",
            ));
        }
        if let Some(p) = &self.prompt {
            if crate::enterprise::prompt_sound(p).is_none() {
                return Err(TelephonyError::Validation("invalid prompt"));
            }
        }
        if let Some(c) = &self.capture {
            let max = match c.purpose.as_str() {
                "VOICEMAIL" => crate::capture::MAX_VOICEMAIL_SECONDS,
                "RECORDING" => crate::capture::MAX_CAPTURE_SECONDS,
                _ => return Err(TelephonyError::Validation("invalid capture")),
            };
            if !(1..=max).contains(&c.max_duration_seconds) {
                return Err(TelephonyError::Validation("invalid capture"));
            }
        }
        if let Some(r) = &self.reason {
            if !matches!(
                r.as_str(),
                "CALL_ENDED" | "CANCELLED" | "POLICY" | "DURATION_LIMIT" | "ORPHAN"
            ) {
                return Err(TelephonyError::Validation("invalid reason"));
            }
        }
        Ok(())
    }

    fn digest(&self, kind: CommandKind, leg: Uuid) -> String {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(
            format!(
                "{kind}|{leg}|{}|{:?}|{:?}|{:?}",
                self.command_id, self.reason, self.prompt, self.capture
            )
            .as_bytes(),
        ))
    }
}

pub(crate) async fn find_by_key<C: ConnectionTrait>(
    db_: &C,
    service: &str,
    key: &str,
) -> TelephonyResult<Option<CommandRow>> {
    db::one(
        db_,
        &format!(
            "SELECT {COMMAND_COLUMNS} FROM telephony_commands WHERE requester_service = $1 AND idempotency_key = $2"
        ),
        vec![service.into(), key.into()],
    )
    .await
}

impl TelephonyService {
    fn require_flags(&self, class: PublicRouteClass) -> TelephonyResult<()> {
        if !self.flags.telephony.enabled() {
            return Err(TelephonyError::Refused(ErrorCode::TelephonyDisabled));
        }
        if !self.flags.public_outbound.enabled() {
            return Err(TelephonyError::Refused(ErrorCode::PublicOutboundDisabled));
        }
        if class == PublicRouteClass::CallbackRelay && !self.flags.callback_relay.enabled() {
            return Err(TelephonyError::Refused(ErrorCode::CallbackRelayDisabled));
        }
        Ok(())
    }

    /// `POST /internal/v1/legs/originate`.
    pub async fn originate(
        &self,
        requester: &Requester,
        command: OriginateCommand,
    ) -> TelephonyResult<(u16, Value)> {
        let now = self.now();
        command.validate(now)?;
        if cf_service_auth::CorrelationId::parse(&command.correlation_id).is_err() {
            return Err(TelephonyError::Validation("invalid correlationId"));
        }
        // The delegated scope (signed by Calling) must be exactly the command's scope.
        let delegated = requester.delegated.clone().ok_or(TelephonyError::Refused(
            ErrorCode::AuthorizationProofInvalid,
        ))?;
        if delegated.tenant_id() != command.scope.tenant_id
            || delegated.application_id() != command.scope.application_id
            || delegated.application_environment_id() != command.scope.environment_id
        {
            return Err(TelephonyError::Refused(
                ErrorCode::AuthorizationProofInvalid,
            ));
        }
        let digest = command.digest();
        // Replays are answered before the kill switches (a retry of an accepted command
        // must not look like a new paid action).
        if let Some(existing) =
            find_by_key(&self.db, &requester.service, &command.idempotency_key).await?
        {
            return self.replay(&existing, &digest).await;
        }
        crate::metrics::inc(
            "telephony_origination_requests_total",
            &[("route_class", command.call.route_class.as_str())],
        );
        self.require_flags(command.call.route_class)?;
        let txn = self.db.begin().await?;
        let prior: Option<(i32, Option<Uuid>)> = db::one::<GenerationRow, _>(
            &txn,
            "SELECT COALESCE(MAX(generation), 0)::int AS max_generation, \
               (SELECT id FROM telephony_legs WHERE calling_call_leg_id = $1 AND status NOT IN ('ENDED','FAILED') LIMIT 1) AS live \
             FROM telephony_legs WHERE calling_call_leg_id = $1",
            vec![command.call.call_leg_id.into()],
        )
        .await?
        .map(|r| (r.max_generation, r.live));
        // A failover attempt (another CallLeg of the same Call to the same protected
        // party) waits until every earlier attempt is resolved: never beside a live or
        // UNKNOWN one (also a unique index).
        let other_live = db::scalar_i64(
            &txn,
            "SELECT count(*) FROM telephony_legs WHERE calling_call_id = $1 AND contact_reference = $2 \
               AND direction = 'OUTBOUND' AND calling_call_leg_id <> $3 AND status NOT IN ('ENDED', 'FAILED')",
            vec![
                command.call.call_id.into(),
                command.destination.contact_reference.into(),
                command.call.call_leg_id.into(),
            ],
        )
        .await?;
        if other_live > 0 {
            drop(txn);
            if let Some(existing) =
                find_by_key(&self.db, &requester.service, &command.idempotency_key).await?
            {
                return self.replay(&existing, &digest).await;
            }
            crate::metrics::inc(
                "telephony_origination_refusals_total",
                &[("code", "EXECUTION_IN_PROGRESS")],
            );
            return Err(TelephonyError::Refused(ErrorCode::ExecutionInProgress));
        }
        if let Some((max_generation, live)) = prior {
            if live.is_some() {
                drop(txn);
                // A concurrent identical request may have committed meanwhile.
                if let Some(existing) =
                    find_by_key(&self.db, &requester.service, &command.idempotency_key).await?
                {
                    return self.replay(&existing, &digest).await;
                }
                crate::metrics::inc(
                    "telephony_origination_refusals_total",
                    &[("code", "EXECUTION_IN_PROGRESS")],
                );
                return Err(TelephonyError::Refused(ErrorCode::ExecutionInProgress));
            }
            if command.expected_generation <= max_generation {
                return Err(TelephonyError::Refused(ErrorCode::StaleGeneration));
            }
        }
        let leg_id = Uuid::new_v4();
        let mut payload = command.stored_payload();
        payload["delegated"] = serde_json::to_value(&delegated)
            .map_err(|_| TelephonyError::Validation("delegated"))?;
        let insert = async {
            db::exec(
                &txn,
                "INSERT INTO telephony_legs (id, calling_call_id, calling_call_leg_id, calling_attempt_id, \
                   tenant_id, application_id, environment_id, direction, route_class, status, generation, \
                   destination_country, contact_reference, relay_session_id, context_id, privacy_decision_id, \
                   call_authorization_id, spend_reservation_id, spend_reserved_minor, spend_currency, \
                   max_duration_seconds, media_attachment, usage_status, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, 'OUTBOUND', $8, 'PLANNED', $9, NULL, $10, $11, $12, $13, \
                   $14, $15, $16, $17, $18, $19, 'PENDING', $20, $20)",
                vec![
                    leg_id.into(),
                    command.call.call_id.into(),
                    command.call.call_leg_id.into(),
                    command.call.attempt_id.into(),
                    command.scope.tenant_id.into(),
                    command.scope.application_id.into(),
                    command.scope.environment_id.into(),
                    command.call.route_class.as_str().into(),
                    command.expected_generation.into(),
                    command.destination.contact_reference.into(),
                    command.authorization.relay_session_id.into(),
                    command.authorization.context_id.into(),
                    command.authorization.privacy_decision_id.into(),
                    command.authorization.call_authorization_id.into(),
                    command.spend.reservation_id.into(),
                    command.spend.reserved_amount_minor.into(),
                    command.spend.currency.clone().into(),
                    command.spend.max_duration_seconds.into(),
                    command.media.as_str().into(),
                    now.into(),
                ],
            )
            .await?;
            db::exec(
                &txn,
                "INSERT INTO telephony_commands (id, kind, telephony_leg_id, requester_service, idempotency_key, \
                   request_digest, status, max_attempts, next_attempt_at, grant_id, grant_secret, \
                   grant_expires_at, payload, request_id, correlation_id, created_at, updated_at, dcn_identity_grant) \
                 VALUES ($1, 'ORIGINATE', $2, $3, $4, $5, 'PENDING', 5, $6, $7, $8, $9, $10, $11, $12, $6, $6, $13)",
                vec![
                    command.command_id.into(),
                    leg_id.into(),
                    requester.service.clone().into(),
                    command.idempotency_key.clone().into(),
                    digest.clone().into(),
                    now.into(),
                    command.destination.grant_id.into(),
                    command.destination.grant_secret.clone().into(),
                    command.destination.grant_expires_at.into(),
                    payload.into(),
                    requester.request_id.clone().into(),
                    command.correlation_id.clone().into(),
                    command
                        .presented_identity
                        .as_ref()
                        .map(|p| p.grant.clone())
                        .into(),
                ],
            )
            .await
        };
        if let Err(e) = insert.await {
            drop(txn);
            let unique = [
                "uq_telephony_legs_one_live_execution",
                "uq_telephony_legs_one_live_per_party",
                "uq_telephony_legs_attempt",
                "uq_telephony_commands_idempotency",
                "telephony_commands_pkey",
            ]
            .iter()
            .any(|c| is_unique_violation(&e, c));
            if unique {
                // A concurrent identical request may have won the race: answer its
                // result; otherwise report the specific conflict.
                if let Some(existing) =
                    find_by_key(&self.db, &requester.service, &command.idempotency_key).await?
                {
                    return self.replay(&existing, &digest).await;
                }
                if is_unique_violation(&e, "uq_telephony_legs_one_live_execution")
                    || is_unique_violation(&e, "uq_telephony_legs_one_live_per_party")
                {
                    crate::metrics::inc(
                        "telephony_origination_refusals_total",
                        &[("code", "EXECUTION_IN_PROGRESS")],
                    );
                    return Err(TelephonyError::Refused(ErrorCode::ExecutionInProgress));
                }
                if is_unique_violation(&e, "uq_telephony_legs_attempt") {
                    return Err(TelephonyError::Refused(ErrorCode::StaleGeneration));
                }
                return Err(TelephonyError::Refused(ErrorCode::IdempotencyConflict));
            }
            return Err(e);
        }
        txn.commit().await?;
        tracing::info!(
            telephony_leg_id = %leg_id,
            call_id = %command.call.call_id,
            call_leg_id = %command.call.call_leg_id,
            command_id = %command.command_id,
            correlation_id = %command.correlation_id,
            "public leg origination accepted for execution"
        );
        let leg = legs::load(&self.db, leg_id)
            .await?
            .ok_or(TelephonyError::Refused(ErrorCode::LegNotFound))?;
        let mut body = json!({ "leg": leg.view(), "commandId": command.command_id });
        body["leg"]["commandStatus"] = json!("PENDING");
        Ok((202, body))
    }

    async fn replay(&self, existing: &CommandRow, digest: &str) -> TelephonyResult<(u16, Value)> {
        if existing.request_digest != digest {
            return Err(TelephonyError::Refused(ErrorCode::IdempotencyConflict));
        }
        let leg = match existing.telephony_leg_id {
            Some(id) => legs::load(&self.db, id).await?,
            None => None,
        };
        let mut body = json!({ "commandId": existing.id, "replayed": true });
        if let Some(leg) = leg {
            body["leg"] = leg.view();
            body["leg"]["commandStatus"] = json!(existing.status);
        }
        Ok((200, body))
    }

    /// `POST /internal/v1/legs/{id}/terminate|hold|resume`.
    pub async fn control(
        &self,
        requester: &Requester,
        kind: CommandKind,
        leg_id: Uuid,
        command: ControlCommand,
    ) -> TelephonyResult<(u16, Value)> {
        command.validate()?;
        let digest = command.digest(kind, leg_id);
        if let Some(existing) =
            find_by_key(&self.db, &requester.service, &command.idempotency_key).await?
        {
            return self.replay(&existing, &digest).await;
        }
        let now = self.now();
        let txn = self.db.begin().await?;
        let leg = legs::lock(&txn, leg_id).await?;
        let status = leg.status()?;
        // Terminating and stopping a capture are never blocked by a kill switch.
        if !matches!(kind, CommandKind::Terminate | CommandKind::CaptureStop)
            && !self.flags.telephony.enabled()
        {
            return Err(TelephonyError::Refused(ErrorCode::TelephonyDisabled));
        }
        if kind == CommandKind::CaptureStop && !leg.is_enterprise() {
            return Err(TelephonyError::Refused(ErrorCode::LegNotActive));
        }
        // Part F: answer / prompt exist only for internal enterprise legs, behind their
        // own switch (terminating is never blocked).
        if matches!(
            kind,
            CommandKind::Answer | CommandKind::Prompt | CommandKind::Capture
        ) && (!leg.is_enterprise() || !self.flags.enterprise_sip.enabled())
        {
            return Err(TelephonyError::Refused(ErrorCode::LegNotActive));
        }
        if kind == CommandKind::Prompt && command.prompt.is_none() {
            return Err(TelephonyError::Validation("prompt is required"));
        }
        if matches!(kind, CommandKind::Capture | CommandKind::CaptureStop)
            != command.capture.is_some()
        {
            return Err(TelephonyError::Validation(
                "capture is required for (and only for) CAPTURE / CAPTURE_STOP",
            ));
        }
        let allowed = match kind {
            CommandKind::Terminate => true,
            CommandKind::Hold => matches!(status, LegStatus::Connected | LegStatus::Answered),
            CommandKind::Resume => status == LegStatus::Held,
            CommandKind::Answer => matches!(
                status,
                LegStatus::Ringing | LegStatus::Answered | LegStatus::Connected
            ),
            CommandKind::Prompt | CommandKind::Capture => matches!(
                status,
                LegStatus::Ringing | LegStatus::Answered | LegStatus::Connected
            ),
            // Stopping is always accepted (a finished capture makes it a no-op).
            CommandKind::CaptureStop => true,
            _ => false,
        };
        if !allowed {
            return Err(TelephonyError::Refused(ErrorCode::LegNotActive));
        }
        // Terminating a leg that was never transmitted: cancel it here, atomically with
        // its pending ORIGINATE (which then can never reach a provider).
        if kind == CommandKind::Terminate && status == LegStatus::Planned {
            legs::transition_in(
                &txn,
                &leg,
                &Change::ended(NormalizedEvent::LegEnded, EndReason::Cancelled, now),
            )
            .await?;
            db::exec(
                &txn,
                "UPDATE telephony_commands SET status = 'FAILED', grant_secret = NULL, finished_at = $2, \
                   updated_at = $2, result_code = 'CANCELLED', failure_category = 'POLICY', \
                   lease_owner = NULL, lease_until = NULL \
                 WHERE telephony_leg_id = $1 AND kind = 'ORIGINATE' AND status IN ('PENDING', 'CLAIMED')",
                vec![leg.id.into(), now.into()],
            )
            .await?;
        }
        let terminal_now = legs::load(&txn, leg.id)
            .await?
            .map(|l| l.status().map(|s| s.is_terminal()).unwrap_or(false))
            .unwrap_or(false);
        let initial = if terminal_now { "SUCCEEDED" } else { "PENDING" };
        let inserted = db::exec(
            &txn,
            "INSERT INTO telephony_commands (id, kind, telephony_leg_id, requester_service, idempotency_key, \
               request_digest, status, max_attempts, next_attempt_at, payload, request_id, correlation_id, \
               created_at, updated_at, finished_at, result_code) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, 10, $8, $9, $10, $11, $8, $8, \
               CASE WHEN $7 = 'SUCCEEDED' THEN $8 END, CASE WHEN $7 = 'SUCCEEDED' THEN 'ALREADY_ENDED' END)",
            vec![
                command.command_id.into(),
                kind.as_str().into(),
                leg.id.into(),
                requester.service.clone().into(),
                command.idempotency_key.clone().into(),
                digest.clone().into(),
                initial.into(),
                now.into(),
                json!({ "schemaVersion": "1", "reason": command.reason, "prompt": command.prompt,
                        "capture": command.capture })
                .into(),
                requester.request_id.clone().into(),
                command.correlation_id.clone().into(),
            ],
        )
        .await;
        if let Err(e) = inserted {
            drop(txn);
            if let Some(existing) =
                find_by_key(&self.db, &requester.service, &command.idempotency_key).await?
            {
                return self.replay(&existing, &digest).await;
            }
            return Err(e);
        }
        txn.commit().await?;
        let leg = legs::load(&self.db, leg_id)
            .await?
            .ok_or(TelephonyError::Refused(ErrorCode::LegNotFound))?;
        let mut body = json!({ "leg": leg.view(), "commandId": command.command_id });
        body["leg"]["commandStatus"] = json!(initial);
        Ok((if initial == "SUCCEEDED" { 200 } else { 202 }, body))
    }

    /// `GET /internal/v1/legs/{id}`.
    pub async fn leg_view(&self, leg_id: Uuid) -> TelephonyResult<Value> {
        let leg: LegRow = legs::load(&self.db, leg_id)
            .await?
            .ok_or(TelephonyError::Refused(ErrorCode::LegNotFound))?;
        Ok(json!({ "leg": leg.view() }))
    }

    /// Backoff of a command retry.
    pub(crate) fn retry_delay(attempts: i32) -> Duration {
        Duration::milliseconds((500_i64 << attempts.clamp(0, 7)).min(60_000))
    }
}

#[derive(Debug, FromQueryResult)]
struct GenerationRow {
    max_generation: i32,
    live: Option<Uuid>,
}
