//! Blueprint Section 15 (PLT-008) — automatic credential lifecycle webhooks.
//!
//! Platform Access stays the only credential authority: the worker READS the credentials
//! of the environments that subscribe to a lifecycle event (delegated reads, no actor),
//! and emits
//!
//! * `credential.expiring_soon` once per configured lead time
//!   (`DEVELOPER_PLATFORM_CREDENTIAL_EXPIRY_WARNING_DAYS`, default 14, 7 and 1 days) — when
//!   several lead times are already crossed (a short-lived credential, a worker outage),
//!   only the smallest crossed one is sent; a larger one is never sent afterwards;
//! * `credential.expired` once, when the expiry has passed (no older than
//!   `DEVELOPER_PLATFORM_CREDENTIAL_EXPIRED_LOOKBACK_DAYS`, so enabling the sweep does not
//!   announce long-dead credentials).
//!
//! Revoked, superseded and rotated (overlap) credentials get no notice: they were
//! replaced or withdrawn on purpose.
//!
//! Each notice goes through the central webhook outbox: a public event built from an
//! internal `developer-platform.credential.*` fact through the catalog's allow-list
//! (ids, codes and integers only), deduplicated on a DETERMINISTIC source event id
//! derived from (credential, event type, threshold, expiry), fanned out to the matching
//! ACTIVE subscriptions and delivered by the worker with the usual signed, retried,
//! dead-lettered deliveries. `devplat_credential_lifecycle_notices` persists the
//! (credential, event type, threshold, expiry) key, and the activity history records one
//! `SERVICE` event per notice in the same transaction. A crash between the outbox write
//! and the notice row is harmless: the next sweep re-publishes the same source event id,
//! which the outbox recognizes (no second delivery), then records the notice.

use std::time::Duration as StdDuration;

use cf_service_auth::CorrelationId;
use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{activity, stmt};
use crate::{
    http::error::ApiError,
    platform_access::{Delegated, DelegationCall, PlatformAccessPort},
    webhooks::{
        catalog::{self, PRODUCER_DEVELOPER_PLATFORM},
        ingest::EnvelopeFacts,
        WebhookService,
    },
};

/// The worker schedule key (`devplat_worker_schedules.job`) and activity actor.
pub const JOB: &str = "credential-lifecycle";
pub const EXPIRING_SOON: &str = "credential.expiring_soon";
pub const EXPIRED: &str = "credential.expired";

/// Configuration of the sweep (see [`crate::config`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialLifecycleSettings {
    pub enabled: bool,
    /// Lead times in days, distinct, descending, each 1–365.
    pub warning_days: Vec<i64>,
    /// How long after its expiry a credential still gets `credential.expired`.
    pub expired_lookback: Duration,
    /// Minimum time between two sweeps (across every worker instance).
    pub interval: StdDuration,
    /// Environments examined per sweep (the rest wait for the next one).
    pub max_environments: u64,
}

impl Default for CredentialLifecycleSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            warning_days: vec![14, 7, 1],
            expired_lookback: Duration::days(7),
            interval: StdDuration::from_secs(3600),
            max_environments: 500,
        }
    }
}

impl CredentialLifecycleSettings {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }
}

/// Parses `14,7,1` (1–8 distinct values of 1–365) into descending order.
pub fn parse_warning_days(value: &str) -> Option<Vec<i64>> {
    let mut days = value
        .split(',')
        .map(|d| {
            d.trim()
                .parse::<i64>()
                .ok()
                .filter(|d| (1..=365).contains(d))
        })
        .collect::<Option<Vec<_>>>()?;
    days.sort_unstable_by(|a, b| b.cmp(a));
    let n = days.len();
    days.dedup();
    (days.len() == n && (1..=8).contains(&n)).then_some(days)
}

/// One notice due for a credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Notice {
    pub event_type: &'static str,
    /// The crossed lead time (0 for `credential.expired`).
    pub threshold_days: i64,
    /// Deterministic: the instant the threshold was crossed (never before creation).
    pub occurred_at: DateTime<Utc>,
}

