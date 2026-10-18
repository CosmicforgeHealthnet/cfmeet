//! Durable outbound submission (Part IV Workstream O, Steps 16 / 17).
//!
//! Every pass (multi-instance safe: `FOR UPDATE SKIP LOCKED`, leases, the live-attempt
//! unique index) does, in order:
//!
//! 1. **Expiry** — an AUTHORIZED message past its validity is EXPIRED (estimate voided).
//! 2. **Lease recovery** — an attempt whose lease ran out (the worker died mid-flight)
//!    MAY have reached the provider: it becomes UNKNOWN, never retried.
//! 3. **Dispatch** — one due AUTHORIZED message at a time: an attempt is CLAIMED under a
//!    lease (SUBMITTING); Numbering confirms the sender is still the scope's ACTIVE
//!    assignment (revocation between authorization and submission → FAILED
//!    `SENDER_REVOKED`; Numbering unreachable → not sent, retried); the provider is called
//!    with the message's stable idempotency key under a bounded timeout. ACCEPTED →
//!    SUBMITTED (provider reference bound internally); REJECTED → FAILED; NOT_SENT →
//!    AUTHORIZED again with backoff (FAILED once the attempts are spent); a timeout or an
//!    ambiguous answer → UNKNOWN.
//! 4. **Reconciliation** — an UNKNOWN message is resolved ONLY by provider read-back
//!    (status query by its idempotency key): found → its provider state; confirmed never
//!    received (after a grace period) → FAILED `NOT_TRANSMITTED`; no answer → asked again
//!    later with backoff. Nothing is ever resubmitted.
//!
//! Every transition and its event commit together.

use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, DbErr, FromQueryResult, TransactionTrait};
use uuid::Uuid;
use zeroize::Zeroizing;

use super::{
    events::{self, MessageEvent},
    protection::Purpose,
    store, SmsError, SmsResult, Status, TransportRow, TRANSPORT_COLUMNS,
};
use crate::{
    db,
    domain::{CausationId, RequestContext},
    identity::OwnerScope,
    numbering::NumberingError,
    providers::{QueryOutcome, ReceiptStatus, SubmitOutcome, SubmitRequest},
    state::AppState,
};

/// What one pass did (counts only).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DispatchReport {
    pub expired: usize,
    pub recovered_unknown: usize,
    pub claimed: usize,
    pub submitted: usize,
    pub rejected: usize,
    pub not_sent: usize,
    pub unknown: usize,
    pub sender_revoked: usize,
    pub reconciled: usize,
}

/// Delay before an UNKNOWN message is first read back, and the read-back backoff cap.
pub const RECONCILE_DELAY_SECONDS: i64 = 30;
pub const RECONCILE_MAX_DELAY_SECONDS: i64 = 900;
/// A provider's "never received" is trusted only this long after the attempt ended.
pub const NOT_FOUND_GRACE_SECONDS: i64 = 120;
const BATCH: usize = 50;

/// The provider idempotency key of a message: stable across its attempts.
pub fn provider_key(transport_id: Uuid) -> String {
    format!("sms-{}", transport_id.simple())
}

fn context_of(row: &TransportRow) -> RequestContext {
    let correlation = cf_service_auth::CorrelationId::parse(&row.correlation_id)
        .unwrap_or_else(|_| cf_service_auth::CorrelationId::generate());
    RequestContext::continuing(correlation, CausationId::of_record("sms", row.id))
}

/// One pass of every dispatcher duty.
pub async fn run_once(state: &AppState) -> SmsResult<DispatchReport> {
    let mut report = DispatchReport::default();
    expire_due(state, &mut report).await?;
    recover_expired_leases(state, &mut report).await?;
    for _ in 0..BATCH {
        if !dispatch_one(state, &mut report).await? {
            break;
        }
    }
    for _ in 0..BATCH {
        if !reconcile_one(state, &mut report).await? {
            break;
        }
    }
    Ok(report)
}

async fn void_usage<C: ConnectionTrait>(
    db: &C,
    transport_id: Uuid,
    now: DateTime<Utc>,
) -> Result<(), DbErr> {
    db::exec(
        db,
        "UPDATE sms_usage_records SET status = 'VOIDED', updated_at = $2 WHERE transport_id = $1 AND status = 'ESTIMATED'",
        vec![transport_id.into(), now.into()],
    )
    .await
    .map(|_| ())
}

