//! Quality and usage records: provider-independent, bounded, idempotent (Part C §16).

use std::collections::BTreeMap;

use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseTransaction, EntityTrait, QueryFilter,
    QueryOrder, TransactionTrait,
};

use super::{data, participation::session_of, routing::leg_of, text, CallService, EventDraft};
use crate::{
    calls::{
        attributes::SafeValue,
        command::CallCommandContext,
        duration::union_duration,
        errors::{CallError, CallResult},
        events::CallEventType,
        ids::{CallId, CallParticipantId, CallQualityRecordId, CallUsageRecordId},
        kinds::{UsageType, UsageUnit},
        quality::{CallQualityRecord, NewQualityRecord},
        sessions::CallSessionInterval,
        usage::{CallUsageRecord, IdempotencyKey, NewUsage},
    },
    models::{
        call_participants, call_quality_records, call_session_intervals, call_sessions,
        call_usage_records, calls,
    },
};

impl CallService {
    /// Appends a quality record. Its session / leg, when given, must belong to the Call.
    /// Allowed in any Call state (observations often arrive after the end).
    pub async fn record_quality(
        &self,
        cmd: &CallCommandContext,
        call_id: CallId,
        new: NewQualityRecord,
    ) -> CallResult<CallQualityRecord> {
        if new.metrics.is_empty() {
            return Err(CallError::Validation(
                "a quality record needs at least one metric",
            ));
        }
        if new.observed_to < new.observed_from {
            return Err(CallError::Validation("the observation window is reversed"));
        }
        let txn = self.db.begin().await?;
        let call = self.lock_call(&txn, cmd.scope(), call_id).await?;
        if let Some(session_id) = new.session_id {
            session_of(&txn, &call, session_id).await?;
        }
        if let Some(leg_id) = new.leg_id {
            leg_of(&txn, &call, leg_id).await?;
        }
        let at = self.now();
        let record = call_quality_records::ActiveModel {
            id: Set(CallQualityRecordId::generate().as_uuid()),
            call_id: Set(call.id),
            session_id: Set(new.session_id.map(|s| s.as_uuid())),
            leg_id: Set(new.leg_id.map(|l| l.as_uuid())),
            tenant_id: Set(call.tenant_id),
            application_id: Set(call.application_id),
            environment_id: Set(call.environment_id),
            observed_from: Set(new.observed_from),
            observed_to: Set(new.observed_to),
            metrics: Set(new.metrics.to_json()),
            created_at: Set(at),
        }
        .insert(&txn)
        .await?;
        let mut details = vec![
            ("quality_record_id", text(record.id)),
            ("metric_count", SafeValue::from(new.metrics.len())),
        ];
        if let Some(session_id) = record.session_id {
            details.push(("session_id", text(session_id)));
        }
        if let Some(leg_id) = record.leg_id {
            details.push(("leg_id", text(leg_id)));
        }
        self.append_event(
            &txn,
            cmd,
            &call,
            EventDraft::fact(CallEventType::CallQualityRecorded, data(details)?),
            at.max(call.updated_at),
        )
        .await?;
        txn.commit().await?;
        Ok(record.into())
    }

    /// Records a usage fact idempotently: the same idempotency key with the same fact
    /// returns the existing record (`false`); with a different fact it is a conflict.
    pub async fn record_usage(
        &self,
        cmd: &CallCommandContext,
        call_id: CallId,
        new: NewUsage,
    ) -> CallResult<(CallUsageRecord, bool)> {
        let txn = self.db.begin().await?;
        let call = self.lock_call(&txn, cmd.scope(), call_id).await?;
        let result = self.insert_usage(&txn, cmd, &call, new).await?;
        txn.commit().await?;
        Ok(result)
    }

    /// Finalizes a usage record (set once). Finalizing again is idempotent (`false`).
    pub async fn finalize_usage(
        &self,
        cmd: &CallCommandContext,
        call_id: CallId,
        usage_id: CallUsageRecordId,
    ) -> CallResult<(CallUsageRecord, bool)> {
        let txn = self.db.begin().await?;
        let call = self.lock_call(&txn, cmd.scope(), call_id).await?;
        let record = call_usage_records::Entity::find_by_id(usage_id.as_uuid())
            .filter(call_usage_records::Column::CallId.eq(call.id))
            .one(&txn)
            .await?
            .ok_or(CallError::NotFound)?;
        let result = self.finalize_usage_row(&txn, cmd, &call, record).await?;
        txn.commit().await?;
        Ok(result)
    }