/// The notice a credential in `effective_state` (Platform Access's view) expiring at
/// `expires_at` is due at `now`, if any.
pub fn classify(
    effective_state: &str,
    expires_at: DateTime<Utc>,
    created_at: Option<DateTime<Utc>>,
    settings: &CredentialLifecycleSettings,
    now: DateTime<Utc>,
) -> Option<Notice> {
    // Replaced or withdrawn on purpose: no lifecycle notice.
    if !matches!(
        effective_state,
        "ACTIVE" | "SUSPENDED" | "NOT_YET_VALID" | "EXPIRED"
    ) {
        return None;
    }
    if expires_at <= now {
        return (now - expires_at <= settings.expired_lookback).then_some(Notice {
            event_type: EXPIRED,
            threshold_days: 0,
            occurred_at: expires_at,
        });
    }
    if effective_state == "EXPIRED" {
        // Platform Access says expired before our clock does: wait for the instant.
        return None;
    }
    let left = expires_at - now;
    let threshold = settings
        .warning_days
        .iter()
        .copied()
        .filter(|d| left <= Duration::days(*d))
        .min()?;
    let crossed = expires_at - Duration::days(threshold);
    Some(Notice {
        event_type: EXPIRING_SOON,
        threshold_days: threshold,
        occurred_at: created_at.map_or(crossed, |c| c.max(crossed)),
    })
}

/// The deterministic source event id of a notice (`evt_<32 hex>`).
pub fn source_event_id(credential_id: Uuid, notice: &Notice, expires_at: DateTime<Utc>) -> String {
    let mut h = Sha256::new();
    h.update(
        format!(
            "cfmeet.developer-platform/credential-lifecycle/v1\n{credential_id}\n{}\n{}\n{}",
            notice.event_type,
            notice.threshold_days,
            expires_at.timestamp()
        )
        .as_bytes(),
    );
    format!("evt_{}", &hex::encode(h.finalize())[..32])
}

/// The outcome of one sweep.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepReport {
    pub environments: u64,
    /// Environments Platform Access could not answer for (retried next sweep).
    pub unavailable: u64,
    pub examined: u64,
    /// Notices emitted by this sweep.
    pub notified: u64,
    /// Webhook deliveries those notices created.
    pub deliveries: u64,
    /// Notices already emitted earlier (persisted deduplication).
    pub deduplicated: u64,
    /// Credential views that did not belong to the environment asked for (skipped).
    pub inconsistent: u64,
}

fn time(v: &Value, k: &str) -> Option<DateTime<Utc>> {
    v.get(k)
        .and_then(Value::as_str)
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&Utc))
}

fn uuid_of(v: &Value, k: &str) -> Option<Uuid> {
    v.get(k)
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
}

async fn list(
    platform: &dyn PlatformAccessPort,
    call: DelegationCall,
    key: &str,
    correlation: &CorrelationId,
) -> Option<Vec<Value>> {
    match platform.delegate(call, correlation).await {
        Ok(Delegated::Ok(v)) => v.get(key).and_then(Value::as_array).cloned(),
        _ => None,
    }
}

/// The environments (tenant, application, environment) with an ACTIVE subscription to a
/// credential lifecycle event.
async fn subscribed<C: ConnectionTrait>(
    db: &C,
    limit: u64,
) -> Result<Vec<(Uuid, Uuid, Uuid)>, ApiError> {
    let rows = db
        .query_all(stmt(
            "SELECT DISTINCT tenant_id, application_id, environment_id FROM devplat_webhook_subscriptions \
              WHERE status = 'ACTIVE' AND event_types && ARRAY[$1, $2]::text[] \
              ORDER BY environment_id LIMIT $3",
            vec![EXPIRING_SOON.into(), EXPIRED.into(), (limit as i64).into()],
        ))
        .await?;
    rows.iter()
        .map(|r| {
            Ok((
                r.try_get("", "tenant_id")?,
                r.try_get("", "application_id")?,
                r.try_get("", "environment_id")?,
            ))
        })
        .collect()
}

async fn already_notified<C: ConnectionTrait>(
    db: &C,
    credential_id: Uuid,
    notice: &Notice,
    expires_at: DateTime<Utc>,
) -> Result<bool, ApiError> {
    Ok(db
        .query_one(stmt(
            "SELECT 1 FROM devplat_credential_lifecycle_notices \
              WHERE credential_id = $1 AND event_type = $2 AND threshold_days = $3 AND expires_at = $4",
            vec![
                credential_id.into(),
                notice.event_type.into(),
                (notice.threshold_days as i32).into(),
                expires_at.into(),
            ],
        ))
        .await?
        .is_some())
}