/// Confirms the provider's cost once (no duplicate accrual).
pub async fn confirm_cost<C: ConnectionTrait>(
    db: &C,
    row: &TransportRow,
    cost_minor: Option<i64>,
    now: DateTime<Utc>,
) -> Result<bool, DbErr> {
    let Some(cost) = cost_minor else {
        return Ok(false);
    };
    Ok(db::exec(
        db,
        "UPDATE sms_usage_records SET status = 'CONFIRMED', confirmed_cost_minor = $2, updated_at = $3 \
         WHERE transport_id = $1 AND status = 'ESTIMATED'",
        vec![row.id.into(), cost.into(), now.into()],
    )
    .await?
        == 1)
}

async fn expire_due(state: &AppState, report: &mut DispatchReport) -> SmsResult<()> {
    let now = state.now();
    for _ in 0..BATCH {
        let txn = state.db.begin().await?;
        let Some(row) = db::one::<TransportRow, _>(
            &txn,
            &format!(
                "SELECT {TRANSPORT_COLUMNS} FROM sms_transports WHERE status = 'AUTHORIZED' AND expires_at <= $1 \
                 ORDER BY expires_at LIMIT 1 FOR UPDATE SKIP LOCKED"
            ),
            vec![now.into()],
        )
        .await?
        else {
            txn.rollback().await?;
            break;
        };
        let row = store::update_in(
            &txn,
            &row,
            Some(Status::Expired),
            vec![
                ("failure_code", "VALIDITY_EXPIRED".into()),
                ("next_attempt_at", Option::<DateTime<Utc>>::None.into()),
            ],
            now,
        )
        .await?;
        void_usage(&txn, row.id, now).await?;
        events::record(&txn, &row, MessageEvent::Failed, &context_of(&row), now).await?;
        txn.commit().await?;
        report.expired += 1;
    }
    Ok(())
}

#[derive(Debug, FromQueryResult)]
struct Ended {
    finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, FromQueryResult)]
struct AttemptRow {
    id: Uuid,
    transport_id: Uuid,
    status: String,
}

async fn recover_expired_leases(state: &AppState, report: &mut DispatchReport) -> SmsResult<()> {
    let now = state.now();
    for _ in 0..BATCH {
        let txn = state.db.begin().await?;
        let Some(a) = db::one::<AttemptRow, _>(
            &txn,
            "SELECT id, transport_id, status FROM sms_attempts \
              WHERE status = 'CLAIMED' AND lease_until < $1 ORDER BY lease_until LIMIT 1 FOR UPDATE SKIP LOCKED",
            vec![now.into()],
        )
        .await?
        else {
            txn.rollback().await?;
            break;
        };
        db::exec(
            &txn,
            "UPDATE sms_attempts SET status = 'UNKNOWN', outcome_code = 'LEASE_EXPIRED', lease_owner = NULL, \
               lease_until = NULL, finished_at = $2 WHERE id = $1 AND status = 'CLAIMED'",
            vec![a.id.into(), now.into()],
        )
        .await?;
        let row = store::lock(&txn, a.transport_id)
            .await?
            .ok_or(SmsError::NotFound)?;
        if row.status() == Status::Submitting {
            let row = store::update_in(
                &txn,
                &row,
                Some(Status::Unknown),
                vec![
                    ("failure_code", "LEASE_EXPIRED".into()),
                    (
                        "next_attempt_at",
                        (now + Duration::seconds(RECONCILE_DELAY_SECONDS)).into(),
                    ),
                ],
                now,
            )
            .await?;
            events::record(&txn, &row, MessageEvent::Unknown, &context_of(&row), now).await?;
        }
        txn.commit().await?;
        crate::metrics::inc("sms_attempts_total", &[("outcome", "lease_expired")]);
        tracing::warn!(message_id = %a.transport_id, "submission lease expired: outcome UNKNOWN (never resubmitted)");
        report.recovered_unknown += 1;
    }
    Ok(())
}

