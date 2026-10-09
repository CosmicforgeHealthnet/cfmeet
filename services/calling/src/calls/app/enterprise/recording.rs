//! Call recording on ONE canonical Call (Part F §61, §115–§129; ADR 0024).
//!
//! ```text
//! request (participant / administrator) or AUTO_WHEN_AUTHORIZED (system, CONNECTED Call)
//!   ─▶ CallRecordingAuthorization (immutable, server-authoritative): kill switch, an
//!      enterprise Call, the scope's ACTIVE default recording policy and its mode, the
//!      Call's state, the actor, notice (issued by the server to EVERY participant:
//!      a realtime notice to application endpoints, a tone at the edge for SIP legs),
//!      consent (each required participant's own latest decision for THIS Call),
//!      protected storage and a capture path ─▶ ALLOWED / DENIED + bounded reason
//!   ALLOWED ─▶ call_recordings (SIP: CAPTURE at the edge; RTC: the neutral
//!      CallMediaCapture interface) ─▶ RECORDING_STARTED
//! stop: explicit, Call end, policy revocation, consent withdrawal, maximum length,
//!   capture failure ─▶ STOPPING ─▶ capture delivered ─▶ protected storage ─▶ READY
//! worker: ended Calls, deadlines, lost captures, AUTO starts, retention, deletion
//! ```
//!
//! The kill switch (`CALL_RECORDING_ENABLED`) refuses NEW recordings only; stopping,
//! finalizing and deleting are never blocked. Audio never passes through logs or metrics.

use chrono::Duration;
use sea_orm::{
    ConnectionTrait, DatabaseTransaction, EntityTrait, FromQueryResult, QuerySelect,
    TransactionTrait,
};
use serde::Serialize;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::{context_of, exec, ivr::sip_leg, stmt};
use crate::{
    calls::{
        app::{public::CommandKind, scope_of, system_cmd, AppCallService, PendingSignals},
        command::{CallCommandContext, CallScope},
        errors::{CallError, CallResult},
        service::{data, text, EventDraft},
        state::CallState,
    },
    enterprise::{
        ivr::BuiltinPrompt,
        recording::{
            self as config, access_audit, AccessOperation, ConsentDecision, ConsentParticipants,
            PolicyRow, RecordingMode, RecordingRow, AUTHORIZATION_COLUMNS, RECORDING_COLUMNS,
        },
        AdminActor,
    },
    models::{call_legs, calls},
    platform::context::PlatformAuthContext,
    signaling::signals::SignalType,
};

use super::voicemail::{CaptureDelivery, CapturePurpose, CaptureRequest, CapturedMedia};

/// How long a finished recording may take to be delivered before it is FAILED.
const DELIVERY_WINDOW_MINUTES: i64 = 30;

/// Who asks for a recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingActor {
    /// The application acting for one principal, who must be a participant of the Call.
    Principal(Uuid),
    /// A recording administrator (`enterprise.recording.manage` without a principal).
    Administrator,
    /// The platform itself (AUTO_WHEN_AUTHORIZED).
    System,
}

/// The result of a start request.
#[derive(Debug, Clone)]
pub enum StartOutcome {
    Started(config::CallRecordingView),
    /// The Call is already being recorded (idempotent start: nothing new happened).
    AlreadyActive(config::CallRecordingView),
    /// Not authorized; the decision (with its reason) is persisted and returned.
    Denied(config::RecordingAuthorizationView),
}

/// A participant's recorded consent decision.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingConsentView {
    pub consent_id: Uuid,
    pub call_id: Uuid,
    pub participant_id: Uuid,
    pub decision: String,
    pub recorded_at: String,
}

#[derive(Debug, Clone, FromQueryResult)]
struct ParticipantRow {
    id: Uuid,
    principal_id: Uuid,
    role: String,
    /// The participant reaches the Call through an application endpoint.
    application: bool,
}

#[derive(Debug, Clone, FromQueryResult)]
struct ConsentRow {
    id: Uuid,
    participant_id: Uuid,
    decision: String,
    recorded_at: chrono::DateTime<chrono::Utc>,
}

/// Why a recording is (not) authorized, and the evidence.
struct Evaluation {
    policy: Option<PolicyRow>,
    /// The highest consent sequence of the Call at decision time.
    consent_watermark: i64,
    required: Vec<Uuid>,
    notice: Value,
    consent: Value,
    capture: Result<Capture, &'static str>,
}

#[derive(Debug, Clone)]
enum Capture {
    Sip(Box<call_legs::Model>),
    Application,
}

fn actor_kind(actor: RecordingActor) -> &'static str {
    match actor {
        RecordingActor::Principal(_) => "PRINCIPAL",
        RecordingActor::Administrator => "ADMINISTRATOR",
        RecordingActor::System => "SYSTEM",
    }
}

async fn active_participants<C: ConnectionTrait>(
    db: &C,
    call_id: Uuid,
) -> CallResult<Vec<ParticipantRow>> {
    Ok(ParticipantRow::find_by_statement(stmt(
        "SELECT p.id, p.principal_id, p.role, \
           EXISTS (SELECT 1 FROM call_sessions s WHERE s.participant_id = p.id \
                     AND s.endpoint_id IS NOT NULL) AS application \
         FROM call_participants p WHERE p.call_id = $1 AND p.ended_at IS NULL \
         ORDER BY p.created_at, p.id",
        vec![call_id.into()],
    ))
    .all(db)
    .await?)
}

/// The latest consent decision of each participant of `call_id`.
async fn latest_consents<C: ConnectionTrait>(db: &C, call_id: Uuid) -> CallResult<Vec<ConsentRow>> {
    Ok(ConsentRow::find_by_statement(stmt(
        "SELECT DISTINCT ON (participant_id) id, participant_id, decision, recorded_at \
           FROM call_recording_consents WHERE call_id = $1 \
          ORDER BY participant_id, seq DESC",
        vec![call_id.into()],
    ))
    .all(db)
    .await?)
}

