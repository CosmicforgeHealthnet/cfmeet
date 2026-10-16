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
//!
//! Part H §55, §59–§60: every provider-supplied identifier is validated against the
//! database rule BEFORE any transaction (an over-long id is a 400, never a rolled-back
//! 500 that makes the provider retry forever), and invalid callbacks are counted in
//! bounded per-minute windows whose subject is a known provider id or the literal
//! `unknown` — never the attacker-supplied path segment. Past
//! `callback_invalid_incident_threshold` hits per minute they raise ONE deduplicated
//! incident (`CALLBACK_MALFORMED`, `CALLBACK_UNKNOWN_PROVIDER`, `CALLBACK_REPLAY_ABUSE`);
//! a single duplicate is normal provider behaviour and is never an incident.

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
    providers::{valid_provider_token, EventRejection, ProviderEvent, ProviderIngress},
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

/// Why a callback was refused before application (bounded; metric label).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidCallback {
    /// Unknown provider id, or a path segment that is not a provider id at all.
    UnknownProvider,
    /// Authenticated but not strictly well-formed (T1 validation included).
    Malformed,
    /// Larger than `MAX_CALLBACK_BYTES` (refused by the listener or here).
    Oversized,
    /// A duplicate event id or an event outside the accepted time window.
    Replay,
}

impl InvalidCallback {
    pub fn as_str(&self) -> &'static str {
        match self {
            InvalidCallback::UnknownProvider => "UNKNOWN_PROVIDER",
            InvalidCallback::Malformed => "MALFORMED",
            InvalidCallback::Oversized => "OVERSIZED",
            InvalidCallback::Replay => "REPLAY",
        }
    }
}