/// Claims the next due message; `false` when none is due.
async fn dispatch_one(state: &AppState, report: &mut DispatchReport) -> SmsResult<bool> {
    let Some(provider) = state.provider.clone() else {
        return Ok(false);
    };
    let now = state.now();
    // 1. Claim (SUBMITTING + a CLAIMED attempt under a lease), committed before the call.
    let txn = state.db.begin().await?;
    let Some(row) = db::one::<TransportRow, _>(
        &txn,
        &format!(
            "SELECT {TRANSPORT_COLUMNS} FROM sms_transports \
              WHERE status = 'AUTHORIZED' AND next_attempt_at <= $1 AND (expires_at IS NULL OR expires_at > $1) \
              ORDER BY next_attempt_at, id LIMIT 1 FOR UPDATE SKIP LOCKED"
        ),
        vec![now.into()],
    )
    .await?
    else {
        txn.rollback().await?;
        return Ok(false);
    };
    let attempt_id = Uuid::new_v4();
    let number = row.attempt_count + 1;
    db::exec(
        &txn,
        "INSERT INTO sms_attempts (id, transport_id, attempt_number, status, lease_owner, lease_until, provider_id, started_at) \
         VALUES ($1, $2, $3, 'CLAIMED', $4, $5, $6, $7)",
        vec![
            attempt_id.into(),
            row.id.into(),
            number.into(),
            state.instance.clone().into(),
            (now + state.workers.lease).into(),
            provider.id().into(),
            now.into(),
        ],
    )
    .await?;
    let row = store::update_in(
        &txn,
        &row,
        Some(Status::Submitting),
        vec![
            ("attempt_count", number.into()),
            ("next_attempt_at", Option::<DateTime<Utc>>::None.into()),
        ],
        now,
    )
    .await?;
    txn.commit().await?;
    report.claimed += 1;
    let ctx = context_of(&row);

    // 2. The sender must still be the scope's ACTIVE assignment (revocation, release or
    //    reassignment since authorization stops the message here, before any provider call).
    let scope = OwnerScope {
        tenant_id: row.tenant_id,
        application_id: row.application_id,
        environment_id: row.environment_id,
        environment_kind: row.environment_kind(),
        service_account_id: row.service_account_id.unwrap_or_default(),
    };
    let sender = state
        .numbering
        .sender_current(
            &scope.delegated(row.principal_id),
            &ctx.correlation_id,
            row.number_resource_id,
        )
        .await;
    let outcome = match sender {
        Ok(s) if s.active && Some(s.assignment_id) == row.assignment_id => None,
        Ok(_) | Err(NumberingError::Refused(_)) => Some(SubmitOutcome::Rejected {
            code: "SENDER_REVOKED",
        }),
        Err(NumberingError::Unavailable) => Some(SubmitOutcome::NotSent {
            code: "NUMBERING_UNAVAILABLE",
        }),
    };
    let outcome = match outcome {
        Some(o) => o,
        None => {
            // 3. The provider call (plaintext only in memory, for this request).
            let p = &state.protection;
            let open = |purpose, sealed: Option<&str>| {
                sealed
                    .and_then(|s| p.open(purpose, row.id, s).ok())
                    .map(Zeroizing::new)
            };
            match (
                open(Purpose::Sender, row.sender_sealed.as_deref()),
                open(Purpose::Remote, Some(&row.remote_sealed)),
                open(Purpose::Content, row.content_sealed.as_deref()),
            ) {
                (Some(from), Some(to), Some(body)) => {
                    let request = SubmitRequest {
                        from,
                        to,
                        body,
                        encoding: super::segments::Encoding::parse(&row.content_encoding)
                            .unwrap_or(super::segments::Encoding::Ucs2),
                        segments: row.segment_count as usize,
                        idempotency_key: provider_key(row.id),
                        validity_until: row.expires_at.unwrap_or(now + Duration::hours(1)),
                    };
                    match tokio::time::timeout(
                        state.transport.submit_timeout,
                        provider.submit(request),
                    )
                    .await
                    {
                        Ok(o) => o,
                        // The provider may have the message: never assume it does not.
                        Err(_) => SubmitOutcome::Unknown {
                            reason: "SUBMIT_TIMEOUT",
                        },
                    }
                }
                _ => SubmitOutcome::Rejected {
                    code: "CONTENT_UNAVAILABLE",
                },
            }
        }
    };
    finish(state, &row, attempt_id, outcome, report).await?;
    Ok(true)
}

