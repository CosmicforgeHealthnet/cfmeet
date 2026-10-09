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
        let out = self.participation_usage_in(&txn, cmd, &call).await?;
        txn.commit().await?;
        Ok(out)
    }

    pub(crate) async fn participation_usage_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
    ) -> CallResult<Vec<CallUsageRecord>> {
        let Some(ended_at) = call.ended_at.filter(|_| call.current_state.is_terminal()) else {
            return Err(CallError::InvalidState(
                "participation usage is final only for ended calls",
            ));
        };
        let participants = call_participants::Entity::find()
            .filter(call_participants::Column::CallId.eq(call.id))
            .order_by_asc(call_participants::Column::CreatedAt)
            .order_by_asc(call_participants::Column::Id)
            .all(txn)
            .await?;
        let sessions = call_sessions::Entity::find()
            .filter(call_sessions::Column::CallId.eq(call.id))
            .all(txn)
            .await?;
        let owner: BTreeMap<_, _> = sessions.iter().map(|s| (s.id, s.participant_id)).collect();
        let intervals: Vec<CallSessionInterval> = call_session_intervals::Entity::find()
            .filter(call_session_intervals::Column::CallId.eq(call.id))
            .all(txn)
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
            let (record, _) = self.insert_usage(txn, cmd, call, new).await?;
            let model = call_usage_records::Entity::find_by_id(record.id.as_uuid())
                .one(txn)
                .await?
                .ok_or(CallError::NotFound)?;
            let (record, _) = self.finalize_usage_row(txn, cmd, call, model).await?;
            out.push(record);
        }
        Ok(out)
    }

    /// Usage of a finished application Call, from authoritative interval evidence:
    /// CALL_CONNECTED_TIME (union of every session's intervals, clipped to the Call's
    /// connected period), PARTICIPANT_CONNECTED_TIME per participant (union of its sessions —
    /// concurrent devices are not double counted) and SESSION_CONNECTED_TIME per session.
    /// Recorded and finalized idempotently (stable idempotency keys). Not billing.
    pub(crate) async fn finalize_app_usage_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
    ) -> CallResult<Vec<CallUsageRecord>> {
        let Some(ended_at) = call.ended_at.filter(|_| call.current_state.is_terminal()) else {
            return Err(CallError::InvalidState(
                "usage is final only for ended calls",
            ));
        };
        let sessions = call_sessions::Entity::find()
            .filter(call_sessions::Column::CallId.eq(call.id))
            .order_by_asc(call_sessions::Column::CreatedAt)
            .order_by_asc(call_sessions::Column::Id)
            .all(txn)
            .await?;
        let intervals: Vec<CallSessionInterval> = call_session_intervals::Entity::find()
            .filter(call_session_intervals::Column::CallId.eq(call.id))
            .all(txn)
            .await?
            .into_iter()
            .map(Into::into)
            .collect();
        let mut facts: Vec<NewUsage> = Vec::new();
        let all_spans: Vec<_> = intervals.iter().map(CallSessionInterval::span).collect();
        let call_quantity = match call.connected_at {
            Some(connected_at) => {
                let clipped: Vec<_> = all_spans
                    .iter()
                    .filter_map(|s| {
                        let start = s.connected_at.max(connected_at);
                        let end = s.disconnected_at.unwrap_or(ended_at).min(ended_at);
                        (end > start).then_some(crate::calls::duration::ConnectionSpan {
                            connected_at: start,
                            disconnected_at: Some(end),
                        })
                    })
                    .collect();
                union_duration(&clipped, ended_at).num_milliseconds().max(0)
            }
            None => 0,
        };
        facts.push(NewUsage {
            usage_type: UsageType::CallConnectedTime,
            participant_id: None,
            session_id: None,
            leg_id: None,
            quantity: call_quantity,
            unit: UsageUnit::Milliseconds,
            usage_started_at: call.connected_at,
            usage_ended_at: call.connected_at.map(|_| ended_at),
            idempotency_key: IdempotencyKey::parse(&format!("call-connected-time:{}", call.id))?,
        });
        for session in &sessions {
            let spans: Vec<_> = intervals
                .iter()
                .filter(|i| i.session_id.as_uuid() == session.id)
                .map(CallSessionInterval::span)
                .collect();
            let first = spans.iter().map(|s| s.connected_at).min();
            facts.push(NewUsage {
                usage_type: UsageType::SessionConnectedTime,
                participant_id: Some(CallParticipantId::from_uuid(session.participant_id)),
                session_id: Some(crate::calls::ids::CallSessionId::from_uuid(session.id)),
                leg_id: None,
                quantity: crate::calls::duration::total_duration(&spans, ended_at)
                    .num_milliseconds()
                    .max(0),
                unit: UsageUnit::Milliseconds,
                usage_started_at: first,
                usage_ended_at: first.map(|_| ended_at),
                idempotency_key: IdempotencyKey::parse(&format!(
                    "session-connected-time:{}",
                    session.id
                ))?,
            });
        }
        let mut out = Vec::new();
        for new in facts {
            let (record, _) = self.insert_usage(txn, cmd, call, new).await?;
            let model = call_usage_records::Entity::find_by_id(record.id.as_uuid())
                .one(txn)
                .await?
                .ok_or(CallError::NotFound)?;
            let (record, _) = self.finalize_usage_row(txn, cmd, call, model).await?;
            out.push(record);
        }
        out.extend(self.participation_usage_in(txn, cmd, call).await?);
        Ok(out)
    }

    /// Records a quality record inside the caller's transaction.
    pub(crate) async fn record_quality_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        new: NewQualityRecord,
    ) -> CallResult<CallQualityRecord> {
        if new.metrics.is_empty() || new.observed_to < new.observed_from {
            return Err(CallError::Validation(
                "a quality record needs a window and metrics",
            ));
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
        .insert(txn)
        .await?;
        self.append_event(
            txn,
            cmd,
            call,
            EventDraft::fact(
                CallEventType::CallQualityRecorded,
                data(vec![
                    ("quality_record_id", text(record.id)),
                    ("metric_count", SafeValue::from(new.metrics.len())),
                ])?,
            ),
            at.max(call.updated_at),
        )
        .await?;
        Ok(record.into())
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
            // Artifact and public-network leg facts (with their cost evidence) are
            // recorded by their own writers only.
            UsageType::LegConnectedTime
            | UsageType::VoicemailArtifact
            | UsageType::RecordingArtifact => false,
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
}

/// The cost evidence of a public-network leg (Part G §69): KNOWN (provider-reported),
/// ESTIMATED (an estimate only) or UNKNOWN (no amount — never zero).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LegCost {
    Known { minor: i64, currency: String },
    Estimated { minor: i64, currency: String },
    Unknown,
}

