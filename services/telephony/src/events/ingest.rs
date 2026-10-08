//! Provider event normalization and application (Part E §49, §118, §125–§131, §169–§172).
//!
//! Every provider event — an authenticated HTTP callback or an edge control-plane event —
//! arrives here already normalized by its adapter and is:
//!
//! 1. deduplicated by (`provider_id`, `provider_event_id`) — a replay is a safe no-op;
//! 2. matched to a leg ONLY through Telephony's own binding (provider call ref, or the
//!    correlation key persisted before the request). An unknown provider object is
//!    QUARANTINED (never attached to an arbitrary Call) and raises an incident;
//! 3. applied through the normalized leg state machine, never regressing: an event that
//!    is older (provider sequence) or less advanced than the current state is recorded
//!    STALE; a success after the leg is terminal is LATE_AFTER_TERMINAL — it never
//!    resurrects anything (a still-live provider call is terminated by reconciliation);
//! 4. stored with a bounded, restricted, redacted detail and a retention bound. Raw
//!    provider payloads are never forwarded to Calling, webhooks or Privacy Relay.

use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    db,
    domain::{EndReason, LegStatus},
    error::{is_unique_violation, ErrorCode, TelephonyError, TelephonyResult},
    incidents::{self, Incident, Severity},
    legs::{self, Change, LegRow},
    providers::{EventRejection, ProviderEvent, ProviderIngress},
    service::TelephonyService,
};

/// Maximum accepted provider callback body.
pub const MAX_CALLBACK_BYTES: usize = 16 * 1024;

/// What happened to one event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    Applied,
    Duplicate,
    Stale,
    LateAfterTerminal,
    Quarantined,
    Ignored,
}

impl Disposition {
    pub fn as_str(&self) -> &'static str {
        match self {
            Disposition::Applied => "APPLIED",
            Disposition::Duplicate => "DUPLICATE",
            Disposition::Stale => "STALE",
            Disposition::LateAfterTerminal => "LATE_AFTER_TERMINAL",
            Disposition::Quarantined => "QUARANTINED",
            Disposition::Ignored => "IGNORED",
        }
    }
}

#[derive(Debug, FromQueryResult)]
struct BindingMatch {
    id: Uuid,
    telephony_leg_id: Uuid,
    provider_call_ref: Option<String>,
}

fn digest(event: &ProviderEvent) -> String {
    hex::encode(Sha256::digest(
        format!(
            "{}|{:?}|{:?}|{}|{}|{:?}",
            event.provider_event_id,
            event.provider_call_ref,
            event.correlation_key,
            event.event,
            event.occurred_at.timestamp_micros(),
            event.sequence
        )
        .as_bytes(),
    ))
}

/// Bounded restricted detail: only short, safe tokens (never a number or a payload).
fn restricted(event: &ProviderEvent) -> Value {
    let mut map = serde_json::Map::new();
    for (k, v) in event.restricted_detail.iter().take(8) {
        let safe = |s: &str| {
            !s.is_empty()
                && s.len() <= 64
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
                && s.bytes().filter(u8::is_ascii_digit).count() < 7
        };
        if safe(k) && safe(v) {
            map.insert(k.clone(), Value::String(v.clone()));
        }
    }
    Value::Object(map)
}

impl TelephonyService {
    /// Authenticates, parses and applies an HTTP provider callback. No state is touched
    /// before authentication succeeds; failures are counted (bounded) and may raise a
    /// CALLBACK_AUTH_FAILURES incident.
    pub async fn ingest_callback(
        &self,
        ingress: ProviderIngress,
    ) -> TelephonyResult<Vec<Disposition>> {
        if ingress.body.len() > MAX_CALLBACK_BYTES {
            return Err(TelephonyError::Refused(ErrorCode::PayloadTooLarge));
        }
        let Some(provider) = self.providers.get(&ingress.provider_id).cloned() else {
            return Err(TelephonyError::Refused(ErrorCode::ProviderEventRejected));
        };
        if let Err(rejection) = provider.verify_event(&ingress) {
            self.count_auth_failure(&ingress.provider_id, rejection)
                .await?;
            return Err(TelephonyError::Refused(ErrorCode::ProviderEventRejected));
        }
        let events = provider
            .normalize_event(&ingress)
            .map_err(|_| TelephonyError::Validation("The provider event is malformed."))?;
        self.count_window("CALLBACK_ACCEPTED", &ingress.provider_id)
            .await?;
        let mut out = Vec::new();
        for event in events {
            out.push(
                self.ingest_provider_event(&ingress.provider_id, event)
                    .await?,
            );
        }
        Ok(out)
    }