async fn active_recording<C: ConnectionTrait>(
    db: &C,
    call_id: Uuid,
    lock: bool,
) -> CallResult<Option<RecordingRow>> {
    Ok(RecordingRow::find_by_statement(stmt(
        &format!(
            "SELECT {RECORDING_COLUMNS} FROM call_recordings WHERE call_id = $1 \
               AND state IN ('STARTING', 'RECORDING', 'STOPPING'){}",
            if lock { " FOR UPDATE" } else { "" }
        ),
        vec![call_id.into()],
    ))
    .one(db)
    .await?)
}

async fn recording_by_id<C: ConnectionTrait>(
    db: &C,
    id: Uuid,
    lock: bool,
) -> CallResult<Option<RecordingRow>> {
    Ok(RecordingRow::find_by_statement(stmt(
        &format!(
            "SELECT {RECORDING_COLUMNS} FROM call_recordings WHERE id = $1{}",
            if lock { " FOR UPDATE" } else { "" }
        ),
        vec![id.into()],
    ))
    .one(db)
    .await?)
}

fn enterprise_error(e: crate::enterprise::EnterpriseError) -> CallError {
    super::enterprise_error(e)
}

impl AppCallService {
    /// A participant's consent decision for ONE Call, taken through that participant's
    /// own endpoint (an endpoint with a session of an active participant of this Call).
    /// Replaying the same decision is idempotent; a DECLINED by a participant whose
    /// consent a running recording relies on stops it (CONSENT_WITHDRAWN).
    pub async fn record_recording_consent(
        &self,
        ctx: &PlatformAuthContext,
        call_id: Uuid,
        endpoint_id: Uuid,
        decision: ConsentDecision,
    ) -> CallResult<RecordingConsentView> {
        let scope = CallScope::of_platform(ctx);
        let txn = self.db().begin().await?;
        let Some(call) = self.scoped_call(&txn, scope, call_id, true).await? else {
            return Err(CallError::NotFound);
        };
        #[derive(FromQueryResult)]
        struct Participant {
            id: Uuid,
            principal_id: Uuid,
        }
        let participant = Participant::find_by_statement(stmt(
            "SELECT p.id, p.principal_id FROM call_participants p \
               JOIN call_sessions s ON s.participant_id = p.id AND s.endpoint_id = $2 \
              WHERE p.call_id = $1 AND p.ended_at IS NULL LIMIT 1",
            vec![call_id.into(), endpoint_id.into()],
        ))
        .one(&txn)
        .await?
        .ok_or(CallError::NotFound)?;
        if call.current_state.is_terminal() {
            return Err(CallError::Conflict("the call is over"));
        }
        let now = self.calls.now();
        let latest = latest_consents(&txn, call_id)
            .await?
            .into_iter()
            .find(|c| c.participant_id == participant.id);
        let row = match latest {
            // A replayed decision changes nothing.
            Some(c) if c.decision == decision.as_str() => c,
            _ => {
                let id = Uuid::new_v4();
                exec(
                    &txn,
                    "INSERT INTO call_recording_consents (id, tenant_id, application_id, environment_id, \
                       call_id, participant_id, principal_id, endpoint_id, decision, request_id, \
                       correlation_id, recorded_at) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
                    vec![
                        id.into(),
                        call.tenant_id.into(),
                        call.application_id.into(),
                        call.environment_id.into(),
                        call_id.into(),
                        participant.id.into(),
                        participant.principal_id.into(),
                        endpoint_id.into(),
                        decision.as_str().into(),
                        ctx.request_id().as_str().into(),
                        ctx.correlation_id().as_str().into(),
                        now.into(),
                    ],
                )
                .await?;
                crate::metrics::inc(
                    "calling_enterprise_recording_consents_total",
                    &[("decision", decision.as_str())],
                );
                ConsentRow {
                    id,
                    participant_id: participant.id,
                    decision: decision.as_str().to_string(),
                    recorded_at: now,
                }
            }
        };
        // A withdrawal stops a recording that relies on this participant's consent.
        let mut stop_app_capture = None;
        let mut pending = PendingSignals::default();
        if decision == ConsentDecision::Declined {
            if let Some(active) = active_recording(&txn, call_id, true).await? {
                let required: Option<Vec<Uuid>> = txn
                    .query_one(stmt(
                        "SELECT required_participant_ids FROM call_recording_authorizations WHERE id = $1",
                        vec![active.authorization_id.into()],
                    ))
                    .await?
                    .map(|r| r.try_get_by_index(0))
                    .transpose()?;
                if required.unwrap_or_default().contains(&participant.id)
                    && active.state != "STOPPING"
                {
                    let cmd = CallCommandContext::customer(ctx);
                    self.stop_in(
                        &txn,
                        &cmd,
                        &call,
                        &active,
                        "CONSENT_WITHDRAWN",
                        &mut pending,
                    )
                    .await?;
                    if active.capture_source == "APPLICATION" {
                        stop_app_capture = Some(active.id);
                    }
                }
            }
        }
        txn.commit().await?;
        self.publish(pending).await;
        if let Some(id) = stop_app_capture {
            let _ = self.capture.stop(call_id, id).await;
        }
        // AUTO_WHEN_AUTHORIZED: a new consent may complete the evidence.
        if decision == ConsentDecision::Granted && call.current_state == CallState::Connected {
            self.try_auto_start(call_id).await;
        }
        Ok(RecordingConsentView {
            consent_id: row.id,
            call_id,
            participant_id: row.participant_id,
            decision: row.decision,
            recorded_at: crate::enterprise::time(row.recorded_at),
        })
    }

    async fn scoped_call<C: ConnectionTrait>(
        &self,
        db: &C,
        scope: CallScope,
        call_id: Uuid,
        lock: bool,
    ) -> CallResult<Option<calls::Model>> {
        let mut q = calls::Entity::find_by_id(call_id);
        if lock {
            q = q.lock_exclusive();
        }
        Ok(q.one(db).await?.filter(|c| scope_of(c) == scope))
    }

