//! Telephony incidents (Part E §50, §137–§138): conditions Telephony refuses to guess
//! about. Deduplicated while OPEN by a stable key (a repeat bumps occurrences and
//! `last_seen_at`). Evidence is bounded and restricted; it never contains a telephone
//! number. Provider ids may appear only here, as restricted operational evidence.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};

use chrono::{DateTime, Utc};
use sea_orm::ConnectionTrait;
use serde_json::Value;
use uuid::Uuid;

use crate::{db, domain::Health, error::TelephonyResult, service::TelephonyService};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

impl Severity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Severity::Low => "LOW",
            Severity::Medium => "MEDIUM",
            Severity::High => "HIGH",
            Severity::Critical => "CRITICAL",
        }
    }
}

/// A new (or repeated) incident observation.
#[derive(Debug, Clone)]
pub struct Incident {
    pub dedupe_key: String,
    pub severity: Severity,
    pub category: &'static str,
    pub provider_id: Option<String>,
    pub route_id: Option<String>,
    pub calling_call_id: Option<Uuid>,
    pub calling_call_leg_id: Option<Uuid>,
    pub telephony_leg_id: Option<Uuid>,
    pub current_state: Option<String>,
    pub evidence: Value,
}

impl Incident {
    pub fn new(category: &'static str, severity: Severity, dedupe_key: String) -> Self {
        Self {
            dedupe_key,
            severity,
            category,
            provider_id: None,
            route_id: None,
            calling_call_id: None,
            calling_call_leg_id: None,
            telephony_leg_id: None,
            current_state: None,
            evidence: Value::Object(Default::default()),
        }
    }

    pub fn leg(mut self, leg: &crate::legs::LegRow) -> Self {
        self.calling_call_id = Some(leg.calling_call_id);
        self.calling_call_leg_id = Some(leg.calling_call_leg_id);
        self.telephony_leg_id = Some(leg.id);
        self.provider_id = leg.provider_id.clone();
        self.route_id = leg.route_id.clone();
        self.current_state = Some(leg.status.clone());
        self
    }

    pub fn evidence(mut self, evidence: Value) -> Self {
        self.evidence = evidence;
        self
    }
}

/// Opens an incident or records another occurrence of the open one with the same key.
/// Returns the incident id.
pub async fn raise<C: ConnectionTrait>(
    db_: &C,
    incident: &Incident,
    at: DateTime<Utc>,
) -> TelephonyResult<Uuid> {
    let evidence = if incident.evidence.to_string().len() > 3500 {
        serde_json::json!({ "truncated": true })
    } else {
        incident.evidence.clone()
    };
    let row: Option<IdRow> = db::one(
        db_,
        "INSERT INTO telephony_incidents (id, dedupe_key, severity, category, status, provider_id, route_id, \
           calling_call_id, calling_call_leg_id, telephony_leg_id, current_state, restricted_evidence, \
           first_seen_at, last_seen_at) \
         VALUES ($1, $2, $3, $4, 'OPEN', $5, $6, $7, $8, $9, $10, $11, $12, $12) \
         ON CONFLICT (dedupe_key) WHERE status = 'OPEN' DO UPDATE SET \
           occurrences = telephony_incidents.occurrences + 1, \
           last_seen_at = GREATEST(telephony_incidents.last_seen_at, EXCLUDED.last_seen_at), \
           current_state = COALESCE(EXCLUDED.current_state, telephony_incidents.current_state), \
           restricted_evidence = EXCLUDED.restricted_evidence \
         RETURNING id",
        vec![
            Uuid::new_v4().into(),
            incident.dedupe_key.clone().into(),
            incident.severity.as_str().into(),
            incident.category.into(),
            incident.provider_id.clone().into(),
            incident.route_id.clone().into(),
            incident.calling_call_id.into(),
            incident.calling_call_leg_id.into(),
            incident.telephony_leg_id.into(),
            incident.current_state.clone().into(),
            evidence.into(),
            at.into(),
        ],
    )
    .await?;
    tracing::warn!(
        category = incident.category,
        severity = incident.severity.as_str(),
        telephony_leg_id = ?incident.telephony_leg_id,
        "telephony incident raised"
    );
    Ok(row.map(|r| r.id).unwrap_or_default())
}

/// Escalates an open incident (another failed resolution attempt).
pub async fn escalate<C: ConnectionTrait>(
    db_: &C,
    dedupe_key: &str,
    at: DateTime<Utc>,
) -> TelephonyResult<()> {
    db::exec(
        db_,
        "UPDATE telephony_incidents SET escalation_count = escalation_count + 1, last_seen_at = GREATEST(last_seen_at, $2), \
           severity = CASE WHEN escalation_count >= 2 THEN 'CRITICAL' ELSE severity END \
         WHERE dedupe_key = $1 AND status = 'OPEN'",
        vec![dedupe_key.into(), at.into()],
    )
    .await
    .map(|_| ())
}

/// Resolves the open incident with this key (its condition no longer holds).
pub async fn resolve<C: ConnectionTrait>(
    db_: &C,
    dedupe_key: &str,
    at: DateTime<Utc>,
) -> TelephonyResult<()> {
    db::exec(
        db_,
        "UPDATE telephony_incidents SET status = 'RESOLVED', resolved_at = GREATEST($2, last_seen_at) \
         WHERE dedupe_key = $1 AND status = 'OPEN'",
        vec![dedupe_key.into(), at.into()],
    )
    .await
    .map(|_| ())
}

#[derive(Debug, sea_orm::FromQueryResult)]
struct IdRow {
    id: Uuid,
}

