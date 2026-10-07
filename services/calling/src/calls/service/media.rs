//! Media authority records (Part C §21–§24) — transaction-scoped primitives over the
//! private media binding, the per-participant authorized permissions and each session's
//! media access / generation. No provider is called from here and no provider detail is
//! ever written into an event.

use chrono::{DateTime, Duration, Utc};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseTransaction, EntityTrait, QueryFilter,
};
use uuid::Uuid;

use super::{data, text, CallService, EventDraft};
use crate::{
    calls::{
        attributes::SafeValue,
        command::CallCommandContext,
        errors::{CallError, CallResult},
        events::CallEventType,
        ids::{CallId, CallMediaSessionId, MediaPermissionsId},
        kinds::{MediaAccess, MediaFailure, MediaSessionStatus},
    },
    media::{MediaPermissions, MediaResourceName, MEDIA_POLICY_VERSION},
    models::{
        call_media_sessions, call_participant_media_permissions, call_participants, call_sessions,
        calls,
    },
};

/// Input of [`CallService::record_media_binding_in`].
pub(crate) struct NewMediaBinding<'a> {
    pub route_id: Uuid,
    pub leg_id: Uuid,
    pub adapter: &'a str,
    pub max_participants: i32,
}

/// Retry back-off of provider operations: 2, 4, 8 … seconds, at most one minute.
pub(crate) fn backoff(failures: i32) -> Duration {
    Duration::seconds(2i64.saturating_pow(failures.clamp(1, 6) as u32).min(60))
}

impl CallService {
    /// Records the Call's private media binding (ENSURING) under its canonical name.
    pub(crate) async fn record_media_binding_in(
        &self,
        txn: &DatabaseTransaction,
        call: &calls::Model,
        new: NewMediaBinding<'_>,
    ) -> CallResult<call_media_sessions::Model> {
        let at = self.now().max(call.updated_at);
        Ok(call_media_sessions::ActiveModel {
            id: Set(CallMediaSessionId::generate().as_uuid()),
            call_id: Set(call.id),
            route_id: Set(new.route_id),
            leg_id: Set(new.leg_id),
            tenant_id: Set(call.tenant_id),
            application_id: Set(call.application_id),
            environment_id: Set(call.environment_id),
            provider_adapter: Set(new.adapter.to_string()),
            provider_resource_ref: Set(None),
            media_resource_name: Set(MediaResourceName::for_call(CallId::from_uuid(call.id))
                .as_str()
                .to_string()),
            status: Set(MediaSessionStatus::Ensuring),
            max_participants: Set(new.max_participants),
            enforcement_pending: Set(false),
            failure_count: Set(0),
            last_failure: Set(None),
            created_at: Set(at),
            ensured_at: Set(None),
            terminated_at: Set(None),
            last_reconciled_at: Set(None),
            next_reconcile_at: Set(at),
            updated_at: Set(at),
        }
        .insert(txn)
        .await?)
    }

    pub(crate) async fn media_binding_in(
        &self,
        txn: &DatabaseTransaction,
        call_id: Uuid,
    ) -> CallResult<Option<call_media_sessions::Model>> {
        Ok(call_media_sessions::Entity::find()
            .filter(call_media_sessions::Column::CallId.eq(call_id))
            .one(txn)
            .await?)
    }

