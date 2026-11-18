//! Attended (consultative) transfer (gap closure C14 / CAL-019).
//!
//! ```text
//! POST /v1/calls/{A}/consultations      A CONNECTED → HELD (canonical HOLD), then the
//!   transferring participant T places an ordinary application Call B to the target X
//!   (the canonical POST /v1/calls path: authorization, personal policy, blocks, limits,
//!   ringing) ─▶ consultation CONSULTING
//! X answers B; T and X talk.
//! POST …/consultations/{id}/complete    B must be CONNECTED. The canonical Part C transfer
//!   of A from T to X starts (TRANSFERRING, X rings on A with a TRANSFER offer; X's
//!   endpoints also get `call.consultation_transfer` on B naming A and the transfer, so
//!   the client answers it at once) ─▶ consultation COMPLETING
//!   X joined A ─▶ T retired from A, A CONNECTED (original party ↔ X), B ended,
//!     consultation COMPLETED
//!   the transfer failed ─▶ A back to HELD, consultation CONSULTING again (B untouched)
//! POST …/consultations/{id}/cancel      B ended, A resumed (if the consultation held it),
//!   consultation CANCELLED
//! ```
//!
//! Nothing here is a second state machine: A and B are canonical Calls driven by the
//! existing hold / create / transfer / end / resume commands (their events, outbox,
//! webhooks and signals unchanged); `call_consultations` only links them. The enterprise
//! policy's `transferAllowed` gates starting and completing a consultation exactly as it
//! gates a transfer. Idempotency: starting is keyed by the request's Idempotency-Key (the
//! consultation Call's creation key); completing is keyed by the consultation and its
//! completion attempt; cancelling is idempotent.

use chrono::{DateTime, SecondsFormat, Utc};
use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, FromQueryResult, QueryFilter, TransactionTrait,
};
use serde::Serialize;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::{
    create::CreateAppCall,
    enterprise::{exec, stmt, TransferTarget},
    summary, system_cmd, AppCallService, CommandOutcome, PendingSignals, TransferRequest,
};
use crate::{
    calls::{
        command::{CallCommandContext, CallScope},
        errors::{CallError, CallResult},
        ids::{CallId, CallParticipantId},
        kinds::{CallKind, CommandOperation, TransferState},
        service::{fingerprint, CommandKey, IdempotencyKey},
        state::CallState,
    },
    domain::PrincipalId,
    endpoints::CallEndpointId,
    models::{call_participants, call_transfers, calls},
    platform::context::PlatformAuthContext,
    signaling::signals::SignalType,
};

/// `POST /v1/calls/{call_id}/consultations` input (Idempotency-Key required).
#[derive(Debug, Clone)]
pub struct ConsultationRequest {
    pub transferring_participant: CallParticipantId,
    /// The target: a principal, or (enterprise, `enterprise.calls.use`) an extension or a
    /// destination resolved to one principal exactly as for a transfer.
    pub target: TransferTarget,
    /// The transferring participant's own endpoint the consultation Call is placed from.
    pub initiator_endpoint: CallEndpointId,
    pub kind: CallKind,
    pub expected_version: i64,
    pub key: CommandKey,
}

/// The result of starting a consultation.
#[derive(Debug, Clone)]
pub struct ConsultationStarted {
    pub status: u16,
    pub body: Value,
    pub replayed: bool,
}

#[derive(Debug, Clone, FromQueryResult)]
struct ConsultationRow {
    id: Uuid,
    call_id: Uuid,
    consultation_call_id: Uuid,
    transferring_participant_id: Uuid,
    target_principal_id: Uuid,
    state: String,
    held_original: bool,
    transfer_id: Option<Uuid>,
    completion_attempts: i32,
    version: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
}

const COLUMNS: &str = "id, call_id, consultation_call_id, transferring_participant_id, \
    target_principal_id, state, held_original, transfer_id, completion_attempts, version, \
    created_at, updated_at, ended_at";

/// The consultation as clients see it (ids, state and timing only).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsultationView {
    pub consultation_id: Uuid,
    pub call_id: Uuid,
    pub consultation_call_id: Uuid,
    pub transferring_participant_id: Uuid,
    pub target_principal_id: Uuid,
    pub state: String,
    pub held_original: bool,
    pub transfer_id: Option<Uuid>,
    pub version: i64,
    pub created_at: String,
    pub updated_at: String,
    pub ended_at: Option<String>,
}

