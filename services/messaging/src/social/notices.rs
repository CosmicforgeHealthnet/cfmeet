//! Status notices for the Notification Service: "mentioned you in a status" (to a mentioned
//! principal inside the status's audience) and "shared your status" (to the publisher).
//!
//! Written in the status / share transaction only while enabled
//! (`MESSAGING_STATUS_NOTICES=on` with `CF_NOTIFICATIONS_BASE_URL`; [`AppState::status_notices`])
//! and relayed like the member notices ([`crate::member_notifications`]): ids only (the
//! status), addressed to the recipient's CF Meet account; a recipient no longer able to see
//! the status, or a status that ended, is `SKIPPED`. The catalogue types
//! `messaging.status.mentioned` / `messaging.status.shared` must exist in the Notification
//! Service catalogue (and `packages/notification-client`) before the switch is turned on;
//! until then nothing is written.

use std::{collections::HashMap, sync::Arc, time::Duration as StdDuration};

use cf_notification_client::{envelope::Scope, EnvelopeBuilder, Producer, RouteRef};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, FromQueryResult, TransactionTrait};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    db,
    error::Result,
    identity::TrustedContext,
    notification_fence::ROUTING_PROTOCOL,
    notification_publications::{IngestResult, NotificationEventSink, PublishFailure},
    realtime::push::{EntitledRecipient, WakeupAuthorization},
    state::AppState,
};

pub const MENTION_EVENT_TYPE: &str = "messaging.status.mentioned";
pub const SHARE_EVENT_TYPE: &str = "messaging.status.shared";
pub const LIFETIME: Duration = Duration::hours(24);
pub const RETENTION: Duration = Duration::days(7);
pub const MAX_BATCH: usize = 100;
pub const METRIC: &str = "messaging_status_notices_total";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Mention,
    Share,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Mention => "MENTION",
            Kind::Share => "SHARE",
        }
    }
}

/// Writes one notice (idempotent per kind, subject and recipient). Nothing while disabled.
#[allow(clippy::too_many_arguments)]
pub async fn enqueue<C: ConnectionTrait>(
    state: &AppState,
    txn: &C,
    ctx: &TrustedContext,
    kind: Kind,
    subject_id: Uuid,
    status_id: Uuid,
    principal: Uuid,
    now: DateTime<Utc>,
) -> std::result::Result<bool, DbErr> {
    if !state.status_notices {
        return Ok(false);
    }
    let written = db::exec(
        txn,
        "INSERT INTO messaging_status_notices (id, event_id, kind, subject_id, tenant_id, application_id, environment_id, \
           status_id, principal_id, service_account_id, environment_kind, first_party, next_attempt_at, occurred_at, \
           created_at, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $13, $13, $14) ON CONFLICT DO NOTHING",
        vec![
            Uuid::new_v4().into(),
            format!("evt_{}", Uuid::new_v4().simple()).into(),
            kind.as_str().into(),
            subject_id.into(),
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            ctx.scope.environment_id.into(),
            status_id.into(),
            principal.into(),
            ctx.scope.service_account_id.into(),
            ctx.scope.environment_kind.as_str().into(),
            ctx.first_party.into(),
            now.into(),
            (now + LIFETIME).into(),
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
    subject_id: Uuid,
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    status_id: Uuid,
    principal_id: Uuid,
    service_account_id: Uuid,
    environment_kind: String,
    first_party: bool,
    envelope: Option<Value>,
    attempts: i32,
    occurred_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    /// The status is still live and (for a mention) still lists the recipient.
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
        return Err(Unbuilt::Skip("STATUS_NOT_VISIBLE"));
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
            mentions_recipient: row.kind == "MENTION",
        }],
    };
    let accounts = crate::notifications::account_refs(state, &decision)
        .await
        .map_err(|_| Unbuilt::Unavailable)?;
    let recipient = accounts
        .get(&row.principal_id)
        .cloned()
        .ok_or(Unbuilt::Skip("NO_ACCOUNT_RECIPIENT"))?;
    let event_type = if row.kind == "MENTION" {
        MENTION_EVENT_TYPE
    } else {
        SHARE_EVENT_TYPE
    };
    let built = EnvelopeBuilder::new(
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
    .subject(row.status_id, None)
    .recipient(recipient)
    .dedupe_key(format!(
        "{event_type}:{}:{}",
        row.subject_id, row.principal_id
    ))
    .idempotency_key(row.event_id.clone())
    .expires_at(rfc3339(row.expires_at))
    .data("statusId", row.status_id.to_string())
    .build()
    .map_err(|_| Unbuilt::Invalid)?;
    serde_json::to_value(built).map_err(|_| Unbuilt::Invalid)
}

const DUE: &str = "SELECT n.id, n.event_id, n.kind, n.subject_id, n.tenant_id, n.application_id, n.environment_id, \
        n.status_id, n.principal_id, n.service_account_id, n.environment_kind, n.first_party, n.envelope, n.attempts, \
        n.occurred_at, n.expires_at, \
        (s.expires_at > $1 AND (n.kind <> 'MENTION' OR EXISTS (SELECT 1 FROM messaging_status_mentions m \
           WHERE m.status_id = n.status_id AND m.principal_id = n.principal_id))) IS TRUE AS still_relevant \
   FROM messaging_status_notices n LEFT JOIN messaging_statuses s ON s.id = n.status_id \
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
        "UPDATE messaging_status_notices SET state = $2, outcome = $3, last_error = $4, attempts = attempts + 1, \
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
        "UPDATE messaging_status_notices SET attempts = $2, last_error = $3, next_attempt_at = $4 WHERE id = $1",
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
                    "UPDATE messaging_status_notices SET envelope = $2 WHERE id = $1",
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

/// Expires overdue notices and deletes settled ones past retention.
pub async fn maintain(state: &AppState, batch: i64) -> Result<()> {
    let now = state.now();
    db::exec(
        &state.db,
        "UPDATE messaging_status_notices SET state = 'EXPIRED', last_error = 'EXPIRED', completed_at = $1 \
          WHERE state = 'PENDING' AND expires_at <= $1",
        vec![now.into()],
    )
    .await?;
    db::exec(
        &state.db,
        "DELETE FROM messaging_status_notices WHERE id IN (SELECT id FROM messaging_status_notices \
          WHERE state <> 'PENDING' AND completed_at < $1 ORDER BY completed_at LIMIT $2)",
        vec![(now - RETENTION).into(), batch.into()],
    )
    .await?;
    Ok(())
}

/// The relay loop (spawned only when status notices are enabled).
pub async fn run(state: AppState, sink: Arc<dyn NotificationEventSink>, interval: StdDuration) {
    loop {
        if let Err(e) = relay_due(&state, sink.as_ref(), MAX_BATCH).await {
            tracing::warn!(error = %e, "status notice relay pass failed");
        }
        tokio::time::sleep(interval).await;
    }
}