/// One sweep over the subscribed environments at `now` (normally the webhook clock).
pub async fn sweep_once(
    webhooks: &WebhookService,
    platform: &dyn PlatformAccessPort,
    settings: &CredentialLifecycleSettings,
    now: DateTime<Utc>,
) -> Result<SweepReport, ApiError> {
    let db = webhooks.db();
    let mut report = SweepReport::default();
    let correlation = CorrelationId::generate();
    for (tenant_id, application_id, environment_id) in
        subscribed(db, settings.max_environments).await?
    {
        report.environments += 1;
        let Some(accounts) = list(
            platform,
            DelegationCall::ListServiceAccounts { environment_id },
            "serviceAccounts",
            &correlation,
        )
        .await
        else {
            report.unavailable += 1;
            continue;
        };
        let mut credentials = Vec::new();
        let mut complete = true;
        for sa in &accounts {
            let Some(service_account_id) = uuid_of(sa, "id") else {
                report.inconsistent += 1;
                continue;
            };
            match list(
                platform,
                DelegationCall::ListCredentials {
                    environment_id,
                    service_account_id,
                },
                "credentials",
                &correlation,
            )
            .await
            {
                Some(c) => credentials.extend(c),
                None => complete = false,
            }
        }
        if !complete {
            report.unavailable += 1;
        }
        for c in &credentials {
            report.examined += 1;
            let (Some(id), Some(service_account_id), Some(expires_at)) = (
                uuid_of(c, "id"),
                uuid_of(c, "serviceAccountId"),
                time(c, "expiresAt"),
            ) else {
                report.inconsistent += 1;
                continue;
            };
            if uuid_of(c, "environmentId") != Some(environment_id)
                || uuid_of(c, "tenantId") != Some(tenant_id)
                || uuid_of(c, "applicationId") != Some(application_id)
            {
                report.inconsistent += 1;
                continue;
            }
            let state = c
                .get("effectiveState")
                .and_then(Value::as_str)
                .unwrap_or("");
            let Some(notice) = classify(state, expires_at, time(c, "createdAt"), settings, now)
            else {
                continue;
            };
            if already_notified(db, id, &notice, expires_at).await? {
                report.deduplicated += 1;
                continue;
            }
            let deliveries = notify(
                webhooks,
                &correlation,
                (tenant_id, application_id, environment_id),
                id,
                service_account_id,
                state,
                expires_at,
                &notice,
                now,
            )
            .await?;
            match deliveries {
                Some(d) => {
                    report.notified += 1;
                    report.deliveries += d;
                }
                None => report.deduplicated += 1,
            }
        }
    }
    Ok(report)
}

