//! The importer: `inventory` (a dry run inside one transaction that is rolled back — it
//! writes nothing) and `import` (resumable runs: each page is applied together with the
//! run's checkpoint in ONE transaction, so a crash loses at most the page in flight and a
//! re-run continues from the checkpoint; every write is an idempotent upsert keyed by
//! Calling's ids, so re-importing never duplicates anything).

use std::collections::{HashMap, HashSet};

use cf_webhooks::secret_box::SecretBox;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::{
    listed,
    port::{
        records, CallingAttempt, CallingDelivery, CallingEvent, CallingSubscription, Entity,
        ExportPage,
    },
    secret_fingerprint, source_error, transfer_owner, Migration, Plain, EXPORT_PURPOSE, MAX_LISTED,
    ORIGIN_MIGRATED, ROTATION_REASON,
};
use crate::{
    control::stmt,
    http::error::{ApiError, ErrorCode},
    platform_access::{Delegated, DelegationCall},
};

/// Import tuning (and the test hook that simulates a crash after N committed pages).
#[derive(Debug, Clone, Copy)]
pub struct ImportOptions {
    /// Rows requested per export page (1–500).
    pub page_size: u32,
    /// Stop with an error after this many pages committed by this invocation, leaving the
    /// run RUNNING exactly as a crashed process would (tests only).
    pub fail_after_pages: Option<u32>,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            page_size: 100,
            fail_after_pages: None,
        }
    }
}

const COUNT_KEYS: &[&str] = &[
    "seen",
    "inserted",
    "updated",
    "unchanged",
    "conflicts",
    "rotationRequired",
    "dpNewer",
    "held",
    "dpOwned",
];

fn empty_counts() -> Value {
    let mut m = Map::new();
    for e in Entity::ALL {
        let mut c = Map::new();
        for k in COUNT_KEYS {
            c.insert(k.to_string(), Value::from(0));
        }
        m.insert(e.as_str().to_string(), Value::Object(c));
    }
    Value::Object(m)
}

/// Accumulates one page's effect.
struct Tally<'a> {
    counts: &'a mut Value,
    conflicts: &'a mut Vec<Value>,
    entity: Entity,
}

impl Tally<'_> {
    fn bump(&mut self, key: &str) {
        let slot = &mut self.counts[self.entity.as_str()][key];
        *slot = Value::from(slot.as_i64().unwrap_or(0) + 1);
    }

    fn conflict(&mut self, id: impl ToString, reason: &str) {
        self.bump("conflicts");
        if self.conflicts.len() < MAX_LISTED {
            self.conflicts
                .push(listed(self.entity.as_str(), id, reason));
        }
    }
}

/// How a subscription's secret is carried over.
enum Carried {
    /// Re-sealed under this deployment's key ring.
    Sealed { sealed: String, kid: String },
    /// Not carried over: the subscription requires a customer rotation.
    Rotation(&'static str),
}

impl Carried {
    fn outcome(&self) -> &'static str {
        match self {
            Carried::Sealed { .. } => "TRANSFERRED",
            Carried::Rotation(o) => o,
        }
    }
}

/// Opens the transfer seal (bound to the subscription and the export direction), checks
/// Calling's fingerprint, and seals the opened secret again under `ring`. It never
/// leaves this function and is zeroized on drop.
fn carry(ring: &SecretBox, transfer: &SecretBox, s: &CallingSubscription) -> Carried {
    if s.secret.state != "TRANSFER_SEALED" {
        return Carried::Rotation("ROTATION_REQUIRED_UNOPENABLE");
    }
    let failed = Carried::Rotation("ROTATION_REQUIRED_TRANSFER");
    let Some(sealed) = s.secret.transfer_sealed.as_deref() else {
        return failed;
    };
    if s.secret.transfer_key_id.as_deref() != Some(transfer.current_kid()) {
        return failed;
    }
    let Some(plain) = Plain::open(
        transfer,
        sealed,
        &transfer_owner(EXPORT_PURPOSE, s.webhook_id),
    ) else {
        return failed;
    };
    if let Some(expected) = &s.secret.fingerprint {
        if secret_fingerprint(plain.expose(), s.webhook_id) != *expected {
            return failed;
        }
    }
    Carried::Sealed {
        sealed: ring.seal(plain.expose(), &s.webhook_id.to_string()),
        kid: ring.current_kid().to_string(),
    }
}

fn valid_token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn is_open(status: &str) -> bool {
    matches!(status, "PENDING" | "RETRY_SCHEDULED" | "DELIVERING")
}

#[derive(Debug, FromQueryResult)]
struct ExistingSubscription {
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    status: String,
    version: i64,
    secret_state: String,
    mapped: bool,
    calling_version: Option<i64>,
    calling_secret_version: Option<i32>,
    imported_version: Option<i64>,
}

