//! Voicemail on ONE canonical Call (Part F §60, §104–§111).
//!
//! ```text
//! router ─▶ EnterVoicemail (no answer / queue overflow / after hours / IVR / fallback)
//!   message RECORDING, context VOICEMAIL — the SAME Call keeps ringing state
//!   SIP caller: ANSWER, built-in VOICEMAIL_GREETING, CAPTURE (provider-neutral edge
//!     capture executed by Telephony / Asterisk; the edge spool is transient)
//!   RTC caller: the neutral call media artifact interface (`CallMediaCapture`)
//! capture delivered ─▶ PROCESSING ─▶ protected object storage ─▶ READY
//!   (storage unavailable: stays PROCESSING, the deliverer retries; never a fake READY)
//!   the Call ends NO_ANSWER with the enterprise outcome VOICEMAIL if still ringing
//! caller hangs up while recording ─▶ the Call is CANCELLED, outcome VOICEMAIL; the
//!   capture still arrives and the message becomes READY
//! worker: lost captures ─▶ FAILED; retention ─▶ DELETING ─▶ object removed ─▶ DELETED
//! ```
//!
//! Message audio never passes through logs or metrics; only bounded codes do. The object
//! key is internal and never returned.

use async_trait::async_trait;
use chrono::Duration;
use sea_orm::{
    ConnectionTrait, DatabaseTransaction, EntityTrait, FromQueryResult, QuerySelect,
    TransactionTrait,
};
use serde_json::{json, Map};
use uuid::Uuid;

use super::{
    context_of, exec, ivr::sip_leg, orphans::ArtifactKind, record_step, stmt, update_context,
    ContextRow, EnterprisePhase,
};
use crate::{
    calls::{
        app::{public::CommandKind, scope_of, system_cmd, AppCallService, PendingSignals},
        command::CallCommandContext,
        errors::{CallError, CallResult},
        kinds::FailureCategory,
        service::{data, text, EventDraft},
        state::CallState,
        transitions::CallAction,
    },
    enterprise::{
        audit::{self, AuditDetails, AuditFact, AuditOperation, AuditResource},
        destinations::Destination,
        ivr::BuiltinPrompt,
        voicemail::{self as config, MailboxRow, MessageRow, MESSAGE_COLUMNS},
        AdminActor,
    },
    models::{call_invitations, calls},
    signaling::signals::SignalType,
};

type Rung = (calls::Model, Vec<call_invitations::Model>);

/// The audio format a SIP edge capture delivers (PCM WAV).
pub const CAPTURE_CONTENT_TYPE: &str = "audio/wav";
/// The audio format an RTC media-server capture delivers (Opus in Ogg).
pub const CAPTURE_CONTENT_TYPE_OGG: &str = "audio/ogg";
/// Upper bound of one captured artifact (600 s of 16-bit 16 kHz mono ≈ 19 MiB).
pub const MAX_CAPTURE_BYTES: usize = 24 * 1024 * 1024;
/// Grace after the maximum message length before the platform ends a recording Call.
const RECORDING_GRACE_SECONDS: i64 = 30;
/// How long a finished recording may take to be delivered before it is FAILED.
const DELIVERY_WINDOW_MINUTES: i64 = 30;

/// What a capture is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapturePurpose {
    Voicemail,
    Recording,
}

impl CapturePurpose {
    pub fn as_str(&self) -> &'static str {
        match self {
            CapturePurpose::Voicemail => "VOICEMAIL",
            CapturePurpose::Recording => "RECORDING",
        }
    }
}

/// A request to capture an application (RTC) Call's audio. Calling decides everything
/// in it; the adapter only executes it.
#[derive(Debug, Clone)]
pub struct CaptureRequest {
    /// The voicemail message / call recording id (the capture's idempotency key).
    pub capture_id: Uuid,
    pub call_id: Uuid,
    pub purpose: CapturePurpose,
    pub max_duration_seconds: i32,
    /// The authorized media scope: the media identities whose audio ALONE is captured
    /// (a voicemail: the caller's sessions). Empty: the Call's mixed audio (a recording
    /// authorized for the whole Call).
    pub sources: Vec<String>,
}