/// Dedupe key of the Privacy Relay unavailability incident (one per deployment).
pub const PRIVACY_RELAY_UNAVAILABLE_KEY: &str = "privacy-relay-unavailable";
/// Dedupe key of the (single) open Numbering unavailability incident (Part IV L).
pub const NUMBERING_UNAVAILABLE_KEY: &str = "numbering-unavailable";
/// Dedupe key of the SIP / media edge health incident.
pub const SIP_EDGE_HEALTH_KEY: &str = "sip-edge-health";

/// Dependency health one instance observed.
#[derive(Debug)]
pub struct DependencyState {
    /// Whether an open Privacy Relay incident may need resolving (starts `true`: another
    /// instance or an earlier run may have opened one).
    privacy_relay_maybe_open: AtomicBool,
    /// Whether an open Numbering incident may need resolving (Part IV L).
    numbering_maybe_open: AtomicBool,
    /// The last SIP / media edge health observed (`None`: no edge, or not yet).
    sip_edge_health: Mutex<Option<Health>>,
}

impl Default for DependencyState {
    fn default() -> Self {
        Self {
            privacy_relay_maybe_open: AtomicBool::new(true),
            numbering_maybe_open: AtomicBool::new(true),
            sip_edge_health: Mutex::new(None),
        }
    }
}

impl DependencyState {
    /// The last observed SIP / media edge health (metrics).
    pub fn sip_edge_health(&self) -> Option<Health> {
        *self
            .sip_edge_health
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }
}

impl TelephonyService {
    /// Privacy Relay could not be reached for `operation` (protected-contact resolution
    /// for an execution, inbound mapping resolution): bounded metric and ONE open
    /// `PRIVACY_RELAY_UNAVAILABLE` incident (repeats bump its occurrences). Telephony
    /// fails closed meanwhile — nothing is dialled or offered.
    pub(crate) async fn privacy_relay_unavailable(
        &self,
        operation: &'static str,
    ) -> TelephonyResult<()> {
        crate::metrics::inc(
            "telephony_dependency_failures_total",
            &[("dependency", "PRIVACY_RELAY"), ("operation", operation)],
        );
        self.dependencies
            .privacy_relay_maybe_open
            .store(true, Ordering::SeqCst);
        raise(
            &self.db,
            &Incident::new(
                "PRIVACY_RELAY_UNAVAILABLE",
                Severity::High,
                PRIVACY_RELAY_UNAVAILABLE_KEY.to_string(),
            )
            .evidence(serde_json::json!({ "operation": operation })),
            self.now(),
        )
        .await
        .map(|_| ())
    }

    /// Privacy Relay answered again: its unavailability incident (if open) resolves.
    pub(crate) async fn privacy_relay_available(&self) -> TelephonyResult<()> {
        if self
            .dependencies
            .privacy_relay_maybe_open
            .swap(false, Ordering::SeqCst)
        {
            resolve(&self.db, PRIVACY_RELAY_UNAVAILABLE_KEY, self.now()).await?;
        }
        Ok(())
    }

    /// Numbering could not be reached for a DCN operation (fails closed): ONE
    /// `NUMBERING_UNAVAILABLE` incident. Relay-number and enterprise calls are unaffected.
    pub(crate) async fn numbering_unavailable(&self) -> TelephonyResult<()> {
        crate::metrics::inc(
            "telephony_dependency_failures_total",
            &[("dependency", "NUMBERING"), ("operation", "DCN")],
        );
        self.dependencies
            .numbering_maybe_open
            .store(true, Ordering::SeqCst);
        raise(
            &self.db,
            &Incident::new(
                "NUMBERING_UNAVAILABLE",
                Severity::High,
                NUMBERING_UNAVAILABLE_KEY.to_string(),
            ),
            self.now(),
        )
        .await
        .map(|_| ())
    }

    /// Numbering answered again: its unavailability incident (if open) resolves.
    pub(crate) async fn numbering_available(&self) -> TelephonyResult<()> {
        if self
            .dependencies
            .numbering_maybe_open
            .swap(false, Ordering::SeqCst)
        {
            resolve(&self.db, NUMBERING_UNAVAILABLE_KEY, self.now()).await?;
        }
        Ok(())
    }

    /// Records the SIP / media edge's health (provider health refresh): a degraded or
    /// unavailable edge opens ONE `SIP_EDGE_UNAVAILABLE` incident (public SIP routes are
    /// then unavailable for routing; enterprise SIP commands retry and dead-letter); a
    /// healthy edge resolves it. Without a configured edge nothing is observed.
    pub async fn observe_sip_edge_health(&self) -> TelephonyResult<Option<Health>> {
        if self.edge.name() == "none" {
            return Ok(None);
        }
        let health = self.edge.health().await;
        *self
            .dependencies
            .sip_edge_health
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(health);
        let now = self.now();
        match health {
            Health::Healthy => resolve(&self.db, SIP_EDGE_HEALTH_KEY, now).await?,
            Health::Degraded | Health::Unavailable => {
                let severity = if health == Health::Unavailable {
                    Severity::High
                } else {
                    Severity::Medium
                };
                raise(
                    &self.db,
                    &Incident::new(
                        "SIP_EDGE_UNAVAILABLE",
                        severity,
                        SIP_EDGE_HEALTH_KEY.to_string(),
                    )
                    .evidence(serde_json::json!({ "health": health.as_str() })),
                    now,
                )
                .await?;
                crate::metrics::inc(
                    "telephony_sip_edge_unhealthy_total",
                    &[("state", health.as_str())],
                );
            }
            Health::Unknown => {}
        }
        Ok(Some(health))
    }
}