/// `telecom_providers.id` syntax (the only path segment ever used as a window subject).
pub fn valid_provider_id(id: &str) -> bool {
    (2..=32).contains(&id.len())
        && id.as_bytes()[0].is_ascii_lowercase()
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A bounded, constraint-safe incident key for an unknown provider object: the raw
/// reference is used while it fits `dedupe_key`, else its digest.
fn unknown_object_key(provider_id: &str, call_ref: Option<&str>) -> String {
    let r = call_ref.unwrap_or("none");
    let key = format!("unknown-object:{provider_id}:{r}");
    if key.len() <= 160 {
        key
    } else {
        format!(
            "unknown-object:{provider_id}:sha256-{}",
            &hex::encode(Sha256::digest(r.as_bytes()))[..32]
        )
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
        if !valid_provider_id(&ingress.provider_id) {
            self.count_invalid_callback(InvalidCallback::UnknownProvider, None)
                .await?;
            return Err(TelephonyError::Refused(ErrorCode::ProviderEventRejected));
        }
        let Some(provider) = self.providers.get(&ingress.provider_id).cloned() else {
            self.count_invalid_callback(InvalidCallback::UnknownProvider, None)
                .await?;
            return Err(TelephonyError::Refused(ErrorCode::ProviderEventRejected));
        };
        if ingress.body.len() > MAX_CALLBACK_BYTES {
            self.count_invalid_callback(InvalidCallback::Oversized, Some(&ingress.provider_id))
                .await?;
            return Err(TelephonyError::Refused(ErrorCode::PayloadTooLarge));
        }
        if let Err(rejection) = provider.verify_event(&ingress) {
            self.count_auth_failure(&ingress.provider_id, rejection)
                .await?;
            return Err(TelephonyError::Refused(ErrorCode::ProviderEventRejected));
        }
        let events = match provider.normalize_event(&ingress) {
            Ok(events) => events,
            Err(_) => {
                self.count_invalid_callback(InvalidCallback::Malformed, Some(&ingress.provider_id))
                    .await?;
                return Err(TelephonyError::Validation(
                    "The provider event is malformed.",
                ));
            }
        };
        // Defence in depth: an adapter that let an invalid id through is still refused
        // before any transaction.
        if events.iter().any(|e| !valid_event_ids(e)) {
            self.count_invalid_callback(InvalidCallback::Malformed, Some(&ingress.provider_id))
                .await?;
            return Err(TelephonyError::Validation(
                "The provider event is malformed.",
            ));
        }
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

    /// Counts a callback the listener refused as too large before the handler ran
    /// (the body limit layer); the subject is the provider id when it is one.
    pub async fn count_oversized_callback(&self, provider_id: Option<&str>) -> TelephonyResult<()> {
        let known = provider_id.filter(|p| valid_provider_id(p) && self.providers.get(p).is_some());
        self.count_invalid_callback(InvalidCallback::Oversized, known)
            .await
    }

    /// Counts one invalid callback (bounded metric + per-minute window) and raises the
    /// aggregated incident once the window crosses the threshold.
    async fn count_invalid_callback(
        &self,
        kind: InvalidCallback,
        provider_id: Option<&str>,
    ) -> TelephonyResult<()> {
        crate::metrics::inc(
            "telephony_provider_callback_rejections_total",
            &[("reason", kind.as_str())],
        );
        let subject = provider_id.unwrap_or("unknown");
        let (window, category, key, severity) = match kind {
            InvalidCallback::UnknownProvider => (
                "CALLBACK_UNKNOWN_PROVIDER",
                "CALLBACK_UNKNOWN_PROVIDER",
                "callback-unknown-provider".to_string(),
                Severity::Medium,
            ),
            InvalidCallback::Malformed | InvalidCallback::Oversized => (
                "CALLBACK_MALFORMED",
                "CALLBACK_MALFORMED",
                format!("callback-malformed:{subject}"),
                Severity::Medium,
            ),
            InvalidCallback::Replay => (
                "CALLBACK_REPLAY",
                "CALLBACK_REPLAY_ABUSE",
                format!("callback-replay:{subject}"),
                Severity::High,
            ),
        };
        let hits = self.count_window(window, subject).await?;
        tracing::warn!(
            provider = subject,
            reason = kind.as_str(),
            "provider callback refused"
        );
        if hits >= self.settings.callback_invalid_incident_threshold {
            let mut incident = Incident::new(category, severity, key)
                .evidence(json!({ "hitsThisMinute": hits, "reason": kind.as_str() }));
            incident.provider_id = provider_id.map(str::to_string);
            incidents::raise(&self.db, &incident, self.now()).await?;
        }
        Ok(())
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
        if !valid_event_ids(&event) {
            return Err(TelephonyError::Validation(
                "The provider event is malformed.",
            ));
        }
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
            self.count_invalid_callback(InvalidCallback::Replay, Some(provider_id))
                .await?;
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
                self.count_invalid_callback(InvalidCallback::Replay, Some(provider_id))
                    .await?;
                return Ok(Disposition::Duplicate);
            }
            return Err(e);
        }
        if disposition == Disposition::Quarantined {
            let mut incident = Incident::new(
                "UNKNOWN_PROVIDER_OBJECT",
                Severity::Medium,
                unknown_object_key(provider_id, event.provider_call_ref.as_deref()),
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

/// Every provider-supplied identifier of `event` satisfies the database rule
/// (`^[A-Za-z0-9][A-Za-z0-9._:@/-]{0,159}$`), the sequence is bounded and a cost is a
/// non-negative amount with an ISO-4217-shaped currency.
pub fn valid_event_ids(event: &ProviderEvent) -> bool {
    valid_provider_token(&event.provider_event_id)
        && event
            .provider_call_ref
            .as_deref()
            .is_none_or(valid_provider_token)
        && event
            .correlation_key
            .as_deref()
            .is_none_or(valid_provider_token)
        && event
            .sequence
            .is_none_or(|s| (0..=i64::from(i32::MAX)).contains(&s))
        && event
            .provider_cost_minor
            .as_ref()
            .is_none_or(|(cost, currency)| {
                (0..=MAX_PROVIDER_COST_MINOR).contains(cost)
                    && currency.len() == 3
                    && currency.bytes().all(|b| b.is_ascii_uppercase())
            })
}

/// Upper bound of a provider-reported cost of one leg (minor units).
pub const MAX_PROVIDER_COST_MINOR: i64 = 1_000_000_000_000;

#[cfg(test)]
mod tests {
    use super::*;

    fn event(id: &str) -> ProviderEvent {
        ProviderEvent {
            provider_event_id: id.into(),
            provider_call_ref: None,
            correlation_key: None,
            event: crate::domain::NormalizedEvent::LegRinging,
            occurred_at: Utc::now(),
            sequence: Some(1),
            restricted_detail: Default::default(),
            inbound: None,
            dtmf: None,
            provider_cost_minor: None,
        }
    }

    #[test]
    fn provider_identifiers_follow_the_database_rule_before_any_transaction() {
        assert!(valid_event_ids(&event("ev-1")));
        assert!(valid_event_ids(&event(&"a".repeat(160))));
        assert!(!valid_event_ids(&event(&"a".repeat(161))));
        assert!(!valid_event_ids(&event("")));
        assert!(!valid_event_ids(&event("-leading")));
        assert!(!valid_event_ids(&event("has space")));
        let mut e = event("ev-2");
        e.provider_call_ref = Some("x".repeat(161));
        assert!(!valid_event_ids(&e));
        let mut e = event("ev-3");
        e.sequence = Some(i64::from(i32::MAX) + 1);
        assert!(!valid_event_ids(&e));
        e.sequence = Some(-1);
        assert!(!valid_event_ids(&e));
        let mut e = event("ev-4");
        e.provider_cost_minor = Some((5, "ngn".into()));
        assert!(!valid_event_ids(&e));
        e.provider_cost_minor = Some((MAX_PROVIDER_COST_MINOR + 1, "NGN".into()));
        assert!(!valid_event_ids(&e));
        e.provider_cost_minor = Some((250, "NGN".into()));
        assert!(valid_event_ids(&e));
        assert!(valid_provider_id("dp-a") && !valid_provider_id("DP-A") && !valid_provider_id("a"));
        assert!(unknown_object_key("dp-a", Some(&"r".repeat(200))).len() <= 160);
        assert_eq!(unknown_object_key("dp-a", None), "unknown-object:dp-a:none");
    }
}
