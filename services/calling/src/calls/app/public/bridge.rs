//! Bridges between a Call's legs (Part E §79, §98, §108).
//!
//! * APPLICATION_MEDIA (app-to-phone, phone-to-app): Telephony anchors the public leg on
//!   the SIP / media edge and returns a single-use media join code; Calling's media
//!   gateway adds the reserved participant `cfm-pstn-<bridge>` to the Call's RTC resource
//!   that dials the edge with it. The code is used once and never stored or logged.
//! * PUBLIC_LEGS (phone-to-phone, callback relay): both public legs on the edge.
//!
//! CONNECTED is never inferred from a request being accepted: Telephony's
//! BRIDGE_ESTABLISHED / LEG_CONNECTED evidence is required (§78).

use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde_json::json;
use uuid::Uuid;

use super::{dispatch::Dispatched, enqueue_in, exec, stmt, CommandKind, CommandRow};
use crate::{
    calls::{
        app::AppCallService,
        errors::{CallError, CallResult},
        kinds::FailureCategory,
    },
    media::provider::MediaResourceName,
    models::calls,
    telephony::{media::bridge_identity, TelephonyError},
};

#[derive(Debug, Clone, FromQueryResult)]
pub(crate) struct BridgeRow {
    pub id: Uuid,
    pub kind: String,
    pub status: String,
    pub media_identity: Option<String>,
}

pub(crate) const BRIDGE_COLUMNS: &str = "id, kind, status, media_identity";

pub(crate) async fn live_bridge<C: ConnectionTrait>(
    db: &C,
    call_id: Uuid,
) -> CallResult<Option<BridgeRow>> {
    Ok(BridgeRow::find_by_statement(stmt(
        &format!(
            "SELECT {BRIDGE_COLUMNS} FROM call_public_bridges WHERE call_id = $1 \
             AND status IN ('REQUESTED', 'ATTACHING', 'ACTIVE')"
        ),
        vec![call_id.into()],
    ))
    .one(db)
    .await?)
}

/// Requests a bridge of `kind` over `telephony_legs` inside the caller's transaction
/// (at most one live bridge per Call; a repeat returns the existing one).
pub(crate) async fn request_bridge_in<C: ConnectionTrait>(
    txn: &C,
    call: &calls::Model,
    kind: &str,
    telephony_legs: &[Uuid],
    now: chrono::DateTime<chrono::Utc>,
) -> CallResult<Uuid> {
    if let Some(existing) = live_bridge(txn, call.id).await? {
        return Ok(existing.id);
    }
    let id = Uuid::new_v4();
    let identity = (kind == "APPLICATION_MEDIA").then(|| bridge_identity(id));
    exec(
        txn,
        "INSERT INTO call_public_bridges (id, call_id, tenant_id, application_id, environment_id, kind, status, \
           media_identity, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, 'REQUESTED', $7, $8, $8)",
        vec![
            id.into(),
            call.id.into(),
            call.tenant_id.into(),
            call.application_id.into(),
            call.environment_id.into(),
            kind.into(),
            identity.into(),
            now.into(),
        ],
    )
    .await?;
    let command_id = Uuid::new_v4();
    enqueue_in(
        txn,
        call,
        CommandKind::Bridge,
        None,
        None,
        Some(id),
        &format!("bridge:{id}"),
        json!({
            "commandId": command_id,
            "correlationId": cf_service_auth::CorrelationId::generate().as_str(),
            "kind": kind,
            "telephonyLegIds": telephony_legs,
        }),
        now,
    )
    .await?;
    Ok(id)
}

