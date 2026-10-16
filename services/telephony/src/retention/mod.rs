//! Retention (Part H §80, §102, §106): Telephony's data classes, their retention
//! policies, the audited purge workers and retention holds.
//!
//! | Data class | Action | Bound |
//! |---|---|---|
//! | `PROVIDER_EVENTS` | Delete | `TELEPHONY_PROVIDER_EVENT_RETENTION_DAYS` (default 30) |
//! | `TELEPHONY_INCIDENTS` | Delete (RESOLVED only) | `TELEPHONY_INCIDENT_RETENTION_DAYS` (default 365) |
//! | `INGRESS_WINDOWS` | StatusOnly (reference) | pruned continuously after 1 h by the ingress counters |
//! | `NORMALIZED_LEG_EVIDENCE` | StatusOnly | legs, bindings, commands, bridges, usage: retained (append-only tables; no number inside) |
//! | `CAPTURE_SPOOL` | StatusOnly (reference) | edge spool files are deleted right after delivery (`capture.rs`) |
//!
//! Purges run in bounded batches (`FOR UPDATE SKIP LOCKED`, so any number of instances
//! delete each row once), inside a transaction flagged `telephony.retention_worker =
//! 'on'` — the only context in which the database accepts a DELETE on these tables —
//! and write one `telephony_retention_audit` row per batch that deleted something. A
//! retention hold (per leg, per provider or class-wide) keeps rows out of every purge
//! until it is released.

use std::time::Duration;

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    db,
    error::{ErrorCode, TelephonyError, TelephonyResult},
    service::{ExecutionSettings, TelephonyService},
};

crate::domain::vocab!(
    /// Telephony's retained data classes.
    DataClass {
        ProviderEvents = "PROVIDER_EVENTS",
        TelephonyIncidents = "TELEPHONY_INCIDENTS",
        IngressWindows = "INGRESS_WINDOWS",
        NormalizedLegEvidence = "NORMALIZED_LEG_EVIDENCE",
        CaptureSpool = "CAPTURE_SPOOL",
    }
);

crate::domain::vocab!(
    /// What a policy does once the bound passes.
    RetentionAction {
        Delete = "DELETE",
        StatusOnly = "STATUS_ONLY",
    }
);

crate::domain::vocab!(
    /// Why a retention hold exists (neutral operational vocabulary).
    HoldReason {
        Investigation = "INVESTIGATION",
        Dispute = "DISPUTE",
        RegulatoryRequest = "REGULATORY_REQUEST",
        IncidentReview = "INCIDENT_REVIEW",
        Other = "OTHER",
    }
);

/// One retention policy (configuration, never a legal claim).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionPolicy {
    pub data_class: DataClass,
    pub action: RetentionAction,
    /// The bound after which `action` applies (None: status-only reference).
    pub retention: Option<Duration>,
    pub description: &'static str,
}

impl RetentionPolicy {
    /// The bound as written in audit rows (`30d`, or `status-only`).
    pub fn label(&self) -> String {
        match self.retention {
            Some(d) => format!("{}d", d.as_secs() / 86_400),
            None => "status-only".to_string(),
        }
    }
}

/// The effective policies for `settings`.
pub fn policies(settings: &ExecutionSettings) -> Vec<RetentionPolicy> {
    vec![
        RetentionPolicy {
            data_class: DataClass::ProviderEvents,
            action: RetentionAction::Delete,
            retention: Some(settings.provider_event_retention),
            description: "restricted provider event evidence; deleted past retain_until unless its leg or provider is held",
        },
        RetentionPolicy {
            data_class: DataClass::TelephonyIncidents,
            action: RetentionAction::Delete,
            retention: Some(settings.incident_retention),
            description: "RESOLVED incidents; deleted past the bound unless held (OPEN incidents are never purged)",
        },
        RetentionPolicy {
            data_class: DataClass::IngressWindows,
            action: RetentionAction::StatusOnly,
            retention: Some(Duration::from_secs(3600)),
            description: "unlogged ingress counters, pruned after one hour by the ingress path itself",
        },
        RetentionPolicy {
            data_class: DataClass::NormalizedLegEvidence,
            action: RetentionAction::StatusOnly,
            retention: None,
            description: "legs, bindings, commands, bridges, usage: retained (append-only; references only, no number)",
        },
        RetentionPolicy {
            data_class: DataClass::CaptureSpool,
            action: RetentionAction::StatusOnly,
            retention: None,
            description: "edge spool files are deleted immediately after delivery to Calling",
        },
    ]
}

