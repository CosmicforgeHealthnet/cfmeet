//! Event notices for the Notification Service (ADR 0071 decision 8): "invited you"
//! (`messaging.event.invited`), "cancelled" (`messaging.event.cancelled`) and reminders
//! (`messaging.event.reminder`), plus the reminder worker.
//!
//! Written only while enabled (`MESSAGING_EVENT_NOTICES=on` with `CF_NOTIFICATIONS_BASE_URL`;
//! [`AppState::event_notices`]) and relayed like the status notices
//! ([`crate::social::notices`]): ids only (the event, and an occurrence start for a
//! reminder), addressed to the recipient's CF Meet account. A recipient who can no longer
//! see the event (or answered NOT_GOING, for a reminder), or an event that is gone, is
//! `SKIPPED`. The catalogue types are registered in the Notification Service with their
//! routing rows seeded OFF; see the ADR's activation steps.

use std::{collections::HashMap, sync::Arc, time::Duration as StdDuration};

use cf_notification_client::{envelope::Scope, EnvelopeBuilder, Producer, RouteRef};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, FromQueryResult, TransactionTrait};
use serde_json::Value;
use uuid::Uuid;

use super::{EventRow, COLUMNS};
use crate::{
    db,
    error::Result,
    identity::TrustedContext,
    notification_fence::ROUTING_PROTOCOL,
    notification_publications::{IngestResult, NotificationEventSink, PublishFailure},
    realtime::push::{EntitledRecipient, WakeupAuthorization},
    state::AppState,
};

pub const INVITED_EVENT_TYPE: &str = "messaging.event.invited";
pub const CANCELLED_EVENT_TYPE: &str = "messaging.event.cancelled";
pub const REMINDER_EVENT_TYPE: &str = "messaging.event.reminder";
pub const RETENTION: Duration = Duration::days(7);
pub const MAX_BATCH: usize = 100;
/// A reminder fires at most this late (else that occurrence's reminder is skipped).
pub const REMINDER_TOLERANCE: Duration = Duration::minutes(15);
pub const METRIC: &str = "messaging_event_notices_total";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Invited,
    Cancelled,
    Reminder,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Invited => "INVITED",
            Kind::Cancelled => "CANCELLED",
            Kind::Reminder => "REMINDER",
        }
    }
}

/// The scope a notice is written in (the request's, or the reminder's stored one).
#[derive(Debug, Clone)]
pub struct NoticeScope {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub service_account_id: Uuid,
    pub environment_kind: String,
    pub first_party: bool,
}

impl NoticeScope {
    pub fn of(ctx: &TrustedContext) -> Self {
        Self {
            tenant_id: ctx.scope.tenant_id,
            application_id: ctx.scope.application_id,
            environment_id: ctx.scope.environment_id,
            service_account_id: ctx.scope.service_account_id,
            environment_kind: ctx.scope.environment_kind.as_str().to_string(),
            first_party: ctx.first_party,
        }
    }
}

/// An invitation notice lives a week, never past the series' end.
pub fn invite_expiry(now: DateTime<Utc>, series_end: Option<DateTime<Utc>>) -> DateTime<Utc> {
    let week = now + Duration::days(7);
    series_end
        .filter(|e| *e > now)
        .map_or(week, |e| e.min(week))
}