    /// Requests a recording of `call_id` (idempotent while one is active).
    pub async fn start_recording(
        &self,
        ctx: Option<&PlatformAuthContext>,
        scope: CallScope,
        call_id: Uuid,
        actor: RecordingActor,
    ) -> CallResult<StartOutcome> {
        let txn = self.db().begin().await?;
        let Some(call) = self.scoped_call(&txn, scope, call_id, true).await? else {
            return Err(CallError::NotFound);
        };
        if let Some(active) = active_recording(&txn, call_id, false).await? {
            txn.commit().await?;
            return Ok(StartOutcome::AlreadyActive(config::recording_view(active)));
        }
        let cmd = match ctx {
            Some(ctx) => CallCommandContext::customer(ctx),
            None => system_cmd(&call),
        };
        let now = self.calls.now();
        let evaluation = self.evaluate_recording(&txn, &call, actor).await?;
        let (decision, reason) = match &evaluation.capture {
            Ok(_) => ("ALLOWED", "AUTHORIZED"),
            Err(reason) => ("DENIED", *reason),
        };
        let authorization_id = Uuid::new_v4();
        let policy = evaluation.policy.as_ref();
        exec(
            &txn,
            "INSERT INTO call_recording_authorizations (id, tenant_id, application_id, environment_id, \
               call_id, policy_id, policy_version, mode, jurisdiction_ref, actor_kind, actor_principal_id, \
               actor_credential_id, required_participant_ids, notice_evidence, consent_evidence, decision, \
               reason, request_id, correlation_id, created_at, consent_watermark) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21)",
            vec![
                authorization_id.into(),
                call.tenant_id.into(),
                call.application_id.into(),
                call.environment_id.into(),
                call.id.into(),
                policy.map(|p| p.id).into(),
                policy.map(|p| p.version).into(),
                policy.map(|p| p.mode.clone()).unwrap_or_else(|| "NONE".into()).into(),
                policy.map(|p| p.jurisdiction_ref.clone()).into(),
                actor_kind(actor).into(),
                match actor {
                    RecordingActor::Principal(p) => Some(p),
                    _ => None,
                }
                .into(),
                match (actor, ctx) {
                    (RecordingActor::System, _) | (_, None) => None,
                    (_, Some(ctx)) => Some(ctx.credential_id().as_uuid()),
                }
                .into(),
                evaluation.required.clone().into(),
                evaluation.notice.clone().into(),
                evaluation.consent.clone().into(),
                decision.into(),
                reason.into(),
                cmd.request_id().as_str().into(),
                cmd.correlation_id().as_str().into(),
                now.into(),
                evaluation.consent_watermark.into(),
            ],
        )
        .await?;
        crate::metrics::inc(
            "calling_enterprise_recording_decisions_total",
            &[("decision", decision), ("reason", reason)],
        );
        let (Ok(capture), Some(policy)) = (evaluation.capture, evaluation.policy) else {
            let row = config::AuthorizationRow::find_by_statement(stmt(
                &format!(
                    "SELECT {AUTHORIZATION_COLUMNS} FROM call_recording_authorizations WHERE id = $1"
                ),
                vec![authorization_id.into()],
            ))
            .one(&txn)
            .await?
            .ok_or(CallError::NotFound)?;
            txn.commit().await?;
            tracing::info!(%call_id, reason, "call recording denied");
            return Ok(StartOutcome::Denied(config::authorization_view(row)));
        };
        // ALLOWED: the recording, the notice to every participant, the capture.
        let recording_id = Uuid::new_v4();
        let (source, leg_id, state) = match &capture {
            Capture::Sip(leg) => ("SIP", Some(leg.id), "RECORDING"),
            Capture::Application => ("APPLICATION", None, "STARTING"),
        };
        exec(
            &txn,
            "INSERT INTO call_recordings (id, tenant_id, application_id, environment_id, call_id, \
               authorization_id, policy_id, capture_source, call_leg_id, state, started_at, \
               recording_deadline, delete_attempts, version, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, 0, 1, $11, $11)",
            vec![
                recording_id.into(),
                call.tenant_id.into(),
                call.application_id.into(),
                call.environment_id.into(),
                call.id.into(),
                authorization_id.into(),
                policy.id.into(),
                source.into(),
                leg_id.into(),
                state.into(),
                now.into(),
                (now + Duration::seconds(policy.max_recording_seconds as i64)).into(),
            ],
        )
        .await?;
        self.calls
            .append_event(
                &txn,
                &cmd,
                &call,
                EventDraft::fact(
                    crate::calls::events::CallEventType::RecordingStarted,
                    data(vec![
                        ("recording_id", text(recording_id)),
                        ("authorization_id", text(authorization_id)),
                        ("capture_source", text(source)),
                        ("notice_required", policy.notice_required.into()),
                    ])?,
                ),
                now,
            )
            .await?;
        let mut pending = PendingSignals::default();
        let mut notice = Map::new();
        notice.insert("recordingId".into(), json!(recording_id));
        notice.insert("noticeRequired".into(), json!(policy.notice_required));
        self.call_signal(
            &txn,
            &mut pending,
            SignalType::RecordingStarted,
            &call,
            cmd.correlation_id().as_str(),
            notice,
        )
        .await?;
        if let Capture::Sip(leg) = &capture {
            if policy.notice_required {
                // The edge notice: a tone before the capture (commands of one leg run in
                // order; the capture key sorts after the prompt keys).
                self.enqueue_sip_prompt_in(&txn, &call, leg, BuiltinPrompt::Beep, 0)
                    .await?;
            }
            crate::calls::app::public::enqueue_in(
                &txn,
                &call,
                CommandKind::Capture,
                Some(leg.id),
                None,
                None,
                &format!("record:{}:{recording_id}", leg.id),
                json!({
                    "correlationId": cmd.correlation_id().as_str(),
                    "capture": {
                        "captureId": recording_id,
                        "purpose": "RECORDING",
                        "maxDurationSeconds": policy.max_recording_seconds,
                        "beep": false,
                    },
                }),
                now,
            )
            .await?;
        }
        let view = config::recording_view(
            recording_by_id(&txn, recording_id, false)
                .await?
                .ok_or(CallError::NotFound)?,
        );
        txn.commit().await?;
        self.publish(pending).await;
        crate::metrics::inc(
            "calling_enterprise_recordings_total",
            &[("result", "STARTED"), ("source", source)],
        );
        tracing::info!(%call_id, recording_id = %recording_id, "call recording started");
        if matches!(capture, Capture::Application) {
            return self
                .start_application_recording(&call, recording_id, policy.max_recording_seconds)
                .await;
        }
        Ok(StartOutcome::Started(view))
    }