/// A retention hold.
#[derive(Debug, Clone, FromQueryResult)]
pub struct RetentionHold {
    pub id: Uuid,
    pub data_class: String,
    pub telephony_leg_id: Option<Uuid>,
    pub provider_id: Option<String>,
    pub reason_code: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub released_at: Option<DateTime<Utc>>,
}

impl RetentionHold {
    pub fn view(&self) -> Value {
        json!({
            "id": self.id,
            "dataClass": self.data_class,
            "telephonyLegId": self.telephony_leg_id,
            "providerId": self.provider_id,
            "reasonCode": self.reason_code,
            "createdBy": self.created_by,
            "createdAt": self.created_at,
            "releasedAt": self.released_at,
        })
    }
}

/// Places a hold (leg-scoped, provider-scoped or class-wide when both are None).
pub async fn place_hold<C: ConnectionTrait>(
    db_: &C,
    data_class: DataClass,
    telephony_leg_id: Option<Uuid>,
    provider_id: Option<&str>,
    reason: HoldReason,
    created_by: &str,
    at: DateTime<Utc>,
) -> TelephonyResult<Uuid> {
    if telephony_leg_id.is_some() && provider_id.is_some() {
        return Err(TelephonyError::Validation(
            "A hold is scoped to a leg, a provider or the whole class.",
        ));
    }
    let id = Uuid::new_v4();
    db::exec(
        db_,
        "INSERT INTO telephony_retention_holds (id, data_class, telephony_leg_id, provider_id, reason_code, created_by, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
        vec![
            id.into(),
            data_class.as_str().into(),
            telephony_leg_id.into(),
            provider_id.map(str::to_string).into(),
            reason.as_str().into(),
            created_by.into(),
            at.into(),
        ],
    )
    .await?;
    Ok(id)
}

/// Releases a hold (idempotent: an already released hold is left as it is).
pub async fn release_hold<C: ConnectionTrait>(
    db_: &C,
    id: Uuid,
    at: DateTime<Utc>,
) -> TelephonyResult<bool> {
    let n = db::exec(
        db_,
        "UPDATE telephony_retention_holds SET released_at = $2 WHERE id = $1 AND released_at IS NULL",
        vec![id.into(), at.into()],
    )
    .await?;
    Ok(n > 0)
}

/// Active (unreleased) holds, newest first.
pub async fn active_holds<C: ConnectionTrait>(db_: &C) -> TelephonyResult<Vec<RetentionHold>> {
    db::all(
        db_,
        "SELECT id, data_class, telephony_leg_id, provider_id, reason_code, created_by, created_at, released_at \
         FROM telephony_retention_holds WHERE released_at IS NULL ORDER BY created_at DESC LIMIT 500",
        vec![],
    )
    .await
}

/// Rows the active holds keep out of a purge of `e` (leg, provider or class-wide).
const NOT_HELD: &str =
    "NOT EXISTS (SELECT 1 FROM telephony_retention_holds h WHERE h.released_at IS NULL \
    AND ((h.telephony_leg_id IS NOT NULL AND h.telephony_leg_id = e.telephony_leg_id) \
      OR (h.provider_id IS NOT NULL AND h.provider_id = e.provider_id) \
      OR (h.telephony_leg_id IS NULL AND h.provider_id IS NULL AND h.data_class = $3)))";