/// Writes one notice (idempotent per kind, event, recipient and, for a reminder,
/// occurrence and offset). Nothing while disabled.
#[allow(clippy::too_many_arguments)]
pub async fn enqueue<C: ConnectionTrait>(
    state: &AppState,
    txn: &C,
    scope: &NoticeScope,
    kind: Kind,
    event: Uuid,
    principal: Uuid,
    reminder: Option<(DateTime<Utc>, i32)>,
    now: DateTime<Utc>,
    expires_at: DateTime<Utc>,
) -> std::result::Result<bool, DbErr> {
    if !state.event_notices || expires_at <= now {
        return Ok(false);
    }
    let dedupe = match reminder {
        Some((occurrence, minutes)) => format!(
            "{}:{}:{}:{}:{}",
            kind.as_str(),
            event.simple(),
            principal.simple(),
            occurrence.timestamp(),
            minutes
        ),
        None => format!(
            "{}:{}:{}",
            kind.as_str(),
            event.simple(),
            principal.simple()
        ),
    };
    let written = db::exec(
        txn,
        "INSERT INTO messaging_event_notices (id, event_id, kind, dedupe_key, tenant_id, application_id, environment_id, \
           subject_event_id, principal_id, occurrence_at, service_account_id, environment_kind, first_party, \
           next_attempt_at, occurred_at, created_at, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $14, $14, $15) ON CONFLICT DO NOTHING",
        vec![
            Uuid::new_v4().into(),
            format!("evt_{}", Uuid::new_v4().simple()).into(),
            kind.as_str().into(),
            dedupe.into(),
            scope.tenant_id.into(),
            scope.application_id.into(),
            scope.environment_id.into(),
            event.into(),
            principal.into(),
            reminder.map(|r| r.0).into(),
            scope.service_account_id.into(),
            scope.environment_kind.clone().into(),
            scope.first_party.into(),
            now.into(),
            expires_at.into(),
        ],
    )
    .await?;
    Ok(written > 0)
}

#[derive(Debug, Clone, FromQueryResult)]
struct Row {
    id: Uuid,
    event_id: String,
    kind: String,
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    subject_event_id: Uuid,
    principal_id: Uuid,
    occurrence_at: Option<DateTime<Utc>>,
    service_account_id: Uuid,
    environment_kind: String,
    first_party: bool,
    envelope: Option<Value>,
    attempts: i32,
    occurred_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    dedupe_key: String,
    /// The event still exists, is active (but for a cancellation) and the recipient may
    /// still see it (and, for a reminder, did not answer NOT_GOING).
    still_relevant: bool,
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Micros, true)
}