fn time(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

impl From<ConsultationRow> for ConsultationView {
    fn from(r: ConsultationRow) -> Self {
        Self {
            consultation_id: r.id,
            call_id: r.call_id,
            consultation_call_id: r.consultation_call_id,
            transferring_participant_id: r.transferring_participant_id,
            target_principal_id: r.target_principal_id,
            state: r.state,
            held_original: r.held_original,
            transfer_id: r.transfer_id,
            version: r.version,
            created_at: time(r.created_at),
            updated_at: time(r.updated_at),
            ended_at: r.ended_at.map(time),
        }
    }
}

fn view(row: ConsultationRow) -> Value {
    serde_json::to_value(ConsultationView::from(row)).unwrap_or(Value::Null)
}

fn scope_values(scope: CallScope) -> Vec<sea_orm::Value> {
    vec![
        scope.tenant_id().as_uuid().into(),
        scope.application_id().as_uuid().into(),
        scope.environment_id().as_uuid().into(),
    ]
}

/// One consultation of `call_id` in `scope` (404 for any other).
async fn consultation_of<C: ConnectionTrait>(
    db: &C,
    scope: CallScope,
    call_id: Uuid,
    id: Uuid,
    lock: bool,
) -> CallResult<ConsultationRow> {
    let mut values = scope_values(scope);
    values.extend([call_id.into(), id.into()]);
    ConsultationRow::find_by_statement(stmt(
        &format!(
            "SELECT {COLUMNS} FROM call_consultations WHERE tenant_id = $1 AND application_id = $2 \
               AND environment_id = $3 AND call_id = $4 AND id = $5{}",
            if lock { " FOR UPDATE" } else { "" }
        ),
        values,
    ))
    .one(db)
    .await?
    .ok_or(CallError::NotFound)
}

async fn consultation_by_id<C: ConnectionTrait>(
    db: &C,
    id: Uuid,
) -> CallResult<Option<ConsultationRow>> {
    Ok(ConsultationRow::find_by_statement(stmt(
        &format!("SELECT {COLUMNS} FROM call_consultations WHERE id = $1"),
        vec![id.into()],
    ))
    .one(db)
    .await?)
}

async fn active_consultation<C: ConnectionTrait>(db: &C, call_id: Uuid) -> CallResult<bool> {
    Ok(db
        .query_one(stmt(
            "SELECT 1 FROM call_consultations WHERE call_id = $1 \
               AND state IN ('CONSULTING', 'COMPLETING')",
            vec![call_id.into()],
        ))
        .await?
        .is_some())
}

fn call_id_of(body: &Value) -> CallResult<Uuid> {
    body["call"]["callId"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .ok_or(CallError::NotFound)
}

impl AppCallService {
    /// `POST /v1/calls/{call_id}/consultations`: holds the original Call (when CONNECTED)
    /// and places the consultation Call from the transferring participant to the target.
    pub async fn start_consultation(
        &self,
        ctx: &PlatformAuthContext,
        call_id: CallId,
        request: ConsultationRequest,
    ) -> CallResult<ConsultationStarted> {
        let scope = CallScope::of_platform(ctx);
        // A replay answers from the recorded consultation (its Call's creation key).
        if let Some(prior) = self
            .calls
            .prior_command(self.db(), scope, &request.key)
            .await?
        {
            let created = self.replay_create(scope, prior).await?;
            let consultation_call = call_id_of(&created.body)?;
            let row = ConsultationRow::find_by_statement(stmt(
                &format!(
                    "SELECT {COLUMNS} FROM call_consultations \
                      WHERE consultation_call_id = $1 AND call_id = $2"
                ),
                vec![consultation_call.into(), call_id.as_uuid().into()],
            ))
            .one(self.db())
            .await?
            .ok_or(CallError::Conflict(
                "the consultation was not recorded; start a new one",
            ))?;
            let call = self.calls_row(scope, call_id).await?;
            return Ok(ConsultationStarted {
                status: created.status,
                body: json!({
                    "consultation": view(row),
                    "call": summary(&call),
                    "consultationCall": created.body,
                }),
                replayed: true,
            });
        }
        // The enterprise policy gates a consultation exactly like the transfer it leads to;
        // an enterprise target is resolved to one principal here.
        let target = self
            .resolve_transfer_target(ctx, call_id, request.target.clone())
            .await?
            .principal;
        let call = self.calls_row(scope, call_id).await?;
        if call.version != request.expected_version {
            return Err(CallError::StaleVersion {
                expected: request.expected_version,
                current: call.version,
            });
        }
        if !matches!(call.current_state, CallState::Connected | CallState::Held) {
            return Err(CallError::InvalidTransition {
                state: call.current_state,
                action: "CONSULT",
            });
        }
        let participants = call_participants::Entity::find()
            .filter(call_participants::Column::CallId.eq(call.id))
            .all(self.db())
            .await?;
        let transferring = participants
            .iter()
            .find(|p| p.id == request.transferring_participant.as_uuid())
            .ok_or(CallError::NotFound)?
            .clone();
        if transferring.ended_at.is_some() {
            return Err(CallError::InvalidState(
                "the transferring participant has left the call",
            ));
        }
        if transferring.principal_id == target.as_uuid() {
            return Err(CallError::Validation(
                "the target is the transferring participant.",
            ));
        }
        if participants
            .iter()
            .any(|p| p.principal_id == target.as_uuid())
        {
            return Err(CallError::Conflict(
                "the target already participates in this call",
            ));
        }
        {
            let txn = self.db().begin().await?;
            let in_flight = self.calls.active_transfer(&txn, &call).await?.is_some();
            txn.commit().await?;
            if in_flight {
                return Err(CallError::InvalidState("a transfer is already in flight"));
            }
        }
        if active_consultation(self.db(), call.id).await? {
            return Err(CallError::Conflict(
                "a consultation is already in progress for this call",
            ));
        }
        let cmd = CallCommandContext::customer(ctx);
        let held = call.current_state == CallState::Connected;
        if held {
            self.hold(&cmd, call_id, call.version, None).await?;
        }
        let created = match self
            .create_app_call(
                ctx,
                CreateAppCall {
                    kind: request.kind,
                    initiator: PrincipalId::from_uuid(transferring.principal_id),
                    initiator_endpoint: request.initiator_endpoint,
                    targets: vec![target],
                    initiator_display_name: None,
                    privacy_relay_session: None,
                    key: request.key.clone(),
                },
            )
            .await
        {
            Ok(created) => created,
            Err(error) => {
                if held {
                    self.resume_original(&cmd, call_id).await;
                }
                return Err(error);
            }
        };
        let consultation_call = call_id_of(&created.body)?;
        let now = self.calls.now();
        let id = Uuid::new_v4();
        let txn = self.db().begin().await?;
        let original = self.calls.lock_call(&txn, scope, call_id).await?;
        if active_consultation(&txn, call.id).await? {
            // A concurrent consultation won: this one's Call is ended again.
            drop(txn);
            self.end_consultation_call(consultation_call).await;
            return Err(CallError::Conflict(
                "a consultation is already in progress for this call",
            ));
        }
        let mut values = vec![id.into()];
        values.extend(scope_values(scope));
        values.extend([
            call.id.into(),
            consultation_call.into(),
            transferring.id.into(),
            target.as_uuid().into(),
            held.into(),
            now.into(),
        ]);
        exec(
            &txn,
            "INSERT INTO call_consultations (id, tenant_id, application_id, environment_id, call_id, \
               consultation_call_id, transferring_participant_id, target_principal_id, state, \
               held_original, transfer_id, completion_attempts, version, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'CONSULTING', $9, NULL, 0, 1, $10, $10)",
            values,
        )
        .await?;
        let row = consultation_by_id(&txn, id)
            .await?
            .ok_or(CallError::NotFound)?;
        txn.commit().await?;
        crate::metrics::inc("calling_consultations_total", &[("result", "STARTED")]);
        tracing::info!(call_id = %call.id, consultation_id = %id, %consultation_call, "consultation started");
        Ok(ConsultationStarted {
            status: 201,
            body: json!({
                "consultation": view(row),
                "call": summary(&original),
                "consultationCall": created.body,
            }),
            replayed: created.replayed,
        })
    }

    /// `POST /v1/calls/{call_id}/consultations/{id}/complete`: the canonical transfer of the
    /// original Call from the transferring participant to the consulted target.
    pub async fn complete_consultation(
        &self,
        ctx: &PlatformAuthContext,
        call_id: CallId,
        consultation_id: Uuid,
        expected_version: i64,
    ) -> CallResult<CommandOutcome> {
        let scope = CallScope::of_platform(ctx);
        let row =
            consultation_of(self.db(), scope, call_id.as_uuid(), consultation_id, false).await?;
        match row.state.as_str() {
            "CONSULTING" => {}
            "COMPLETING" | "COMPLETED" => {
                // Already completing / completed: the current state (idempotent).
                let call = self.calls_row(scope, call_id).await?;
                return Ok(CommandOutcome {
                    body: json!({ "consultation": view(row), "call": summary(&call) }),
                    replayed: true,
                });
            }
            _ => return Err(CallError::Conflict("the consultation was cancelled")),
        }
        let consulted = calls::Entity::find_by_id(row.consultation_call_id)
            .one(self.db())
            .await?
            .ok_or(CallError::NotFound)?;
        if consulted.current_state != CallState::Connected {
            return Err(CallError::InvalidState(
                "the consultation call is not connected",
            ));
        }
        let resolved = self
            .resolve_transfer_target(
                ctx,
                call_id,
                TransferTarget::Principal(PrincipalId::from_uuid(row.target_principal_id)),
            )
            .await?;
        let attempt = row.completion_attempts + 1;
        let key = CommandKey {
            operation: CommandOperation::Transfer,
            key: IdempotencyKey::parse(&format!("consultation:{}:{attempt}", row.id))?,
            fingerprint: fingerprint(
                CommandOperation::Transfer,
                Some(call_id.as_uuid()),
                &json!({ "consultationId": row.id, "attempt": attempt }),
            ),
        };
        let outcome = self
            .transfer(
                ctx,
                call_id,
                TransferRequest {
                    transferring_participant: CallParticipantId::from_uuid(
                        row.transferring_participant_id,
                    ),
                    target: resolved.principal,
                    expected_version,
                    key,
                    enterprise_destination: resolved.destination,
                },
            )
            .await?;
        let transfer_id: Uuid = outcome.body["transfer"]["transferId"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .ok_or(CallError::NotFound)?;
        let now = self.calls.now();
        exec(
            self.db(),
            "UPDATE call_consultations SET state = 'COMPLETING', transfer_id = $2, \
               completion_attempts = $3, version = version + 1, \
               updated_at = GREATEST(updated_at, $4) \
             WHERE id = $1 AND state = 'CONSULTING' AND completion_attempts = $5",
            vec![
                row.id.into(),
                transfer_id.into(),
                attempt.into(),
                now.into(),
                row.completion_attempts.into(),
            ],
        )
        .await?;
        // The transfer may already have finished (a very fast decline or join) before it
        // was recorded here: settle the consultation from the transfer's outcome.
        if let Some(transfer) = call_transfers::Entity::find_by_id(transfer_id)
            .one(self.db())
            .await?
        {
            match transfer.state {
                TransferState::Completed => {
                    self.consultation_transfer_completed(call_id.as_uuid(), transfer_id)
                        .await
                }
                TransferState::Failed | TransferState::Cancelled => {
                    exec(
                        self.db(),
                        "UPDATE call_consultations SET state = 'CONSULTING', version = version + 1, \
                           updated_at = GREATEST(updated_at, $3) \
                         WHERE call_id = $1 AND transfer_id = $2 AND state = 'COMPLETING'",
                        vec![call_id.as_uuid().into(), transfer_id.into(), now.into()],
                    )
                    .await?;
                }
                _ => {}
            }
        }
        // The target's client learns, on the consultation Call, that the ring offer of
        // the original Call is this consultation's transfer.
        let mut data = Map::new();
        data.insert("consultationId".into(), json!(row.id));
        data.insert("originalCallId".into(), json!(row.call_id));
        data.insert("transferId".into(), json!(transfer_id));
        let mut pending = PendingSignals::default();
        let cmd = CallCommandContext::customer(ctx);
        self.call_signal(
            self.db(),
            &mut pending,
            SignalType::ConsultationTransfer,
            &consulted,
            cmd.correlation_id().as_str(),
            data,
        )
        .await?;
        self.publish(pending).await;
        crate::metrics::inc("calling_consultations_total", &[("result", "COMPLETING")]);
        let row = consultation_by_id(self.db(), row.id)
            .await?
            .ok_or(CallError::NotFound)?;
        Ok(CommandOutcome {
            body: json!({
                "consultation": view(row),
                "call": outcome.body["call"].clone(),
                "transfer": outcome.body["transfer"].clone(),
            }),
            replayed: outcome.replayed,
        })
    }

    /// `POST /v1/calls/{call_id}/consultations/{id}/cancel`: the consultation Call ends and
    /// the original Call resumes (when the consultation held it).
    pub async fn cancel_consultation(
        &self,
        ctx: &PlatformAuthContext,
        call_id: CallId,
        consultation_id: Uuid,
    ) -> CallResult<CommandOutcome> {
        let scope = CallScope::of_platform(ctx);
        let txn = self.db().begin().await?;
        let row = consultation_of(&txn, scope, call_id.as_uuid(), consultation_id, true).await?;
        match row.state.as_str() {
            "CONSULTING" => {}
            "CANCELLED" => {
                drop(txn);
                let call = self.calls_row(scope, call_id).await?;
                return Ok(CommandOutcome {
                    body: json!({ "consultation": view(row), "call": summary(&call),
                                  "resumed": false }),
                    replayed: true,
                });
            }
            "COMPLETING" => {
                return Err(CallError::InvalidState(
                    "the consultation's transfer is in flight",
                ))
            }
            _ => return Err(CallError::Conflict("the consultation already completed")),
        }
        let now = self.calls.now();
        exec(
            &txn,
            "UPDATE call_consultations SET state = 'CANCELLED', ended_at = $2, \
               version = version + 1, updated_at = GREATEST(updated_at, $2) WHERE id = $1",
            vec![row.id.into(), now.into()],
        )
        .await?;
        txn.commit().await?;
        self.end_consultation_call(row.consultation_call_id).await;
        let cmd = CallCommandContext::customer(ctx);
        let resumed = row.held_original && self.resume_original(&cmd, call_id).await;
        crate::metrics::inc("calling_consultations_total", &[("result", "CANCELLED")]);
        let call = self.calls_row(scope, call_id).await?;
        let row = consultation_by_id(self.db(), row.id)
            .await?
            .ok_or(CallError::NotFound)?;
        Ok(CommandOutcome {
            body: json!({ "consultation": view(row), "call": summary(&call), "resumed": resumed }),
            replayed: false,
        })
    }

    /// Inside a failing transfer's transaction: a consultation completing through it goes
    /// back to CONSULTING (the consultation Call is untouched; complete or cancel again).
    pub(crate) async fn consultation_transfer_failed_in<C: ConnectionTrait>(
        &self,
        db: &C,
        call_id: Uuid,
        transfer_id: Uuid,
    ) -> CallResult<()> {
        exec(
            db,
            "UPDATE call_consultations SET state = 'CONSULTING', version = version + 1, \
               updated_at = GREATEST(updated_at, $3) \
             WHERE call_id = $1 AND transfer_id = $2 AND state = 'COMPLETING'",
            vec![call_id.into(), transfer_id.into(), self.calls.now().into()],
        )
        .await?;
        Ok(())
    }

    /// After a transfer of `call_id` completed: a consultation completing through it is
    /// COMPLETED and its consultation Call ends (best effort; errors are logged).
    pub(crate) async fn consultation_transfer_completed(&self, call_id: Uuid, transfer_id: Uuid) {
        let now = self.calls.now();
        let ended = self
            .db()
            .query_one(stmt(
                "UPDATE call_consultations SET state = 'COMPLETED', ended_at = $3, \
                   version = version + 1, updated_at = GREATEST(updated_at, $3) \
                 WHERE call_id = $1 AND transfer_id = $2 AND state = 'COMPLETING' \
                 RETURNING consultation_call_id",
                vec![call_id.into(), transfer_id.into(), now.into()],
            ))
            .await;
        match ended {
            Ok(Some(row)) => {
                if let Ok(consultation_call) = row.try_get_by_index::<Uuid>(0) {
                    crate::metrics::inc("calling_consultations_total", &[("result", "COMPLETED")]);
                    tracing::info!(%call_id, %consultation_call, "attended transfer completed");
                    self.end_consultation_call(consultation_call).await;
                }
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(%call_id, %error, "consultation completion not recorded")
            }
        }
    }

    /// Ends (or cancels, before it was answered) a consultation Call as the system.
    async fn end_consultation_call(&self, call_id: Uuid) {
        let Ok(Some(call)) = calls::Entity::find_by_id(call_id).one(self.db()).await else {
            return;
        };
        let cmd = system_cmd(&call);
        let id = CallId::from_uuid(call.id);
        let result = match call.current_state {
            CallState::Answered
            | CallState::Connected
            | CallState::Held
            | CallState::Transferring => self.end(&cmd, id, call.version, None).await.map(|_| ()),
            CallState::Ending => Ok(()),
            state if state.is_terminal() => Ok(()),
            _ => self.cancel(&cmd, id, call.version, None).await.map(|_| ()),
        };
        if let Err(error) = result {
            tracing::warn!(%call_id, %error, "consultation call not ended; it ends by its own rules");
        }
    }

    /// Resumes the original Call the consultation held (best effort). True when resumed.
    async fn resume_original(&self, cmd: &CallCommandContext, call_id: CallId) -> bool {
        let Ok(call) = self.calls_row(cmd.scope(), call_id).await else {
            return false;
        };
        if call.current_state != CallState::Held {
            return false;
        }
        match self.resume(cmd, call_id, call.version, None).await {
            Ok(_) => true,
            Err(error) => {
                tracing::warn!(call_id = %call.id, %error, "original call stays held after the consultation");
                false
            }
        }
    }
}