impl TelephonyService {
    /// Deletes provider event evidence past its retention bound (bounded batches; held
    /// legs / providers are skipped; audited).
    pub async fn purge_expired_provider_events(&self, limit: u64) -> TelephonyResult<u64> {
        let policy = policies(&self.settings).remove(0);
        self.purge_batch(
            &policy,
            &format!(
                "DELETE FROM provider_events WHERE id IN (SELECT e.id FROM provider_events e \
                   WHERE e.retain_until < $1 AND {NOT_HELD} \
                   ORDER BY e.retain_until LIMIT $2 FOR UPDATE SKIP LOCKED)"
            ),
            limit,
        )
        .await
    }

    /// Deletes RESOLVED incidents past the incident retention bound (OPEN incidents are
    /// never purged; held legs / providers are skipped; audited).
    pub async fn purge_resolved_incidents(&self, limit: u64) -> TelephonyResult<u64> {
        let policy = policies(&self.settings).remove(1);
        self.purge_batch(
            &policy,
            &format!(
                "DELETE FROM telephony_incidents WHERE id IN (SELECT e.id FROM telephony_incidents e \
                   WHERE e.status = 'RESOLVED' AND e.resolved_at < $1 AND {NOT_HELD} \
                   ORDER BY e.resolved_at LIMIT $2 FOR UPDATE SKIP LOCKED)"
            ),
            limit,
        )
        .await
    }

    async fn purge_batch(
        &self,
        policy: &RetentionPolicy,
        sql: &str,
        limit: u64,
    ) -> TelephonyResult<u64> {
        let now = self.now();
        let bound = policy
            .retention
            .and_then(|d| chrono::Duration::from_std(d).ok())
            .ok_or(TelephonyError::Refused(ErrorCode::InternalError))?;
        let txn = self.db.begin().await?;
        txn.execute_unprepared("SET LOCAL telephony.retention_worker = 'on'")
            .await?;
        // Provider events carry their own bound (`retain_until`); incidents are bounded
        // from their resolution time.
        let cutoff = if policy.data_class == DataClass::ProviderEvents {
            now
        } else {
            now - bound
        };
        let deleted = db::exec(
            &txn,
            sql,
            vec![
                cutoff.into(),
                (limit.clamp(1, 1000) as i64).into(),
                policy.data_class.as_str().into(),
            ],
        )
        .await?;
        if deleted > 0 {
            db::exec(
                &txn,
                "INSERT INTO telephony_retention_audit (id, data_class, action, batch_count, policy, actor, outcome, occurred_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, 'DELETED', $7)",
                vec![
                    Uuid::new_v4().into(),
                    policy.data_class.as_str().into(),
                    policy.action.as_str().into(),
                    (deleted.min(i32::MAX as u64) as i32).into(),
                    policy.label().into(),
                    format!("worker:{}", self.worker_id).into(),
                    now.into(),
                ],
            )
            .await?;
        }
        txn.commit().await?;
        if deleted > 0 {
            crate::metrics::inc(
                "telephony_retention_batches_total",
                &[("data_class", policy.data_class.as_str())],
            );
            tracing::info!(
                data_class = policy.data_class.as_str(),
                deleted,
                "retention batch applied"
            );
        }
        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_data_class_has_exactly_one_policy() {
        let p = policies(&ExecutionSettings::default());
        assert_eq!(p.len(), DataClass::ALL.len());
        for class in DataClass::ALL {
            assert_eq!(p.iter().filter(|x| x.data_class == *class).count(), 1);
        }
        assert_eq!(p[0].label(), "30d");
        assert_eq!(p[1].label(), "365d");
        assert_eq!(p[3].label(), "status-only");
        assert!(p
            .iter()
            .filter(|x| x.action == RetentionAction::Delete)
            .all(|x| x.retention.is_some()));
    }
}