impl AppCallService {
    pub(super) async fn dispatch_bridge(
        &self,
        row: &CommandRow,
        call: &calls::Model,
    ) -> CallResult<Dispatched> {
        let bridge_id = row.bridge_id.ok_or(CallError::NotFound)?;
        let bridge = BridgeRow::find_by_statement(stmt(
            &format!("SELECT {BRIDGE_COLUMNS} FROM call_public_bridges WHERE id = $1"),
            vec![bridge_id.into()],
        ))
        .one(self.db())
        .await?
        .ok_or(CallError::NotFound)?;
        if bridge.status != "REQUESTED" || call.current_state.is_terminal() {
            self.complete_bridge_command(row.id, "DEAD", Some("NOT_REQUESTED"), None)
                .await?;
            return Ok(Dispatched::Dead);
        }
        let correlation = row.payload["correlationId"]
            .as_str()
            .and_then(|c| cf_service_auth::CorrelationId::parse(c).ok())
            .unwrap_or_else(cf_service_auth::CorrelationId::generate);
        let body = json!({
            "schemaVersion": "1",
            "commandId": row.id,
            "idempotencyKey": row.idempotency_key,
            "correlationId": correlation.as_str(),
            "callId": call.id,
            "kind": row.payload["kind"],
            "telephonyLegIds": row.payload["telephonyLegIds"],
        });
        match self
            .public
            .telephony
            .create_bridge(&correlation, &body)
            .await
        {
            Ok(answer) => {
                let telephony_bridge = answer["bridge"]["bridgeId"]
                    .as_str()
                    .and_then(|s| Uuid::parse_str(s).ok())
                    .ok_or(CallError::Validation("malformed bridge answer"))?;
                let join_code = answer["bridge"]["mediaJoinCode"]
                    .as_str()
                    .map(|code| zeroize::Zeroizing::new(code.to_string()));
                let now = self.calls.now();
                let txn = self.db().begin().await?;
                exec(
                    &txn,
                    "UPDATE call_public_bridges SET telephony_bridge_id = $2, \
                       status = CASE WHEN kind = 'APPLICATION_MEDIA' THEN 'ATTACHING' ELSE status END, \
                       updated_at = GREATEST(updated_at, $3) WHERE id = $1 AND status = 'REQUESTED'",
                    vec![bridge_id.into(), telephony_bridge.into(), now.into()],
                )
                .await?;
                exec(
                    &txn,
                    "UPDATE call_telephony_commands SET status = 'ACCEPTED', telephony_ref = $2, \
                       completed_at = GREATEST($3, created_at), lease_until = NULL, updated_at = GREATEST(updated_at, $3) \
                     WHERE id = $1 AND status = 'PENDING'",
                    vec![row.id.into(), telephony_bridge.into(), now.into()],
                )
                .await?;
                txn.commit().await?;
                if bridge.kind == "APPLICATION_MEDIA" {
                    self.attach_application_media(call, &bridge, join_code)
                        .await?;
                }
                Ok(Dispatched::Accepted)
            }
            Err(TelephonyError::Refused { code, .. }) => {
                self.complete_bridge_command(row.id, "REFUSED", Some(&code), None)
                    .await?;
                self.bridge_failed(call, bridge_id).await?;
                Ok(Dispatched::Refused)
            }
            Err(TelephonyError::Unavailable) => {
                exec(
                    self.db(),
                    "UPDATE call_telephony_commands SET attempts = attempts + 1, lease_until = NULL, \
                       next_attempt_at = $2, last_error_code = 'TELEPHONY_UNAVAILABLE' WHERE id = $1 AND status = 'PENDING'",
                    vec![
                        row.id.into(),
                        (self.calls.now() + super::retry_delay(row.attempts)).into(),
                    ],
                )
                .await?;
                Ok(Dispatched::Retry)
            }
        }
    }

    async fn complete_bridge_command(
        &self,
        id: Uuid,
        status: &str,
        code: Option<&str>,
        telephony_ref: Option<Uuid>,
    ) -> CallResult<()> {
        let now = self.calls.now();
        exec(
            self.db(),
            "UPDATE call_telephony_commands SET status = $2, completed_at = GREATEST($3, created_at), \
               last_error_code = COALESCE($4, last_error_code), telephony_ref = COALESCE(telephony_ref, $5), \
               lease_until = NULL, updated_at = GREATEST(updated_at, $3) WHERE id = $1 AND status = 'PENDING'",
            vec![
                id.into(),
                status.into(),
                now.into(),
                code.map(|c| c[..c.len().min(64)].to_string()).into(),
                telephony_ref.into(),
            ],
        )
        .await?;
        Ok(())
    }

    /// Attaches the RTC side with the single-use join code (used here once, dropped).
    async fn attach_application_media(
        &self,
        call: &calls::Model,
        bridge: &BridgeRow,
        join_code: Option<zeroize::Zeroizing<String>>,
    ) -> CallResult<()> {
        let (Some(gateway), Some(code), Some(identity)) = (
            self.public.media.as_ref(),
            join_code,
            bridge.media_identity.as_deref(),
        ) else {
            tracing::warn!(call_id = %call.id, "no public media gateway / join code: bridge cannot attach");
            return self.bridge_failed(call, bridge.id).await;
        };
        let room = MediaResourceName::for_call(crate::calls::ids::CallId::from_uuid(call.id));
        let room = room.as_str();
        match gateway.attach(room, identity, code.as_str()).await {
            Ok(()) => {
                tracing::info!(call_id = %call.id, bridge_id = %bridge.id, "application media attaching to the public bridge");
                Ok(())
            }
            Err(error) => {
                tracing::warn!(call_id = %call.id, %error, "public bridge media attachment failed");
                self.bridge_failed(call, bridge.id).await
            }
        }
    }