/// Applies a submission outcome (guarded by the attempt's claim).
async fn finish(
    state: &AppState,
    claimed: &TransportRow,
    attempt_id: Uuid,
    outcome: SubmitOutcome,
    report: &mut DispatchReport,
) -> SmsResult<()> {
    let now = state.now();
    let ctx = context_of(claimed);
    let txn = state.db.begin().await?;
    let row = store::lock(&txn, claimed.id)
        .await?
        .ok_or(SmsError::NotFound)?;
    let attempt = db::one::<AttemptRow, _>(
        &txn,
        "SELECT id, transport_id, status FROM sms_attempts WHERE id = $1 FOR UPDATE",
        vec![attempt_id.into()],
    )
    .await?
    .ok_or(SmsError::NotFound)?;
    let claim_held = attempt.status == "CLAIMED" && row.status() == Status::Submitting;
    let close = |status: &'static str, code: Option<&str>, reference: Option<&str>| {
        db::stmt(
            "UPDATE sms_attempts SET status = $2, outcome_code = $3, provider_message_ref = COALESCE($4, provider_message_ref), \
               lease_owner = NULL, lease_until = NULL, finished_at = COALESCE(finished_at, $5) WHERE id = $1",
            vec![
                attempt_id.into(),
                status.into(),
                code.map(str::to_string).into(),
                reference.map(str::to_string).into(),
                now.into(),
            ],
        )
    };
    match outcome {
        SubmitOutcome::Accepted {
            provider_message_ref,
            cost_minor,
        } => {
            // Late evidence after a lost lease (UNKNOWN) is still evidence.
            if !(claim_held || (attempt.status == "UNKNOWN" && row.status() == Status::Unknown)) {
                txn.rollback().await?;
                return Ok(());
            }
            txn.execute(close(
                "SUBMITTED",
                Some("ACCEPTED"),
                Some(&provider_message_ref),
            ))
            .await?;
            bind(&txn, &row, attempt_id, &provider_message_ref, now).await?;
            let row = store::update_in(
                &txn,
                &row,
                Some(Status::Submitted),
                vec![
                    ("submitted_at", now.into()),
                    ("failure_code", Option::<String>::None.into()),
                    ("next_attempt_at", Option::<DateTime<Utc>>::None.into()),
                ],
                now,
            )
            .await?;
            confirm_cost(&txn, &row, cost_minor, now).await?;
            events::record(&txn, &row, MessageEvent::Submitted, &ctx, now).await?;
            report.submitted += 1;
            crate::metrics::inc("sms_attempts_total", &[("outcome", "accepted")]);
        }
        _ if !claim_held => {
            // The claim was lost (lease recovery made it UNKNOWN): only read-back decides.
            txn.rollback().await?;
            return Ok(());
        }
        SubmitOutcome::Rejected { code } => {
            txn.execute(close("REJECTED", Some(code), None)).await?;
            let row = store::update_in(
                &txn,
                &row,
                Some(Status::Failed),
                vec![("failure_code", code.into()), ("failed_at", now.into())],
                now,
            )
            .await?;
            void_usage(&txn, row.id, now).await?;
            events::record(&txn, &row, MessageEvent::Failed, &ctx, now).await?;
            if code == "SENDER_REVOKED" {
                report.sender_revoked += 1;
            } else {
                report.rejected += 1;
            }
            crate::metrics::inc("sms_attempts_total", &[("outcome", "rejected")]);
        }
        SubmitOutcome::NotSent { code } => {
            txn.execute(close("NOT_SENT", Some(code), None)).await?;
            if row.attempt_count >= row.max_attempts {
                let row = store::update_in(
                    &txn,
                    &row,
                    Some(Status::Failed),
                    vec![("failure_code", code.into()), ("failed_at", now.into())],
                    now,
                )
                .await?;
                void_usage(&txn, row.id, now).await?;
                events::record(&txn, &row, MessageEvent::Failed, &ctx, now).await?;
            } else {
                // Definitely not transmitted: safe to try again later.
                store::update_in(
                    &txn,
                    &row,
                    Some(Status::Authorized),
                    vec![(
                        "next_attempt_at",
                        (now + state.workers.retry.delay(row.attempt_count as i32)).into(),
                    )],
                    now,
                )
                .await?;
            }
            report.not_sent += 1;
            crate::metrics::inc("sms_attempts_total", &[("outcome", "not_sent")]);
        }
        SubmitOutcome::Unknown { reason } => {
            txn.execute(close("UNKNOWN", Some(reason), None)).await?;
            let row = store::update_in(
                &txn,
                &row,
                Some(Status::Unknown),
                vec![
                    ("failure_code", reason.into()),
                    (
                        "next_attempt_at",
                        (now + Duration::seconds(RECONCILE_DELAY_SECONDS)).into(),
                    ),
                ],
                now,
            )
            .await?;
            events::record(&txn, &row, MessageEvent::Unknown, &ctx, now).await?;
            report.unknown += 1;
            crate::metrics::inc("sms_attempts_total", &[("outcome", "unknown")]);
            tracing::warn!(message_id = %row.id, "provider outcome unknown: the message is never resubmitted");
        }
    }
    txn.commit().await?;
    Ok(())
}

