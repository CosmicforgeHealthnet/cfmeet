//! Part VI §55 usage producer: the Meeting Service's facts for the Developer Platform's
//! unified usage ledger (exported by the shared `cf_usage_facts` exporter), for meetings
//! owned by a Platform Access application environment only (CF Meet consumer meetings
//! are not platform usage).
//!
//! * [`MeetingFacts`] (event stream): MEETING_PARTICIPANT_MINUTE — the connected duration
//!   of every closed participant session; RECORDING_MINUTE — the duration of every READY
//!   recording.
//! * [`ArtifactStorage`] (interval stream): STORAGE_BYTE_TIME of READY artefacts with a
//!   stored size, until deletion.
//!
//! Only safe refs (`participant_session:<id>`, `meeting_artifact:<id>`) and conference ids
//! leave the service: never a display name, media identity, storage key or content. Off
//! unless `CFMEET_USAGE_EXPORT_ENABLED=true` (and the platform integration is configured).

use std::collections::BTreeMap;

use async_trait::async_trait;
use cf_usage_facts::{
    exporter::{ActiveInterval, EventSource, IntervalSource, Position, Total},
    UsageFact,
};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use serde_json::json;
use uuid::Uuid;

fn err(e: sea_orm::DbErr) -> String {
    format!("usage source unavailable: {e}")
        .chars()
        .take(200)
        .collect()
}

/// Facts of environment-owned meetings: `(position, id, kind, conference, scope, quantity)`.
const FACTS: &str = "\
SELECT 'PARTICIPANT' AS kind, ps.id, ps.disconnected_at AS at, ps.conference_id AS conference, \
       s.platform_tenant_id AS tenant_id, s.platform_application_id AS application_id, \
       s.platform_environment_id AS environment_id, \
       (EXTRACT(EPOCH FROM (ps.disconnected_at - ps.connected_at)) * 1000)::bigint AS quantity, \
       cp.identity_type AS detail \
  FROM participant_sessions ps \
  JOIN conference_participants cp ON cp.id = ps.conference_participant_id \
  JOIN conference_sessions c ON c.id = ps.conference_id \
  JOIN meeting_spaces s ON s.id = c.meeting_space_id \
 WHERE s.platform_environment_id IS NOT NULL AND ps.connected_at IS NOT NULL \
   AND ps.disconnected_at IS NOT NULL AND ps.disconnected_at >= ps.connected_at \
UNION ALL \
SELECT 'RECORDING', a.id, COALESCE(a.finalized_at, a.available_at) AS at, a.conference_id, \
       s.platform_tenant_id, s.platform_application_id, s.platform_environment_id, \
       a.duration_ms AS quantity, NULL \
  FROM meeting_artifacts a JOIN meeting_spaces s ON s.id = a.meeting_space_id \
 WHERE s.platform_environment_id IS NOT NULL AND a.artifact_type = 'RECORDING' \
   AND a.status = 'READY' AND a.duration_ms IS NOT NULL \
   AND COALESCE(a.finalized_at, a.available_at) IS NOT NULL";

pub struct MeetingFacts {
    db: DatabaseConnection,
}

impl MeetingFacts {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

#[async_trait]
impl EventSource for MeetingFacts {
    fn stream(&self) -> &'static str {
        "meetings.usage"
    }