    /// For a terminal Call: records and finalizes each participant's logical connected
    /// time (union of its sessions' intervals) as PARTICIPANT_CONNECTED_TIME, keyed
    /// `participant-connected-time:<participant id>`. Idempotent.
    pub async fn finalize_participation_usage(
        &self,
        cmd: &CallCommandContext,
        call_id: CallId,
    ) -> CallResult<Vec<CallUsageRecord>> {
        let txn = self.db.begin().await?;
        let call = self.lock_call(&txn, cmd.scope(), call_id).await?;
        let Some(ended_at) = call.ended_at.filter(|_| call.current_state.is_terminal()) else {
            return Err(CallError::InvalidState(
                "participation usage is final only for ended calls",
            ));
        };
        let participants = call_participants::Entity::find()
            .filter(call_participants::Column::CallId.eq(call.id))
            .order_by_asc(call_participants::Column::CreatedAt)
            .order_by_asc(call_participants::Column::Id)
            .all(&txn)
            .await?;
        let sessions = call_sessions::Entity::find()
            .filter(call_sessions::Column::CallId.eq(call.id))
            .all(&txn)
            .await?;
        let owner: BTreeMap<_, _> = sessions.iter().map(|s| (s.id, s.participant_id)).collect();
        let intervals: Vec<CallSessionInterval> = call_session_intervals::Entity::find()
            .filter(call_session_intervals::Column::CallId.eq(call.id))
            .all(&txn)
            .await?
            .into_iter()
            .map(Into::into)
            .collect();
        let mut out = Vec::new();
        for participant in participants {
            let spans: Vec<_> = intervals
                .iter()
                .filter(|i| owner.get(&i.session_id.as_uuid()) == Some(&participant.id))
                .map(CallSessionInterval::span)
                .collect();
            let first = spans.iter().map(|s| s.connected_at).min();
            let quantity = union_duration(&spans, ended_at).num_milliseconds().max(0);
            let new = NewUsage {
                usage_type: UsageType::ParticipantConnectedTime,
                participant_id: Some(CallParticipantId::from_uuid(participant.id)),
                session_id: None,
                leg_id: None,
                quantity,
                unit: UsageUnit::Milliseconds,
                usage_started_at: first,
                usage_ended_at: first.map(|_| ended_at),
                idempotency_key: IdempotencyKey::parse(&format!(
                    "participant-connected-time:{}",
                    participant.id
                ))?,
            };
            let (record, _) = self.insert_usage(&txn, cmd, &call, new).await?;
            let model = call_usage_records::Entity::find_by_id(record.id.as_uuid())
                .one(&txn)
                .await?
                .ok_or(CallError::NotFound)?;
            let (record, _) = self.finalize_usage_row(&txn, cmd, &call, model).await?;
            out.push(record);
        }
        txn.commit().await?;
        Ok(out)
    }

    async fn insert_usage(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        new: NewUsage,
    ) -> CallResult<(CallUsageRecord, bool)> {
        let subject_ok = match new.usage_type {
            UsageType::CallConnectedTime => true,
            UsageType::ParticipantConnectedTime => new.participant_id.is_some(),
            UsageType::SessionConnectedTime => new.session_id.is_some(),
            UsageType::LegConnectedTime => new.leg_id.is_some(),
        };
        if !subject_ok || new.quantity < 0 {
            return Err(CallError::Validation(
                "usage subject or quantity is invalid",
            ));
        }
        if let (Some(start), Some(end)) = (new.usage_started_at, new.usage_ended_at) {
            if end < start {
                return Err(CallError::Validation("the usage window is reversed"));
            }
        }
        if let Some(existing) = call_usage_records::Entity::find()
            .filter(call_usage_records::Column::CallId.eq(call.id))
            .filter(call_usage_records::Column::IdempotencyKey.eq(new.idempotency_key.as_str()))
            .one(txn)
            .await?
        {
            let existing: CallUsageRecord = existing.into();
            return if existing.matches(&new) {
                Ok((existing, false))
            } else {
                Err(CallError::Conflict(
                    "the idempotency key was already used for a different usage fact",
                ))
            };
        }
        if let Some(participant_id) = new.participant_id {
            call_participants::Entity::find_by_id(participant_id.as_uuid())
                .filter(call_participants::Column::CallId.eq(call.id))
                .one(txn)
                .await?
                .ok_or(CallError::NotFound)?;
        }
        if let Some(session_id) = new.session_id {
            session_of(txn, call, session_id).await?;
        }
        if let Some(leg_id) = new.leg_id {
            leg_of(txn, call, leg_id).await?;
        }
        let at = self.now();
        let record = call_usage_records::ActiveModel {
            id: Set(CallUsageRecordId::generate().as_uuid()),
            call_id: Set(call.id),
            participant_id: Set(new.participant_id.map(|p| p.as_uuid())),
            session_id: Set(new.session_id.map(|s| s.as_uuid())),
            leg_id: Set(new.leg_id.map(|l| l.as_uuid())),
            tenant_id: Set(call.tenant_id),
            application_id: Set(call.application_id),
            environment_id: Set(call.environment_id),
            usage_type: Set(new.usage_type),
            quantity: Set(new.quantity),
            unit: Set(new.unit),
            usage_started_at: Set(new.usage_started_at),
            usage_ended_at: Set(new.usage_ended_at),
            idempotency_key: Set(new.idempotency_key.as_str().to_string()),
            finalized_at: Set(None),
            created_at: Set(at),
            updated_at: Set(at),
        }
        .insert(txn)
        .await?;
        self.append_event(
            txn,
            cmd,
            call,
            EventDraft::fact(
                CallEventType::CallUsageRecorded,
                data(vec![
                    ("usage_record_id", text(record.id)),
                    ("usage_type", text(record.usage_type)),
                    ("quantity", SafeValue::Integer(record.quantity)),
                    ("unit", text(record.unit)),
                ])?,
            ),
            at.max(call.updated_at),
        )
        .await?;
        Ok((record.into(), true))
    }

    async fn finalize_usage_row(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        record: call_usage_records::Model,
    ) -> CallResult<(CallUsageRecord, bool)> {
        if record.finalized_at.is_some() {
            return Ok((record.into(), false));
        }
        let at = self.now().max(record.updated_at);
        let mut active: call_usage_records::ActiveModel = record.into();
        active.finalized_at = Set(Some(at));
        active.updated_at = Set(at);
        let record = active.update(txn).await?;
        self.append_event(
            txn,
            cmd,
            call,
            EventDraft::fact(
                CallEventType::CallUsageFinalized,
                data(vec![
                    ("usage_record_id", text(record.id)),
                    ("quantity", SafeValue::Integer(record.quantity)),
                ])?,
            ),
            at.max(call.updated_at),
        )
        .await?;
        Ok((record.into(), true))
    }
}
