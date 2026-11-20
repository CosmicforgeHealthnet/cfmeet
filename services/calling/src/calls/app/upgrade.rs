//! C5 — upgrading a voice call to video (`POST /v1/calls/{id}/upgrade`).
//!
//! * Authority: the application backend (`calls.write`) in its own scope, like every
//!   call-control command; the CF Meet web adapter additionally allows it only for an
//!   active party of the Call (CAL-002 party rules).
//! * Rules: VOICE → VIDEO exactly once (never back), while the Call is DIALING, RINGING,
//!   ANSWERED, CONNECTED or HELD (CF2: switching to video while it still rings offers the
//!   Call as a video call — the callee's offer reads the Call's kind; ENDING, TRANSFERRING
//!   and terminal Calls, and the setup states before DIALING, are refused). Protected (Privacy Relay) calls and
//!   calls with a public / private network leg stay VOICE (422). Upgrading a Call that is
//!   already VIDEO is an idempotent no-op answering the current Call.
//! * Effect (one transaction, Call row locked, `expectedVersion` checked): the kind
//!   changes, every ACTIVE participant's authorized permissions gain video publishing,
//!   CALL_KIND_CHANGED is recorded and `call.kind_changed` is signalled to every endpoint
//!   of the Call. The version is unchanged (it counts state transitions).
//! * Media: after commit the provider receives each connected session's re-derived
//!   effective permissions (HOLD still masks them). Granting `canPublishVideo` never turns
//!   a camera on — the client publishes only when its user chooses. A failed push is
//!   recorded (CALL_MEDIA_ENFORCEMENT_PENDING) and repaired by reconciliation, which
//!   compares provider permissions with the server-derived ones; refreshed and newly
//!   issued media credentials derive the VIDEO grant from the database.

use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};
use serde_json::{json, Map, Value};

use super::{ringing::replay, summary, AppCallService, CommandOutcome, PendingSignals};
use crate::{
    calls::{
        attributes::SafeValue,
        command::CallCommandContext,
        errors::{CallError, CallResult},
        events::CallEventType,
        ids::CallId,
        kinds::{CallKind, CommandOperation},
        service::CommandKey,
        state::CallState,
    },
    models::calls,
    signaling::signals::SignalType,
};

fn upgrade_body(call: &calls::Model) -> Value {
    json!({ "call": summary(call), "operation": CommandOperation::Upgrade.as_str() })
}

/// States in which a VOICE Call may become VIDEO (mirrors the database guard of 000049,
/// widened to DIALING and RINGING by the call-kind-offer history, CF2).
pub(crate) fn upgradable_state(state: CallState) -> bool {
    matches!(
        state,
        CallState::Dialing
            | CallState::Ringing
            | CallState::Answered
            | CallState::Connected
            | CallState::Held
    )
}

