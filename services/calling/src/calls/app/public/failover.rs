//! Failover of a public leg (Part E §51, §142–§152).
//!
//! Business authorization and interconnect choice stay separate: Calling decides
//! WHETHER another attempt is allowed (the Route Plan's failover policy through the
//! Routing Engine's `revise`, before CONNECTED only, after a DEFINITE provider-side
//! failure, within the attempt budget); Telephony decides WHICH eligible interconnect
//! executes it (excluding the ones that already failed for this party, honouring
//! health, circuits and capacity).
//!
//! A failover attempt is a NEW CallLeg (one execution binding per leg), a new
//! CallAttempt chained to the failed one, a new spend reservation (the failed leg's is
//! released: nothing was connected) and a new durable ORIGINATE command; the protected
//! party moves to the new leg (the database allows that only after its leg FAILED).
//!
//! Never on an UNKNOWN outcome (it is not a failure: reconcile first), never on the
//! callee's own answer (busy / no answer / declined), never on a failure before anything
//! was transmitted for lack of a route (NOT_SENT: no eligible interconnect is left), and
//! never after CONNECTED. Telephony enforces the same at its boundary: it refuses an
//! execution to the same party of the same Call while an earlier one is live or
//! unresolved.

use chrono::Duration;
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{enqueue_in, stmt, CommandKind};
use crate::{
    calls::{
        app::AppCallService,
        attempts::NewAttempt,
        errors::CallResult,
        ids::{CallAttemptId, CallLegId, CallParticipantId, CallRouteId},
        kinds::{FailureCategory, LegDirection, LegKind},
        legs::NewLeg,
    },
    models::{call_attempts, call_legs, calls},
    routing::engine::{
        revise, AttemptFact, Revision, RevisionFacts, RoutePlan, PAID_PLAN_VALIDITY_SECONDS,
    },
    telephony::{
        events::{TelephonyEvent, TelephonyEventType},
        spend,
    },
};

/// Provider-side failure categories that justify another interconnect.
fn provider_side(category: Option<&str>) -> bool {
    matches!(
        category.map(FailureCategory::parse),
        Some(Some(
            FailureCategory::Routing | FailureCategory::Network | FailureCategory::Execution
        ))
    )
}

/// Whether this terminal leg event may lead to a failover attempt at all.
pub(crate) fn failover_candidate(event: &TelephonyEvent, leg: &call_legs::Model) -> bool {
    event.kind() == TelephonyEventType::LegFailed
        && event.definite == Some(true)
        && event.end_reason.as_deref() != Some("NOT_SENT")
        && (provider_side(event.failure_category.as_deref())
            || event.end_reason.as_deref() == Some("ABSENT_AT_PROVIDER"))
        && leg.direction == LegDirection::Outbound
        && leg.leg_kind == LegKind::PublicNetwork
        && leg.answered_at.is_none()
}

#[derive(Debug, FromQueryResult)]
struct FailedOriginate {
    payload: Value,
}

#[derive(Debug, FromQueryResult)]
struct Party {
    id: Uuid,
    contact_reference: Uuid,
}

#[derive(Debug, FromQueryResult)]
struct Plan {
    route_plan: Option<Value>,
}

#[derive(Debug, FromQueryResult)]
struct Validity {
    valid_until: chrono::DateTime<chrono::Utc>,
}

