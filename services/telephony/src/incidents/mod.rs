//! Telephony incidents (Part E §50, §137–§138): conditions Telephony refuses to guess
//! about. Deduplicated while OPEN by a stable key (a repeat bumps occurrences and
//! `last_seen_at`). Evidence is bounded and restricted; it never contains a telephone
//! number. Provider ids may appear only here, as restricted operational evidence.

use chrono::{DateTime, Utc};
use sea_orm::ConnectionTrait;
use serde_json::Value;
use uuid::Uuid;

use crate::{db, error::TelephonyResult};

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