impl AppCallService {
    /// `POST /v1/calls/{id}/upgrade` (C5).
    pub async fn upgrade(
        &self,
        cmd: &CallCommandContext,
        call_id: CallId,
        kind: CallKind,
        expected_version: i64,
        key: Option<&CommandKey>,
    ) -> CallResult<CommandOutcome> {
        if kind != CallKind::Video {
            return Err(CallError::Validation(
                "A call can only be upgraded to VIDEO.",
            ));
        }
        let scope = cmd.scope();
        if let Some(key) = key {
            if let Some(prior) = self.calls.prior_command(self.db(), scope, key).await? {
                return replay(prior);
            }
        }
        let provider = self.provider()?.clone();
        if !provider.capabilities().permission_update {
            return Err(CallError::CapabilityUnavailable(
                "upgrading a call needs server-side media permission updates",
            ));
        }
        let txn = self.db().begin().await?;
        if let Some(key) = key {
            if let Some(prior) = self.calls.begin_command_in(&txn, scope, key).await? {
                return replay(prior);
            }
        }
        let call = self.calls.lock_call(&txn, scope, call_id).await?;
        if call.call_kind == CallKind::Video {
            // Already VIDEO (upgraded before, or created as video): nothing changes.
            let body = upgrade_body(&call);
            txn.commit().await?;
            return Ok(CommandOutcome {
                body,
                replayed: true,
            });
        }
        if call.version != expected_version {
            return Err(CallError::StaleVersion {
                expected: expected_version,
                current: call.version,
            });
        }
        if call.current_state.is_terminal() {
            return Err(CallError::Terminal {
                state: call.current_state,
            });
        }
        if !upgradable_state(call.current_state) {
            return Err(CallError::InvalidTransition {
                state: call.current_state,
                action: CommandOperation::Upgrade.as_str(),
            });
        }
        if call.privacy_relay_ref.is_some() || has_network_leg(&txn, call.id).await? {
            return Err(CallError::CapabilityUnavailable(
                "protected and public-network calls stay VOICE",
            ));
        }
        self.require_personal_call(&call, None).await?;
        let (call, granted) = self.calls.upgrade_kind_in(&txn, cmd, call).await?;
        let body = upgrade_body(&call);
        if let Some(key) = key {
            self.calls
                .record_command_in(
                    &txn,
                    cmd,
                    key,
                    Some(call.id),
                    Some((200, &body)),
                    Some(call.version),
                    None,
                )
                .await?;
        }
        // The signal's callKind (VIDEO) is the change; data stays empty (ids / numbers only).
        let mut pending = PendingSignals::default();
        let mut data = Map::new();
        data.insert("participantsGrantedVideo".into(), json!(granted));
        self.call_signal(
            &txn,
            &mut pending,
            SignalType::KindChanged,
            &call,
            cmd.correlation_id().as_str(),
            data,
        )
        .await?;
        txn.commit().await?;
        self.publish(pending).await;
        crate::metrics::inc("calling_call_upgrades_total", &[("kind", "VIDEO")]);
        tracing::info!(call_id = %call.id, granted, "call upgraded to video");
        self.push_effective_permissions(&call, "UPGRADE").await;
        Ok(CommandOutcome {
            body,
            replayed: false,
        })
    }

    /// Pushes the effective permissions of the Call's current state to every authorized
    /// session at the provider after a committed permission change, and records the
    /// outcome: CALL_MEDIA_PERMISSIONS_UPDATED, or CALL_MEDIA_ENFORCEMENT_PENDING (the
    /// reconciler re-applies the server-derived permissions on its next pass).
    pub(crate) async fn push_effective_permissions(&self, call: &calls::Model, reason: &str) {
        let outcome = self
            .apply_effective_permissions(call, call.current_state)
            .await;
        let result: CallResult<()> = async {
            let txn = self.db().begin().await?;
            let locked = self
                .calls
                .lock_call(&txn, super::scope_of(call), CallId::from_uuid(call.id))
                .await?;
            let cmd = super::system_cmd(&locked);
            match outcome {
                Ok(applied) => {
                    self.calls
                        .record_media_fact_in(
                            &txn,
                            &cmd,
                            &locked,
                            CallEventType::CallMediaPermissionsUpdated,
                            vec![
                                ("reason", SafeValue::Text(reason.into())),
                                ("sessions_applied", SafeValue::from(applied)),
                            ],
                        )
                        .await?
                }
                Err(error) => {
                    tracing::warn!(call_id = %call.id, %error, reason, "media permission push pending; reconciliation repairs");
                    if let Some(binding) = self.calls.media_binding_in(&txn, call.id).await? {
                        self.calls
                            .record_media_failure_in(
                                &txn,
                                &cmd,
                                &locked,
                                binding,
                                "UPDATE_PERMISSIONS",
                                error.failure(),
                            )
                            .await?;
                    }
                }
            }
            txn.commit().await?;
            Ok(())
        }
        .await;
        if let Err(error) = result {
            tracing::warn!(call_id = %call.id, %error, "media permission evidence deferred");
        }
    }
}

/// Whether the Call has a public- or private-network leg (a telephone party): such a
/// Call stays VOICE.
async fn has_network_leg<C: ConnectionTrait>(db: &C, call_id: uuid::Uuid) -> CallResult<bool> {
    Ok(db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT 1 FROM call_legs WHERE call_id = $1 AND leg_kind <> 'APPLICATION' LIMIT 1",
            vec![call_id.into()],
        ))
        .await?
        .is_some())
}