#[derive(Debug, FromQueryResult)]
struct ExistingDelivery {
    origin: String,
    migration_hold: bool,
    status: String,
    attempt_count: i32,
    next_attempt_at: DateTime<Utc>,
    lease_until: Option<DateTime<Utc>>,
    last_http_status: Option<i32>,
    failure_category: Option<String>,
    completed_at: Option<DateTime<Utc>>,
}

impl Migration<'_> {
    /// Environment kinds Calling did not know (its projection lacked the environment),
    /// resolved through Platform Access.
    async fn resolve_kinds(&self, subs: &[CallingSubscription]) -> HashMap<Uuid, String> {
        let mut out = HashMap::new();
        for s in subs {
            if let Some(kind) = &s.environment_kind {
                out.insert(s.environment_id, kind.clone());
                continue;
            }
            if out.contains_key(&s.environment_id) {
                continue;
            }
            if let Ok(Delegated::Ok(v)) = self
                .platform_access
                .delegate(
                    DelegationCall::GetEnvironment {
                        environment_id: s.environment_id,
                    },
                    &self.correlation,
                )
                .await
            {
                if let Some(kind) = v.get("kind").and_then(Value::as_str) {
                    out.insert(s.environment_id, kind.to_string());
                }
            }
        }
        out
    }

    /// Applies one export page of `entity` in `db` (the caller's transaction).
    #[allow(clippy::too_many_arguments)]
    async fn apply_page<C: ConnectionTrait>(
        &self,
        db: &C,
        entity: Entity,
        page: &ExportPage,
        kinds: &HashMap<Uuid, String>,
        run_id: Option<Uuid>,
        counts: &mut Value,
        conflicts: &mut Vec<Value>,
    ) -> Result<(), ApiError> {
        let mut t = Tally {
            counts,
            conflicts,
            entity,
        };
        let malformed = |_| ApiError(ErrorCode::ServiceUnavailable);
        match entity {
            Entity::Subscriptions => {
                let ring = self.ring()?;
                let transfer = self.transfer()?;
                for s in records::<CallingSubscription>(page).map_err(malformed)? {
                    t.bump("seen");
                    self.apply_subscription(db, ring, transfer, &s, kinds, run_id, &mut t)
                        .await?;
                }
            }
            Entity::Events => {
                for e in records::<CallingEvent>(page).map_err(malformed)? {
                    t.bump("seen");
                    self.apply_event(db, &e, &mut t).await?;
                }
            }
            Entity::Deliveries => {
                let mut rows = records::<CallingDelivery>(page).map_err(malformed)?;
                // A replay after its original (the export order already guarantees it
                // across pages; this keeps it within a page whatever the ties).
                let ids: HashSet<Uuid> = rows.iter().map(|d| d.delivery_id).collect();
                rows.sort_by_key(|d| d.replay_of.is_some_and(|r| ids.contains(&r)));
                for d in rows {
                    t.bump("seen");
                    self.apply_delivery(db, &d, &mut t).await?;
                }
            }
            Entity::Attempts => {
                for a in records::<CallingAttempt>(page).map_err(malformed)? {
                    t.bump("seen");
                    self.apply_attempt(db, &a, &mut t).await?;
                }
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn apply_subscription<C: ConnectionTrait>(
        &self,
        db: &C,
        ring: &SecretBox,
        transfer: &SecretBox,
        s: &CallingSubscription,
        kinds: &HashMap<Uuid, String>,
        run_id: Option<Uuid>,
        t: &mut Tally<'_>,
    ) -> Result<(), ApiError> {
        let id = s.webhook_id;
        if !matches!(s.status.as_str(), "ACTIVE" | "DISABLED" | "DELETED")
            || (s.status == "DELETED") != s.deleted_at.is_some()
            || s.event_types.is_empty()
            || s.event_types.len() > 64
            || s.version < 1
            || s.secret_version < 1
        {
            t.conflict(id, "INVALID_RECORD");
            return Ok(());
        }
        let Some(kind) = kinds
            .get(&s.environment_id)
            .filter(|k| matches!(k.as_str(), "SANDBOX" | "TEST" | "PRODUCTION"))
        else {
            t.conflict(id, "ENVIRONMENT_UNKNOWN");
            return Ok(());
        };
        let mut types = s.event_types.clone();
        types.sort();
        types.dedup();
        let existing = ExistingSubscription::find_by_statement(stmt(
            "SELECT s.tenant_id, s.application_id, s.environment_id, s.status, s.version, \
                    s.secret_state, (m.subscription_id IS NOT NULL) AS mapped, m.calling_version, \
                    m.calling_secret_version, m.imported_version \
               FROM devplat_webhook_subscriptions s \
               LEFT JOIN devplat_webhook_migration_subscriptions m ON m.subscription_id = s.id \
              WHERE s.id = $1 FOR UPDATE OF s",
            vec![id.into()],
        ))
        .one(db)
        .await?;
        let now = self.now();
        let Some(row) = existing else {
            let carried = carry(ring, transfer, s);
            let (sealed, kid, state) = match &carried {
                Carried::Sealed { sealed, kid } => {
                    (Some(sealed.clone()), Some(kid.clone()), "VALID")
                }
                Carried::Rotation(_) => (None, None, "ROTATION_REQUIRED"),
            };
            let rotation = state == "ROTATION_REQUIRED";
            let status = if rotation && s.status == "ACTIVE" {
                "DISABLED"
            } else {
                s.status.as_str()
            };
            let reason = (rotation && s.status != "DELETED").then_some(ROTATION_REASON);
            db.execute(stmt(
                "INSERT INTO devplat_webhook_subscriptions (id, tenant_id, application_id, \
                   environment_id, environment_kind, url, description, event_types, status, \
                   secret_sealed, secret_key_id, secret_state, secret_version, version, \
                   disabled_reason, origin, created_by, created_at, updated_at, deleted_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, \
                         'MIGRATED_FROM_CALLING', 'migration:calling', $16, $17, $18)",
                vec![
                    id.into(),
                    s.tenant_id.into(),
                    s.application_id.into(),
                    s.environment_id.into(),
                    kind.clone().into(),
                    s.url.clone().into(),
                    s.description.clone().into(),
                    types.into(),
                    status.into(),
                    sealed.into(),
                    kid.into(),
                    state.into(),
                    s.secret_version.into(),
                    s.version.into(),
                    reason.into(),
                    s.created_at.into(),
                    s.updated_at.into(),
                    s.deleted_at.into(),
                ],
            ))
            .await?;
            self.map_subscription(db, s, s.version, carried.outcome(), run_id, now)
                .await?;
            t.bump("inserted");
            if rotation {
                t.bump("rotationRequired");
            }
            return Ok(());
        };
        if !row.mapped {
            // A Developer Platform subscription with the same id that the migration never
            // wrote: never overwritten.
            t.conflict(id, "ID_COLLISION");
            return Ok(());
        }
        if (row.tenant_id, row.application_id, row.environment_id)
            != (s.tenant_id, s.application_id, s.environment_id)
        {
            t.conflict(id, "SCOPE_MISMATCH");
            return Ok(());
        }
        let calling_version = row.calling_version.unwrap_or(0);
        let dp_changed = Some(row.version) != row.imported_version;
        let needs_secret = s.secret_version > row.calling_secret_version.unwrap_or(0)
            || row.secret_state != "VALID";
        if s.version == calling_version {
            // Unchanged at Calling. A secret that could not be carried before (and the
            // subscription untouched here since) is repaired when it can be now.
            if row.secret_state != "VALID" && !dp_changed {
                if let carried @ Carried::Sealed { .. } = carry(ring, transfer, s) {
                    self.update_subscription(db, s, &types, Some(&carried), now)
                        .await?;
                    self.map_subscription(db, s, s.version, carried.outcome(), run_id, now)
                        .await?;
                    t.bump("updated");
                    return Ok(());
                }
                t.bump("rotationRequired");
            }
            if dp_changed {
                t.bump("dpNewer");
            } else {
                t.bump("unchanged");
            }
            return Ok(());
        }
        if s.version < calling_version {
            t.conflict(id, "CALLING_VERSION_REGRESSED");
            return Ok(());
        }
        if dp_changed {
            t.conflict(id, "DIVERGED");
            return Ok(());
        }
        if row.status == "DELETED" {
            t.conflict(id, "DELETED_IN_DEVELOPER_PLATFORM");
            return Ok(());
        }
        let carried = needs_secret.then(|| carry(ring, transfer, s));
        self.update_subscription(db, s, &types, carried.as_ref(), now)
            .await?;
        let outcome = match &carried {
            Some(c) => c.outcome(),
            None => "TRANSFERRED",
        };
        self.map_subscription(db, s, s.version, outcome, run_id, now)
            .await?;
        t.bump("updated");
        if matches!(carried, Some(Carried::Rotation(_))) {
            t.bump("rotationRequired");
        }
        Ok(())
    }

    /// Moves a migrated subscription to Calling's newer state (`carried`: a new secret, or
    /// a required rotation; `None`: the secret is unchanged).
    async fn update_subscription<C: ConnectionTrait>(
        &self,
        db: &C,
        s: &CallingSubscription,
        types: &[String],
        carried: Option<&Carried>,
        now: DateTime<Utc>,
    ) -> Result<(), ApiError> {
        let (secret_change, sealed, kid) = match carried {
            None => ("KEEP", None, None),
            Some(Carried::Sealed { sealed, kid }) => {
                ("VALID", Some(sealed.clone()), Some(kid.clone()))
            }
            Some(Carried::Rotation(_)) => ("ROTATION_REQUIRED", None, None),
        };
        db.execute(stmt(
            "UPDATE devplat_webhook_subscriptions SET url = $2, description = $3, event_types = $4, \
               secret_state = CASE WHEN $5 = 'KEEP' THEN secret_state ELSE $5 END, \
               secret_sealed = CASE WHEN $5 = 'KEEP' THEN secret_sealed ELSE $6 END, \
               secret_key_id = CASE WHEN $5 = 'KEEP' THEN secret_key_id ELSE $7 END, \
               status = CASE WHEN $8 = 'ACTIVE' AND ((CASE WHEN $5 = 'KEEP' THEN secret_state ELSE $5 END) \
                             = 'ROTATION_REQUIRED') THEN 'DISABLED' ELSE $8 END, \
               disabled_reason = CASE WHEN $8 <> 'DELETED' AND ((CASE WHEN $5 = 'KEEP' THEN secret_state \
                             ELSE $5 END) = 'ROTATION_REQUIRED') THEN $12 ELSE NULL END, \
               secret_version = GREATEST(secret_version, $9), version = $10, updated_at = $11, \
               deleted_at = $13 \
             WHERE id = $1",
            vec![
                s.webhook_id.into(),
                s.url.clone().into(),
                s.description.clone().into(),
                types.to_vec().into(),
                secret_change.into(),
                sealed.into(),
                kid.into(),
                s.status.clone().into(),
                s.secret_version.into(),
                s.version.into(),
                s.updated_at.into(),
                ROTATION_REASON.into(),
                s.deleted_at.into(),
            ],
        ))
        .await?;
        if s.status == "DELETED" {
            db.execute(stmt(
                "UPDATE devplat_webhook_deliveries SET status = 'CANCELLED', completed_at = $2, \
                   lease_until = NULL, updated_at = $2 \
                 WHERE subscription_id = $1 AND NOT migration_hold \
                   AND status IN ('PENDING', 'RETRY_SCHEDULED', 'DELIVERING')",
                vec![s.webhook_id.into(), now.into()],
            ))
            .await?;
        }
        Ok(())
    }

    async fn map_subscription<C: ConnectionTrait>(
        &self,
        db: &C,
        s: &CallingSubscription,
        imported_version: i64,
        outcome: &str,
        run_id: Option<Uuid>,
        now: DateTime<Utc>,
    ) -> Result<(), ApiError> {
        db.execute(stmt(
            "INSERT INTO devplat_webhook_migration_subscriptions (subscription_id, calling_version, \
               calling_status, calling_secret_version, imported_version, secret_outcome, run_id, \
               imported_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8) \
             ON CONFLICT (subscription_id) DO UPDATE SET calling_version = EXCLUDED.calling_version, \
               calling_status = EXCLUDED.calling_status, \
               calling_secret_version = EXCLUDED.calling_secret_version, \
               imported_version = EXCLUDED.imported_version, secret_outcome = EXCLUDED.secret_outcome, \
               run_id = EXCLUDED.run_id, updated_at = EXCLUDED.updated_at, \
               rotation_acknowledged_by = CASE WHEN EXCLUDED.secret_outcome = 'TRANSFERRED' THEN NULL \
                 ELSE devplat_webhook_migration_subscriptions.rotation_acknowledged_by END, \
               rotation_acknowledged_at = CASE WHEN EXCLUDED.secret_outcome = 'TRANSFERRED' THEN NULL \
                 ELSE devplat_webhook_migration_subscriptions.rotation_acknowledged_at END",
            vec![
                s.webhook_id.into(),
                s.version.into(),
                s.status.clone().into(),
                s.secret_version.into(),
                imported_version.into(),
                outcome.into(),
                run_id.into(),
                now.into(),
            ],
        ))
        .await?;
        Ok(())
    }

    async fn apply_event<C: ConnectionTrait>(
        &self,
        db: &C,
        e: &CallingEvent,
        t: &mut Tally<'_>,
    ) -> Result<(), ApiError> {
        let valid_id = e.event_id.len() == 36
            && e.event_id.starts_with("whe_")
            && e.event_id[4..]
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase());
        if !valid_id
            || !valid_token(&e.source_event_id)
            || e.body.get("id").and_then(Value::as_str) != Some(e.event_id.as_str())
            || e.body.get("type").and_then(Value::as_str) != Some(e.event_type.as_str())
            || !e.body.get("data").is_some_and(Value::is_object)
        {
            t.conflict(&e.event_id, "INVALID_RECORD");
            return Ok(());
        }
        let inserted = db
            .execute(stmt(
                "INSERT INTO devplat_webhook_events (event_id, producer, source_event_id, event_type, \
                   tenant_id, application_id, environment_id, occurred_at, payload, received_at, origin) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'MIGRATED_FROM_CALLING') \
                 ON CONFLICT (event_id) DO NOTHING",
                vec![
                    e.event_id.clone().into(),
                    e.producer.clone().into(),
                    e.source_event_id.clone().into(),
                    e.event_type.clone().into(),
                    e.tenant_id.into(),
                    e.application_id.into(),
                    e.environment_id.into(),
                    e.occurred_at.into(),
                    e.body.clone().into(),
                    e.created_at.into(),
                ],
            ))
            .await?
            .rows_affected();
        if inserted == 1 {
            t.bump("inserted");
            return Ok(());
        }
        let origin = db
            .query_one(stmt(
                "SELECT origin FROM devplat_webhook_events WHERE event_id = $1",
                vec![e.event_id.clone().into()],
            ))
            .await?
            .map(|r| r.try_get_by_index::<String>(0))
            .transpose()?;
        if origin.as_deref() == Some(ORIGIN_MIGRATED) {
            t.bump("unchanged");
        } else {
            t.conflict(&e.event_id, "ID_COLLISION");
        }
        Ok(())
    }

    async fn apply_delivery<C: ConnectionTrait>(
        &self,
        db: &C,
        d: &CallingDelivery,
        t: &mut Tally<'_>,
    ) -> Result<(), ApiError> {
        let id = d.delivery_id;
        let refs = db
            .query_one(stmt(
                "SELECT EXISTS (SELECT 1 FROM devplat_webhook_migration_subscriptions m \
                                  JOIN devplat_webhook_subscriptions s ON s.id = m.subscription_id \
                                 WHERE m.subscription_id = $1 AND s.tenant_id = $3 \
                                   AND s.application_id = $4 AND s.environment_id = $5), \
                        EXISTS (SELECT 1 FROM devplat_webhook_events WHERE event_id = $2 \
                                   AND origin = 'MIGRATED_FROM_CALLING'), \
                        ($6::uuid IS NULL OR EXISTS (SELECT 1 FROM devplat_webhook_deliveries \
                                                       WHERE id = $6))",
                vec![
                    d.webhook_id.into(),
                    d.event_id.clone().into(),
                    d.tenant_id.into(),
                    d.application_id.into(),
                    d.environment_id.into(),
                    d.replay_of.into(),
                ],
            ))
            .await?
            .ok_or(ApiError(ErrorCode::InternalError))?;
        let (sub_ok, event_ok, replay_ok): (bool, bool, bool) = (
            refs.try_get_by_index(0)?,
            refs.try_get_by_index(1)?,
            refs.try_get_by_index(2)?,
        );
        let refusal = if !sub_ok {
            Some("SUBSCRIPTION_NOT_IMPORTED")
        } else if !event_ok {
            Some("EVENT_NOT_IMPORTED")
        } else if !replay_ok {
            Some("REPLAY_TARGET_MISSING")
        } else if !matches!(
            d.status.as_str(),
            "PENDING" | "DELIVERING" | "RETRY_SCHEDULED" | "SUCCEEDED" | "FAILED" | "CANCELLED"
        ) || !(0..=64).contains(&d.attempt_count)
        {
            Some("INVALID_RECORD")
        } else {
            None
        };
        if let Some(reason) = refusal {
            t.conflict(id, reason);
            return Ok(());
        }
        let open = is_open(&d.status);
        let lease = (d.status == "DELIVERING").then(|| d.lease_until.unwrap_or(d.next_attempt_at));
        let completed = (!open).then(|| d.completed_at.unwrap_or(d.updated_at));
        let existing = ExistingDelivery::find_by_statement(stmt(
            "SELECT origin, migration_hold, status, attempt_count, next_attempt_at, lease_until, \
                    last_http_status, failure_category, completed_at \
               FROM devplat_webhook_deliveries WHERE id = $1 FOR UPDATE",
            vec![id.into()],
        ))
        .one(db)
        .await?;
        let Some(row) = existing else {
            db.execute(stmt(
                "INSERT INTO devplat_webhook_deliveries (id, subscription_id, event_id, tenant_id, \
                   application_id, environment_id, replay_of, status, attempt_count, next_attempt_at, \
                   lease_until, last_http_status, failure_category, created_at, updated_at, \
                   completed_at, origin, migration_hold) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, \
                         'MIGRATED_FROM_CALLING', $17)",
                vec![
                    id.into(),
                    d.webhook_id.into(),
                    d.event_id.clone().into(),
                    d.tenant_id.into(),
                    d.application_id.into(),
                    d.environment_id.into(),
                    d.replay_of.into(),
                    d.status.clone().into(),
                    d.attempt_count.into(),
                    d.next_attempt_at.into(),
                    lease.into(),
                    d.last_http_status.into(),
                    d.failure_category.clone().into(),
                    d.created_at.into(),
                    d.updated_at.into(),
                    completed.into(),
                    open.into(),
                ],
            ))
            .await?;
            t.bump("inserted");
            if open {
                t.bump("held");
            }
            return Ok(());
        };
        if row.origin != ORIGIN_MIGRATED {
            t.conflict(id, "ID_COLLISION");
            return Ok(());
        }
        let same = row.status == d.status
            && row.attempt_count == d.attempt_count
            && row.next_attempt_at == d.next_attempt_at
            && row.lease_until == lease
            && row.last_http_status == d.last_http_status
            && row.failure_category == d.failure_category
            && row.completed_at == completed;
        if !row.migration_hold {
            // Settled history, or released to this platform by the takeover.
            t.bump(if same { "unchanged" } else { "dpOwned" });
            return Ok(());
        }
        if same {
            t.bump("unchanged");
            t.bump("held");
            return Ok(());
        }
        if d.attempt_count < row.attempt_count {
            t.conflict(id, "ATTEMPTS_REGRESSED");
            return Ok(());
        }
        db.execute(stmt(
            "UPDATE devplat_webhook_deliveries SET status = $2, attempt_count = $3, \
               next_attempt_at = $4, lease_until = $5, last_http_status = $6, failure_category = $7, \
               completed_at = $8, updated_at = $9, migration_hold = $10 \
             WHERE id = $1 AND migration_hold",
            vec![
                id.into(),
                d.status.clone().into(),
                d.attempt_count.into(),
                d.next_attempt_at.into(),
                lease.into(),
                d.last_http_status.into(),
                d.failure_category.clone().into(),
                completed.into(),
                d.updated_at.into(),
                open.into(),
            ],
        ))
        .await?;
        t.bump("updated");
        if open {
            t.bump("held");
        }
        Ok(())
    }

    async fn apply_attempt<C: ConnectionTrait>(
        &self,
        db: &C,
        a: &CallingAttempt,
        t: &mut Tally<'_>,
    ) -> Result<(), ApiError> {
        let key = format!("{}:{}", a.delivery_id, a.attempt_number);
        let migrated = db
            .query_one(stmt(
                "SELECT origin = 'MIGRATED_FROM_CALLING' FROM devplat_webhook_deliveries WHERE id = $1",
                vec![a.delivery_id.into()],
            ))
            .await?
            .map(|r| r.try_get_by_index::<bool>(0))
            .transpose()?;
        if migrated != Some(true) {
            t.conflict(key, "DELIVERY_NOT_IMPORTED");
            return Ok(());
        }
        if !(1..=64).contains(&a.attempt_number)
            || !matches!(a.outcome.as_str(), "SUCCEEDED" | "RETRY" | "FAILED")
        {
            t.conflict(key, "INVALID_RECORD");
            return Ok(());
        }
        let inserted = db
            .execute(stmt(
                "INSERT INTO devplat_webhook_attempts (delivery_id, attempt_number, started_at, \
                   finished_at, http_status, outcome, error_class) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT DO NOTHING",
                vec![
                    a.delivery_id.into(),
                    a.attempt_number.into(),
                    a.started_at.into(),
                    a.finished_at.max(a.started_at).into(),
                    a.http_status.into(),
                    a.outcome.clone().into(),
                    a.failure_category.clone().into(),
                ],
            ))
            .await?
            .rows_affected();
        t.bump(if inserted == 1 {
            "inserted"
        } else {
            "unchanged"
        });
        Ok(())
    }

    /// The dry run: every Calling page applied inside ONE transaction that is rolled back.
    /// Returns what an import would do (inserted / updated / unchanged / conflicts /
    /// rotation-required) — nothing is written.
    pub async fn inventory(&self, page_size: u32) -> Result<Value, ApiError> {
        self.ring()?;
        self.transfer()?;
        let calling = self.calling_inventory().await?;
        let mut counts = empty_counts();
        let mut conflicts = Vec::new();
        let txn = self.db.begin().await?;
        for entity in Entity::ALL {
            let mut cursor: Option<String> = None;
            loop {
                let page = self
                    .calling
                    .page(entity, cursor.as_deref(), page_size, false)
                    .await
                    .map_err(source_error)?;
                let kinds = if entity == Entity::Subscriptions {
                    self.resolve_kinds(
                        &records::<CallingSubscription>(&page).map_err(source_error)?,
                    )
                    .await
                } else {
                    HashMap::new()
                };
                self.apply_page(
                    &txn,
                    entity,
                    &page,
                    &kinds,
                    None,
                    &mut counts,
                    &mut conflicts,
                )
                .await?;
                match page.next_cursor {
                    Some(c) => cursor = Some(c),
                    None => break,
                }
            }
        }
        txn.rollback().await?;
        tracing::info!(operator = %self.operator, "webhook migration dry run (nothing written)");
        Ok(json!({
            "dryRun": true,
            "calling": calling,
            "wouldImport": counts,
            "rotationRequired": counts["subscriptions"]["rotationRequired"],
            "conflicts": conflicts,
        }))
    }

    /// Imports Calling's records: resumes the open run (crash or failure) from its
    /// checkpoint, else starts a new full run. Idempotent: ids are Calling's, every write
    /// an upsert; a completed run imported again changes nothing.
    pub async fn import(&self, opts: ImportOptions) -> Result<Value, ApiError> {
        self.ring()?;
        self.transfer()?;
        if !(1..=500).contains(&opts.page_size) {
            return Err(ApiError(ErrorCode::ValidationError));
        }
        let calling = self.calling_inventory().await?;
        let snapshot = json!({
            "at": calling.get("generatedAt").cloned().unwrap_or(Value::Null),
            "mutationsFrozen": calling.pointer("/flags/mutationsFrozen").cloned().unwrap_or(Value::Null),
            "deliveryEnabled": calling.pointer("/flags/deliveryEnabled").cloned().unwrap_or(Value::Null),
            "inFlight": calling.pointer("/deliveries/inFlight").cloned().unwrap_or(Value::Null),
        });
        let now = self.now();
        let open = RunRow::find_by_statement(stmt(
            &format!("SELECT {RUN_COLUMNS} FROM devplat_webhook_migration_runs WHERE state <> 'COMPLETED'"),
            vec![],
        ))
        .one(self.db)
        .await?;
        let (run_id, resumed) = match open {
            Some(r) => {
                self.db
                    .execute(stmt(
                        "UPDATE devplat_webhook_migration_runs SET state = 'RUNNING', last_error = NULL, \
                           updated_at = $2 WHERE id = $1 AND state <> 'COMPLETED'",
                        vec![r.id.into(), now.into()],
                    ))
                    .await?;
                (r.id, true)
            }
            None => {
                let id = Uuid::new_v4();
                let txn = self.db.begin().await?;
                let inserted = txn
                    .execute(stmt(
                        "INSERT INTO devplat_webhook_migration_runs (id, state, phase, counts, \
                           calling_start, operator, started_at, updated_at) \
                         VALUES ($1, 'RUNNING', 'SUBSCRIPTIONS', $2, $3, $4, $5, $5) \
                         ON CONFLICT DO NOTHING",
                        vec![
                            id.into(),
                            empty_counts().into(),
                            snapshot.clone().into(),
                            self.operator.into(),
                            now.into(),
                        ],
                    ))
                    .await?
                    .rows_affected();
                if inserted != 1 {
                    // Another operator opened a run concurrently.
                    return Err(ApiError(ErrorCode::VersionConflict));
                }
                txn.execute(stmt(
                    "INSERT INTO devplat_webhook_migration_state (id, phase, changed_by, changed_at) \
                     VALUES (1, 'PREPARING', $1, $2) ON CONFLICT (id) DO NOTHING",
                    vec![self.operator.into(), now.into()],
                ))
                .await?;
                self.audit(
                    &txn,
                    "import_started",
                    Some(id),
                    json!({ "calling": snapshot }),
                )
                .await?;
                txn.commit().await?;
                (id, false)
            }
        };
        if resumed {
            self.audit(
                self.db,
                "import_resumed",
                Some(run_id),
                json!({ "calling": snapshot }),
            )
            .await?;
        }
        let mut committed = 0u32;
        loop {
            let run = self.run(run_id).await?;
            let Some(entity) = Entity::from_phase(&run.phase) else {
                break;
            };
            let cursor = run.cursors[entity.as_str()].as_str().map(str::to_string);
            let page = match self
                .calling
                .page(entity, cursor.as_deref(), opts.page_size, false)
                .await
            {
                Ok(p) => p,
                Err(e) => {
                    self.fail_run(run_id, e.code()).await?;
                    return Err(source_error(e));
                }
            };
            let kinds = if entity == Entity::Subscriptions {
                match records::<CallingSubscription>(&page) {
                    Ok(subs) => self.resolve_kinds(&subs).await,
                    Err(e) => {
                        self.fail_run(run_id, e.code()).await?;
                        return Err(source_error(e));
                    }
                }
            } else {
                HashMap::new()
            };
            let txn = self.db.begin().await?;
            let locked = RunRow::find_by_statement(stmt(
                &format!("SELECT {RUN_COLUMNS} FROM devplat_webhook_migration_runs WHERE id = $1 FOR UPDATE"),
                vec![run_id.into()],
            ))
            .one(&txn)
            .await?
            .ok_or(ApiError(ErrorCode::InternalError))?;
            if locked.phase != run.phase
                || locked.cursors[entity.as_str()].as_str() != cursor.as_deref()
                || locked.state == "COMPLETED"
            {
                // Another importer advanced the run: never apply a page twice.
                txn.rollback().await?;
                return Err(ApiError(ErrorCode::VersionConflict));
            }
            let mut counts = locked.counts.clone();
            if !counts.is_object() || counts.as_object().is_some_and(|m| m.is_empty()) {
                counts = empty_counts();
            }
            let mut conflicts: Vec<Value> =
                locked.conflicts.as_array().cloned().unwrap_or_default();
            self.apply_page(
                &txn,
                entity,
                &page,
                &kinds,
                Some(run_id),
                &mut counts,
                &mut conflicts,
            )
            .await?;
            let mut cursors = locked.cursors.clone();
            if !cursors.is_object() {
                cursors = json!({});
            }
            let (phase, next) = match &page.next_cursor {
                Some(c) => (entity.phase(), Value::String(c.clone())),
                None => (
                    entity.next().map(Entity::phase).unwrap_or("DONE"),
                    Value::Null,
                ),
            };
            cursors[entity.as_str()] = next;
            let done = phase == "DONE";
            let now = self.now();
            txn.execute(stmt(
                "UPDATE devplat_webhook_migration_runs SET phase = $2, cursors = $3, counts = $4, \
                   conflicts = $5, pages = pages + 1, state = $6, last_error = NULL, updated_at = $7, \
                   finished_at = CASE WHEN $6 = 'COMPLETED' THEN $7 END WHERE id = $1",
                vec![
                    run_id.into(),
                    phase.into(),
                    cursors.into(),
                    counts.clone().into(),
                    Value::Array(conflicts.clone()).into(),
                    (if done { "COMPLETED" } else { "RUNNING" }).into(),
                    now.into(),
                ],
            ))
            .await?;
            if done {
                self.audit(
                    &txn,
                    "import_completed",
                    Some(run_id),
                    json!({ "counts": counts, "conflicts": conflicts.len() }),
                )
                .await?;
            }
            txn.commit().await?;
            committed += 1;
            if done {
                break;
            }
            if opts.fail_after_pages.is_some_and(|n| committed >= n) {
                tracing::warn!(run_id = %run_id, committed, "webhook migration import interrupted (simulated crash)");
                return Err(ApiError(ErrorCode::ServiceUnavailable));
            }
        }
        let mut view = self.run(run_id).await?.view();
        view["resumed"] = Value::Bool(resumed);
        Ok(view)
    }

    async fn run(&self, id: Uuid) -> Result<RunRow, ApiError> {
        RunRow::find_by_statement(stmt(
            &format!("SELECT {RUN_COLUMNS} FROM devplat_webhook_migration_runs WHERE id = $1"),
            vec![id.into()],
        ))
        .one(self.db)
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))
    }

    async fn fail_run(&self, id: Uuid, code: &str) -> Result<(), ApiError> {
        self.db
            .execute(stmt(
                "UPDATE devplat_webhook_migration_runs SET state = 'FAILED', last_error = $2, \
                   updated_at = $3 WHERE id = $1 AND state = 'RUNNING'",
                vec![id.into(), code.into(), self.now().into()],
            ))
            .await?;
        Ok(())
    }
}