impl AppCallService {
    /// Starts the next attempt for the party served by `failed` (already FAILED, its
    /// attempt finished), when the plan permits. Returns whether one was started.
    pub(crate) async fn failover_in(
        &self,
        txn: &sea_orm::DatabaseTransaction,
        call: &calls::Model,
        failed: &call_legs::Model,
        failed_attempt: Option<&call_attempts::Model>,
    ) -> CallResult<bool> {
        if call.connected_at.is_some() || call.current_state.is_terminal() {
            return Ok(false);
        }
        let Some(route_id) = failed.route_id else {
            return Ok(false);
        };
        let failed_id = failed.id;
        let Some(party) = Party::find_by_statement(stmt(
            "SELECT id, contact_reference FROM call_public_parties WHERE call_leg_id = $1",
            vec![failed_id.into()],
        ))
        .one(txn)
        .await?
        else {
            return Ok(false);
        };
        let Some(original) = FailedOriginate::find_by_statement(stmt(
            "SELECT payload FROM call_telephony_commands WHERE call_leg_id = $1 AND kind = 'ORIGINATE'",
            vec![failed_id.into()],
        ))
        .one(txn)
        .await?
        else {
            return Ok(false);
        };
        let plan: Option<RoutePlan> = Plan::find_by_statement(stmt(
            "SELECT route_plan FROM call_routes WHERE id = $1",
            vec![route_id.into()],
        ))
        .one(txn)
        .await?
        .and_then(|p| p.route_plan)
        .and_then(|v| serde_json::from_value(v).ok());
        let Some(plan) = plan else {
            return Ok(false);
        };
        // Every execution to this party of this Call so far (each a leg of its own).
        let attempts_used = txn
            .query_one(stmt(
                "SELECT count(*) FROM call_telephony_commands WHERE call_id = $1 AND kind = 'ORIGINATE' \
                   AND payload -> 'destination' ->> 'contactReference' = $2",
                vec![call.id.into(), party.contact_reference.to_string().into()],
            ))
            .await?
            .map(|r| r.try_get_by_index::<i64>(0))
            .transpose()?
            .unwrap_or(1);
        // An earlier attempt of this party still UNKNOWN blocks any further one.
        let unresolved = txn
            .query_one(stmt(
                "SELECT count(*) FROM call_attempts a JOIN call_telephony_commands c ON c.attempt_id = a.id \
                 WHERE a.call_id = $1 AND c.kind = 'ORIGINATE' AND a.outcome IN ('PENDING', 'UNKNOWN') \
                   AND c.payload -> 'destination' ->> 'contactReference' = $2 AND a.leg_id <> $3",
                vec![
                    call.id.into(),
                    party.contact_reference.to_string().into(),
                    failed_id.into(),
                ],
            ))
            .await?
            .map(|r| r.try_get_by_index::<i64>(0))
            .transpose()?
            .unwrap_or(0);
        let revision = revise(
            &plan,
            &RevisionFacts {
                call_connected: false,
                last_attempt: if unresolved > 0 {
                    AttemptFact::Unknown
                } else {
                    AttemptFact::DefiniteFailure
                },
                attempts_used: attempts_used.clamp(0, i64::from(u8::MAX)) as u8,
                // Telephony owns the interconnect history and excludes what failed.
                attempted_routes: Vec::new(),
                unavailable_routes: Vec::new(),
            },
        );
        if !matches!(revision, Revision::Failover { .. }) {
            tracing::info!(call_id = %call.id, ?revision, "no failover for the failed public leg");
            return Ok(false);
        }
        let now = self.calls.now();
        let cmd = super::super::system_cmd(call);
        // All-or-nothing inside the event's transaction (a savepoint): a refused spend
        // reservation leaves no half-built attempt behind.
        let sp = txn.begin().await?;
        let leg = self
            .calls
            .record_leg_in(
                &sp,
                &cmd,
                call,
                NewLeg {
                    route_id: Some(CallRouteId::from_uuid(route_id)),
                    participant_id: failed.participant_id.map(CallParticipantId::from_uuid),
                    direction: LegDirection::Outbound,
                    kind: LegKind::PublicNetwork,
                },
            )
            .await?;
        let attempt = self
            .calls
            .record_attempt_in(
                &sp,
                &cmd,
                call,
                NewAttempt {
                    route_id: Some(CallRouteId::from_uuid(route_id)),
                    leg_id: Some(CallLegId::from_uuid(leg.id)),
                    previous_attempt_id: failed_attempt.map(|a| CallAttemptId::from_uuid(a.id)),
                },
            )
            .await?;
        super::exec(
            &sp,
            "UPDATE call_public_parties SET call_leg_id = $2 WHERE id = $1 AND call_leg_id = $3",
            vec![party.id.into(), leg.id.into(), failed_id.into()],
        )
        .await?;
        // Nothing was connected on the failed leg: its reservation is released; the new
        // attempt reserves anew (limits and velocity apply to it like to any other).
        spend::release_in(&sp, failed_id, now).await?;
        let reservation = match spend::reserve_in(
            &sp,
            call,
            leg.id,
            Some(party.contact_reference),
            now,
        )
        .await?
        {
            Ok(r) => r,
            Err(denial) => {
                sp.rollback().await?;
                // The outer transaction is still open: never hold a second connection.
                {
                    let (db, call) = (self.db().clone(), call.clone());
                    tokio::spawn(
                        async move { spend::notify_refusal(&db, &call, denial, now).await },
                    );
                }
                tracing::info!(call_id = %call.id, reason = denial.as_str(), "failover refused by the spend policy");
                return Ok(false);
            }
        };
        // The same command, for the new leg / attempt, with a fresh proof window (capped
        // by the recorded Privacy Relay decision) and the new reservation.
        let valid_until = Validity::find_by_statement(stmt(
            "SELECT valid_until FROM call_privacy_authorizations WHERE call_id = $1 \
             ORDER BY created_at DESC LIMIT 1",
            vec![call.id.into()],
        ))
        .one(&sp)
        .await?
        .map(|v| {
            v.valid_until
                .min(now + Duration::seconds(PAID_PLAN_VALIDITY_SECONDS))
        });
        let key = format!("originate:{}:{}", leg.id, attempt.id);
        let mut payload = original.payload;
        payload["commandId"] = json!(Uuid::new_v4());
        payload["idempotencyKey"] = json!(key);
        payload["correlationId"] = json!(cmd.correlation_id().as_str());
        payload["expectedGeneration"] = json!(1);
        payload["call"]["callLegId"] = json!(leg.id);
        payload["call"]["attemptId"] = json!(attempt.id);
        if let Some(v) = valid_until {
            payload["authorization"]["validUntil"] = json!(v);
        }
        payload["spend"] = json!({
            "reservationId": reservation.id,
            "reservedAmountMinor": reservation.amount_minor,
            "currency": reservation.currency,
            "maxDurationSeconds": reservation.max_duration_seconds,
        });
        enqueue_in(
            &sp,
            call,
            CommandKind::Originate,
            Some(leg.id),
            Some(attempt.id),
            None,
            &key,
            payload,
            now,
        )
        .await?;
        sp.commit().await?;
        crate::metrics::inc("calling_public_failovers_total", &[]);
        tracing::info!(
            call_id = %call.id,
            failed_leg_id = %failed_id,
            call_leg_id = %leg.id,
            attempts_used,
            "public leg failed definitely before connection; failover attempt started"
        );
        Ok(true)
    }
}