    /// After commit: an application capture is started through the neutral interface; a
    /// refusal FAILS the recording (the Call is unaffected).
    async fn start_application_recording(
        &self,
        call: &calls::Model,
        recording_id: Uuid,
        max_seconds: i32,
    ) -> CallResult<StartOutcome> {
        let started = self
            .capture
            .start(CaptureRequest {
                capture_id: recording_id,
                call_id: call.id,
                purpose: CapturePurpose::Recording,
                max_duration_seconds: max_seconds,
                // Authorized for the whole Call: its mixed audio.
                sources: Vec::new(),
            })
            .await;
        match started {
            Ok(()) => {
                exec(
                    self.db(),
                    "UPDATE call_recordings SET state = 'RECORDING', version = version + 1, \
                     updated_at = GREATEST(updated_at, $2) WHERE id = $1 AND state = 'STARTING'",
                    vec![recording_id.into(), self.calls.now().into()],
                )
                .await?;
            }
            Err(code) => {
                self.recording_capture_failed(recording_id, code).await?;
            }
        }
        let row = recording_by_id(self.db(), recording_id, false)
            .await?
            .ok_or(CallError::NotFound)?;
        Ok(StartOutcome::Started(config::recording_view(row)))
    }

    /// The authorization rules (inside the Call's transaction).
    async fn evaluate_recording(
        &self,
        txn: &DatabaseTransaction,
        call: &calls::Model,
        actor: RecordingActor,
    ) -> CallResult<Evaluation> {
        let scope = scope_of(call);
        let consent_watermark: i64 = txn
            .query_one(stmt(
                "SELECT COALESCE(max(seq), 0) FROM call_recording_consents WHERE call_id = $1",
                vec![call.id.into()],
            ))
            .await?
            .map(|r| r.try_get_by_index(0))
            .transpose()?
            .unwrap_or(0);
        let mut out = Evaluation {
            consent_watermark,
            policy: None,
            required: Vec::new(),
            notice: json!({ "required": false }),
            consent: json!({ "required": false, "participants": [] }),
            capture: Err("RECORDING_DISABLED"),
        };
        // Kill switch: no NEW recording (stopping / finalizing / deleting continue).
        if !self.enterprise_flags.call_recording {
            return Ok(out);
        }
        if context_of(txn, call.id).await?.is_none() {
            out.capture = Err("NOT_AN_ENTERPRISE_CALL");
            return Ok(out);
        }
        let policy = config::effective_recording_policy(txn, scope)
            .await
            .map_err(enterprise_error)?;
        let Some(policy) = policy else {
            out.capture = Err("NO_RECORDING_POLICY");
            return Ok(out);
        };
        let mode = policy.mode();
        out.notice = json!({ "required": policy.notice_required });
        out.consent = json!({ "required": policy.consent_required, "participants": [] });
        out.policy = Some(policy.clone());
        match (mode, actor) {
            (RecordingMode::Disabled, _) => {
                out.capture = Err("POLICY_DISABLED");
                return Ok(out);
            }
            (RecordingMode::ManualAllowed, RecordingActor::System) => {
                out.capture = Err("AUTO_RECORDING_NOT_CONFIGURED");
                return Ok(out);
            }
            _ => {}
        }
        if call.current_state != CallState::Connected {
            out.capture = Err("CALL_STATE_NOT_ELIGIBLE");
            return Ok(out);
        }
        let participants = active_participants(txn, call.id).await?;
        let actor_participant = match actor {
            RecordingActor::Principal(p) => {
                match participants.iter().find(|x| x.principal_id == p) {
                    Some(x) => Some(x.id),
                    None => {
                        out.capture = Err("ACTOR_NOT_PARTICIPANT");
                        return Ok(out);
                    }
                }
            }
            _ => None,
        };
        // The SIP device's participant (an enterprise device dialog's caller).
        let sip = sip_leg(txn, call)
            .await?
            .filter(|leg| !leg.state.is_terminal());
        let sip_participant = match &sip {
            Some(_) => participants
                .iter()
                .find(|p| p.role == "INITIATOR")
                .map(|p| p.id),
            None => None,
        };
        // Notice: issued by the server to EVERY participant, through a channel it controls.
        if policy.notice_required {
            let mut deliveries = Vec::new();
            for p in &participants {
                let channel = if Some(p.id) == sip_participant {
                    "EDGE_TONE"
                } else if p.application {
                    "APPLICATION_SIGNAL"
                } else {
                    out.notice = json!({ "required": true, "deliveries": deliveries });
                    out.capture = Err("NOTICE_UNDELIVERABLE");
                    return Ok(out);
                };
                deliveries.push(json!({ "participantId": p.id, "channel": channel }));
            }
            out.notice = json!({ "required": true, "deliveries": deliveries });
        }
        // Consent: each required participant's own latest decision for THIS Call.
        let required: Vec<Uuid> = match policy.consent_participants() {
            ConsentParticipants::Nobody => Vec::new(),
            ConsentParticipants::AllParticipants => participants.iter().map(|p| p.id).collect(),
            ConsentParticipants::OtherParticipants => participants
                .iter()
                .map(|p| p.id)
                .filter(|id| Some(*id) != actor_participant)
                .collect(),
        };
        out.required = required.clone();
        if !required.is_empty() {
            let consents = latest_consents(txn, call.id).await?;
            let mut evidence = Vec::new();
            let mut missing = None;
            for id in &required {
                match consents.iter().find(|c| c.participant_id == *id) {
                    Some(c) => {
                        evidence.push(json!({ "participantId": id, "consentId": c.id,
                                               "decision": c.decision }));
                        if c.decision != "GRANTED" {
                            missing = missing.or(Some("CONSENT_DECLINED"));
                        }
                    }
                    None => {
                        evidence.push(json!({ "participantId": id, "consentId": null,
                                               "decision": null }));
                        missing = missing.or(Some("CONSENT_MISSING"));
                    }
                }
            }
            out.consent = json!({ "required": true, "participants": evidence });
            if let Some(reason) = missing {
                out.capture = Err(reason);
                return Ok(out);
            }
        }
        if !self.artifacts.configured() {
            out.capture = Err("ARTIFACT_STORAGE_UNAVAILABLE");
            return Ok(out);
        }
        out.capture = match sip {
            Some(leg) => Ok(Capture::Sip(Box::new(leg))),
            None if self.capture.available() => Ok(Capture::Application),
            None => Err("CAPTURE_UNAVAILABLE"),
        };
        Ok(out)
    }

