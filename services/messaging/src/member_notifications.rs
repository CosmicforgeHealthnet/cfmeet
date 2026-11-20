//! "Added to a group" notices for the Notification Service (owner request 2026-11-19: the
//! bell must list chat invitations). Catalogue type #14 `messaging.member.invited`
//! (category `GROUP_INVITATIONS`, in-app "Added to a group" + push), addressed to the CF
//! Meet account of a person SOMEONE ELSE added to, or invited into, a multi-party
//! conversation. Self-joins, accepted invitations, organization team snapshots and DIRECT
//! conversations are never announced here.
//!
//! * **Enqueue** ([`enqueue`]) writes one `messaging_member_notifications` row (its own
//!   migration history) in the membership transaction, only while the Notification Service
//!   is configured (`CF_NOTIFICATIONS_BASE_URL`, [`AppState::member_notices`]); otherwise
//!   nothing is written and Messaging behaves exactly as before.
//! * **Relay** ([`relay_due`], [`run`]): there is no legacy notice for this fact, so there
//!   is no routing fence: each row carries its own route decision (`routingVersion` 1) and
//!   delivery is governed only by the service's `notif_routing` rows. The envelope is ids
//!   only (the conversation and its event cursor) and is built once, at the first attempt,
//!   with the member's CF Meet account reference (the same Platform Access mapping as the
//!   wake-up publications, [`crate::notifications::account_refs`]); a member who is no
//!   longer ACTIVE / INVITED, a conversation no longer ACTIVE, or a principal without a CF
//!   Meet account is `SKIPPED`. Posting, acknowledgement and dead-lettering follow the
//!   wake-up relay ([`crate::notification_publications`]): `ACCEPTED`, `DUPLICATE` and
//!   `HELD` acknowledge, a refusal dead-letters, 5xx / 401 / 403 / 408 / 429 and transport
//!   failures retry with backoff until the row expires.

use std::{collections::HashMap, sync::Arc, time::Duration as StdDuration};

use cf_notification_client::{envelope::Scope, EnvelopeBuilder, Producer, RouteRef};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, FromQueryResult, TransactionTrait};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    db,
    error::Result,
    identity::OwnerScope,
    notification_fence::{push_key, ROUTING_PROTOCOL},
    notification_publications::{IngestResult, NotificationEventSink, PublishFailure},
    realtime::push::{EntitledRecipient, WakeupAuthorization},
    state::AppState,
};

/// Catalogue type #14.
pub const EVENT_TYPE: &str = "messaging.member.invited";
/// Every row carries its own decision: there is no legacy path to fence.
pub const ROUTING_VERSION: i32 = 1;
/// How long a notice may wait for the relay.
pub const LIFETIME: Duration = Duration::hours(24);
/// How long a settled row is kept after `completed_at`.
pub const RETENTION: Duration = Duration::days(30);
/// Most events in one I1 batch (contract `maxItems`).
pub const MAX_BATCH: usize = 100;
/// Counter of relay outcomes (`outcome`: acked / retried / dead / skipped / expired).
pub const METRIC: &str = "messaging_member_notifications_total";

/// The membership change to announce.
pub struct MemberNotice<'a> {
    /// The member's `messaging.member.added` / `messaging.member.invited` outbox event.
    pub outbox_event_id: Uuid,
    pub event_sequence: i64,
    pub conversation_id: Uuid,
    pub context_class: &'a str,
    pub principal_id: Uuid,
    /// The person who added or invited them (`None`: the application backend).
    pub actor_principal_id: Option<Uuid>,
    pub scope: &'a OwnerScope,
    pub first_party: bool,
    pub occurred_at: DateTime<Utc>,
}

/// Whether `n` is announced: a person added someone else to a multi-party conversation.
pub fn announced(n: &MemberNotice<'_>) -> bool {
    n.context_class != "DIRECT" && n.actor_principal_id.is_some_and(|a| a != n.principal_id)
}