async fn bind<C: ConnectionTrait>(
    db: &C,
    row: &TransportRow,
    attempt_id: Uuid,
    reference: &str,
    now: DateTime<Utc>,
) -> Result<(), DbErr> {
    db::exec(
        db,
        "INSERT INTO sms_provider_bindings (provider_id, provider_message_ref, transport_id, attempt_id, created_at) \
         VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING",
        vec![
            row.provider_id.clone().unwrap_or_default().into(),
            reference.into(),
            row.id.into(),
            attempt_id.into(),
            now.into(),
        ],
    )
    .await
    .map(|_| ())
}

/// Reads back one due UNKNOWN message; `false` when none is due.
async fn reconcile_one(state: &AppState, report: &mut DispatchReport) -> SmsResult<bool> {
    let Some(provider) = state.provider.clone() else {
        return Ok(false);
    };
    let now = state.now();
    // Claim the read-back slot (pushes the next one out) so instances do not query twice.
    let txn = state.db.begin().await?;
    let Some(row) = db::one::<TransportRow, _>(
        &txn,
        &format!(
            "SELECT {TRANSPORT_COLUMNS} FROM sms_transports WHERE status = 'UNKNOWN' AND next_attempt_at <= $1 \
             ORDER BY next_attempt_at, id LIMIT 1 FOR UPDATE SKIP LOCKED"
        ),
        vec![now.into()],
    )
    .await?
    else {
        txn.rollback().await?;
        return Ok(false);
    };
    // Read-back backoff grows with the time the outcome has been unknown (bounded).
    let since = db::one::<Ended, _>(
        &txn,
        "SELECT max(finished_at) AS finished_at FROM sms_attempts WHERE transport_id = $1",
        vec![row.id.into()],
    )
    .await?
    .and_then(|e| e.finished_at)
    .map(|f| (now - f).num_seconds())
    .unwrap_or(0);
    let backoff =
        Duration::seconds((since / 2).clamp(RECONCILE_DELAY_SECONDS, RECONCILE_MAX_DELAY_SECONDS));
    let row = store::update_in(
        &txn,
        &row,
        None,
        vec![("next_attempt_at", (now + backoff).into())],
        now,
    )
    .await?;
    txn.commit().await?;

    let answer = tokio::time::timeout(
        StdDuration::from_millis(state.transport.submit_timeout.as_millis() as u64),
        provider.query(&provider_key(row.id)),
    )
    .await
    .unwrap_or(QueryOutcome::Unknown);
    let ctx = context_of(&row);
    let txn = state.db.begin().await?;
    let current = store::lock(&txn, row.id).await?.ok_or(SmsError::NotFound)?;
    if current.status() != Status::Unknown {
        txn.rollback().await?;
        return Ok(true);
    }
    #[derive(FromQueryResult)]
    struct Last {
        id: Uuid,
        finished_at: Option<DateTime<Utc>>,
    }
    let last = db::one::<Last, _>(
        &txn,
        "SELECT id, finished_at FROM sms_attempts WHERE transport_id = $1 ORDER BY attempt_number DESC LIMIT 1",
        vec![current.id.into()],
    )
    .await?;
    match answer {
        QueryOutcome::Found {
            provider_message_ref,
            status,
        } => {
            if let Some(a) = &last {
                db::exec(
                    &txn,
                    "UPDATE sms_attempts SET status = 'SUBMITTED', provider_message_ref = COALESCE(provider_message_ref, $2), \
                       outcome_code = 'READ_BACK' WHERE id = $1 AND status = 'UNKNOWN'",
                    vec![a.id.into(), provider_message_ref.clone().into()],
                )
                .await?;
                bind(&txn, &current, a.id, &provider_message_ref, now).await?;
            }
            apply_provider_status(&txn, &current, status, None, None, &ctx, now).await?;
            report.reconciled += 1;
        }
        QueryOutcome::NotFound => {
            let ended = last.and_then(|a| a.finished_at).unwrap_or(now);
            if now - ended >= Duration::seconds(NOT_FOUND_GRACE_SECONDS) {
                // The provider confirms it never received the message: FAILED, not resent.
                let row = store::update_in(
                    &txn,
                    &current,
                    Some(Status::Failed),
                    vec![
                        ("failure_code", "NOT_TRANSMITTED".into()),
                        ("failed_at", now.into()),
                        ("next_attempt_at", Option::<DateTime<Utc>>::None.into()),
                    ],
                    now,
                )
                .await?;
                void_usage(&txn, row.id, now).await?;
                events::record(&txn, &row, MessageEvent::Failed, &ctx, now).await?;
                report.reconciled += 1;
            }
        }
        QueryOutcome::Unknown => {}
    }
    txn.commit().await?;
    Ok(true)
}