    /// ENSURING → ACTIVE once the provider confirmed the resource; CALL_MEDIA_SESSION_
    /// CREATED (no provider detail in the event).
    pub(crate) async fn activate_media_binding_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        binding: call_media_sessions::Model,
        resource_ref: Option<String>,
        next_reconcile_at: DateTime<Utc>,
    ) -> CallResult<call_media_sessions::Model> {
        if binding.status != MediaSessionStatus::Ensuring {
            return Ok(binding);
        }
        let at = self.now().max(binding.updated_at);
        let mut active: call_media_sessions::ActiveModel = binding.into();
        active.status = Set(MediaSessionStatus::Active);
        active.ensured_at = Set(Some(at));
        if resource_ref.is_some() {
            active.provider_resource_ref = Set(resource_ref);
        }
        active.next_reconcile_at = Set(next_reconcile_at.max(at));
        active.last_reconciled_at = Set(Some(at));
        active.updated_at = Set(at);
        let binding = active.update(txn).await?;
        self.append_event(
            txn,
            cmd,
            call,
            EventDraft::fact(
                CallEventType::CallMediaSessionCreated,
                data(vec![
                    ("media_session_id", text(binding.id)),
                    ("route_id", text(binding.route_id)),
                    ("leg_id", text(binding.leg_id)),
                    (
                        "max_participants",
                        SafeValue::Integer(binding.max_participants.into()),
                    ),
                ])?,
            ),
            at,
        )
        .await?;
        Ok(binding)
    }

    /// Records a failed provider operation on the binding (retry evidence, back-off) and
    /// a CALL_MEDIA_ENFORCEMENT_PENDING event naming the neutral failure.
    pub(crate) async fn record_media_failure_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        binding: call_media_sessions::Model,
        operation: &'static str,
        failure: MediaFailure,
    ) -> CallResult<call_media_sessions::Model> {
        let at = self.now().max(binding.updated_at);
        let failures = binding.failure_count.saturating_add(1);
        let mut active: call_media_sessions::ActiveModel = binding.into();
        active.failure_count = Set(failures);
        active.last_failure = Set(Some(failure));
        active.enforcement_pending = Set(true);
        active.next_reconcile_at = Set(at + backoff(failures));
        active.updated_at = Set(at);
        let binding = active.update(txn).await?;
        self.append_event(
            txn,
            cmd,
            call,
            EventDraft::fact(
                CallEventType::CallMediaEnforcementPending,
                data(vec![
                    ("media_session_id", text(binding.id)),
                    ("operation", text(operation)),
                    ("failure", text(failure)),
                    ("failure_count", SafeValue::Integer(failures.into())),
                ])?,
            ),
            at,
        )
        .await?;
        Ok(binding)
    }

    /// → TERMINATED once the provider confirmed the resource is gone; CALL_MEDIA_SESSION_
    /// TERMINATED.
    pub(crate) async fn terminate_media_binding_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        binding: call_media_sessions::Model,
        already_absent: bool,
    ) -> CallResult<call_media_sessions::Model> {
        if binding.status == MediaSessionStatus::Terminated {
            return Ok(binding);
        }
        let at = self.now().max(binding.updated_at);
        let mut active: call_media_sessions::ActiveModel = binding.into();
        active.status = Set(MediaSessionStatus::Terminated);
        active.terminated_at = Set(Some(at));
        active.enforcement_pending = Set(false);
        active.last_reconciled_at = Set(Some(at));
        active.updated_at = Set(at);
        let binding = active.update(txn).await?;
        self.append_event(
            txn,
            cmd,
            call,
            EventDraft::fact(
                CallEventType::CallMediaSessionTerminated,
                data(vec![
                    ("media_session_id", text(binding.id)),
                    ("already_absent", SafeValue::Bool(already_absent)),
                ])?,
            ),
            at,
        )
        .await?;
        Ok(binding)
    }

    /// Marks the binding reconciled (clearing enforcement when `clean`) and schedules
    /// the next pass.
    pub(crate) async fn mark_media_reconciled_in(
        &self,
        txn: &DatabaseTransaction,
        binding: call_media_sessions::Model,
        clean: bool,
        next_at: DateTime<Utc>,
    ) -> CallResult<call_media_sessions::Model> {
        let at = self.now().max(binding.updated_at);
        let mut active: call_media_sessions::ActiveModel = binding.into();
        active.last_reconciled_at = Set(Some(at));
        active.next_reconcile_at = Set(next_at.max(at));
        if clean {
            active.enforcement_pending = Set(false);
            active.failure_count = Set(0);
            active.last_failure = Set(None);
        }
        active.updated_at = Set(at);
        Ok(active.update(txn).await?)
    }

    /// Creates the participant's authorized permissions from the Call kind's policy
    /// defaults if absent (never from client input).
    pub(crate) async fn ensure_media_permissions_in(
        &self,
        txn: &DatabaseTransaction,
        call: &calls::Model,
        participant: &call_participants::Model,
    ) -> CallResult<call_participant_media_permissions::Model> {
        if let Some(existing) = call_participant_media_permissions::Entity::find()
            .filter(call_participant_media_permissions::Column::CallId.eq(call.id))
            .filter(call_participant_media_permissions::Column::ParticipantId.eq(participant.id))
            .one(txn)
            .await?
        {
            return Ok(existing);
        }
        let defaults = MediaPermissions::default_for(call.call_kind);
        let at = self.now().max(participant.updated_at);
        Ok(call_participant_media_permissions::ActiveModel {
            id: Set(MediaPermissionsId::generate().as_uuid()),
            call_id: Set(call.id),
            participant_id: Set(participant.id),
            tenant_id: Set(call.tenant_id),
            application_id: Set(call.application_id),
            environment_id: Set(call.environment_id),
            call_kind: Set(call.call_kind),
            revision: Set(1),
            can_publish_audio: Set(defaults.can_publish_audio),
            can_publish_video: Set(defaults.can_publish_video),
            can_publish_data: Set(defaults.can_publish_data),
            can_subscribe: Set(defaults.can_subscribe),
            policy_version: Set(MEDIA_POLICY_VERSION.to_string()),
            created_at: Set(at),
            updated_at: Set(at),
        }
        .insert(txn)
        .await?)
    }

    /// The participant's persisted authorized permissions (none if never authorized).
    pub(crate) async fn authorized_permissions_in(
        &self,
        txn: &DatabaseTransaction,
        call: &calls::Model,
        participant_id: Uuid,
    ) -> CallResult<MediaPermissions> {
        Ok(call_participant_media_permissions::Entity::find()
            .filter(call_participant_media_permissions::Column::CallId.eq(call.id))
            .filter(call_participant_media_permissions::Column::ParticipantId.eq(participant_id))
            .one(txn)
            .await?
            .map(|p| {
                MediaPermissions {
                    can_publish_audio: p.can_publish_audio,
                    can_publish_video: p.can_publish_video,
                    can_publish_data: p.can_publish_data,
                    can_subscribe: p.can_subscribe,
                }
                .clamp_to(call.call_kind)
            })
            .unwrap_or(MediaPermissions::NONE))
    }

    /// NONE → AUTHORIZED: the session may now obtain media credentials. Database-guarded:
    /// a recipient session must have won its participant's answer.
    pub(crate) async fn authorize_session_media_in(
        &self,
        txn: &DatabaseTransaction,
        session: call_sessions::Model,
    ) -> CallResult<call_sessions::Model> {
        if session.media_access == MediaAccess::Authorized {
            return Ok(session);
        }
        if session.media_access == MediaAccess::Revoked || session.removed_at.is_some() {
            return Err(CallError::InvalidState(
                "the session's media access was revoked",
            ));
        }
        let at = self.now().max(session.updated_at);
        let mut active: call_sessions::ActiveModel = session.into();
        active.media_access = Set(MediaAccess::Authorized);
        active.updated_at = Set(at);
        Ok(active.update(txn).await?)
    }

    /// Advances the session's media generation for a new join credential and records the
    /// issuance (generation, expiry, capabilities — never the credential).
    pub(crate) async fn issue_media_generation_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        session: call_sessions::Model,
        expires_at: DateTime<Utc>,
        permissions: MediaPermissions,
    ) -> CallResult<call_sessions::Model> {
        if session.media_access != MediaAccess::Authorized {
            return Err(CallError::InvalidState("the session holds no media access"));
        }
        let generation = session.media_generation.saturating_add(1);
        let at = self.now().max(session.updated_at);
        let mut active: call_sessions::ActiveModel = session.into();
        active.media_generation = Set(generation);
        active.updated_at = Set(at);
        let session = active.update(txn).await?;
        self.append_event(
            txn,
            cmd,
            call,
            EventDraft::fact(
                CallEventType::CallMediaCredentialIssued,
                data(vec![
                    ("session_id", text(session.id)),
                    ("participant_id", text(session.participant_id)),
                    ("media_generation", SafeValue::Integer(generation.into())),
                    (
                        "expires_at",
                        text(expires_at.to_rfc3339_opts(chrono::SecondsFormat::Micros, true)),
                    ),
                    (
                        "can_publish_audio",
                        SafeValue::Bool(permissions.can_publish_audio),
                    ),
                    (
                        "can_publish_video",
                        SafeValue::Bool(permissions.can_publish_video),
                    ),
                    (
                        "can_publish_data",
                        SafeValue::Bool(permissions.can_publish_data),
                    ),
                    ("can_subscribe", SafeValue::Bool(permissions.can_subscribe)),
                ])?,
            ),
            at,
        )
        .await?;
        Ok(session)
    }

    /// Records that the provider connection of the session's current generation was
    /// verified and received its server-derived permissions.
    pub(crate) async fn admit_session_media_in(
        &self,
        txn: &DatabaseTransaction,
        session: call_sessions::Model,
    ) -> CallResult<call_sessions::Model> {
        let at = self.now().max(session.updated_at);
        let mut active: call_sessions::ActiveModel = session.into();
        active.media_admitted_at = Set(Some(at));
        active.updated_at = Set(at);
        Ok(active.update(txn).await?)
    }

    /// Sets / clears the session's pending provider enforcement flag.
    pub(crate) async fn set_session_enforcement_in(
        &self,
        txn: &DatabaseTransaction,
        session: call_sessions::Model,
        pending: bool,
    ) -> CallResult<call_sessions::Model> {
        if session.media_enforcement_pending == pending {
            return Ok(session);
        }
        let at = self.now().max(session.updated_at);
        let mut active: call_sessions::ActiveModel = session.into();
        active.media_enforcement_pending = Set(pending);
        active.updated_at = Set(at);
        Ok(active.update(txn).await?)
    }

    /// Appends a media fact (permissions applied / eviction / reconciliation) with safe
    /// details.
    pub(crate) async fn record_media_fact_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        event_type: CallEventType,
        details: Vec<(&'static str, SafeValue)>,
    ) -> CallResult<()> {
        let at = self.now().max(call.updated_at);
        self.append_event(
            txn,
            cmd,
            call,
            EventDraft::fact(event_type, data(details)?),
            at,
        )
        .await?;
        Ok(())
    }
}