    async fn facts_after(
        &self,
        after: Option<&Position>,
        until: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<(Position, UsageFact)>, String> {
        let (at, key) = after
            .map(|p| (p.at, p.key.clone()))
            .unwrap_or((DateTime::<Utc>::UNIX_EPOCH, String::new()));
        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DbBackend::Postgres,
                format!(
                    "SELECT * FROM ({FACTS}) f WHERE (f.at, f.id::text) > ($1, $2) AND f.at <= $3 \
                     ORDER BY f.at, f.id::text LIMIT $4"
                ),
                [at.into(), key.into(), until.into(), (limit as i64).into()],
            ))
            .await
            .map_err(err)?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let id: Uuid = r.try_get("", "id").map_err(err)?;
            let kind: String = r.try_get("", "kind").map_err(err)?;
            let conference: Uuid = r.try_get("", "conference").map_err(err)?;
            let at: DateTime<Utc> = r.try_get("", "at").map_err(err)?;
            let mut dimensions = BTreeMap::new();
            let (usage_type, resource) = if kind == "PARTICIPANT" {
                dimensions.insert("conferenceId".to_string(), json!(conference.to_string()));
                if let Some(d) = r.try_get::<Option<String>>("", "detail").map_err(err)? {
                    dimensions.insert("participantKind".to_string(), json!(d));
                }
                (
                    "MEETING_PARTICIPANT_MINUTE",
                    format!("participant_session:{id}"),
                )
            } else {
                dimensions.insert("recordingDomain".to_string(), json!("MEETING"));
                ("RECORDING_MINUTE", format!("meeting_artifact:{id}"))
            };
            let fact = UsageFact {
                source_fact_id: format!("{}:{id}", kind.to_ascii_lowercase()),
                usage_type: usage_type.into(),
                unit: "MILLISECOND".into(),
                quantity: r.try_get::<i64>("", "quantity").map_err(err)?.max(0) as u64,
                tenant_id: r.try_get("", "tenant_id").map_err(err)?,
                application_id: r.try_get("", "application_id").map_err(err)?,
                environment_id: r.try_get("", "environment_id").map_err(err)?,
                environment_kind: None,
                resource_ref: resource,
                occurred_at: at,
                period_start: None,
                period_end: None,
                dimensions,
                digest: String::new(),
            }
            .normalized()
            .sealed();
            out.push((
                Position {
                    at: fact.occurred_at,
                    key: id.to_string(),
                },
                fact,
            ));
        }
        Ok(out)
    }

    async fn totals(&self, start: DateTime<Utc>, end: DateTime<Utc>) -> Result<Vec<Total>, String> {
        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DbBackend::Postgres,
                format!(
                    "SELECT environment_id, kind, COALESCE(SUM(quantity), 0)::bigint AS q, COUNT(*)::bigint AS n \
                     FROM ({FACTS}) f WHERE f.at >= $1 AND f.at < $2 GROUP BY environment_id, kind"
                ),
                [start.into(), end.into()],
            ))
            .await
            .map_err(err)?;
        rows.iter()
            .map(|r| {
                let kind: String = r.try_get("", "kind").map_err(err)?;
                Ok(Total {
                    environment_id: r.try_get("", "environment_id").map_err(err)?,
                    usage_type: if kind == "PARTICIPANT" {
                        "MEETING_PARTICIPANT_MINUTE"
                    } else {
                        "RECORDING_MINUTE"
                    },
                    quantity: r.try_get::<i64>("", "q").map_err(err)?.max(0) as u64,
                    count: r.try_get::<i64>("", "n").map_err(err)?.max(0) as u64,
                })
            })
            .collect()
    }
}

/// STORAGE_BYTE_TIME of READY artefacts of environment-owned meetings.
pub struct ArtifactStorage {
    db: DatabaseConnection,
}

impl ArtifactStorage {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

const ARTIFACTS: &str = "\
SELECT a.id, a.artifact_type, s.platform_tenant_id AS tenant_id, \
       s.platform_application_id AS application_id, s.platform_environment_id AS environment_id, \
       COALESCE(a.available_at, a.finalized_at) AS stored_at, a.deleted_at, a.size_bytes \
  FROM meeting_artifacts a JOIN meeting_spaces s ON s.id = a.meeting_space_id \
 WHERE s.platform_environment_id IS NOT NULL AND a.size_bytes IS NOT NULL AND a.size_bytes > 0 \
   AND COALESCE(a.available_at, a.finalized_at) IS NOT NULL";

#[async_trait]
impl IntervalSource for ArtifactStorage {
    fn stream(&self) -> &'static str {
        "meetings.artifact-storage"
    }

    async fn earliest(&self) -> Result<Option<DateTime<Utc>>, String> {
        let row = self
            .db
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                format!("SELECT MIN(stored_at) AS t FROM ({ARTIFACTS}) a"),
            ))
            .await
            .map_err(err)?;
        Ok(row.and_then(|r| r.try_get::<Option<DateTime<Utc>>>("", "t").ok().flatten()))
    }

    async fn active(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<ActiveInterval>, String> {
        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DbBackend::Postgres,
                format!(
                    "SELECT * FROM ({ARTIFACTS}) a WHERE a.stored_at < $2 \
                       AND (a.deleted_at IS NULL OR a.deleted_at > $1)"
                ),
                [start.into(), end.into()],
            ))
            .await
            .map_err(err)?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let id: Uuid = r.try_get("", "id").map_err(err)?;
            let bytes = r.try_get::<i64>("", "size_bytes").map_err(err)?.max(0) as u64;
            let mut dimensions = BTreeMap::new();
            dimensions.insert("storageDomain".to_string(), json!("MEETING_ARTIFACT"));
            dimensions.insert("storedBytes".to_string(), json!(bytes));
            out.push(ActiveInterval {
                resource_ref: format!("meeting_artifact:{id}"),
                usage_type: "STORAGE_BYTE_TIME",
                tenant_id: r.try_get("", "tenant_id").map_err(err)?,
                application_id: r.try_get("", "application_id").map_err(err)?,
                environment_id: r.try_get("", "environment_id").map_err(err)?,
                environment_kind: None,
                active_from: r.try_get("", "stored_at").map_err(err)?,
                active_until: r.try_get("", "deleted_at").map_err(err)?,
                weight: bytes,
                dimensions,
            });
        }
        Ok(out)
    }
}