    /// The bridge cannot carry media: it fails, the public legs are released and the Call
    /// fails (MEDIA) unless it already ended.
    pub(crate) async fn bridge_failed(
        &self,
        call: &calls::Model,
        bridge_id: Uuid,
    ) -> CallResult<()> {
        let now = self.calls.now();
        exec(
            self.db(),
            "UPDATE call_public_bridges SET status = 'FAILED', ended_at = GREATEST($2, created_at), \
               updated_at = GREATEST(updated_at, $2) WHERE id = $1 AND status IN ('REQUESTED', 'ATTACHING', 'ACTIVE')",
            vec![bridge_id.into(), now.into()],
        )
        .await?;
        self.release_public_legs(call.id).await?;
        self.fail_public_call(call.id, FailureCategory::Media).await
    }

    /// Admission of a reserved public-bridge identity: only the identity of THIS Call's
    /// live APPLICATION_MEDIA bridge is admitted (audio both ways, no video / data);
    /// anything else is evicted like an unknown identity.
    pub(crate) async fn admit_public_bridge(
        &self,
        call: &calls::Model,
        participant: &crate::media::provider::ProviderParticipant,
    ) -> CallResult<Option<crate::media::events::EvictionReason>> {
        use crate::media::{
            events::EvictionReason,
            provider::{MediaParticipantRef, MediaSessionRef},
            MediaPermissions,
        };
        let provider = self.provider()?.clone();
        let bridge = live_bridge(self.db(), call.id).await?;
        let admitted = !call.current_state.is_terminal()
            && bridge.as_ref().is_some_and(|b| {
                b.kind == "APPLICATION_MEDIA"
                    && b.media_identity.as_deref() == Some(participant.identity.as_str())
                    && matches!(b.status.as_str(), "ATTACHING" | "ACTIVE")
            });
        let target = MediaParticipantRef {
            session: MediaSessionRef {
                name: MediaResourceName::for_call(crate::calls::ids::CallId::from_uuid(call.id)),
            },
            identity: participant.identity.clone(),
        };
        if admitted {
            provider
                .update_permissions(
                    &target,
                    MediaPermissions {
                        can_publish_audio: true,
                        can_publish_video: false,
                        can_publish_data: false,
                        can_subscribe: true,
                    },
                )
                .await
                .map_err(|_| CallError::MediaUnavailable)?;
            tracing::info!(call_id = %call.id, "public bridge media admitted");
            return Ok(None);
        }
        let _ = provider
            .update_permissions(&target, MediaPermissions::NONE)
            .await;
        let _ = provider.remove_session(&target).await;
        tracing::warn!(call_id = %call.id, "unexpected public bridge identity evicted");
        Ok(Some(EvictionReason::UnknownIdentity))
    }

    pub(super) async fn dispatch_unbridge(
        &self,
        row: &CommandRow,
        _call: &calls::Model,
    ) -> CallResult<Dispatched> {
        let Some(telephony_bridge) = row.payload["telephonyBridgeId"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
        else {
            self.complete_bridge_command(row.id, "DEAD", Some("NO_BRIDGE"), None)
                .await?;
            return Ok(Dispatched::Dead);
        };
        let correlation = cf_service_auth::CorrelationId::generate();
        match self
            .public
            .telephony
            .end_bridge(telephony_bridge, &correlation)
            .await
        {
            Ok(()) => {
                self.complete_bridge_command(row.id, "ACCEPTED", None, Some(telephony_bridge))
                    .await?;
                Ok(Dispatched::Accepted)
            }
            Err(TelephonyError::Refused { code, .. }) => {
                self.complete_bridge_command(row.id, "REFUSED", Some(&code), None)
                    .await?;
                Ok(Dispatched::Refused)
            }
            Err(TelephonyError::Unavailable) => {
                exec(
                    self.db(),
                    "UPDATE call_telephony_commands SET attempts = attempts + 1, lease_until = NULL, \
                       next_attempt_at = $2 WHERE id = $1 AND status = 'PENDING'",
                    vec![
                        row.id.into(),
                        (self.calls.now() + super::retry_delay(row.attempts)).into(),
                    ],
                )
                .await?;
                Ok(Dispatched::Retry)
            }
        }
    }
}
