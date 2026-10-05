//! Operational incidents (Part I §73). `rtc_drift_events` is generalised instead of a
//! second drift store: every reconciler finding is recorded there; ambiguous or
//! destructive drift (ESCALATED / FAILED) is an OPEN incident deduplicated by
//! `dedupe_key` (repeat detections bump `occurrences`), and a later successful repair
//! of the same subject resolves it. Incidents never carry PII.

use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::Value;
use uuid::Uuid;

use crate::error::ApiResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncidentAction {
    /// Safe deterministic drift repaired automatically.
    Repaired,
    /// Ambiguous / destructive drift: recorded, alerted, not guessed.
    Escalated,
    /// Repair attempted and failed (retried later).
    Failed,
}

impl IncidentAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Repaired => "REPAIRED",
            Self::Escalated => "ESCALATED",
            Self::Failed => "FAILED",
        }
    }
}

/// Stable incident kinds surfaced to operations (Part I §73).
pub mod kind {
    pub const RTC_ROOM_MISSING: &str = "RTC_ROOM_MISSING";
    pub const ORPHAN_RTC_ROOM: &str = "ORPHAN_RTC_ROOM";
    pub const ORPHAN_PROVIDER_PARTICIPANT: &str = "ORPHAN_PROVIDER_PARTICIPANT";
    pub const STALE_CONNECTED_SESSION: &str = "STALE_CONNECTED_SESSION";
    pub const BREAKOUT_CONTEXT_DRIFT: &str = "BREAKOUT_CONTEXT_DRIFT";
    pub const RECORDING_PROVIDER_DRIFT: &str = "RECORDING_PROVIDER_DRIFT";
    pub const RECORDING_START_TIMEOUT: &str = "RECORDING_START_TIMEOUT";
    pub const ARTIFACT_MISSING: &str = "ARTIFACT_MISSING";
    pub const ARTIFACT_INTEGRITY_FAILED: &str = "ARTIFACT_INTEGRITY_FAILED";
    pub const ARTIFACT_DELETE_FAILED: &str = "ARTIFACT_DELETE_FAILED";
    pub const TRANSCRIPTION_PROVIDER_DRIFT: &str = "TRANSCRIPTION_PROVIDER_DRIFT";
    pub const BACKSTAGE_CONTEXT_DRIFT: &str = "BACKSTAGE_CONTEXT_DRIFT";
}

#[derive(Debug, Clone)]
pub struct Incident<'a> {
    pub kind: &'a str,
    pub category: &'a str,
    pub conference_id: Option<Uuid>,
    pub room_name: Option<&'a str>,
    /// Dedupe subject inside the conference (participant session, artefact, room).
    pub subject: Option<String>,
    pub action: IncidentAction,
    pub critical: bool,
    pub detail: Value,
}

impl Incident<'_> {
    pub fn dedupe_key(&self) -> String {
        format!(
            "{}:{}:{}",
            self.kind,
            self.conference_id
                .map(|c| c.to_string())
                .unwrap_or_else(|| "-".into()),
            self.subject
                .clone()
                .or_else(|| self.room_name.map(str::to_string))
                .unwrap_or_else(|| "-".into())
        )
    }
}

/// Record a finding. ESCALATED / FAILED → OPEN incident (deduplicated); REPAIRED →
/// history row + resolves an OPEN incident of the same subject.
pub async fn record<C: ConnectionTrait>(conn: &C, i: &Incident<'_>) -> ApiResult<()> {
    let key = i.dedupe_key();
    let severity = if i.critical { "CRITICAL" } else { "WARNING" };
    match i.action {
        IncidentAction::Repaired => {
            conn.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO rtc_drift_events
                    (id, conference_id, room_name, kind, action, detail, status, category, severity, dedupe_key)
                 VALUES ($1,$2,$3,$4,'REPAIRED',$5,'CLOSED',$6,$7,$8)",
                [
                    Uuid::new_v4().into(),
                    i.conference_id.into(),
                    i.room_name.map(str::to_string).into(),
                    i.kind.into(),
                    i.detail.clone().into(),
                    i.category.into(),
                    severity.into(),
                    key.clone().into(),
                ],
            ))
            .await?;
            conn.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE rtc_drift_events SET status = 'RESOLVED', resolved_at = now()
                  WHERE dedupe_key = $1 AND status = 'OPEN'",
                [key.into()],
            ))
            .await?;
        }
        IncidentAction::Escalated | IncidentAction::Failed => {
            conn.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO rtc_drift_events
                    (id, conference_id, room_name, kind, action, detail, status, category, severity, dedupe_key)
                 VALUES ($1,$2,$3,$4,$5,$6,'OPEN',$7,$8,$9)
                 ON CONFLICT (dedupe_key) WHERE status = 'OPEN' AND dedupe_key IS NOT NULL
                 DO UPDATE SET occurrences = rtc_drift_events.occurrences + 1,
                               last_seen_at = now(),
                               action = EXCLUDED.action,
                               detail = EXCLUDED.detail",
                [
                    Uuid::new_v4().into(),
                    i.conference_id.into(),
                    i.room_name.map(str::to_string).into(),
                    i.kind.into(),
                    i.action.as_str().into(),
                    i.detail.clone().into(),
                    i.category.into(),
                    severity.into(),
                    key.into(),
                ],
            ))
            .await?;
        }
    }
    if let Some(cid) = i.conference_id {
        let drift = matches!(i.category, "RTC" | "RECORDING" | "TRANSCRIPTION");
        conn.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE conference_sessions
                SET last_incident_at = now(),
                    provider_drift_count = provider_drift_count + CASE WHEN $2 THEN 1 ELSE 0 END
              WHERE id = $1",
            [cid.into(), drift.into()],
        ))
        .await?;
    }
    Ok(())
}

/// Resolve an OPEN incident explicitly (e.g. drift cleared without a repair action).
pub async fn resolve<C: ConnectionTrait>(conn: &C, dedupe_key: &str) -> ApiResult<u64> {
    let r = conn
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE rtc_drift_events SET status = 'RESOLVED', resolved_at = now()
              WHERE dedupe_key = $1 AND status = 'OPEN'",
            [dedupe_key.into()],
        ))
        .await?;
    Ok(r.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedupe_key_is_stable_and_pii_free() {
        let cid = Uuid::nil();
        let i = Incident {
            kind: kind::ARTIFACT_MISSING,
            category: "ARTIFACT",
            conference_id: Some(cid),
            room_name: None,
            subject: Some("artifact:1".into()),
            action: IncidentAction::Escalated,
            critical: true,
            detail: Value::Null,
        };
        assert_eq!(i.dedupe_key(), format!("ARTIFACT_MISSING:{cid}:artifact:1"));
    }
}