/// What an adapter reports about one capture (no provider identifier crosses this).
#[derive(Debug)]
pub enum CaptureStatus {
    /// The capture is still recording (a due stop is (re-)issued for it).
    Running,
    /// Nothing to collect yet (finishing, or the adapter could not tell).
    Pending,
    /// The capture finished; its media is handed over for protected storage.
    Finished(CapturedMedia),
    /// The capture ended without usable media (bounded code).
    Failed(&'static str),
}

/// The neutral call media artifact interface (§105): an RTC media capture adapter (e.g.
/// a media-server egress) records the Call's media; Calling collects the result
/// ([`CallMediaCapture::poll`]) or the adapter pushes it through
/// [`AppCallService::deliver_capture`]. No Meeting Service type is involved.
#[async_trait]
pub trait CallMediaCapture: Send + Sync {
    /// Whether this deployment can capture application media at all.
    fn available(&self) -> bool;

    /// Starts the capture. Idempotent per `capture_id`: a repeated start never creates
    /// a second capture.
    async fn start(&self, request: CaptureRequest) -> Result<(), &'static str>;

    /// Stops a running capture early (idempotent); its media is still collected.
    async fn stop(&self, _call_id: Uuid, _capture_id: Uuid) -> Result<(), &'static str> {
        Ok(())
    }

    /// The capture's current outcome (collecting adapters). A pushing adapter delivers
    /// on its own and always reports [`CaptureStatus::Pending`].
    async fn poll(&self, _call_id: Uuid, _capture_id: Uuid) -> CaptureStatus {
        CaptureStatus::Pending
    }

    /// Releases the adapter's intermediate media once Calling has stored (or abandoned)
    /// the artifact (idempotent).
    async fn release(&self, _call_id: Uuid, _capture_id: Uuid) -> Result<(), &'static str> {
        Ok(())
    }
}

/// No application media capture in this deployment (SIP captures still work).
pub struct UnavailableCapture;

#[async_trait]
impl CallMediaCapture for UnavailableCapture {
    fn available(&self) -> bool {
        false
    }

    async fn start(&self, _: CaptureRequest) -> Result<(), &'static str> {
        Err("CAPTURE_UNAVAILABLE")
    }
}

/// Captured media as a source delivers it.
#[derive(Clone)]
pub struct CapturedMedia {
    pub content_type: String,
    pub duration_ms: i32,
    /// Lower-case hex SHA-256 the source computed.
    pub sha256: String,
    pub bytes: Vec<u8>,
}

impl std::fmt::Debug for CapturedMedia {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CapturedMedia")
            .field("content_type", &self.content_type)
            .field("duration_ms", &self.duration_ms)
            .field("bytes", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

/// What happened to a delivered capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureDelivery {
    /// Stored in protected object storage; the message is READY.
    Stored,
    /// The same capture was already stored (idempotent re-delivery).
    AlreadyStored,
}

impl CaptureDelivery {
    pub fn as_str(&self) -> &'static str {
        match self {
            CaptureDelivery::Stored => "STORED",
            CaptureDelivery::AlreadyStored => "ALREADY_STORED",
        }
    }
}

async fn message_by_id<C: ConnectionTrait>(
    db: &C,
    id: Uuid,
    lock: bool,
) -> CallResult<Option<MessageRow>> {
    Ok(MessageRow::find_by_statement(stmt(
        &format!(
            "SELECT {MESSAGE_COLUMNS} FROM voicemail_messages WHERE id = $1{}",
            if lock { " FOR UPDATE" } else { "" }
        ),
        vec![id.into()],
    ))
    .one(db)
    .await?)
}

async fn mailbox_by_id<C: ConnectionTrait>(
    db: &C,
    call: &calls::Model,
    id: Uuid,
) -> CallResult<MailboxRow> {
    config::mailbox_row(db, scope_of(call), id, false)
        .await
        .map_err(super::enterprise_error)
}

fn backoff(attempts: i32) -> Duration {
    Duration::seconds(2_i64.saturating_pow(attempts.clamp(0, 11) as u32).min(3600))
}

