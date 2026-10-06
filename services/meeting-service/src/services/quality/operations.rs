//! OperationsService — internal operations views (Part I §71–§73).
//!
//! Served only on internal routes protected by `INTERNAL_API_TOKEN`; never part of the
//! public API and never visible to conference hosts. Responses contain identifiers,
//! states, counts and sanitized provider region / node labels — no names, emails,
//! phone numbers or clinical data.
//!
//! Health is derived from PostgreSQL aggregates (join counters, session quality
//! aggregates, open incidents, recording / transcription failures, room readiness);
//! high-frequency raw telemetry lives in the metrics sink, not here.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use super::{
    health::{classify_conference, join_failure_rate, ConferenceSignals, HealthReason},
    sink::HealthState,
};
use crate::{
    error::{ApiError, ApiResult, ErrorCode},
    services::meeting_core::Core,
};

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConferenceHealthSummary {
    pub conference_id: Uuid,
    pub meeting_space_id: Uuid,
    pub lifecycle_state: String,
    pub health_state: String,
    pub reasons: Vec<HealthReason>,
    pub active_participants: i64,
    pub healthy_sessions: i64,
    pub degraded_sessions: i64,
    pub unhealthy_sessions: i64,
    pub join_attempts: i64,
    pub join_successes: i64,
    pub join_media_connected: i64,
    pub join_denials: i64,
    pub join_provider_failures: i64,
    pub join_timeouts: i64,
    /// Infrastructure join failures / (credentials issued + provider failures).
    pub join_failure_rate: Option<f64>,
    pub avg_join_latency_ms: Option<f64>,
    pub max_join_latency_ms: Option<f64>,
    pub reconnects: i64,
    pub publish_failures: i64,
    pub subscribe_failures: i64,
    pub avg_rtt_ms: Option<f64>,
    pub avg_jitter_ms: Option<f64>,
    pub avg_packet_loss_ratio: Option<f64>,
    /// Sanitized provider regions / nodes seen by this conference's sessions.
    pub provider_regions: Vec<String>,
    pub provider_nodes: Vec<String>,
    pub provider_drift_count: i64,
    pub open_incidents: i64,
    pub recording_status: String,
    pub recording_failures: i64,
    pub transcription_status: String,
    pub transcription_failures: i64,
    pub last_incident_at: Option<DateTime<Utc>>,
    pub evaluated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct IncidentView {
    pub id: Uuid,
    pub conference_id: Option<Uuid>,
    pub kind: String,
    pub category: String,
    pub severity: String,
    pub status: String,
    pub action: String,
    pub occurrences: i32,
    #[schema(value_type = Object)]
    pub detail: Value,
    pub detected_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

pub struct OperationsService {
    core: Core,
}

fn f(r: &sea_orm::QueryResult, col: &str) -> Option<f64> {
    r.try_get::<Option<f64>>("", col).ok().flatten()
}

fn i(r: &sea_orm::QueryResult, col: &str) -> i64 {
    r.try_get::<Option<i64>>("", col)
        .ok()
        .flatten()
        .unwrap_or(0)
}

impl OperationsService {
    pub fn new(core: Core) -> Self {
        Self { core }
    }

    /// Evaluate (and persist) one conference's health.
    pub async fn evaluate(&self, conference_id: Uuid) -> ApiResult<ConferenceHealthSummary> {
        let db = &self.core.db;
        let c = db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT id, meeting_space_id, status, rtc_room_ready_at IS NOT NULL AS room_ready,
                        join_attempts, join_successes, join_media_connected, join_denials,
                        join_provider_failures, join_timeouts, reconnect_count AS reconnects,
                        publish_failure_count AS publish_failures,
                        subscribe_failure_count AS subscribe_failures,
                        provider_drift_count, recording_failure_count AS recording_failures,
                        transcription_failure_count AS transcription_failures,
                        recording_state, transcription_state, last_incident_at
                   FROM conference_sessions WHERE id = $1",
                [conference_id.into()],
            ))
            .await?
            .ok_or_else(|| {
                ApiError::domain(ErrorCode::ConferenceNotFound, "Conference not found")
            })?;
        let sessions = db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT count(*) FILTER (WHERE state IN ('PENDING','CONNECTED')) AS active,
                        count(*) FILTER (WHERE state IN ('PENDING','CONNECTED') AND quality_state = 'HEALTHY') AS healthy,
                        count(*) FILTER (WHERE state IN ('PENDING','CONNECTED') AND quality_state = 'DEGRADED') AS degraded,
                        count(*) FILTER (WHERE state IN ('PENDING','CONNECTED') AND quality_state = 'UNHEALTHY') AS unhealthy,
                        avg(join_latency_ms)::float8 AS avg_join,
                        max(join_latency_ms)::float8 AS max_join,
                        avg(avg_rtt_ms) FILTER (WHERE state IN ('PENDING','CONNECTED'))::float8 AS rtt,
                        avg(avg_jitter_ms) FILTER (WHERE state IN ('PENDING','CONNECTED'))::float8 AS jitter,
                        (avg(packet_loss_pct) FILTER (WHERE state IN ('PENDING','CONNECTED')) / 100.0)::float8 AS loss,
                        array_remove(array_agg(DISTINCT provider_region), NULL) AS regions,
                        array_remove(array_agg(DISTINCT provider_node), NULL) AS nodes
                   FROM participant_sessions WHERE conference_id = $1",
                [conference_id.into()],
            ))
            .await?
            .ok_or_else(|| ApiError::InternalError("session aggregate".into()))?;
        let incidents = db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT count(*) FILTER (WHERE status = 'OPEN') AS open,
                        count(*) FILTER (WHERE status = 'OPEN' AND severity = 'CRITICAL') AS critical
                   FROM rtc_drift_events WHERE conference_id = $1",
                [conference_id.into()],
            ))
            .await?
            .ok_or_else(|| ApiError::InternalError("incident aggregate".into()))?;
        let status: String = c.try_get("", "status").unwrap_or_default();
        let signals = ConferenceSignals {
            live: status == "LIVE",
            room_ready: c.try_get::<bool>("", "room_ready").unwrap_or(false),
            credentials_issued: i(&c, "join_successes"),
            provider_failures: i(&c, "join_provider_failures"),
            timeouts: i(&c, "join_timeouts"),
            active_sessions: i(&sessions, "active"),
            unhealthy_sessions: i(&sessions, "unhealthy"),
            degraded_sessions: i(&sessions, "degraded"),
            open_critical_incidents: i(&incidents, "critical"),
            open_incidents: i(&incidents, "open"),
            recording_failures: i(&c, "recording_failures"),
            transcription_failures: i(&c, "transcription_failures"),
            avg_join_latency_ms: f(&sessions, "avg_join"),
        };
        let (state, reasons) = classify_conference(&signals, &self.core.config.quality);
        let now = Utc::now();
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE conference_sessions SET health_state = $2, unhealthy_participant_count = $3,
                    health_evaluated_at = now() WHERE id = $1",
            [
                conference_id.into(),
                state.as_str().into(),
                (signals.unhealthy_sessions as i32).into(),
            ],
        ))
        .await?;
        let _ = self.core.telemetry.record_conference_health(state);
        let labels = |col: &str| -> Vec<String> {
            sessions
                .try_get::<Vec<String>>("", col)
                .unwrap_or_default()
                .into_iter()
                .take(20)
                .collect()
        };
        Ok(ConferenceHealthSummary {
            conference_id,
            meeting_space_id: c.try_get("", "meeting_space_id").unwrap_or_default(),
            lifecycle_state: status,
            health_state: state.as_str().to_string(),
            reasons,
            active_participants: signals.active_sessions,
            healthy_sessions: i(&sessions, "healthy"),
            degraded_sessions: signals.degraded_sessions,
            unhealthy_sessions: signals.unhealthy_sessions,
            join_attempts: i(&c, "join_attempts"),
            join_successes: signals.credentials_issued,
            join_media_connected: i(&c, "join_media_connected"),
            join_denials: i(&c, "join_denials"),
            join_provider_failures: signals.provider_failures,
            join_timeouts: signals.timeouts,
            join_failure_rate: join_failure_rate(&signals),
            avg_join_latency_ms: signals.avg_join_latency_ms,
            max_join_latency_ms: f(&sessions, "max_join"),
            reconnects: i(&c, "reconnects"),
            publish_failures: i(&c, "publish_failures"),
            subscribe_failures: i(&c, "subscribe_failures"),
            avg_rtt_ms: f(&sessions, "rtt"),
            avg_jitter_ms: f(&sessions, "jitter"),
            avg_packet_loss_ratio: f(&sessions, "loss"),
            provider_regions: labels("regions"),
            provider_nodes: labels("nodes"),
            provider_drift_count: i(&c, "provider_drift_count"),
            open_incidents: signals.open_incidents,
            recording_status: c.try_get("", "recording_state").unwrap_or_default(),
            recording_failures: signals.recording_failures,
            transcription_status: c.try_get("", "transcription_state").unwrap_or_default(),
            transcription_failures: signals.transcription_failures,
            last_incident_at: c.try_get("", "last_incident_at").ok().flatten(),
            evaluated_at: now,
        })
    }

    /// Active (LIVE / ENDING) conferences, evaluated, filtered by health.
    pub async fn conferences(
        &self,
        state: Option<HealthState>,
        limit: u64,
    ) -> ApiResult<Vec<ConferenceHealthSummary>> {
        let rows = self
            .core
            .db
            .query_all(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT id FROM conference_sessions WHERE status IN ('LIVE','ENDING')
                  ORDER BY started_at DESC NULLS LAST LIMIT $1",
                [(limit.clamp(1, 500) as i64).into()],
            ))
            .await?;
        let mut out = vec![];
        for r in rows {
            let Ok(id) = r.try_get_by_index::<Uuid>(0) else {
                continue;
            };
            let s = self.evaluate(id).await?;
            let keep = match state {
                None => true,
                Some(HealthState::Degraded) => {
                    matches!(s.health_state.as_str(), "DEGRADED" | "UNHEALTHY")
                }
                Some(st) => s.health_state == st.as_str(),
            };
            if keep {
                out.push(s);
            }
        }
        Ok(out)
    }

    pub async fn incidents(
        &self,
        status: Option<&str>,
        limit: u64,
    ) -> ApiResult<Vec<IncidentView>> {
        let status = match status {
            Some(s @ ("OPEN" | "RESOLVED" | "CLOSED")) => Some(s.to_string()),
            Some(_) => {
                return Err(ApiError::domain(
                    ErrorCode::ValidationError,
                    "status must be OPEN, RESOLVED or CLOSED",
                ))
            }
            None => None,
        };
        let rows = self
            .core
            .db
            .query_all(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT id, conference_id, kind, category, severity, status, action, occurrences,
                        detail, detected_at, last_seen_at, resolved_at
                   FROM rtc_drift_events
                  WHERE ($1::text IS NULL OR status = $1)
                  ORDER BY last_seen_at DESC LIMIT $2",
                [status.into(), (limit.clamp(1, 1000) as i64).into()],
            ))
            .await?;
        Ok(rows
            .iter()
            .filter_map(|r| {
                Some(IncidentView {
                    id: r.try_get("", "id").ok()?,
                    conference_id: r.try_get("", "conference_id").ok().flatten(),
                    kind: r.try_get("", "kind").ok()?,
                    category: r.try_get("", "category").ok()?,
                    severity: r.try_get("", "severity").ok()?,
                    status: r.try_get("", "status").ok()?,
                    action: r.try_get("", "action").ok()?,
                    occurrences: r.try_get("", "occurrences").ok()?,
                    detail: r.try_get("", "detail").ok()?,
                    detected_at: r.try_get("", "detected_at").ok()?,
                    last_seen_at: r.try_get("", "last_seen_at").ok()?,
                    resolved_at: r.try_get("", "resolved_at").ok().flatten(),
                })
            })
            .collect())
    }

    /// Reconciler hook: credentialed sessions that never connected within the join
    /// timeout count once as a join timeout (and stay PENDING for the presence rules).
    pub async fn record_join_timeouts<C: ConnectionTrait>(conn: &C, core: &Core) -> ApiResult<u64> {
        let rows = conn
            .query_all(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE participant_sessions SET join_outcome = 'TIMEOUT'
                  WHERE state = 'PENDING' AND join_outcome IS NULL AND connected_at IS NULL
                    AND established_at < now() - make_interval(secs => $1)
                  RETURNING conference_id",
                [(core.config.quality.join_timeout_secs as f64).into()],
            ))
            .await?;
        for r in &rows {
            if let Ok(cid) = r.try_get_by_index::<Uuid>(0) {
                crate::services::artifacts::bump(
                    conn,
                    cid,
                    crate::services::artifacts::ConferenceCounter::JoinTimeouts,
                    1,
                )
                .await?;
                let _ = core
                    .telemetry
                    .record_join(super::sink::JoinOutcome::Timeout, None);
            }
        }
        Ok(rows.len() as u64)
    }
}