/// Publishes one notice through the webhook outbox and records it. `None`: another
/// worker recorded the same notice first.
#[allow(clippy::too_many_arguments)]
async fn notify(
    webhooks: &WebhookService,
    correlation: &CorrelationId,
    scope: (Uuid, Uuid, Uuid),
    credential_id: Uuid,
    service_account_id: Uuid,
    state: &str,
    expires_at: DateTime<Utc>,
    notice: &Notice,
    now: DateTime<Utc>,
) -> Result<Option<u64>, ApiError> {
    let source = source_event_id(credential_id, notice, expires_at);
    let internal_type = if notice.event_type == EXPIRED {
        "developer-platform.credential.expired"
    } else {
        "developer-platform.credential.expiring-soon"
    };
    let mut payload = Map::new();
    payload.insert("credential_id".into(), json!(credential_id));
    payload.insert("service_account_id".into(), json!(service_account_id));
    payload.insert("environment_id".into(), json!(scope.2));
    payload.insert(
        "status".into(),
        json!(if notice.event_type == EXPIRED {
            "EXPIRED"
        } else {
            state
        }),
    );
    if notice.threshold_days > 0 {
        payload.insert("threshold_days".into(), json!(notice.threshold_days));
    }
    if expires_at.timestamp() >= 0 {
        payload.insert(
            "expires_at_epoch_seconds".into(),
            json!(expires_at.timestamp()),
        );
    }
    let envelope = json!({ "event_type": internal_type, "payload": Value::Object(payload) });
    let event = catalog::project(PRODUCER_DEVELOPER_PLATFORM, &envelope)
        .ok()
        .flatten()
        .ok_or(ApiError(crate::http::error::ErrorCode::InternalError))?;
    let facts = EnvelopeFacts {
        source_event_id: source.clone(),
        scope,
        occurred_at: notice.occurred_at,
        correlation_id: Some(correlation.as_str().to_string()),
    };
    let ingested = webhooks
        .ingest_public_event(PRODUCER_DEVELOPER_PLATFORM, &facts, &event)
        .await?;
    let event_id = ingested
        .event_id
        .ok_or(ApiError(crate::http::error::ErrorCode::InternalError))?;
    let txn = webhooks.db().begin().await?;
    let inserted = txn
        .execute(stmt(
            "INSERT INTO devplat_credential_lifecycle_notices (credential_id, event_type, threshold_days, \
               expires_at, tenant_id, application_id, environment_id, service_account_id, source_event_id, \
               webhook_event_id, deliveries, notified_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) \
             ON CONFLICT (credential_id, event_type, threshold_days, expires_at) DO NOTHING",
            vec![
                credential_id.into(),
                notice.event_type.into(),
                (notice.threshold_days as i32).into(),
                expires_at.into(),
                scope.0.into(),
                scope.1.into(),
                scope.2.into(),
                service_account_id.into(),
                source.clone().into(),
                event_id.clone().into(),
                (ingested.deliveries.min(i32::MAX as u64) as i32).into(),
                now.into(),
            ],
        ))
        .await?
        .rows_affected();
    if inserted == 0 {
        txn.rollback().await?;
        return Ok(None);
    }
    activity::record_system(
        &txn,
        JOB,
        &source,
        correlation.as_str(),
        activity::Event {
            tenant_id: scope.0,
            action: notice.event_type,
            target_type: "CREDENTIAL",
            target_id: credential_id.to_string(),
            summary: json!({
                "environmentId": scope.2,
                "projectId": scope.1,
                "serviceAccountId": service_account_id,
                "thresholdDays": notice.threshold_days,
                "expiresAt": expires_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                "webhookEventId": event_id,
                "deliveries": ingested.deliveries,
            }),
        },
    )
    .await?;
    txn.commit().await?;
    Ok(Some(ingested.deliveries))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(days: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
            + Duration::days(days)
    }

    #[test]
    fn lead_times_parse_strictly() {
        assert_eq!(parse_warning_days("1, 14,7"), Some(vec![14, 7, 1]));
        assert_eq!(parse_warning_days("30"), Some(vec![30]));
        for bad in ["", "0", "366", "7,7", "a", "1,2,3,4,5,6,7,8,9", "-1"] {
            assert_eq!(parse_warning_days(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_smallest_crossed_threshold_is_due_and_expiry_has_a_lookback() {
        let s = CredentialLifecycleSettings::default();
        let now = t(0);
        // Outside every lead time: nothing.
        assert_eq!(classify("ACTIVE", t(20), None, &s, now), None);
        // Inside 14 days only.
        let n = classify("ACTIVE", t(10), None, &s, now).unwrap();
        assert_eq!((n.event_type, n.threshold_days), (EXPIRING_SOON, 14));
        assert_eq!(n.occurred_at, t(-4));
        // Exactly on the 7-day boundary: 7.
        assert_eq!(
            classify("SUSPENDED", t(7), None, &s, now)
                .unwrap()
                .threshold_days,
            7
        );
        // Inside 1 day: 1 (never 14 or 7 again), not before creation.
        let n = classify("ACTIVE", t(0) + Duration::hours(3), Some(t(-3)), &s, now).unwrap();
        assert_eq!(n.threshold_days, 1);
        assert_eq!(n.occurred_at, t(-1) + Duration::hours(3));
        let n = classify("ACTIVE", t(0) + Duration::hours(3), Some(t(0)), &s, now).unwrap();
        assert_eq!(n.occurred_at, t(0), "created inside the lead time");
        // Expired: once, within the lookback only.
        let n = classify("EXPIRED", t(-2), None, &s, now).unwrap();
        assert_eq!(
            (n.event_type, n.threshold_days, n.occurred_at),
            (EXPIRED, 0, t(-2))
        );
        assert_eq!(
            classify("ACTIVE", t(-1), None, &s, now).unwrap().event_type,
            EXPIRED
        );
        assert_eq!(classify("EXPIRED", t(-8), None, &s, now), None);
        // Replaced or withdrawn credentials get nothing.
        for state in ["REVOKED", "SUPERSEDED", "ROTATION_OVERLAP", ""] {
            assert_eq!(classify(state, t(1), None, &s, now), None, "{state}");
            assert_eq!(classify(state, t(-1), None, &s, now), None, "{state}");
        }
        // Configured lead times are honoured.
        let custom = CredentialLifecycleSettings {
            warning_days: vec![30],
            ..s
        };
        assert_eq!(
            classify("ACTIVE", t(20), None, &custom, now)
                .unwrap()
                .threshold_days,
            30
        );
        assert_eq!(classify("ACTIVE", t(31), None, &custom, now), None);
    }

    #[test]
    fn source_event_ids_are_deterministic_per_key() {
        let id = Uuid::new_v4();
        let a = Notice {
            event_type: EXPIRING_SOON,
            threshold_days: 7,
            occurred_at: t(0),
        };
        let x = source_event_id(id, &a, t(7));
        assert!(x.starts_with("evt_") && x.len() == 36);
        assert_eq!(x, source_event_id(id, &a, t(7)));
        assert_ne!(
            x,
            source_event_id(id, &a, t(8)),
            "a new expiry is a new notice"
        );
        let b = Notice {
            threshold_days: 1,
            ..a
        };
        assert_ne!(x, source_event_id(id, &b, t(7)));
        assert_ne!(x, source_event_id(Uuid::new_v4(), &a, t(7)));
    }
}