    /// Deletes provider event evidence past its retention bound (bounded batches).
    pub async fn purge_expired_provider_events(&self, limit: u64) -> TelephonyResult<u64> {
        let deleted = db::exec(
            &self.db,
            "DELETE FROM provider_events WHERE id IN (SELECT id FROM provider_events WHERE retain_until < $1 \
             ORDER BY retain_until LIMIT $2 FOR UPDATE SKIP LOCKED)",
            vec![self.now().into(), (limit.clamp(1, 1000) as i64).into()],
        )
        .await?;
        Ok(deleted)
    }

    async fn count_window(&self, kind: &str, subject: &str) -> TelephonyResult<i64> {
        let now = self.now();
        let window = now
            - Duration::seconds(now.timestamp() % 60)
            - Duration::nanoseconds(i64::from(now.timestamp_subsec_nanos()));
        let row: Option<HitsRow> = db::one(
            &self.db,
            "INSERT INTO telephony_ingress_windows (subject_kind, subject_id, window_start, hits) VALUES ($1, $2, $3, 1) \
             ON CONFLICT (subject_kind, subject_id, window_start) DO UPDATE SET hits = telephony_ingress_windows.hits + 1 \
             RETURNING hits",
            vec![kind.into(), subject.into(), window.into()],
        )
        .await?;
        db::exec(
            &self.db,
            "DELETE FROM telephony_ingress_windows WHERE window_start < $1",
            vec![(now - Duration::hours(1)).into()],
        )
        .await?;
        Ok(row.map(|r| r.hits).unwrap_or(1))
    }

    async fn count_auth_failure(
        &self,
        provider_id: &str,
        rejection: EventRejection,
    ) -> TelephonyResult<()> {
        crate::metrics::inc(
            "telephony_provider_event_auth_failures_total",
            &[("reason", rejection.as_str())],
        );
        let hits = self
            .count_window("CALLBACK_AUTH_FAILURE", provider_id)
            .await?;
        tracing::warn!(
            provider = provider_id,
            reason = rejection.as_str(),
            "provider callback rejected"
        );
        if hits >= self.settings.callback_auth_failure_incident_threshold {
            let mut incident = Incident::new(
                "CALLBACK_AUTH_FAILURES",
                Severity::High,
                format!("callback-auth:{provider_id}"),
            )
            .evidence(json!({ "failuresThisMinute": hits, "reason": rejection.as_str() }));
            incident.provider_id = Some(provider_id.to_string());
            incidents::raise(&self.db, &incident, self.now()).await?;
        }
        Ok(())
    }