pub(crate) const RUN_COLUMNS: &str = "id, state, phase, cursors, counts, conflicts, pages, \
    calling_start, last_error, operator, started_at, updated_at, finished_at";

#[derive(Debug, Clone, FromQueryResult)]
pub(crate) struct RunRow {
    pub id: Uuid,
    pub state: String,
    pub phase: String,
    pub cursors: Value,
    pub counts: Value,
    pub conflicts: Value,
    pub pages: i32,
    pub calling_start: Value,
    pub last_error: Option<String>,
    pub operator: String,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

impl RunRow {
    pub fn view(&self) -> Value {
        json!({
            "runId": self.id,
            "state": self.state,
            "phase": self.phase,
            "pages": self.pages,
            "counts": self.counts,
            "conflicts": self.conflicts,
            "callingAtStart": self.calling_start,
            "lastError": self.last_error,
            "operator": self.operator,
            "startedAt": super::exact(self.started_at),
            "updatedAt": super::exact(self.updated_at),
            "finishedAt": super::exact_opt(self.finished_at),
        })
    }
}

/// The most recent run (any state).
pub(crate) async fn latest_run<C: ConnectionTrait>(db: &C) -> Result<Option<RunRow>, ApiError> {
    Ok(RunRow::find_by_statement(stmt(
        &format!(
            "SELECT {RUN_COLUMNS} FROM devplat_webhook_migration_runs ORDER BY started_at DESC, id LIMIT 1"
        ),
        vec![],
    ))
    .one(db)
    .await?)
}