/// Writes the notice of `n` in the caller's membership transaction (idempotent per outbox
/// event). Nothing while the Notification Service is not configured, and nothing for a
/// change [`announced`] refuses. Returns whether a row was written.
pub async fn enqueue<C: ConnectionTrait>(
    state: &AppState,
    txn: &C,
    n: MemberNotice<'_>,
) -> std::result::Result<bool, DbErr> {
    if !state.member_notices || !announced(&n) {
        return Ok(false);
    }
    // Only where a principal can be a CF Meet account (the relay's mapping rule): CF Meet's
    // governed personal scope or a first-party (organization) scope. A customer's
    // conversations never write a row.
    let governed = state.communications.as_ref().is_some_and(|p| {
        p.governs(cf_communications_policy::Scope {
            tenant_id: n.scope.tenant_id,
            application_id: n.scope.application_id,
            environment_id: n.scope.environment_id,
        })
    });
    if !governed && !n.first_party {
        return Ok(false);
    }
    let now = state.now();
    let written = db::exec(
        txn,
        "INSERT INTO messaging_member_notifications (id, event_id, outbox_event_id, decision_id, tenant_id, \
           application_id, environment_id, conversation_id, event_sequence, principal_id, actor_principal_id, \
           service_account_id, environment_kind, first_party, correlation_id, next_attempt_at, occurred_at, created_at, \
           expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $16, $18) \
         ON CONFLICT DO NOTHING",
        vec![
            Uuid::new_v4().into(),
            push_key(n.outbox_event_id).into(),
            n.outbox_event_id.into(),
            Uuid::new_v4().into(),
            n.scope.tenant_id.into(),
            n.scope.application_id.into(),
            n.scope.environment_id.into(),
            n.conversation_id.into(),
            n.event_sequence.into(),
            n.principal_id.into(),
            n.actor_principal_id.into(),
            n.scope.service_account_id.into(),
            n.scope.environment_kind.as_str().into(),
            n.first_party.into(),
            format!("corr_{}", n.outbox_event_id.simple()).into(),
            now.into(),
            n.occurred_at.into(),
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
    outbox_event_id: Uuid,
    decision_id: Uuid,
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    conversation_id: Uuid,
    event_sequence: i64,
    principal_id: Uuid,
    service_account_id: Uuid,
    environment_kind: String,
    first_party: bool,
    correlation_id: String,
    envelope: Option<Value>,
    attempts: i32,
    occurred_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    /// The membership and conversation still hold (read with the row).
    still_member: bool,
    security_profile: Option<String>,
    context_class: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unbuilt {
    Skip(&'static str),
    Unavailable,
    Invalid,
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Micros, true)
}

/// The member's CF Meet account (the wake-up publications' mapping and governance rules).
async fn account_ref(state: &AppState, row: &Row) -> std::result::Result<String, Unbuilt> {
    let decision = WakeupAuthorization {
        tenant_id: row.tenant_id,
        application_id: row.application_id,
        environment_id: row.environment_id,
        conversation_active: true,
        security_profile: row.security_profile.clone().unwrap_or_default(),
        context_class: row.context_class.clone().unwrap_or_default(),
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
    accounts
        .get(&row.principal_id)
        .cloned()
        .ok_or(Unbuilt::Skip("NO_ACCOUNT_RECIPIENT"))
}

/// The ids-only envelope of a row for its member's account `recipient`.
fn envelope(row: &Row, recipient: String) -> std::result::Result<Value, Unbuilt> {
    let route = RouteRef {
        decision_id: row.decision_id,
        routing_version: ROUTING_VERSION,
        protocol: ROUTING_PROTOCOL,
    };
    let built = EnvelopeBuilder::new(
        Producer::Messaging,
        EVENT_TYPE,
        &rfc3339(row.occurred_at),
        route,
    )
    .map_err(|_| Unbuilt::Invalid)?
    .event_id(row.event_id.clone())
    .correlation_id(row.correlation_id.clone())
    .scope(Scope {
        tenant_id: row.tenant_id,
        application_id: row.application_id,
        environment_id: row.environment_id,
    })
    .subject(row.conversation_id, None)
    .recipient(recipient)
    .dedupe_key(format!("{EVENT_TYPE}:{}", row.outbox_event_id))
    .idempotency_key(row.event_id.clone())
    .expires_at(rfc3339(row.expires_at))
    .data("conversationId", row.conversation_id.to_string())
    .data("eventSequence", row.event_sequence)
    .build()
    .map_err(|_| Unbuilt::Invalid)?;
    serde_json::to_value(built).map_err(|_| Unbuilt::Invalid)
}

async fn build(state: &AppState, row: &Row) -> std::result::Result<Value, Unbuilt> {
    if !row.still_member {
        return Err(Unbuilt::Skip("NOT_A_MEMBER"));
    }
    let recipient = account_ref(state, row).await?;
    envelope(row, recipient)
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NoticeReport {
    pub acked: usize,
    pub retried: usize,
    pub dead: usize,
    pub skipped: usize,
    pub expired: u64,
    pub not_configured: usize,
}

fn backoff(attempts: i32) -> Duration {
    Duration::seconds((1i64 << attempts.clamp(0, 9)).min(300))
}

fn reason_code(reason: Option<&str>) -> String {
    reason
        .filter(|r| {
            (2..=64).contains(&r.len())
                && r.as_bytes()[0].is_ascii_uppercase()
                && r.bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        })
        .unwrap_or("REJECTED")
        .to_owned()
}

async fn settle(
    txn: &DatabaseTransaction,
    id: Uuid,
    state_: &str,
    outcome: Option<&str>,
    error: Option<String>,
    now: DateTime<Utc>,
) -> std::result::Result<(), DbErr> {
    db::exec(
        txn,
        "UPDATE messaging_member_notifications SET state = $2, outcome = $3, last_error = $4, \
           attempts = attempts + CASE WHEN $2 IN ('ACKED', 'DEAD') THEN 1 ELSE 0 END, completed_at = $5 \
         WHERE id = $1",
        vec![
            id.into(),
            state_.into(),
            outcome.map(str::to_owned).into(),
            error.into(),
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
    counted: bool,
    now: DateTime<Utc>,
) -> std::result::Result<(), DbErr> {
    let attempts = row.attempts + i32::from(counted);
    let next = if counted {
        now + backoff(attempts)
    } else {
        now + Duration::seconds(60)
    };
    db::exec(
        txn,
        "UPDATE messaging_member_notifications SET attempts = $2, last_error = $3, next_attempt_at = $4 WHERE id = $1",
        vec![row.id.into(), attempts.into(), error.into(), next.into()],
    )
    .await?;
    Ok(())
}

const DUE: &str = "SELECT n.id, n.event_id, n.outbox_event_id, n.decision_id, n.tenant_id, n.application_id, \
        n.environment_id, n.conversation_id, n.event_sequence, n.principal_id, n.service_account_id, \
        n.environment_kind, n.first_party, n.correlation_id, n.envelope, n.attempts, n.occurred_at, n.expires_at, \
        c.security_profile, c.context_class, \
        (c.lifecycle_state = 'ACTIVE' AND EXISTS (SELECT 1 FROM messaging_conversation_members m \
          WHERE m.conversation_id = n.conversation_id AND m.principal_id = n.principal_id \
            AND m.state IN ('ACTIVE', 'INVITED'))) IS TRUE AS still_member \
   FROM messaging_member_notifications n \
   LEFT JOIN messaging_conversations c ON c.id = n.conversation_id \
  WHERE n.state = 'PENDING' AND n.next_attempt_at <= $1 AND n.expires_at > $1 \
  ORDER BY n.next_attempt_at, n.created_at LIMIT $2 FOR UPDATE OF n SKIP LOCKED";

/// One relay pass over at most `limit` due rows, in batches of ≤ [`MAX_BATCH`]
/// (multi-instance safe: rows are locked for their attempt and settled with it).
pub async fn relay_due(
    state: &AppState,
    sink: &dyn NotificationEventSink,
    limit: usize,
) -> Result<NoticeReport> {
    let mut report = NoticeReport {
        expired: expire_overdue(state).await?,
        ..Default::default()
    };
    let mut remaining = limit;
    while remaining > 0 {
        let batch = remaining.min(MAX_BATCH);
        let txn = state.db.begin().await?;
        let now = state.now();
        let rows: Vec<Row> = db::all(&txn, DUE, vec![now.into(), (batch as i64).into()]).await?;
        if rows.is_empty() {
            txn.rollback().await?;
            break;
        }
        remaining = remaining.saturating_sub(rows.len());
        let full = rows.len() == batch;
        let mut ready: Vec<(Row, Value)> = Vec::with_capacity(rows.len());
        let mut stop = false;
        for mut row in rows {
            let built = match row.envelope.take() {
                Some(e) => Ok(e),
                None if stop => Err(Unbuilt::Unavailable),
                None => match build(state, &row).await {
                    Ok(e) => {
                        db::exec(
                            &txn,
                            "UPDATE messaging_member_notifications SET envelope = $2 WHERE id = $1",
                            vec![row.id.into(), e.clone().into()],
                        )
                        .await?;
                        Ok(e)
                    }
                    Err(u) => Err(u),
                },
            };
            match built {
                Ok(e) => ready.push((row, e)),
                Err(Unbuilt::Skip(reason)) => {
                    settle(&txn, row.id, "SKIPPED", None, Some(reason.into()), now).await?;
                    report.skipped += 1;
                    crate::metrics::inc(METRIC, &[("outcome", "skipped")]);
                }
                Err(Unbuilt::Unavailable) => {
                    retry(&txn, &row, "PRINCIPALS_UNAVAILABLE", true, now).await?;
                    report.retried += 1;
                    crate::metrics::inc(METRIC, &[("outcome", "retried")]);
                    stop = true;
                }
                Err(Unbuilt::Invalid) => {
                    settle(
                        &txn,
                        row.id,
                        "DEAD",
                        None,
                        Some("ENVELOPE_INVALID".into()),
                        now,
                    )
                    .await?;
                    report.dead += 1;
                    crate::metrics::inc(METRIC, &[("outcome", "dead")]);
                }
            }
        }
        if !ready.is_empty() {
            let events: Vec<Value> = ready.iter().map(|(_, e)| e.clone()).collect();
            match sink.ingest(&events, false).await {
                Ok(results) => {
                    let by_id: HashMap<&str, &IngestResult> =
                        results.iter().map(|r| (r.event_id.as_str(), r)).collect();
                    for (row, _) in &ready {
                        match by_id.get(row.event_id.as_str()) {
                            Some(r)
                                if matches!(
                                    r.outcome.as_str(),
                                    "ACCEPTED" | "DUPLICATE" | "HELD"
                                ) =>
                            {
                                settle(&txn, row.id, "ACKED", Some(&r.outcome), None, now).await?;
                                report.acked += 1;
                                crate::metrics::inc(METRIC, &[("outcome", "acked")]);
                            }
                            Some(r) if r.outcome == "REJECTED" => {
                                let reason = reason_code(r.reason.as_deref());
                                settle(&txn, row.id, "DEAD", Some("REJECTED"), Some(reason), now)
                                    .await?;
                                report.dead += 1;
                                crate::metrics::inc(METRIC, &[("outcome", "dead")]);
                            }
                            _ => {
                                retry(&txn, row, "NO_RESULT", true, now).await?;
                                report.retried += 1;
                                crate::metrics::inc(METRIC, &[("outcome", "retried")]);
                            }
                        }
                    }
                }
                Err(PublishFailure::Rejected) => {
                    for (row, _) in &ready {
                        settle(
                            &txn,
                            row.id,
                            "DEAD",
                            None,
                            Some("CONTRACT_REJECTED".into()),
                            now,
                        )
                        .await?;
                        report.dead += 1;
                        crate::metrics::inc(METRIC, &[("outcome", "dead")]);
                    }
                    stop = true;
                }
                Err(PublishFailure::Unavailable) => {
                    for (row, _) in &ready {
                        retry(&txn, row, "UNAVAILABLE", true, now).await?;
                        report.retried += 1;
                        crate::metrics::inc(METRIC, &[("outcome", "retried")]);
                    }
                    stop = true;
                }
                Err(PublishFailure::NotConfigured) => {
                    for (row, _) in &ready {
                        retry(&txn, row, "NOT_CONFIGURED", false, now).await?;
                        report.not_configured += 1;
                    }
                    stop = true;
                }
            }
        }
        txn.commit().await?;
        if stop || !full {
            break;
        }
    }
    Ok(report)
}

async fn expire_overdue(state: &AppState) -> std::result::Result<u64, DbErr> {
    let expired = db::exec(
        &state.db,
        "UPDATE messaging_member_notifications SET state = 'EXPIRED', last_error = 'EXPIRED', completed_at = $1 \
          WHERE state = 'PENDING' AND expires_at <= $1",
        vec![state.now().into()],
    )
    .await?;
    for _ in 0..expired {
        crate::metrics::inc(METRIC, &[("outcome", "expired")]);
    }
    Ok(expired)
}

/// Hourly maintenance: expires overdue rows and deletes at most `batch` settled rows older
/// than [`RETENTION`]. A database without the table is a no-op. Returns (expired, purged).
pub async fn maintain(state: &AppState, batch: i64) -> Result<(u64, u64)> {
    let undefined = |e: &DbErr| db::sqlstate(e).as_deref() == Some("42P01");
    let expired = match expire_overdue(state).await {
        Err(e) if undefined(&e) => return Ok((0, 0)),
        other => other?,
    };
    let purged = db::exec(
        &state.db,
        "DELETE FROM messaging_member_notifications WHERE id IN (SELECT id FROM messaging_member_notifications \
          WHERE state <> 'PENDING' AND completed_at < $1 ORDER BY completed_at LIMIT $2)",
        vec![(state.now() - RETENTION).into(), batch.into()],
    )
    .await?;
    Ok((expired, purged))
}

/// The background relay loop (spawned only with `CF_NOTIFICATIONS_BASE_URL`).
pub async fn run(state: AppState, sink: Arc<dyn NotificationEventSink>, interval: StdDuration) {
    loop {
        if let Err(e) = relay_due(&state, sink.as_ref(), 500).await {
            tracing::warn!(error = %e, "member notification relay pass failed");
        }
        tokio::time::sleep(interval).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::EnvironmentKind;

    fn scope() -> OwnerScope {
        OwnerScope {
            tenant_id: Uuid::new_v4(),
            application_id: Uuid::new_v4(),
            environment_id: Uuid::new_v4(),
            environment_kind: EnvironmentKind::parse("PRODUCTION").expect("kind"),
            service_account_id: Uuid::new_v4(),
        }
    }

    #[test]
    fn only_a_person_adding_someone_else_to_a_group_is_announced() {
        let s = scope();
        let me = Uuid::new_v4();
        let other = Uuid::new_v4();
        let notice = |class: &'static str, actor: Option<Uuid>| MemberNotice {
            outbox_event_id: Uuid::new_v4(),
            event_sequence: 3,
            conversation_id: Uuid::new_v4(),
            context_class: class,
            principal_id: me,
            actor_principal_id: actor,
            scope: &s,
            first_party: true,
            occurred_at: Utc::now(),
        };
        assert!(announced(&notice("GROUP", Some(other))));
        assert!(announced(&notice("CHANNEL", Some(other))));
        assert!(!announced(&notice("GROUP", Some(me))));
        assert!(!announced(&notice("GROUP", None)));
        assert!(!announced(&notice("DIRECT", Some(other))));
    }

    #[test]
    fn the_envelope_is_the_catalogue_type_with_ids_only() {
        let now = Utc::now();
        let row = Row {
            id: Uuid::new_v4(),
            event_id: format!("evt_{}", "a".repeat(32)),
            outbox_event_id: Uuid::new_v4(),
            decision_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            application_id: Uuid::new_v4(),
            environment_id: Uuid::new_v4(),
            conversation_id: Uuid::new_v4(),
            event_sequence: 7,
            principal_id: Uuid::new_v4(),
            service_account_id: Uuid::new_v4(),
            environment_kind: "PRODUCTION".into(),
            first_party: true,
            correlation_id: format!("corr_{}", "b".repeat(32)),
            envelope: None,
            attempts: 0,
            occurred_at: now,
            expires_at: now + LIFETIME,
            still_member: true,
            security_profile: Some("PRIVATE_E2EE".into()),
            context_class: Some("GROUP".into()),
        };
        let account = format!("cfmeet-user:{}", Uuid::new_v4());
        let e = envelope(&row, account.clone()).expect("valid envelope");
        assert_eq!(e["eventType"], EVENT_TYPE);
        assert_eq!(e["producer"], "cfmeet.messaging");
        assert_eq!(e["route"]["routingVersion"], 1);
        let data = e["data"].as_object().unwrap();
        let mut keys: Vec<&str> = data.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, ["conversationId", "eventSequence"]);
        assert!(e.get("sealed").is_none());
        assert!(e.to_string().contains(&account));
    }

    #[test]
    fn backoff_is_bounded() {
        assert_eq!(backoff(1), Duration::seconds(2));
        assert_eq!(backoff(40), Duration::seconds(300));
    }
}