    /// Moves an active recording to STOPPING with `reason` (inside the Call's
    /// transaction): event, notice, and the edge stop for a SIP capture.
    async fn stop_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        recording: &RecordingRow,
        reason: &'static str,
        pending: &mut PendingSignals,
    ) -> CallResult<()> {
        let now = self.calls.now();
        let changed = exec(
            txn,
            "UPDATE call_recordings SET state = 'STOPPING', stop_reason = $2, stop_requested_at = $3, \
             version = version + 1, updated_at = GREATEST(updated_at, $3) \
             WHERE id = $1 AND state IN ('STARTING', 'RECORDING')",
            vec![recording.id.into(), reason.into(), now.into()],
        )
        .await?;
        if changed == 0 {
            return Ok(());
        }
        self.calls
            .append_event(
                txn,
                cmd,
                call,
                EventDraft::fact(
                    crate::calls::events::CallEventType::RecordingStopped,
                    data(vec![
                        ("recording_id", text(recording.id)),
                        ("reason", text(reason)),
                    ])?,
                ),
                now,
            )
            .await?;
        let mut stopped = Map::new();
        stopped.insert("recordingId".into(), json!(recording.id));
        self.call_signal(
            txn,
            pending,
            SignalType::RecordingStopped,
            call,
            cmd.correlation_id().as_str(),
            stopped,
        )
        .await?;
        // A live Call's SIP capture is stopped at the edge; an ending Call's leg ends it.
        if let (Some(leg), false) = (
            recording.call_leg_id,
            call.current_state.is_terminal() || call.current_state == CallState::Ending,
        ) {
            crate::calls::app::public::enqueue_in(
                txn,
                call,
                CommandKind::CaptureStop,
                Some(leg),
                None,
                None,
                &format!("stop-record:{leg}:{}", recording.id),
                json!({
                    "correlationId": cmd.correlation_id().as_str(),
                    "capture": {
                        "captureId": recording.id,
                        "purpose": "RECORDING",
                        "maxDurationSeconds": 60,
                        "beep": false,
                    },
                }),
                now,
            )
            .await?;
        }
        crate::metrics::inc(
            "calling_enterprise_recording_stops_total",
            &[("reason", reason)],
        );
        Ok(())
    }

    /// Stops a running recording (participant of the Call or recording administrator).
    pub async fn stop_recording(
        &self,
        ctx: &PlatformAuthContext,
        call_id: Uuid,
        recording_id: Uuid,
        actor: RecordingActor,
    ) -> CallResult<config::CallRecordingView> {
        let scope = CallScope::of_platform(ctx);
        let admin = AdminActor::of(ctx);
        let txn = self.db().begin().await?;
        let Some(call) = self.scoped_call(&txn, scope, call_id, true).await? else {
            return Err(CallError::NotFound);
        };
        let Some(recording) = recording_by_id(&txn, recording_id, true)
            .await?
            .filter(|r| r.call_id == call_id)
        else {
            return Err(CallError::NotFound);
        };
        let now = self.calls.now();
        if let RecordingActor::Principal(p) = actor {
            if !config::was_participant(&txn, call_id, p)
                .await
                .map_err(enterprise_error)?
            {
                drop(txn);
                access_audit(
                    self.db(),
                    &admin,
                    recording_id,
                    call_id,
                    Some(p),
                    AccessOperation::Stop,
                    false,
                    Some("NOT_A_PARTICIPANT"),
                    now,
                )
                .await
                .map_err(enterprise_error)?;
                return Err(CallError::NotFound);
            }
        }
        if !matches!(recording.state.as_str(), "STARTING" | "RECORDING") {
            return Err(CallError::Conflict("the recording is not running"));
        }
        let cmd = CallCommandContext::customer(ctx);
        let mut pending = PendingSignals::default();
        self.stop_in(&txn, &cmd, &call, &recording, "EXPLICIT", &mut pending)
            .await?;
        access_audit(
            &txn,
            &admin,
            recording_id,
            call_id,
            match actor {
                RecordingActor::Principal(p) => Some(p),
                _ => None,
            },
            AccessOperation::Stop,
            true,
            None,
            now,
        )
        .await
        .map_err(enterprise_error)?;
        let row = recording_by_id(&txn, recording_id, false)
            .await?
            .ok_or(CallError::NotFound)?;
        txn.commit().await?;
        self.publish(pending).await;
        if recording.capture_source == "APPLICATION" {
            let _ = self.capture.stop(call_id, recording_id).await;
        }
        Ok(config::recording_view(row))
    }

    /// A recording policy stopped permitting recording: every active recording under it
    /// stops (POLICY_REVOKED). Never blocked by kill switches.
    pub async fn revoke_recordings_of_policy(
        &self,
        scope: CallScope,
        policy_id: Uuid,
    ) -> CallResult<usize> {
        let ids: Vec<(Uuid, Uuid)> = self
            .db()
            .query_all(stmt(
                "SELECT id, call_id FROM call_recordings WHERE tenant_id = $1 AND application_id = $2 \
                   AND environment_id = $3 AND policy_id = $4 AND state IN ('STARTING', 'RECORDING')",
                vec![
                    scope.tenant_id().as_uuid().into(),
                    scope.application_id().as_uuid().into(),
                    scope.environment_id().as_uuid().into(),
                    policy_id.into(),
                ],
            ))
            .await?
            .into_iter()
            .map(|r| Ok((r.try_get_by_index(0)?, r.try_get_by_index(1)?)))
            .collect::<Result<_, sea_orm::DbErr>>()?;
        let mut stopped = 0;
        for (id, call_id) in ids {
            if self.system_stop(call_id, id, "POLICY_REVOKED").await? {
                stopped += 1;
            }
        }
        Ok(stopped)
    }

    /// Stops a recording as the system; returns whether it was running.
    async fn system_stop(
        &self,
        call_id: Uuid,
        recording_id: Uuid,
        reason: &'static str,
    ) -> CallResult<bool> {
        let txn = self.db().begin().await?;
        let Some(call) = calls::Entity::find_by_id(call_id)
            .lock_exclusive()
            .one(&txn)
            .await?
        else {
            return Ok(false);
        };
        let Some(recording) = recording_by_id(&txn, recording_id, true).await? else {
            return Ok(false);
        };
        if !matches!(recording.state.as_str(), "STARTING" | "RECORDING") {
            return Ok(false);
        }
        let mut pending = PendingSignals::default();
        self.stop_in(
            &txn,
            &system_cmd(&call),
            &call,
            &recording,
            reason,
            &mut pending,
        )
        .await?;
        txn.commit().await?;
        self.publish(pending).await;
        if recording.capture_source == "APPLICATION" {
            let _ = self.capture.stop(call_id, recording_id).await;
        }
        Ok(true)
    }

    /// AUTO_WHEN_AUTHORIZED: tries a system start (a denial is recorded with its reason).
    pub(crate) async fn try_auto_start(&self, call_id: Uuid) {
        let Ok(Some(call)) = calls::Entity::find_by_id(call_id).one(self.db()).await else {
            return;
        };
        let auto = config::effective_recording_policy(self.db(), scope_of(&call))
            .await
            .ok()
            .flatten()
            .is_some_and(|p| p.mode() == RecordingMode::AutoWhenAuthorized);
        if !auto || !self.enterprise_flags.call_recording {
            return;
        }
        if let Err(error) = self
            .start_recording(None, scope_of(&call), call_id, RecordingActor::System)
            .await
        {
            tracing::warn!(%call_id, %error, "automatic recording attempt failed; retried by the worker");
        }
    }

    /// Whether `capture_id` is a SIP recording of `call_id` on leg `call_leg_id`.
    pub(crate) async fn is_sip_recording(
        &self,
        capture_id: Uuid,
        call_id: Uuid,
        call_leg_id: Uuid,
    ) -> CallResult<bool> {
        Ok(self
            .db()
            .query_one(stmt(
                "SELECT 1 FROM call_recordings WHERE id = $1 AND call_id = $2 \
                   AND call_leg_id = $3 AND capture_source = 'SIP'",
                vec![capture_id.into(), call_id.into(), call_leg_id.into()],
            ))
            .await?
            .is_some())
    }

    /// A recording's captured audio arrived (integrity already verified by the caller):
    /// PROCESSING → protected storage → READY. Storage failure keeps it PROCESSING and
    /// asks the deliverer to retry.
    pub(crate) async fn deliver_recording_capture(
        &self,
        recording_id: Uuid,
        call_id: Uuid,
        media: CapturedMedia,
        digest: String,
    ) -> CallResult<CaptureDelivery> {
        let Some(recording) = recording_by_id(self.db(), recording_id, false).await? else {
            return Err(CallError::NotFound);
        };
        if recording.call_id != call_id {
            return Err(CallError::NotFound);
        }
        match recording.state.as_str() {
            "READY" if recording.sha256.as_deref() == Some(digest.as_str()) => {
                return Ok(CaptureDelivery::AlreadyStored)
            }
            "READY" | "DELETING" | "DELETED" => {
                return Err(CallError::Conflict("the recording is already finalized"))
            }
            _ => {}
        }
        let now = self.calls.now();
        exec(
            self.db(),
            "UPDATE call_recordings SET state = 'PROCESSING', \
               stopped_at = COALESCE(stopped_at, $2), stop_reason = COALESCE(stop_reason, 'CALL_ENDED'), \
               version = version + 1, updated_at = GREATEST(updated_at, $2) \
             WHERE id = $1 AND state IN ('STARTING', 'RECORDING', 'STOPPING')",
            vec![recording_id.into(), now.into()],
        )
        .await?;
        let call = calls::Entity::find_by_id(call_id)
            .one(self.db())
            .await?
            .ok_or(CallError::NotFound)?;
        let scope = scope_of(&call);
        // Validated by `deliver_capture` (audio/wav or a verified audio/ogg).
        let content_type = media.content_type.clone();
        let key = config::object_key(scope, recording_id, &content_type);
        let stored = match self.artifacts.put(&key, media.bytes, &content_type).await {
            Ok(s) if s.sha256 == digest => s,
            outcome => {
                let code = match outcome {
                    Ok(_) | Err(cf_object_storage::StorageError::Integrity) => "STORAGE_INTEGRITY",
                    Err(cf_object_storage::StorageError::NotConfigured) => "STORAGE_NOT_CONFIGURED",
                    Err(_) => "STORAGE_UNAVAILABLE",
                };
                exec(
                    self.db(),
                    "UPDATE call_recordings SET failure_code = $2, version = version + 1, \
                     updated_at = GREATEST(updated_at, $3) WHERE id = $1 AND state IN ('PROCESSING', 'FAILED')",
                    vec![recording_id.into(), code.into(), now.into()],
                )
                .await?;
                crate::metrics::inc(
                    "calling_enterprise_capture_deliveries_total",
                    &[("result", code)],
                );
                tracing::warn!(%recording_id, code, "recording artifact not stored; the deliverer retries");
                return Err(CallError::ArtifactStorageUnavailable);
            }
        };
        let retention_days: i32 = self
            .db()
            .query_one(stmt(
                "SELECT retention_days FROM call_recording_policies WHERE id = $1",
                vec![recording.policy_id.into()],
            ))
            .await?
            .map(|r| r.try_get_by_index(0))
            .transpose()?
            .unwrap_or(30);
        let txn = self.db().begin().await?;
        let call = calls::Entity::find_by_id(call_id)
            .lock_exclusive()
            .one(&txn)
            .await?
            .ok_or(CallError::NotFound)?;
        let Some(current) = recording_by_id(&txn, recording_id, true).await? else {
            return Err(CallError::NotFound);
        };
        if !matches!(current.state.as_str(), "PROCESSING" | "FAILED") {
            txn.rollback().await?;
            return Ok(CaptureDelivery::AlreadyStored);
        }
        let now = self.calls.now();
        exec(
            &txn,
            "UPDATE call_recordings SET state = 'READY', object_key = $2, content_type = $3, \
               size_bytes = $4, duration_ms = $5, sha256 = $6, encrypted = $7, ready_at = $8, \
               retention_until = $9, failed_at = NULL, failure_code = NULL, \
               stopped_at = COALESCE(stopped_at, $8), version = version + 1, \
               updated_at = GREATEST(updated_at, $8) WHERE id = $1",
            vec![
                recording_id.into(),
                key.into(),
                content_type.into(),
                (stored.size as i64).into(),
                media.duration_ms.into(),
                stored.sha256.clone().into(),
                stored.encrypted.into(),
                now.into(),
                (now + Duration::days(retention_days as i64)).into(),
            ],
        )
        .await?;
        self.calls
            .append_event(
                &txn,
                &system_cmd(&call),
                &call,
                EventDraft::fact(
                    crate::calls::events::CallEventType::RecordingReady,
                    data(vec![
                        ("recording_id", text(recording_id)),
                        (
                            "duration_ms",
                            crate::calls::attributes::SafeValue::from(
                                media.duration_ms.max(0) as usize
                            ),
                        ),
                    ])?,
                ),
                now,
            )
            .await?;
        txn.commit().await?;
        crate::metrics::inc(
            "calling_enterprise_capture_deliveries_total",
            &[("result", "STORED")],
        );
        crate::metrics::inc(
            "calling_enterprise_recordings_total",
            &[("result", "READY")],
        );
        tracing::info!(%recording_id, %call_id, "call recording ready");
        Ok(CaptureDelivery::Stored)
    }

    /// The capture of a recording failed (bounded code): FAILED (the Call is unaffected).
    pub(crate) async fn recording_capture_failed(
        &self,
        recording_id: Uuid,
        code: &str,
    ) -> CallResult<()> {
        let code: String = code
            .chars()
            .filter(|c| c.is_ascii_uppercase() || *c == '_')
            .take(64)
            .collect();
        let code = if code.len() < 2 {
            "CAPTURE_FAILED".to_string()
        } else {
            code
        };
        let Some(recording) = recording_by_id(self.db(), recording_id, false).await? else {
            return Err(CallError::NotFound);
        };
        let txn = self.db().begin().await?;
        let Some(call) = calls::Entity::find_by_id(recording.call_id)
            .lock_exclusive()
            .one(&txn)
            .await?
        else {
            return Err(CallError::NotFound);
        };
        let now = self.calls.now();
        let changed = exec(
            &txn,
            "UPDATE call_recordings SET state = 'FAILED', failed_at = $2, failure_code = $3, \
               stopped_at = COALESCE(stopped_at, $2), stop_reason = COALESCE(stop_reason, 'CAPTURE_FAILED'), \
               version = version + 1, updated_at = GREATEST(updated_at, $2) \
             WHERE id = $1 AND state IN ('STARTING', 'RECORDING', 'STOPPING', 'PROCESSING')",
            vec![recording_id.into(), now.into(), code.clone().into()],
        )
        .await?;
        if changed > 0 {
            self.calls
                .append_event(
                    &txn,
                    &system_cmd(&call),
                    &call,
                    EventDraft::fact(
                        crate::calls::events::CallEventType::RecordingFailed,
                        data(vec![
                            ("recording_id", text(recording_id)),
                            ("reason", text(&code)),
                        ])?,
                    ),
                    now,
                )
                .await?;
        }
        txn.commit().await?;
        if changed > 0 {
            crate::metrics::inc(
                "calling_enterprise_recordings_total",
                &[("result", "FAILED")],
            );
            tracing::warn!(%recording_id, "call recording capture failed");
            crate::calls::app::incidents::raise(
                self.db(),
                &crate::calls::app::incidents::Incident::new(
                    crate::calls::app::incidents::IncidentCategory::RecordingArtifactFailure,
                    crate::calls::app::incidents::IncidentSeverity::Medium,
                    scope_of(&call),
                    crate::calls::app::incidents::IncidentResource::CallRecording,
                    recording_id,
                )
                .call(call.id)
                .evidence(json!({ "failureCode": code })),
                now,
            )
            .await;
        }
        Ok(())
    }

    /// Worker pass (never blocked by kill switches except for NEW automatic starts):
    /// recordings of ended Calls stop, deadlines stop, lost captures fail, automatic
    /// starts, retention, deletion. Returns (stopped, failed, auto_started, deleted).
    pub(crate) async fn recording_tick(
        &self,
        limit: u64,
    ) -> CallResult<(usize, usize, usize, usize)> {
        let now = self.calls.now();
        let mut out = (0, 0, 0, 0);
        let limit_v = (limit as i64).into();
        // Ended Calls / past the maximum length.
        let due: Vec<(Uuid, Uuid, bool)> = self
            .db()
            .query_all(stmt(
                "SELECT r.id, r.call_id, (c.current_state IN ('ENDING', 'ENDED', 'FAILED', 'CANCELLED', \
                   'DECLINED', 'NO_ANSWER', 'BUSY', 'DENIED')) AS ended \
                 FROM call_recordings r JOIN calls c ON c.id = r.call_id \
                 WHERE r.state IN ('STARTING', 'RECORDING') AND (r.recording_deadline <= $1 \
                   OR c.current_state IN ('ENDING', 'ENDED', 'FAILED', 'CANCELLED', 'DECLINED', \
                                          'NO_ANSWER', 'BUSY', 'DENIED')) \
                 ORDER BY r.started_at LIMIT $2",
                vec![now.into(), limit_v],
            ))
            .await?
            .into_iter()
            .map(|r| Ok((r.try_get_by_index(0)?, r.try_get_by_index(1)?, r.try_get_by_index(2)?)))
            .collect::<Result<_, sea_orm::DbErr>>()?;
        for (id, call_id, ended) in due {
            let reason = if ended { "CALL_ENDED" } else { "MAX_DURATION" };
            if self.system_stop(call_id, id, reason).await? {
                out.0 += 1;
            }
        }
        // Lost captures (nothing delivered long after the deadline).
        let lost: Vec<Uuid> = self
            .db()
            .query_all(stmt(
                "SELECT id FROM call_recordings WHERE state IN ('STARTING', 'RECORDING', 'STOPPING', 'PROCESSING') \
                   AND recording_deadline <= $1 ORDER BY recording_deadline LIMIT $2",
                vec![
                    (now - Duration::minutes(DELIVERY_WINDOW_MINUTES)).into(),
                    (limit as i64).into(),
                ],
            ))
            .await?
            .into_iter()
            .map(|r| r.try_get_by_index::<Uuid>(0))
            .collect::<Result<_, _>>()?;
        for id in lost {
            self.recording_capture_failed(id, "CAPTURE_LOST").await?;
            out.1 += 1;
        }
        // AUTO_WHEN_AUTHORIZED: CONNECTED enterprise Calls not yet decided since their
        // latest consent.
        if self.enterprise_flags.call_recording {
            let candidates: Vec<Uuid> = self
                .db()
                .query_all(stmt(
                    "SELECT c.id FROM calls c \
                       JOIN enterprise_call_contexts x ON x.call_id = c.id \
                       JOIN call_recording_policies p ON p.tenant_id = c.tenant_id \
                         AND p.application_id = c.application_id AND p.environment_id = c.environment_id \
                         AND p.is_default AND p.status = 'ACTIVE' AND p.mode = 'AUTO_WHEN_AUTHORIZED' \
                      WHERE c.current_state = 'CONNECTED' \
                        AND NOT EXISTS (SELECT 1 FROM call_recordings r WHERE r.call_id = c.id) \
                        AND NOT EXISTS (SELECT 1 FROM call_recording_authorizations a \
                              WHERE a.call_id = c.id AND a.actor_kind = 'SYSTEM' \
                                AND a.consent_watermark >= COALESCE((SELECT max(k.seq) \
                                      FROM call_recording_consents k WHERE k.call_id = c.id), 0)) \
                      LIMIT $1",
                    vec![(limit as i64).into()],
                ))
                .await?
                .into_iter()
                .map(|r| r.try_get_by_index::<Uuid>(0))
                .collect::<Result<_, _>>()?;
            for call_id in candidates {
                let call = calls::Entity::find_by_id(call_id).one(self.db()).await?;
                let Some(call) = call else { continue };
                if let StartOutcome::Started(_) = self
                    .start_recording(None, scope_of(&call), call_id, RecordingActor::System)
                    .await?
                {
                    out.2 += 1;
                }
            }
        }
        // Retention: READY -> DELETING (audited as the system).
        let expired: Vec<RecordingRow> = RecordingRow::find_by_statement(stmt(
            &format!(
                "SELECT {RECORDING_COLUMNS} FROM call_recordings WHERE state = 'READY' \
                   AND retention_until <= $1 ORDER BY retention_until LIMIT $2"
            ),
            vec![now.into(), (limit as i64).into()],
        ))
        .all(self.db())
        .await?;
        for recording in expired {
            let Some(call) = calls::Entity::find_by_id(recording.call_id)
                .one(self.db())
                .await?
            else {
                continue;
            };
            let txn = self.db().begin().await?;
            let changed = exec(
                &txn,
                "UPDATE call_recordings SET state = 'DELETING', deleting_at = $2, \
                 delete_reason = 'RETENTION', next_attempt_at = $2, version = version + 1, \
                 updated_at = GREATEST(updated_at, $2) WHERE id = $1 AND state = 'READY'",
                vec![recording.id.into(), now.into()],
            )
            .await?;
            if changed > 0 {
                access_audit(
                    &txn,
                    &AdminActor::system(
                        scope_of(&call),
                        cf_service_auth::CorrelationId::generate().as_str(),
                    ),
                    recording.id,
                    recording.call_id,
                    None,
                    AccessOperation::RetentionDelete,
                    true,
                    None,
                    now,
                )
                .await
                .map_err(enterprise_error)?;
            }
            txn.commit().await?;
        }
        // DELETING -> object removed (idempotent) -> DELETED.
        let deleting: Vec<RecordingRow> = RecordingRow::find_by_statement(stmt(
            &format!(
                "SELECT {RECORDING_COLUMNS} FROM call_recordings WHERE state = 'DELETING' \
                   AND next_attempt_at <= $1 ORDER BY next_attempt_at LIMIT $2"
            ),
            vec![now.into(), (limit as i64).into()],
        ))
        .all(self.db())
        .await?;
        for recording in deleting {
            if let Some(key) = recording.object_key.as_deref() {
                if let Err(error) = self.artifacts.delete(key).await {
                    exec(
                        self.db(),
                        "UPDATE call_recordings SET delete_attempts = delete_attempts + 1, \
                         next_attempt_at = $2, version = version + 1, \
                         updated_at = GREATEST(updated_at, $3) WHERE id = $1 AND state = 'DELETING'",
                        vec![
                            recording.id.into(),
                            (now + Duration::seconds(
                                2_i64
                                    .saturating_pow(recording.delete_attempts.clamp(0, 11) as u32)
                                    .min(3600),
                            ))
                            .into(),
                            now.into(),
                        ],
                    )
                    .await?;
                    tracing::warn!(recording_id = %recording.id, %error, "recording artifact deletion pending; retried");
                    continue;
                }
            }
            let txn = self.db().begin().await?;
            let Some(call) = calls::Entity::find_by_id(recording.call_id)
                .one(&txn)
                .await?
            else {
                continue;
            };
            let changed = exec(
                &txn,
                "UPDATE call_recordings SET state = 'DELETED', object_key = NULL, deleted_at = $2, \
                 version = version + 1, updated_at = GREATEST(updated_at, $2) \
                 WHERE id = $1 AND state = 'DELETING'",
                vec![recording.id.into(), now.into()],
            )
            .await?;
            if changed > 0 {
                self.calls
                    .append_event(
                        &txn,
                        &system_cmd(&call),
                        &call,
                        EventDraft::fact(
                            crate::calls::events::CallEventType::RecordingDeleted,
                            data(vec![
                                ("recording_id", text(recording.id)),
                                (
                                    "reason",
                                    text(recording.delete_reason.as_deref().unwrap_or("USER")),
                                ),
                            ])?,
                        ),
                        now,
                    )
                    .await?;
                out.3 += 1;
            }
            txn.commit().await?;
        }
        Ok(out)
    }
}