    /// Records and applies one normalized provider event.
    pub async fn ingest_provider_event(
        &self,
        provider_id: &str,
        event: ProviderEvent,
    ) -> TelephonyResult<Disposition> {
        let now = self.now();
        let retention = Duration::from_std(self.settings.provider_event_retention)
            .unwrap_or(Duration::days(30));
        // Dedupe records live for the retention window only, so an event older than that
        // window (minus a margin of a tenth of it) could be a replay whose record was
        // already purged: it is ignored and never applied. A provider clock far in the
        // future is refused too (its record would outlive nothing).
        let margin = retention / 10;
        if event.occurred_at < now - retention + margin || event.occurred_at > now + margin {
            tracing::warn!(
                provider = provider_id,
                "provider event outside the accepted time window ignored"
            );
            return Ok(Disposition::Ignored);
        }
        let txn = self.db.begin().await?;
        // Match ONLY through Telephony's own binding.
        let binding: Option<BindingMatch> = match (&event.provider_call_ref, &event.correlation_key)
        {
            (Some(r), _) => {
                let by_ref = db::one(
                    &txn,
                    "SELECT id, telephony_leg_id, provider_call_ref FROM provider_call_bindings \
                     WHERE provider_id = $1 AND provider_call_ref = $2",
                    vec![provider_id.into(), r.clone().into()],
                )
                .await?;
                match (by_ref, &event.correlation_key) {
                    (Some(b), _) => Some(b),
                    (None, Some(k)) => {
                        db::one(
                            &txn,
                            "SELECT id, telephony_leg_id, provider_call_ref FROM provider_call_bindings \
                             WHERE provider_id = $1 AND correlation_key = $2",
                            vec![provider_id.into(), k.clone().into()],
                        )
                        .await?
                    }
                    (None, None) => None,
                }
            }
            (None, Some(k)) => {
                db::one(
                    &txn,
                    "SELECT id, telephony_leg_id, provider_call_ref FROM provider_call_bindings \
                     WHERE provider_id = $1 AND correlation_key = $2",
                    vec![provider_id.into(), k.clone().into()],
                )
                .await?
            }
            (None, None) => None,
        };
        let leg = match &binding {
            Some(b) => Some(legs::lock(&txn, b.telephony_leg_id).await?),
            None => None,
        };
        let disposition = match (&binding, &leg) {
            (Some(b), Some(leg)) => self.apply_to_leg(&txn, b, leg, &event, now).await?,
            _ => Disposition::Quarantined,
        };
        let inserted = db::exec(
            &txn,
            "INSERT INTO provider_events (id, provider_id, provider_event_id, provider_call_ref, telephony_leg_id, \
               normalized_event, disposition, provider_sequence, occurred_at, received_at, payload_digest, \
               restricted_detail, retain_until) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
            vec![
                Uuid::new_v4().into(),
                provider_id.into(),
                event.provider_event_id.clone().into(),
                event.provider_call_ref.clone().into(),
                (if disposition == Disposition::Quarantined { None } else { leg.as_ref().map(|l| l.id) }).into(),
                event.event.as_str().into(),
                disposition.as_str().into(),
                event.sequence.into(),
                event.occurred_at.into(),
                now.into(),
                digest(&event).into(),
                restricted(&event).into(),
                (now + retention).into(),
            ],
        )
        .await;
        if let Err(e) = inserted {
            drop(txn);
            if is_unique_violation(&e, "uq_provider_events_provider_event") {
                // Replay of an already-recorded event: nothing changes.
                crate::metrics::inc(
                    "telephony_provider_events_total",
                    &[("disposition", Disposition::Duplicate.as_str())],
                );
                return Ok(Disposition::Duplicate);
            }
            return Err(e);
        }
        if disposition == Disposition::Quarantined {
            let mut incident = Incident::new(
                "UNKNOWN_PROVIDER_OBJECT",
                Severity::Medium,
                format!(
                    "unknown-object:{provider_id}:{}",
                    event.provider_call_ref.as_deref().unwrap_or("none")
                ),
            )
            .evidence(json!({ "event": event.event.as_str() }));
            incident.provider_id = Some(provider_id.to_string());
            incidents::raise(&txn, &incident, now).await?;
        }
        if disposition == Disposition::LateAfterTerminal {
            if let Some(leg) = &leg {
                incidents::raise(
                    &txn,
                    &Incident::new(
                        "LATE_EVENT_AFTER_TERMINAL",
                        Severity::Medium,
                        format!("late-event:{}", leg.id),
                    )
                    .leg(leg)
                    .evidence(json!({ "event": event.event.as_str() })),
                    now,
                )
                .await?;
                // A provider call that is still alive while the leg is over is an
                // orphan: the reconciler terminates it.
                db::exec(
                    &txn,
                    "UPDATE telephony_legs SET reconcile_after = $2 WHERE id = $1",
                    vec![leg.id.into(), now.into()],
                )
                .await?;
            }
        }
        txn.commit().await?;
        // Bridge bookkeeping after commit (a member leg that ended leaves its bridge).
        if disposition == Disposition::Applied
            && event.event.target_status().is_some_and(|s| s.is_terminal())
        {
            if let Some(leg) = &leg {
                self.bridge_member_left(Some(leg.id), None).await?;
            }
        }
        tracing::info!(
            provider = provider_id,
            event = event.event.as_str(),
            disposition = disposition.as_str(),
            telephony_leg_id = ?leg.as_ref().map(|l| l.id),
            "provider event"
        );
        crate::metrics::inc(
            "telephony_provider_events_total",
            &[("disposition", disposition.as_str())],
        );
        Ok(disposition)
    }