impl LegCost {
    /// From Telephony's usage fact: a provider-confirmed amount is KNOWN, an amount the
    /// provider did not confirm is ESTIMATED, no amount (or no currency) is UNKNOWN.
    pub fn from_evidence(minor: Option<i64>, known: bool, currency: Option<&str>) -> Self {
        let currency = currency
            .filter(|c| c.len() == 3 && c.bytes().all(|b| b.is_ascii_uppercase()))
            .map(str::to_string);
        match (minor.filter(|m| *m >= 0), currency) {
            (Some(minor), Some(currency)) if known => LegCost::Known { minor, currency },
            (Some(minor), Some(currency)) => LegCost::Estimated { minor, currency },
            _ => LegCost::Unknown,
        }
    }

    fn columns(&self) -> (&'static str, Option<i64>, Option<String>) {
        match self {
            LegCost::Known { minor, currency } => ("KNOWN", Some(*minor), Some(currency.clone())),
            LegCost::Estimated { minor, currency } => {
                ("ESTIMATED", Some(*minor), Some(currency.clone()))
            }
            LegCost::Unknown => ("UNKNOWN", None, None),
        }
    }
}

/// One public-network leg's consumption fact.
#[derive(Debug, Clone)]
pub struct LegUsageFact {
    pub leg_id: uuid::Uuid,
    pub connected_ms: i64,
    pub billable_seconds: i64,
    pub destination_country: Option<String>,
    pub cost: LegCost,
}

/// One stored voicemail message / call recording's consumption fact.
#[derive(Debug, Clone, Copy)]
pub struct ArtifactUsageFact {
    pub usage_type: UsageType,
    pub artifact_id: uuid::Uuid,
    pub duration_ms: i64,
    pub size_bytes: i64,
}

/// The leg columns of a public-network fact.
struct LegColumns<'a> {
    leg_id: uuid::Uuid,
    route_class: Option<String>,
    country: Option<String>,
    billable_seconds: i64,
    cost: &'a LegCost,
}

/// A FINAL fact as written by [`CallService::record_final_fact_in`].
struct FinalFact<'a> {
    key: String,
    usage_type: UsageType,
    quantity: i64,
    leg: Option<LegColumns<'a>>,
    size_bytes: Option<i64>,
}