enum Unbuilt {
    Skip(&'static str),
    Unavailable,
    Invalid,
}

async fn build(state: &AppState, row: &Row) -> std::result::Result<Value, Unbuilt> {
    if !row.still_relevant {
        return Err(Unbuilt::Skip("EVENT_NOT_VISIBLE"));
    }
    let decision = WakeupAuthorization {
        tenant_id: row.tenant_id,
        application_id: row.application_id,
        environment_id: row.environment_id,
        conversation_active: true,
        security_profile: "MANAGED".into(),
        context_class: "DIRECT".into(),
        sender: None,
        initiating_service_account_id: Some(row.service_account_id),
        environment_kind: Some(row.environment_kind.clone()),
        first_party: row.first_party,
        candidates: vec![row.principal_id],
        entitled: vec![EntitledRecipient {
            principal_id: row.principal_id,
            mentions_recipient: false,
        }],
    };
    let accounts = crate::notifications::account_refs(state, &decision)
        .await
        .map_err(|_| Unbuilt::Unavailable)?;
    let recipient = accounts
        .get(&row.principal_id)
        .cloned()
        .ok_or(Unbuilt::Skip("NO_ACCOUNT_RECIPIENT"))?;
    let event_type = match row.kind.as_str() {
        "INVITED" => INVITED_EVENT_TYPE,
        "CANCELLED" => CANCELLED_EVENT_TYPE,
        _ => REMINDER_EVENT_TYPE,
    };
    let mut builder = EnvelopeBuilder::new(
        Producer::Messaging,
        event_type,
        &rfc3339(row.occurred_at),
        RouteRef {
            decision_id: row.id,
            routing_version: 1,
            protocol: ROUTING_PROTOCOL,
        },
    )
    .map_err(|_| Unbuilt::Invalid)?
    .event_id(row.event_id.clone())
    .correlation_id(format!("corr_{}", row.id.simple()))
    .scope(Scope {
        tenant_id: row.tenant_id,
        application_id: row.application_id,
        environment_id: row.environment_id,
    })
    .subject(row.subject_event_id, None)
    .recipient(recipient)
    .dedupe_key(format!("{event_type}:{}", row.dedupe_key))
    .idempotency_key(row.event_id.clone())
    .expires_at(rfc3339(row.expires_at))
    .data("eventId", row.subject_event_id.to_string());
    if row.kind == "REMINDER" {
        let at = row.occurrence_at.ok_or(Unbuilt::Invalid)?;
        builder = builder.data("startsAt", rfc3339(at));
    }
    let built = builder.build().map_err(|_| Unbuilt::Invalid)?;
    serde_json::to_value(built).map_err(|_| Unbuilt::Invalid)
}

const DUE: &str = "SELECT n.id, n.event_id, n.kind, n.tenant_id, n.application_id, n.environment_id, \
        n.subject_event_id, n.principal_id, n.occurrence_at, n.service_account_id, n.environment_kind, n.first_party, \
        n.envelope, n.attempts, n.occurred_at, n.expires_at, n.dedupe_key, \
        (e.id IS NOT NULL AND (n.kind = 'CANCELLED' OR e.state = 'ACTIVE') \
          AND (e.organizer_principal_id = n.principal_id \
            OR (e.scope = 'PERSONAL' AND EXISTS (SELECT 1 FROM messaging_event_people p WHERE p.event_id = e.id \
                  AND p.principal_id = n.principal_id) \
                AND NOT EXISTS (SELECT 1 FROM messaging_blocks b WHERE b.environment_id = e.environment_id AND \
                  ((b.blocker_principal_id = n.principal_id AND b.blocked_principal_id = e.organizer_principal_id) \
                OR (b.blocked_principal_id = n.principal_id AND b.blocker_principal_id = e.organizer_principal_id)))) \
            OR (e.scope = 'CONVERSATION' AND EXISTS (SELECT 1 FROM messaging_conversation_members m \
                  WHERE m.conversation_id = e.conversation_id AND m.principal_id = n.principal_id AND m.state = 'ACTIVE')) \
            OR (e.scope = 'COMMUNITY' AND EXISTS (SELECT 1 FROM messaging_community_members cm \
                  JOIN messaging_communities cc ON cc.id = cm.community_id WHERE cm.community_id = e.community_id \
                  AND cm.principal_id = n.principal_id AND cm.state = 'ACTIVE' AND cc.state = 'ACTIVE'))) \
          AND (n.kind <> 'REMINDER' OR NOT EXISTS (SELECT 1 FROM messaging_event_people r WHERE r.event_id = e.id \
                AND r.principal_id = n.principal_id AND r.response = 'NOT_GOING'))) IS TRUE AS still_relevant \
   FROM messaging_event_notices n LEFT JOIN messaging_events e ON e.id = n.subject_event_id \
  WHERE n.state = 'PENDING' AND n.next_attempt_at <= $1 AND n.expires_at > $1 \
  ORDER BY n.next_attempt_at LIMIT $2 FOR UPDATE OF n SKIP LOCKED";

async fn settle(
    txn: &DatabaseTransaction,
    id: Uuid,
    state_: &str,
    outcome: Option<&str>,
    error: Option<&str>,
    now: DateTime<Utc>,
) -> std::result::Result<(), DbErr> {
    db::exec(
        txn,
        "UPDATE messaging_event_notices SET state = $2, outcome = $3, last_error = $4, attempts = attempts + 1, \
           completed_at = $5 WHERE id = $1",
        vec![
            id.into(),
            state_.into(),
            outcome.map(str::to_owned).into(),
            error.map(str::to_owned).into(),
            now.into(),
        ],
    )
    .await?;
    Ok(())
}

async fn retry(
    txn: &DatabaseTransaction,
    row: &Row,
    error: &str,
    now: DateTime<Utc>,
) -> std::result::Result<(), DbErr> {
    let attempts = row.attempts + 1;
    let next = now + Duration::seconds((1i64 << attempts.clamp(0, 9)).min(300));
    db::exec(
        txn,
        "UPDATE messaging_event_notices SET attempts = $2, last_error = $3, next_attempt_at = $4 WHERE id = $1",
        vec![row.id.into(), attempts.into(), error.into(), next.into()],
    )
    .await?;
    Ok(())
}

/// One relay pass over at most `limit` due notices.
pub async fn relay_due(
    state: &AppState,
    sink: &dyn NotificationEventSink,
    limit: usize,
) -> Result<usize> {
    let txn = state.db.begin().await?;
    let now = state.now();
    let rows: Vec<Row> = db::all(
        &txn,
        DUE,
        vec![now.into(), (limit.min(MAX_BATCH) as i64).into()],
    )
    .await?;
    let mut ready: Vec<(Row, Value)> = Vec::new();
    for mut row in rows {
        let built = match row.envelope.take() {
            Some(e) => Ok(e),
            None => build(state, &row).await,
        };
        match built {
            Ok(e) => {
                db::exec(
                    &txn,
                    "UPDATE messaging_event_notices SET envelope = $2 WHERE id = $1",
                    vec![row.id.into(), e.clone().into()],
                )
                .await?;
                ready.push((row, e));
            }
            Err(Unbuilt::Skip(reason)) => {
                settle(&txn, row.id, "SKIPPED", None, Some(reason), now).await?;
                crate::metrics::inc(METRIC, &[("outcome", "skipped")]);
            }
            Err(Unbuilt::Unavailable) => retry(&txn, &row, "PRINCIPALS_UNAVAILABLE", now).await?,
            Err(Unbuilt::Invalid) => {
                settle(&txn, row.id, "DEAD", None, Some("ENVELOPE_INVALID"), now).await?;
                crate::metrics::inc(METRIC, &[("outcome", "dead")]);
            }
        }
    }
    let sent = ready.len();
    if !ready.is_empty() {
        let events: Vec<Value> = ready.iter().map(|(_, e)| e.clone()).collect();
        match sink.ingest(&events, false).await {
            Ok(results) => {
                let by_id: HashMap<&str, &IngestResult> =
                    results.iter().map(|r| (r.event_id.as_str(), r)).collect();
                for (row, _) in &ready {
                    match by_id.get(row.event_id.as_str()) {
                        Some(r)
                            if matches!(r.outcome.as_str(), "ACCEPTED" | "DUPLICATE" | "HELD") =>
                        {
                            settle(&txn, row.id, "ACKED", Some(&r.outcome), None, now).await?;
                            crate::metrics::inc(METRIC, &[("outcome", "acked")]);
                        }
                        Some(r) if r.outcome == "REJECTED" => {
                            settle(
                                &txn,
                                row.id,
                                "DEAD",
                                Some("REJECTED"),
                                Some("REJECTED"),
                                now,
                            )
                            .await?;
                            crate::metrics::inc(METRIC, &[("outcome", "dead")]);
                        }
                        _ => retry(&txn, row, "NO_RESULT", now).await?,
                    }
                }
            }
            Err(PublishFailure::Rejected) => {
                for (row, _) in &ready {
                    settle(&txn, row.id, "DEAD", None, Some("CONTRACT_REJECTED"), now).await?;
                }
            }
            Err(PublishFailure::Unavailable) => {
                for (row, _) in &ready {
                    retry(&txn, row, "UNAVAILABLE", now).await?;
                }
            }
            Err(PublishFailure::NotConfigured) => {
                for (row, _) in &ready {
                    retry(&txn, row, "NOT_CONFIGURED", now).await?;
                }
            }
        }
    }
    txn.commit().await?;
    Ok(sent)
}

#[derive(FromQueryResult)]
struct DueReminder {
    principal_id: Uuid,
    minutes_before: i32,
    next_fire_at: DateTime<Utc>,
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    service_account_id: Uuid,
    environment_kind: String,
    first_party: bool,
    event_id: Uuid,
}

/// One reminder pass: every reminder due at `now` writes its notice (at most
/// [`REMINDER_TOLERANCE`] late) and moves on to the series' next occurrence.
/// Returns the reminders handled.
pub async fn fire_due(state: &AppState, limit: i64) -> Result<usize> {
    let now = state.now();
    let txn = state.db.begin().await?;
    let due: Vec<DueReminder> = db::all(
        &txn,
        "SELECT principal_id, minutes_before, next_fire_at, tenant_id, application_id, environment_id, \
                service_account_id, environment_kind, first_party, event_id \
           FROM messaging_event_reminders WHERE next_fire_at <= $1 \
          ORDER BY next_fire_at LIMIT $2 FOR UPDATE SKIP LOCKED",
        vec![now.into(), limit.into()],
    )
    .await?;
    let mut events: HashMap<Uuid, Option<EventRow>> = HashMap::new();
    for r in &due {
        if let std::collections::hash_map::Entry::Vacant(slot) = events.entry(r.event_id) {
            let row: Option<EventRow> = db::one(
                &txn,
                &format!("SELECT {COLUMNS} FROM messaging_events e WHERE e.id = $1"),
                vec![r.event_id.into()],
            )
            .await?;
            slot.insert(row);
        }
        let Some(Some(event)) = events.get(&r.event_id) else {
            continue;
        };
        let minutes = Duration::minutes(i64::from(r.minutes_before));
        let occurrence = r.next_fire_at + minutes;
        if event.active() && now - r.next_fire_at <= REMINDER_TOLERANCE {
            let scope = NoticeScope {
                tenant_id: r.tenant_id,
                application_id: r.application_id,
                environment_id: r.environment_id,
                service_account_id: r.service_account_id,
                environment_kind: r.environment_kind.clone(),
                first_party: r.first_party,
            };
            enqueue(
                state,
                &txn,
                &scope,
                Kind::Reminder,
                event.id,
                r.principal_id,
                Some((occurrence, r.minutes_before)),
                now,
                occurrence.max(now + Duration::minutes(1)),
            )
            .await?;
        }
        let next = if event.active() {
            event
                .series()
                .first_start_after(occurrence.max(now - minutes))
                .map(|(s, _)| s - minutes)
        } else {
            None
        };
        db::exec(
            &txn,
            "UPDATE messaging_event_reminders SET next_fire_at = $4 \
              WHERE event_id = $1 AND principal_id = $2 AND minutes_before = $3",
            vec![
                event.id.into(),
                r.principal_id.into(),
                r.minutes_before.into(),
                next.into(),
            ],
        )
        .await?;
    }
    txn.commit().await?;
    Ok(due.len())
}

/// Expires overdue notices and deletes settled ones past retention.
pub async fn maintain(state: &AppState, batch: i64) -> Result<()> {
    let now = state.now();
    db::exec(
        &state.db,
        "UPDATE messaging_event_notices SET state = 'EXPIRED', last_error = 'EXPIRED', completed_at = $1 \
          WHERE state = 'PENDING' AND expires_at <= $1",
        vec![now.into()],
    )
    .await?;
    db::exec(
        &state.db,
        "DELETE FROM messaging_event_notices WHERE id IN (SELECT id FROM messaging_event_notices \
          WHERE state <> 'PENDING' AND completed_at < $1 ORDER BY completed_at LIMIT $2)",
        vec![(now - RETENTION).into(), batch.into()],
    )
    .await?;
    Ok(())
}

/// The reminder and relay loop (spawned only when event notices are enabled).
pub async fn run(state: AppState, sink: Arc<dyn NotificationEventSink>, interval: StdDuration) {
    loop {
        if let Err(e) = fire_due(&state, 500).await {
            tracing::warn!(error = %e, "event reminder pass failed");
        }
        if let Err(e) = relay_due(&state, sink.as_ref(), MAX_BATCH).await {
            tracing::warn!(error = %e, "event notice relay pass failed");
        }
        if let Err(e) = maintain(&state, 500).await {
            tracing::warn!(error = %e, "event notice maintenance failed");
        }
        tokio::time::sleep(interval).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invitations_expire_within_a_week_and_never_after_the_series() {
        let now = DateTime::<Utc>::from_timestamp(1_800_000_000, 0).unwrap();
        assert_eq!(invite_expiry(now, None), now + Duration::days(7));
        assert_eq!(
            invite_expiry(now, Some(now + Duration::days(2))),
            now + Duration::days(2)
        );
        assert_eq!(
            invite_expiry(now, Some(now - Duration::days(1))),
            now + Duration::days(7)
        );
    }
}