    async fn apply_to_leg<C: ConnectionTrait>(
        &self,
        txn: &C,
        binding: &BindingMatch,
        leg: &LegRow,
        event: &ProviderEvent,
        now: DateTime<Utc>,
    ) -> TelephonyResult<Disposition> {
        // Late binding: the event names the provider object of a SENT binding whose
        // request outcome was lost (UNKNOWN) or not yet recorded.
        if binding.provider_call_ref.is_none() {
            if let Some(r) = &event.provider_call_ref {
                db::exec(
                    txn,
                    "UPDATE provider_call_bindings SET provider_call_ref = $2, status = 'BOUND', \
                       bound_at = COALESCE(bound_at, $3), updated_at = GREATEST(updated_at, $3) \
                     WHERE id = $1 AND provider_call_ref IS NULL AND status IN ('PREPARED', 'SENT')",
                    vec![binding.id.into(), r.clone().into(), now.into()],
                )
                .await?;
                // An UNKNOWN origination is resolved by the provider's own evidence.
                db::exec(
                    txn,
                    "UPDATE telephony_commands SET status = 'SUCCEEDED', result_code = 'RESOLVED_BY_EVENT', \
                       finished_at = GREATEST(created_at, $2), updated_at = GREATEST(updated_at, $2) \
                     WHERE telephony_leg_id = $1 AND kind = 'ORIGINATE' AND status IN ('UNKNOWN_OUTCOME', 'RECONCILING')",
                    vec![leg.id.into(), now.into()],
                )
                .await?;
                incidents::resolve(txn, &format!("unknown-origination:{}", leg.id), now).await?;
            }
        }
        let current = leg.status()?;
        let Some(target) = event.event.target_status() else {
            return Ok(Disposition::Ignored);
        };
        if current.is_terminal() {
            return Ok(if target.is_terminal() {
                Disposition::Stale
            } else {
                Disposition::LateAfterTerminal
            });
        }
        if let (Some(seq), Some(last)) = (event.sequence, leg.last_provider_sequence) {
            if seq <= last {
                return Ok(Disposition::Stale);
            }
        }
        // Never regress: an event less advanced than the current state is stale (e.g. a
        // delayed RINGING after CONNECTED). HELD <-> CONNECTED are explicit moves.
        let lateral = matches!(
            (current, target),
            (LegStatus::Held, LegStatus::Connected) | (LegStatus::Connected, LegStatus::Held)
        );
        if !lateral
            && target != LegStatus::Unknown
            && target.progress() <= current.progress()
            && current != LegStatus::Unknown
        {
            return Ok(Disposition::Stale);
        }
        let change = if target.is_terminal() {
            let reason = event.event.end_reason().unwrap_or(EndReason::Completed);
            Change::ended(
                event.event,
                reason,
                event.occurred_at.min(now).max(leg.created_at),
            )
        } else {
            Change::to(
                target,
                event.event,
                event.occurred_at.min(now).max(leg.created_at),
            )
        }
        .sequence(event.sequence);
        // From UNKNOWN the evidence decides; from ORIGINATING / RINGING jumping to
        // CONNECTED is allowed by the graph (answer evidence implied).
        match legs::transition_in(txn, leg, &change).await? {
            Some(_) => {
                if let Some((cost, currency)) = &event.provider_cost_minor {
                    self.stash_provider_cost(txn, leg.id, *cost, currency)
                        .await?;
                }
                if target.is_terminal() {
                    db::exec(
                        txn,
                        "UPDATE provider_call_bindings SET status = 'RELEASED', released_at = $2, \
                           updated_at = GREATEST(updated_at, $2) WHERE id = $1 AND status IN ('SENT', 'BOUND')",
                        vec![binding.id.into(), now.into()],
                    )
                    .await?;
                }
                Ok(Disposition::Applied)
            }
            None => Ok(Disposition::Stale),
        }
    }

    /// Provider-reported cost of a final event, kept (set once) until usage finalization.
    async fn stash_provider_cost<C: ConnectionTrait>(
        &self,
        txn: &C,
        leg_id: Uuid,
        cost_minor: i64,
        currency: &str,
    ) -> TelephonyResult<()> {
        db::exec(
            txn,
            "UPDATE telephony_legs SET provider_cost_minor = $2, provider_cost_currency = $3 \
             WHERE id = $1 AND provider_cost_minor IS NULL",
            vec![leg_id.into(), cost_minor.into(), currency.into()],
        )
        .await
        .map(|_| ())
    }
}

#[derive(Debug, FromQueryResult)]
struct HitsRow {
    hits: i64,
}