impl AppCallService {
    /// The router reached a mailbox (inside the routing transaction).
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn enter_voicemail(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        context: &ContextRow,
        mailbox: MailboxRow,
        destination: Destination,
    ) -> CallResult<Rung> {
        let now = self.calls.now();
        let refuse = |reason: &'static str| async move {
            record_step(
                &self.calls,
                txn,
                call,
                "NO_ROUTE",
                Some(destination),
                reason,
                None,
                now,
            )
            .await?;
            crate::metrics::inc(
                "calling_enterprise_voicemail_total",
                &[("result", "REFUSED")],
            );
            self.enterprise_no_route(txn, cmd, call, context, FailureCategory::Routing)
                .await
        };
        // Kill switch: no NEW voicemail (cleanup / retention never stop).
        if !self.enterprise_flags.voicemail {
            return refuse("VOICEMAIL_DISABLED").await;
        }
        let sip = sip_leg(txn, call).await?;
        let source = match &sip {
            Some(_) => "SIP",
            None if self.capture.available() => "APPLICATION",
            None => return refuse("VOICEMAIL_CAPTURE_UNAVAILABLE").await,
        };
        let message_id = Uuid::new_v4();
        let deadline =
            now + Duration::seconds(mailbox.max_message_seconds as i64 + RECORDING_GRACE_SECONDS);
        let inserted = exec(
            txn,
            "INSERT INTO voicemail_messages (id, tenant_id, application_id, environment_id, mailbox_id, \
             call_id, caller_principal_id, capture_source, call_leg_id, state, read_state, received_at, \
             recording_deadline, delete_attempts, version, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'RECORDING', 'UNREAD', $10, $11, 0, 1, $10, $10) \
             ON CONFLICT (call_id) DO NOTHING",
            vec![
                message_id.into(),
                call.tenant_id.into(),
                call.application_id.into(),
                call.environment_id.into(),
                mailbox.id.into(),
                call.id.into(),
                call.initiator_principal_id.into(),
                source.into(),
                sip.as_ref().map(|l| l.id).into(),
                now.into(),
                deadline.into(),
            ],
        )
        .await?;
        if inserted == 0 {
            // A Call reaches voicemail once.
            return refuse("VOICEMAIL_ALREADY_TAKEN").await;
        }
        update_context(
            txn,
            context,
            EnterprisePhase::Voicemail,
            Some(destination),
            None,
            now,
        )
        .await?;
        record_step(
            &self.calls,
            txn,
            call,
            "VOICEMAIL",
            Some(destination),
            "RECORDING",
            None,
            now,
        )
        .await?;
        let call = self.locked(txn, call).await?;
        self.calls
            .append_event(
                txn,
                cmd,
                &call,
                EventDraft::fact(
                    crate::calls::events::CallEventType::VoicemailReceived,
                    data(vec![
                        ("message_id", text(message_id)),
                        ("mailbox_id", text(mailbox.id)),
                        ("capture_source", text(source)),
                    ])?,
                ),
                now,
            )
            .await?;
        if let Some(leg) = &sip {
            // The edge answers, plays the greeting and captures — commands of one leg run
            // in order (the capture key sorts after the prompt keys).
            self.enqueue_sip_answer_in(txn, &call, leg).await?;
            self.enqueue_sip_prompt_in(txn, &call, leg, BuiltinPrompt::VoicemailGreeting, 0)
                .await?;
            crate::calls::app::public::enqueue_in(
                txn,
                &call,
                CommandKind::Capture,
                Some(leg.id),
                None,
                None,
                &format!("record:{}:{message_id}", leg.id),
                json!({
                    "correlationId": cmd.correlation_id().as_str(),
                    "capture": {
                        "captureId": message_id,
                        "purpose": "VOICEMAIL",
                        "maxDurationSeconds": mailbox.max_message_seconds,
                        "beep": true,
                    },
                }),
                now,
            )
            .await?;
        }
        crate::metrics::inc(
            "calling_enterprise_voicemail_total",
            &[("result", "RECORDING")],
        );
        Ok((call, Vec::new()))
    }

    /// After commit: an application caller's capture is started through the neutral
    /// interface (a start failure FAILS the message; the Call ends safely).
    pub(crate) async fn start_application_capture(&self, call_id: Uuid) {
        let row = MessageRow::find_by_statement(stmt(
            &format!(
                "SELECT {MESSAGE_COLUMNS} FROM voicemail_messages WHERE call_id = $1 \
                 AND capture_source = 'APPLICATION' AND state = 'RECORDING'"
            ),
            vec![call_id.into()],
        ))
        .one(self.db())
        .await;
        let Ok(Some(message)) = row else {
            return;
        };
        let max = (message.recording_deadline - message.received_at).num_seconds() as i32
            - RECORDING_GRACE_SECONDS as i32;
        // The authorized media scope: the caller's own live sessions, nobody else.
        let sources = match caller_media_identities(self.db(), call_id, message.caller_principal_id)
            .await
        {
            Ok(sources) => sources,
            Err(error) => {
                tracing::warn!(%call_id, %error, "voicemail capture scope not read; the worker retries");
                return;
            }
        };
        if let Err(code) = self
            .capture
            .start(CaptureRequest {
                capture_id: message.id,
                call_id,
                purpose: CapturePurpose::Voicemail,
                max_duration_seconds: max.max(1),
                sources,
            })
            .await
        {
            if let Err(error) = self.capture_failed(message.id, code).await {
                tracing::warn!(%call_id, %error, "voicemail capture failure not recorded; the worker retries");
            }
            return;
        }
        // The caller's endpoints learn that they are now leaving a message.
        if let Ok(Some(call)) = calls::Entity::find_by_id(call_id).one(self.db()).await {
            let mut pending = PendingSignals::default();
            let mut data = serde_json::Map::new();
            data.insert("maxSeconds".into(), json!(max.max(1)));
            let correlation = system_cmd(&call).correlation_id().as_str().to_string();
            match self
                .initiator_signal(
                    self.db(),
                    &mut pending,
                    SignalType::VoicemailStarted,
                    &call,
                    &correlation,
                    data,
                )
                .await
            {
                Ok(()) => self.publish(pending).await,
                Err(error) => tracing::warn!(%call_id, %error, "voicemail signal not built"),
            }
        }
    }

    /// Whether `capture_id` is the SIP capture (voicemail message or call recording) of
    /// `call_id` on leg `call_leg_id` — the edge may deliver only what it was asked to
    /// capture.
    pub async fn is_sip_capture(
        &self,
        capture_id: Uuid,
        call_id: Uuid,
        call_leg_id: Uuid,
    ) -> CallResult<bool> {
        if self
            .is_sip_recording(capture_id, call_id, call_leg_id)
            .await?
        {
            return Ok(true);
        }
        Ok(self
            .db()
            .query_one(stmt(
                "SELECT 1 FROM voicemail_messages WHERE id = $1 AND call_id = $2 \
                   AND call_leg_id = $3 AND capture_source = 'SIP'",
                vec![capture_id.into(), call_id.into(), call_leg_id.into()],
            ))
            .await?
            .is_some())
    }

    /// A capture source delivers the captured audio (Telephony for SIP callers, the RTC
    /// capture adapter for application callers). Validated, stored in protected object
    /// storage, then READY. `Err(ArtifactStorageUnavailable)` asks the deliverer to retry.
    pub async fn deliver_capture(
        &self,
        capture_id: Uuid,
        call_id: Uuid,
        media: CapturedMedia,
    ) -> CallResult<CaptureDelivery> {
        let content_type = match media.content_type.as_str() {
            CAPTURE_CONTENT_TYPE => CAPTURE_CONTENT_TYPE,
            // An Ogg container is verified, never trusted by its label.
            CAPTURE_CONTENT_TYPE_OGG if media.bytes.starts_with(b"OggS") => {
                CAPTURE_CONTENT_TYPE_OGG
            }
            _ => {
                return Err(CallError::Validation(
                    "the capture must be audio/wav or audio/ogg.",
                ))
            }
        };
        if media.bytes.is_empty() || media.bytes.len() > MAX_CAPTURE_BYTES {
            return Err(CallError::Validation("the capture size is out of bounds."));
        }
        if !(0..=3_600_000).contains(&media.duration_ms) {
            return Err(CallError::Validation(
                "the capture duration is out of bounds.",
            ));
        }
        let digest = cf_object_storage::sha256_hex(&media.bytes);
        if digest != media.sha256 {
            crate::metrics::inc(
                "calling_enterprise_capture_deliveries_total",
                &[("result", "INTEGRITY_MISMATCH")],
            );
            return Err(CallError::Validation("the capture digest does not match."));
        }
        let Some(message) = message_by_id(self.db(), capture_id, false).await? else {
            // Not a voicemail message: a call recording's capture (§61), or unknown.
            return self
                .deliver_recording_capture(capture_id, call_id, media, digest)
                .await;
        };
        if message.call_id != call_id {
            return Err(CallError::NotFound);
        }
        match message.state.as_str() {
            "READY" if message.sha256.as_deref() == Some(digest.as_str()) => {
                return Ok(CaptureDelivery::AlreadyStored)
            }
            "READY" | "DELETING" | "DELETED" => {
                return Err(CallError::Conflict("the message is no longer recording"))
            }
            _ => {}
        }
        // PROCESSING while the audio is uploaded.
        let txn = self.db().begin().await?;
        exec(
            &txn,
            "UPDATE voicemail_messages SET state = 'PROCESSING', version = version + 1, \
             updated_at = GREATEST(updated_at, $2) WHERE id = $1 AND state = 'RECORDING'",
            vec![capture_id.into(), self.calls.now().into()],
        )
        .await?;
        txn.commit().await?;
        let scope = message_scope(self.db(), capture_id).await?;
        let key = config::object_key(scope, capture_id, content_type);
        let stored = match self.artifacts.put(&key, media.bytes, content_type).await {
            Ok(s) if s.sha256 == digest => s,
            outcome => {
                let code = match outcome {
                    Ok(_) | Err(cf_object_storage::StorageError::Integrity) => "STORAGE_INTEGRITY",
                    Err(cf_object_storage::StorageError::NotConfigured) => "STORAGE_NOT_CONFIGURED",
                    Err(_) => "STORAGE_UNAVAILABLE",
                };
                if code == "STORAGE_INTEGRITY" {
                    // Written, but not what was captured: never leave it behind (§79.26).
                    self.discard_written_object(
                        scope,
                        ArtifactKind::Voicemail,
                        capture_id,
                        Some(call_id),
                        &key,
                        "INTEGRITY_MISMATCH",
                    )
                    .await;
                }
                exec(
                    self.db(),
                    "UPDATE voicemail_messages SET failure_code = $2, version = version + 1, \
                     updated_at = GREATEST(updated_at, $3) \
                     WHERE id = $1 AND state IN ('PROCESSING', 'FAILED')",
                    vec![capture_id.into(), code.into(), self.calls.now().into()],
                )
                .await?;
                crate::metrics::inc(
                    "calling_enterprise_capture_deliveries_total",
                    &[("result", code)],
                );
                tracing::warn!(message_id = %capture_id, code, "voicemail artifact not stored; the deliverer retries");
                return Err(CallError::ArtifactStorageUnavailable);
            }
        };
        match self
            .finalize_voicemail(
                capture_id,
                call_id,
                media.duration_ms,
                content_type,
                &key,
                &stored,
            )
            .await
        {
            Ok(Some((call, pending))) => {
                self.publish(pending).await;
                self.ivr_after_commit(&call).await;
                crate::metrics::inc(
                    "calling_enterprise_capture_deliveries_total",
                    &[("result", "STORED")],
                );
                crate::metrics::inc("calling_enterprise_voicemail_total", &[("result", "READY")]);
                tracing::info!(message_id = %capture_id, call_id = %call_id, "voicemail message ready");
                Ok(CaptureDelivery::Stored)
            }
            Ok(None) => {
                self.discard_written_object(
                    scope,
                    ArtifactKind::Voicemail,
                    capture_id,
                    Some(call_id),
                    &key,
                    "NOT_AWAITING",
                )
                .await;
                Ok(CaptureDelivery::AlreadyStored)
            }
            Err(error) => {
                self.discard_written_object(
                    scope,
                    ArtifactKind::Voicemail,
                    capture_id,
                    Some(call_id),
                    &key,
                    "FINALIZATION_FAILED",
                )
                .await;
                Err(error)
            }
        }
    }

    /// The READY transaction of a delivered voicemail message; the Call (if still
    /// ringing in voicemail) ends NO_ANSWER / VOICEMAIL. `Ok(None)`: the message no
    /// longer awaits a capture (nothing changed).
    async fn finalize_voicemail(
        &self,
        capture_id: Uuid,
        call_id: Uuid,
        duration_ms: i32,
        content_type: &str,
        key: &str,
        stored: &cf_object_storage::StoredObject,
    ) -> CallResult<Option<(calls::Model, PendingSignals)>> {
        let txn = self.db().begin().await?;
        let Some(call) = calls::Entity::find_by_id(call_id)
            .lock_exclusive()
            .one(&txn)
            .await?
        else {
            return Err(CallError::NotFound);
        };
        let Some(message) = message_by_id(&txn, capture_id, true).await? else {
            return Err(CallError::NotFound);
        };
        if !matches!(
            message.state.as_str(),
            "PROCESSING" | "RECORDING" | "FAILED"
        ) {
            txn.rollback().await?;
            return Ok(None);
        }
        let mailbox = mailbox_by_id(&txn, &call, message.mailbox_id).await?;
        let now = self.calls.now();
        exec(
            &txn,
            "UPDATE voicemail_messages SET state = 'READY', object_key = $2, content_type = $3, \
             size_bytes = $4, duration_ms = $5, sha256 = $6, encrypted = $7, ready_at = $8, \
             retention_until = $9, failed_at = NULL, failure_code = NULL, version = version + 1, \
             updated_at = GREATEST(updated_at, $8) WHERE id = $1",
            vec![
                capture_id.into(),
                key.into(),
                content_type.into(),
                (stored.size as i64).into(),
                duration_ms.into(),
                stored.sha256.clone().into(),
                stored.encrypted.into(),
                now.into(),
                (now + Duration::days(mailbox.retention_days as i64)).into(),
            ],
        )
        .await?;
        let cmd = system_cmd(&call);
        self.calls
            .append_event(
                &txn,
                &cmd,
                &call,
                EventDraft::fact(
                    crate::calls::events::CallEventType::VoicemailReady,
                    data(vec![
                        ("message_id", text(capture_id)),
                        ("mailbox_id", text(mailbox.id)),
                        (
                            "duration_ms",
                            crate::calls::attributes::SafeValue::from(duration_ms.max(0) as usize),
                        ),
                    ])?,
                ),
                now,
            )
            .await?;
        self.calls
            .record_artifact_usage_in(
                &txn,
                &cmd,
                &call,
                crate::calls::service::ArtifactUsageFact {
                    usage_type: crate::calls::kinds::UsageType::VoicemailArtifact,
                    artifact_id: capture_id,
                    duration_ms: i64::from(duration_ms),
                    size_bytes: stored.size as i64,
                },
            )
            .await?;
        let (call, pending) = self
            .end_voicemail_call_in(&txn, &cmd, &call, "VOICEMAIL")
            .await?;
        // A queued orphan of this very key is the artifact now (never deleted later).
        exec(
            &txn,
            "DELETE FROM calling_orphan_objects WHERE object_key = $1",
            vec![key.into()],
        )
        .await?;
        txn.commit().await?;
        Ok(Some((call, pending)))
    }

    /// A capture source reports that the capture failed (bounded code): the message is
    /// FAILED and a still-ringing Call ends NO_ANSWER.
    pub async fn capture_failed(&self, capture_id: Uuid, code: &str) -> CallResult<()> {
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
        let Some(message) = message_by_id(self.db(), capture_id, false).await? else {
            return self.recording_capture_failed(capture_id, &code).await;
        };
        let txn = self.db().begin().await?;
        let Some(call) = calls::Entity::find_by_id(message.call_id)
            .lock_exclusive()
            .one(&txn)
            .await?
        else {
            return Err(CallError::NotFound);
        };
        let now = self.calls.now();
        let changed = exec(
            &txn,
            "UPDATE voicemail_messages SET state = 'FAILED', failed_at = $2, failure_code = $3, \
             version = version + 1, updated_at = GREATEST(updated_at, $2) \
             WHERE id = $1 AND state IN ('RECORDING', 'PROCESSING')",
            vec![capture_id.into(), now.into(), code.clone().into()],
        )
        .await?;
        let cmd = system_cmd(&call);
        let (call, pending) = self
            .end_voicemail_call_in(&txn, &cmd, &call, "NO_ANSWER")
            .await?;
        txn.commit().await?;
        self.publish(pending).await;
        self.ivr_after_commit(&call).await;
        if changed > 0 {
            crate::metrics::inc(
                "calling_enterprise_voicemail_total",
                &[("result", "FAILED")],
            );
            tracing::warn!(message_id = %capture_id, "voicemail capture failed");
            crate::calls::app::incidents::raise(
                self.db(),
                &crate::calls::app::incidents::Incident::new(
                    crate::calls::app::incidents::IncidentCategory::VoicemailFinalizationFailure,
                    crate::calls::app::incidents::IncidentSeverity::Medium,
                    scope_of(&call),
                    crate::calls::app::incidents::IncidentResource::VoicemailMessage,
                    capture_id,
                )
                .call(call.id)
                .evidence(json!({ "failureCode": code })),
                now,
            )
            .await;
        }
        Ok(())
    }

    /// Ends a Call still ringing in its voicemail phase (NO_ANSWER) with `outcome`.
    async fn end_voicemail_call_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        outcome: &'static str,
    ) -> CallResult<(calls::Model, PendingSignals)> {
        let mut pending = PendingSignals::default();
        let call = self.locked(txn, call).await?;
        let Some(context) = context_of(txn, call.id).await? else {
            return Ok((call, pending));
        };
        if context.phase != EnterprisePhase::Voicemail.as_str()
            || !matches!(call.current_state, CallState::Dialing | CallState::Ringing)
        {
            return Ok((call, pending));
        }
        let call = self
            .calls
            .transition_in(txn, cmd, &call, call.version, CallAction::MarkNoAnswer)
            .await?
            .call;
        update_context(
            txn,
            &context,
            EnterprisePhase::Ended,
            None,
            Some(outcome),
            self.calls.now(),
        )
        .await?;
        self.call_signal(
            txn,
            &mut pending,
            SignalType::NoAnswer,
            &call,
            cmd.correlation_id().as_str(),
            Map::new(),
        )
        .await?;
        Ok((call, pending))
    }

    /// Worker pass (never blocked by kill switches): lost captures FAILED, overdue
    /// recording Calls ended, retention moved to DELETING, DELETING objects removed.
    /// Returns (failed, ended, retention, deleted).
    pub(crate) async fn voicemail_tick(
        &self,
        limit: u64,
    ) -> CallResult<(usize, usize, usize, usize)> {
        let now = self.calls.now();
        let mut out = (0, 0, 0, 0);
        // Lost captures.
        let lost = strings_uuid(
            self.db(),
            "SELECT id FROM voicemail_messages WHERE state IN ('RECORDING', 'PROCESSING') \
               AND recording_deadline <= $1 ORDER BY recording_deadline LIMIT $2",
            vec![
                (now - Duration::minutes(DELIVERY_WINDOW_MINUTES)).into(),
                (limit as i64).into(),
            ],
        )
        .await?;
        for id in lost {
            self.capture_failed(id, "CAPTURE_LOST").await?;
            out.0 += 1;
        }
        // Recording Calls past their deadline (the edge should already have ended them).
        let overdue = strings_uuid(
            self.db(),
            "SELECT m.call_id FROM voicemail_messages m JOIN calls c ON c.id = m.call_id \
              WHERE m.state = 'RECORDING' AND m.recording_deadline <= $1 \
                AND c.current_state IN ('DIALING', 'RINGING') LIMIT $2",
            vec![now.into(), (limit as i64).into()],
        )
        .await?;
        for call_id in overdue {
            let txn = self.db().begin().await?;
            let Some(call) = calls::Entity::find_by_id(call_id)
                .lock_with_behavior(
                    sea_orm::sea_query::LockType::Update,
                    sea_orm::sea_query::LockBehavior::SkipLocked,
                )
                .one(&txn)
                .await?
            else {
                txn.rollback().await?;
                continue;
            };
            let cmd = system_cmd(&call);
            let (call, pending) = self
                .end_voicemail_call_in(&txn, &cmd, &call, "VOICEMAIL")
                .await?;
            txn.commit().await?;
            self.publish(pending).await;
            self.ivr_after_commit(&call).await;
            out.1 += 1;
        }
        // Retention: READY -> DELETING (audited as the system). A retention hold (Part H
        // §80) on the message, its mailbox or its Call keeps it.
        let expired = MessageRow::find_by_statement(stmt(
            &format!(
                "SELECT {MESSAGE_COLUMNS} FROM voicemail_messages WHERE state = 'READY' \
                   AND retention_until <= $1 \
                   AND NOT calling_retention_held('VOICEMAIL_ARTIFACT', id, tenant_id, application_id, environment_id) \
                   AND NOT calling_retention_held('VOICEMAIL_ARTIFACT', mailbox_id, tenant_id, application_id, environment_id) \
                   AND NOT calling_retention_held('VOICEMAIL_ARTIFACT', call_id, tenant_id, application_id, environment_id) \
                 ORDER BY retention_until LIMIT $2"
            ),
            vec![now.into(), (limit as i64).into()],
        ))
        .all(self.db())
        .await?;
        for message in expired {
            let txn = self.db().begin().await?;
            let changed = exec(
                &txn,
                "UPDATE voicemail_messages SET state = 'DELETING', deleting_at = $2, \
                 delete_reason = 'RETENTION', next_attempt_at = $2, version = version + 1, \
                 updated_at = GREATEST(updated_at, $2) WHERE id = $1 AND state = 'READY'",
                vec![message.id.into(), now.into()],
            )
            .await?;
            if changed > 0 {
                let scope = message_scope(&txn, message.id).await?;
                audit::record(
                    &txn,
                    &AdminActor::system(scope, cf_service_auth::CorrelationId::generate().as_str()),
                    AuditFact {
                        resource: AuditResource::VoicemailMessage,
                        resource_id: message.id,
                        operation: AuditOperation::RetentionDelete,
                        old_version: Some(message.version),
                        new_version: Some(message.version + 1),
                        details: AuditDetails::new(),
                    },
                    now,
                )
                .await
                .map_err(super::enterprise_error)?;
                out.2 += 1;
            }
            txn.commit().await?;
        }
        // DELETING -> object removed (idempotent) -> DELETED.
        let deleting = MessageRow::find_by_statement(stmt(
            &format!(
                "SELECT {MESSAGE_COLUMNS} FROM voicemail_messages WHERE state = 'DELETING' \
                   AND next_attempt_at <= $1 ORDER BY next_attempt_at LIMIT $2"
            ),
            vec![now.into(), (limit as i64).into()],
        ))
        .all(self.db())
        .await?;
        for message in deleting {
            if let Some(key) = message.object_key.as_deref() {
                if let Err(error) = self.artifacts.delete(key).await {
                    exec(
                        self.db(),
                        "UPDATE voicemail_messages SET delete_attempts = delete_attempts + 1, \
                         next_attempt_at = $2, version = version + 1, \
                         updated_at = GREATEST(updated_at, $3) WHERE id = $1 AND state = 'DELETING'",
                        vec![
                            message.id.into(),
                            (now + backoff(message.delete_attempts)).into(),
                            now.into(),
                        ],
                    )
                    .await?;
                    let attempts = message.delete_attempts + 1;
                    crate::metrics::inc(
                        "calling_enterprise_artifact_deletions_total",
                        &[("artifact", "VOICEMAIL"), ("result", "RETRY")],
                    );
                    tracing::warn!(message_id = %message.id, attempts, %error, "voicemail artifact deletion pending; retried");
                    if attempts == self.config.artifact_delete_incident_attempts {
                        // Once, when the threshold is crossed (still DELETING, retried).
                        let scope = message_scope(self.db(), message.id).await?;
                        crate::calls::app::incidents::raise(
                            self.db(),
                            &crate::calls::app::incidents::Incident::new(
                                crate::calls::app::incidents::IncidentCategory::VoicemailFinalizationFailure,
                                crate::calls::app::incidents::IncidentSeverity::Medium,
                                scope,
                                crate::calls::app::incidents::IncidentResource::VoicemailMessage,
                                message.id,
                            )
                            .bucket("delete")
                            .call(message.call_id)
                            .evidence(json!({ "deletePending": true, "deleteAttempts": attempts })),
                            now,
                        )
                        .await;
                    }
                    continue;
                }
            }
            let txn = self.db().begin().await?;
            let Some(call) = calls::Entity::find_by_id(message.call_id).one(&txn).await? else {
                continue;
            };
            let changed = exec(
                &txn,
                "UPDATE voicemail_messages SET state = 'DELETED', object_key = NULL, deleted_at = $2, \
                 version = version + 1, updated_at = GREATEST(updated_at, $2) \
                 WHERE id = $1 AND state = 'DELETING'",
                vec![message.id.into(), now.into()],
            )
            .await?;
            if changed > 0 {
                self.calls
                    .append_event(
                        &txn,
                        &system_cmd(&call),
                        &call,
                        EventDraft::fact(
                            crate::calls::events::CallEventType::VoicemailDeleted,
                            data(vec![
                                ("message_id", text(message.id)),
                                (
                                    "reason",
                                    text(message.delete_reason.as_deref().unwrap_or("USER")),
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

/// The media identities of `caller`'s live sessions in `call_id`.
async fn caller_media_identities<C: ConnectionTrait>(
    db: &C,
    call_id: Uuid,
    caller: Uuid,
) -> CallResult<Vec<String>> {
    Ok(db
        .query_all(stmt(
            "SELECT s.media_identity FROM call_sessions s \
               JOIN call_participants p ON p.id = s.participant_id \
              WHERE s.call_id = $1 AND p.principal_id = $2 AND s.ended_at IS NULL \
                AND p.ended_at IS NULL ORDER BY s.created_at, s.id",
            vec![call_id.into(), caller.into()],
        ))
        .await?
        .into_iter()
        .map(|r| r.try_get_by_index::<String>(0))
        .collect::<Result<_, _>>()?)
}

/// (tenant, application, environment) of a message.
async fn message_tenant<C: ConnectionTrait>(db: &C, id: Uuid) -> CallResult<(Uuid, Uuid, Uuid)> {
    let row = db
        .query_one(stmt(
            "SELECT tenant_id, application_id, environment_id FROM voicemail_messages WHERE id = $1",
            vec![id.into()],
        ))
        .await?
        .ok_or(CallError::NotFound)?;
    Ok((
        row.try_get_by_index(0)?,
        row.try_get_by_index(1)?,
        row.try_get_by_index(2)?,
    ))
}

async fn message_scope<C: ConnectionTrait>(
    db: &C,
    id: Uuid,
) -> CallResult<crate::calls::command::CallScope> {
    let (t, a, e) = message_tenant(db, id).await?;
    Ok(crate::calls::command::CallScope::from_parts(
        crate::domain::TenantId::from_uuid(t),
        crate::domain::ApplicationId::from_uuid(a),
        crate::domain::EnvironmentId::from_uuid(e),
    ))
}

async fn strings_uuid<C: ConnectionTrait>(
    db: &C,
    sql: &str,
    values: Vec<sea_orm::Value>,
) -> CallResult<Vec<Uuid>> {
    db.query_all(stmt(sql, values))
        .await?
        .into_iter()
        .map(|r| r.try_get_by_index::<Uuid>(0).map_err(Into::into))
        .collect()
}