/// Applies provider evidence (a receipt or a read-back) to a message. Returns the
/// disposition: APPLIED, or STALE when the evidence would regress or repeat a state.
pub async fn apply_provider_status<C: ConnectionTrait>(
    db: &C,
    row: &TransportRow,
    status: ReceiptStatus,
    error_code: Option<&str>,
    cost_minor: Option<i64>,
    ctx: &RequestContext,
    now: DateTime<Utc>,
) -> SmsResult<&'static str> {
    let current = row.status();
    let (next, event, sets): (Status, MessageEvent, Vec<(&'static str, sea_orm::Value)>) =
        match status {
            ReceiptStatus::Delivered if matches!(current, Status::Submitted | Status::Unknown) => (
                Status::Delivered,
                MessageEvent::Delivered,
                vec![
                    ("delivered_at", now.into()),
                    ("failure_code", Option::<String>::None.into()),
                    ("confirmed_cost_minor", cost_minor.into()),
                ],
            ),
            ReceiptStatus::Failed | ReceiptStatus::Expired
                if matches!(current, Status::Submitted | Status::Unknown) =>
            {
                let code = error_code.map(str::to_string).unwrap_or_else(|| {
                    if status == ReceiptStatus::Expired {
                        "DELIVERY_EXPIRED"
                    } else {
                        "DELIVERY_FAILED"
                    }
                    .into()
                });
                (
                    Status::Failed,
                    MessageEvent::Failed,
                    vec![
                        ("failure_code", code.into()),
                        ("failed_at", now.into()),
                        ("confirmed_cost_minor", cost_minor.into()),
                    ],
                )
            }
            ReceiptStatus::Accepted if current == Status::Unknown => (
                Status::Submitted,
                MessageEvent::Submitted,
                vec![
                    ("submitted_at", now.into()),
                    ("failure_code", Option::<String>::None.into()),
                ],
            ),
            _ => return Ok("STALE"),
        };
    let mut sets = sets;
    sets.push(("next_attempt_at", Option::<DateTime<Utc>>::None.into()));
    let updated = store::update_in(db, row, Some(next), sets, now).await?;
    confirm_cost(db, &updated, cost_minor, now).await?;
    events::record(db, &updated, event, ctx, now).await?;
    Ok("APPLIED")
}