impl CallService {
    /// Records a FINAL usage fact with the Call's event (inside the caller's transaction).
    /// Idempotent on its key: returns false (and changes nothing) when already recorded.
    async fn record_final_fact_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        fact: FinalFact<'_>,
    ) -> CallResult<bool> {
        use sea_orm::{ConnectionTrait, DbBackend, Statement};
        let FinalFact {
            key,
            usage_type,
            quantity,
            leg,
            size_bytes,
        } = fact;
        let at = self.now().max(call.updated_at);
        let id = CallUsageRecordId::generate().as_uuid();
        let (leg_id, route_class, country, billable, cost) = match leg {
            Some(l) => (
                Some(l.leg_id),
                l.route_class,
                l.country,
                Some(l.billable_seconds.clamp(0, i64::from(i32::MAX)) as i32),
                Some(l.cost.columns()),
            ),
            None => (None, None, None, None, None),
        };
        let inserted = txn
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO call_usage_records (id, call_id, leg_id, tenant_id, application_id, \
                   environment_id, usage_type, quantity, unit, idempotency_key, finalized_at, \
                   route_class, destination_country, billable_seconds, cost_status, cost_minor, \
                   cost_currency, size_bytes, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'MILLISECONDS', $9, $10, $11, $12, $13, \
                         $14, $15, $16, $17, $10, $10) \
                 ON CONFLICT (call_id, idempotency_key) DO NOTHING",
                vec![
                    id.into(),
                    call.id.into(),
                    leg_id.into(),
                    call.tenant_id.into(),
                    call.application_id.into(),
                    call.environment_id.into(),
                    usage_type.as_str().into(),
                    quantity.max(0).into(),
                    key.into(),
                    at.into(),
                    route_class.into(),
                    country.into(),
                    billable.into(),
                    cost.as_ref().map(|c| c.0).into(),
                    cost.as_ref().and_then(|c| c.1).into(),
                    cost.and_then(|c| c.2).into(),
                    size_bytes.into(),
                ],
            ))
            .await?
            .rows_affected();
        if inserted == 0 {
            return Ok(false);
        }
        self.append_event(
            txn,
            cmd,
            call,
            EventDraft::fact(
                CallEventType::CallUsageFinalized,
                data(vec![
                    ("usage_record_id", text(id)),
                    ("usage_type", text(usage_type)),
                    ("quantity", SafeValue::Integer(quantity.max(0))),
                    ("unit", text(UsageUnit::Milliseconds)),
                ])?,
            ),
            at,
        )
        .await?;
        crate::metrics::inc(
            "calling_usage_facts_total",
            &[("usage_type", usage_type.as_str())],
        );
        Ok(true)
    }

    /// Part G §69: a public-network leg's usage (each leg separately — a callback's two
    /// legs are two facts of ONE Call). A late or repeated provider usage event changes
    /// nothing (one fact per leg).
    pub(crate) async fn record_leg_usage_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        fact: &LegUsageFact,
    ) -> CallResult<bool> {
        use sea_orm::{ConnectionTrait, DbBackend, Statement};
        let route_class: Option<String> = txn
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT COALESCE(r.routing_class, r.route_class) FROM call_legs l \
                   JOIN call_routes r ON r.id = l.route_id \
                  WHERE l.id = $1 AND l.call_id = $2",
                vec![fact.leg_id.into(), call.id.into()],
            ))
            .await?
            .map(|r| r.try_get_by_index::<String>(0))
            .transpose()?;
        let country = fact
            .destination_country
            .as_deref()
            .filter(|c| c.len() == 2 && c.bytes().all(|b| b.is_ascii_uppercase()))
            .map(str::to_string);
        self.record_final_fact_in(
            txn,
            cmd,
            call,
            FinalFact {
                key: format!("leg-usage:{}", fact.leg_id.simple()),
                usage_type: UsageType::LegConnectedTime,
                quantity: fact.connected_ms,
                leg: Some(LegColumns {
                    leg_id: fact.leg_id,
                    route_class,
                    country,
                    billable_seconds: fact.billable_seconds,
                    cost: &fact.cost,
                }),
                size_bytes: None,
            },
        )
        .await
    }

    /// Part G §69: a voicemail message / call recording artifact (duration and stored
    /// size), once per artifact.
    pub(crate) async fn record_artifact_usage_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        fact: ArtifactUsageFact,
    ) -> CallResult<bool> {
        let prefix = match fact.usage_type {
            UsageType::VoicemailArtifact => "voicemail-artifact",
            UsageType::RecordingArtifact => "recording-artifact",
            _ => return Err(CallError::Validation("not an artifact usage type")),
        };
        self.record_final_fact_in(
            txn,
            cmd,
            call,
            FinalFact {
                key: format!("{prefix}:{}", fact.artifact_id.simple()),
                usage_type: fact.usage_type,
                quantity: fact.duration_ms,
                leg: None,
                size_bytes: Some(fact.size_bytes.max(0)),
            },
        )
        .await
    }
}

#[cfg(test)]
mod usage_fact_tests {
    use super::LegCost;

    #[test]
    fn cost_evidence_is_never_invented() {
        assert_eq!(
            LegCost::from_evidence(Some(240), true, Some("NGN")),
            LegCost::Known {
                minor: 240,
                currency: "NGN".into()
            }
        );
        assert_eq!(
            LegCost::from_evidence(Some(240), false, Some("NGN")),
            LegCost::Estimated {
                minor: 240,
                currency: "NGN".into()
            }
        );
        // No amount, no currency, a negative or malformed figure: UNKNOWN (never zero).
        for (minor, known, currency) in [
            (None, true, Some("NGN")),
            (Some(240), true, None),
            (Some(-1), true, Some("NGN")),
            (Some(240), true, Some("naira")),
            (None, false, None),
        ] {
            assert_eq!(
                LegCost::from_evidence(minor, known, currency),
                LegCost::Unknown
            );
        }
    }
}
