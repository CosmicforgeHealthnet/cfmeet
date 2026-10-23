//! Shadow parity, cutover preconditions, rotation acknowledgements, the takeover (the
//! delivery switch) and the rollback (subscription export back to Calling and settlement
//! of deliveries already delivered here). The cutover itself is configuration, in the
//! order the runbook gives; these commands verify and complete each step.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::port::{records, CallingDelivery, CallingEvent, Entity};
use super::{
    import::latest_run, phase, set_phase, source_error, transfer_owner, Migration, Plain,
    MAX_LISTED, ROLLBACK_PURPOSE,
};
use crate::{
    control::stmt,
    http::error::{ApiError, ErrorCode},
    webhooks::DeliveryMode,
};

/// (subscription, producer, source event id): one logical delivery on either side.
type Pair = (Uuid, String, String);

#[derive(Debug, FromQueryResult)]
struct NativeDelivery {
    subscription_id: Uuid,
    producer: String,
    source_event_id: String,
    status: String,
    attempts: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Acknowledge {
    pub webhook_ids: Vec<Uuid>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RollbackOptions {
    /// Proceed although deliveries exist here for events Calling never recorded (they
    /// stay recorded here, unsent).
    #[serde(default)]
    pub accept_orphans: bool,
}

fn flag(v: &Value, pointer: &str) -> Option<bool> {
    v.pointer(pointer).and_then(Value::as_bool)
}

impl Migration<'_> {
    async fn calling_all<T: serde::de::DeserializeOwned>(
        &self,
        entity: Entity,
        open_only: bool,
    ) -> Result<Vec<T>, ApiError> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let page = self
                .calling
                .page(entity, cursor.as_deref(), 500, open_only)
                .await
                .map_err(source_error)?;
            out.extend(records::<T>(&page).map_err(source_error)?);
            match page.next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        Ok(out)
    }

    /// Native original deliveries (to migrated subscriptions) since `since`.
    async fn native_deliveries(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<NativeDelivery>, ApiError> {
        Ok(NativeDelivery::find_by_statement(stmt(
            "SELECT d.subscription_id, e.producer, e.source_event_id, d.status, \
                    (SELECT count(*) FROM devplat_webhook_attempts a WHERE a.delivery_id = d.id) AS attempts \
               FROM devplat_webhook_deliveries d \
               JOIN devplat_webhook_events e ON e.event_id = d.event_id AND e.origin = 'NATIVE' \
               JOIN devplat_webhook_migration_subscriptions m ON m.subscription_id = d.subscription_id \
              WHERE d.origin = 'NATIVE' AND d.replay_of IS NULL \
                AND ($1::timestamptz IS NULL OR d.created_at >= $1)",
            vec![since.into()],
        ))
        .all(self.db)
        .await?)
    }

    /// Dual-ingestion parity of the shadow window: for every event BOTH services ingested,
    /// each (subscription, event) Calling delivered has a native delivery here and vice
    /// versa, and no native delivery to a migrated subscription was actually sent.
    pub async fn shadow_report(&self, since: Option<DateTime<Utc>>) -> Result<Value, ApiError> {
        let events: Vec<CallingEvent> = self.calling_all(Entity::Events, false).await?;
        let calling_sources: HashSet<(String, String)> = events
            .iter()
            .filter(|e| since.is_none_or(|s| e.created_at >= s))
            .map(|e| (e.producer.clone(), e.source_event_id.clone()))
            .collect();
        let deliveries: Vec<CallingDelivery> = self.calling_all(Entity::Deliveries, false).await?;
        let native = self.native_deliveries(since).await?;
        let native_sources: HashSet<(String, String)> = native
            .iter()
            .map(|d| (d.producer.clone(), d.source_event_id.clone()))
            .collect();
        let both: HashSet<&(String, String)> =
            calling_sources.intersection(&native_sources).collect();
        let calling_pairs: HashMap<Pair, String> = deliveries
            .iter()
            .filter(|d| d.replay_of.is_none())
            .filter(|d| both.contains(&(d.producer.clone(), d.source_event_id.clone())))
            .map(|d| {
                (
                    (d.webhook_id, d.producer.clone(), d.source_event_id.clone()),
                    d.status.clone(),
                )
            })
            .collect();
        let native_pairs: HashMap<Pair, &NativeDelivery> = native
            .iter()
            .filter(|d| both.contains(&(d.producer.clone(), d.source_event_id.clone())))
            .map(|d| {
                (
                    (
                        d.subscription_id,
                        d.producer.clone(),
                        d.source_event_id.clone(),
                    ),
                    d,
                )
            })
            .collect();
        let pair_json =
            |p: &Pair| json!({ "webhookId": p.0, "producer": p.1, "sourceEventId": p.2 });
        let missing_here: Vec<Value> = calling_pairs
            .keys()
            .filter(|p| !native_pairs.contains_key(*p))
            .take(MAX_LISTED)
            .map(pair_json)
            .collect();
        let missing_in_calling: Vec<Value> = native_pairs
            .keys()
            .filter(|p| !calling_pairs.contains_key(*p))
            .take(MAX_LISTED)
            .map(pair_json)
            .collect();
        let missing_here_count = calling_pairs
            .keys()
            .filter(|p| !native_pairs.contains_key(*p))
            .count();
        let missing_in_calling_count = native_pairs
            .keys()
            .filter(|p| !calling_pairs.contains_key(*p))
            .count();
        let sent = native.iter().filter(|d| d.attempts > 0).count();
        let not_shadow = native
            .iter()
            .filter(|d| d.status != "SHADOW" && d.status != "PENDING")
            .count();
        let parity = missing_here_count == 0 && missing_in_calling_count == 0 && sent == 0;
        tracing::info!(operator = %self.operator, parity, "webhook migration shadow report");
        Ok(json!({
            "since": since.map(super::exact),
            "eventsComparedBothSides": both.len(),
            "eventsOnlyInCalling": calling_sources.difference(&native_sources).count(),
            "eventsOnlyInDeveloperPlatform": native_sources.difference(&calling_sources).count(),
            "pairsInCalling": calling_pairs.len(),
            "pairsInDeveloperPlatform": native_pairs.len(),
            "missingInDeveloperPlatformCount": missing_here_count,
            "missingInDeveloperPlatform": missing_here,
            "missingInCallingCount": missing_in_calling_count,
            "missingInCalling": missing_in_calling,
            "developerPlatformDeliveriesSent": sent,
            "developerPlatformDeliveriesNotShadow": not_shadow,
            "parity": parity,
        }))
    }

    async fn unacknowledged_rotations(&self) -> Result<i64, ApiError> {
        Ok(self
            .db
            .query_one(stmt(
                "SELECT count(*) FROM devplat_webhook_migration_subscriptions m \
                   JOIN devplat_webhook_subscriptions s ON s.id = m.subscription_id \
                  WHERE s.secret_state = 'ROTATION_REQUIRED' AND s.status <> 'DELETED' \
                    AND m.rotation_acknowledged_at IS NULL",
                vec![],
            ))
            .await?
            .ok_or(ApiError(ErrorCode::InternalError))?
            .try_get_by_index::<i64>(0)?)
    }

    /// The cutover preconditions: Calling frozen (and this platform's mutations too), the
    /// last import completed without conflicts, reconcile clean, shadow parity, every
    /// required rotation acknowledged, the transfer key configured.
    pub async fn cutover_check(&self) -> Result<Value, ApiError> {
        let calling = self.calling_inventory().await?;
        let run = latest_run(self.db).await?;
        let reconcile = self.reconcile().await?;
        let shadow = self.shadow_report(None).await?;
        let rotations = self.unacknowledged_rotations().await?;
        let completed = run.as_ref().is_some_and(|r| r.state == "COMPLETED");
        let conflicts = run
            .as_ref()
            .and_then(|r| r.conflicts.as_array().map(Vec::len))
            .unwrap_or(0);
        let check = |name: &str, ok: bool, detail: Value| json!({ "name": name, "ok": ok, "detail": detail });
        let checks = vec![
            check(
                "callingMutationsFrozen",
                flag(&calling, "/flags/mutationsFrozen") == Some(true),
                calling["flags"].clone(),
            ),
            check(
                "developerPlatformMutationsFrozen",
                self.webhooks.settings().mutations_frozen,
                json!(null),
            ),
            check(
                "importCompleted",
                completed,
                run.as_ref()
                    .map(|r| json!({ "runId": r.id, "state": r.state, "phase": r.phase }))
                    .unwrap_or(Value::Null),
            ),
            check(
                "importWithoutConflicts",
                completed && conflicts == 0,
                json!(conflicts),
            ),
            check(
                "reconcileClean",
                reconcile["clean"] == true,
                json!({ "driftCount": reconcile["driftCount"] }),
            ),
            check(
                "shadowParity",
                shadow["parity"] == true,
                json!({
                    "missingInDeveloperPlatform": shadow["missingInDeveloperPlatformCount"],
                    "missingInCalling": shadow["missingInCallingCount"],
                    "sent": shadow["developerPlatformDeliveriesSent"],
                }),
            ),
            check("rotationsAcknowledged", rotations == 0, json!(rotations)),
            check(
                "transferKeyConfigured",
                self.webhooks.settings().transfer_key.is_some(),
                json!(null),
            ),
        ];
        let ready = checks.iter().all(|c| c["ok"] == true);
        tracing::info!(operator = %self.operator, ready, "webhook migration cutover check");
        Ok(json!({
            "ready": ready,
            "checks": checks,
            "info": {
                "callingDeliveryEnabled": flag(&calling, "/flags/deliveryEnabled"),
                "callingInFlight": calling.pointer("/deliveries/inFlight"),
                "developerPlatformDeliveryMode": self.webhooks.settings().mode.as_str(),
                "phase": phase(self.db).await?,
            },
        }))
    }

    /// Records the operator's acknowledgement that the named subscriptions must be rotated
    /// by their customers (no secret was carried over; nothing is substituted).
    pub async fn acknowledge(&self, body: Acknowledge) -> Result<Value, ApiError> {
        if body.webhook_ids.is_empty() || body.webhook_ids.len() > 500 {
            return Err(ApiError(ErrorCode::ValidationError));
        }
        let txn = self.db.begin().await?;
        let n = txn
            .execute(stmt(
                "UPDATE devplat_webhook_migration_subscriptions SET rotation_acknowledged_by = $2, \
                   rotation_acknowledged_at = $3, updated_at = $3 \
                 WHERE subscription_id = ANY($1) AND secret_outcome LIKE 'ROTATION_REQUIRED%' \
                   AND rotation_acknowledged_at IS NULL",
                vec![
                    body.webhook_ids.clone().into(),
                    self.operator.into(),
                    self.now().into(),
                ],
            ))
            .await?
            .rows_affected();
        self.audit(
            &txn,
            "rotation_acknowledged",
            None,
            json!({ "requested": body.webhook_ids.len(), "acknowledged": n }),
        )
        .await?;
        txn.commit().await?;
        Ok(json!({ "acknowledged": n }))
    }

    /// The delivery switch: verifies that Calling stopped sending (delivery flag off, no
    /// live claim, the last completed import started after that) and that this platform
    /// sends (`active`), releases the held mirrors of Calling's open deliveries and
    /// de-duplicates every logical delivery to exactly one sender, then moves to CUT_OVER.
    pub async fn takeover(&self) -> Result<Value, ApiError> {
        if self.webhooks.settings().mode != DeliveryMode::Active {
            return Err(ApiError(ErrorCode::InvalidState));
        }
        let calling = self.calling_inventory().await?;
        let run = latest_run(self.db).await?;
        let calling_stopped = flag(&calling, "/flags/deliveryEnabled") == Some(false)
            && calling
                .pointer("/deliveries/inFlight")
                .and_then(Value::as_i64)
                == Some(0);
        let final_import = run.as_ref().is_some_and(|r| {
            r.state == "COMPLETED" && flag(&r.calling_start, "/deliveryEnabled") == Some(false)
        });
        if !calling_stopped || !final_import {
            tracing::warn!(
                calling_stopped,
                final_import,
                "webhook migration takeover refused"
            );
            return Err(ApiError(ErrorCode::InvalidState));
        }
        let now = self.now();
        let txn = self.db.begin().await?;
        // 1. Held mirrors whose logical delivery this platform has natively: the mirror
        //    (Calling's ids, continuity) wins over a never-attempted native delivery; a
        //    native delivery already attempted wins over the mirror.
        let natives_won = txn
            .execute(stmt(
                "UPDATE devplat_webhook_deliveries h SET status = 'CANCELLED', migration_hold = FALSE, \
                   lease_until = NULL, completed_at = $1, updated_at = $1, failure_category = 'DUPLICATE_OF_NATIVE' \
                  FROM devplat_webhook_events he, devplat_webhook_deliveries n, devplat_webhook_events ne \
                 WHERE h.migration_hold AND h.replay_of IS NULL AND he.event_id = h.event_id \
                   AND n.origin = 'NATIVE' AND n.replay_of IS NULL AND n.subscription_id = h.subscription_id \
                   AND ne.event_id = n.event_id AND ne.origin = 'NATIVE' \
                   AND ne.producer = he.producer AND ne.source_event_id = he.source_event_id \
                   AND (n.status IN ('DELIVERING', 'RETRY_SCHEDULED', 'SUCCEEDED', 'FAILED') \
                        OR EXISTS (SELECT 1 FROM devplat_webhook_attempts a WHERE a.delivery_id = n.id))",
                vec![now.into()],
            ))
            .await?
            .rows_affected();
        let natives_cancelled = txn
            .execute(stmt(
                "UPDATE devplat_webhook_deliveries n SET status = 'CANCELLED', completed_at = $1, \
                   updated_at = $1, failure_category = 'DUPLICATE_OF_MIGRATED' \
                  FROM devplat_webhook_events ne, devplat_webhook_deliveries h, devplat_webhook_events he \
                 WHERE n.origin = 'NATIVE' AND n.replay_of IS NULL AND n.status = 'PENDING' \
                   AND NOT EXISTS (SELECT 1 FROM devplat_webhook_attempts a WHERE a.delivery_id = n.id) \
                   AND ne.event_id = n.event_id AND ne.origin = 'NATIVE' \
                   AND h.migration_hold AND h.replay_of IS NULL AND h.subscription_id = n.subscription_id \
                   AND he.event_id = h.event_id AND he.producer = ne.producer \
                   AND he.source_event_id = ne.source_event_id",
                vec![now.into()],
            ))
            .await?
            .rows_affected();
        // 2. Release every remaining held mirror: this platform continues Calling's
        //    delivery (same delivery id, same event id, attempts continued).
        let released = txn
            .execute(stmt(
                "UPDATE devplat_webhook_deliveries SET migration_hold = FALSE, updated_at = $1 \
                 WHERE migration_hold",
                vec![now.into()],
            ))
            .await?
            .rows_affected();
        // 3. Native deliveries to migrated subscriptions whose logical delivery Calling
        //    already settled (delivered, dead-lettered or cancelled there): never re-sent.
        let delivered_by_calling = txn
            .execute(stmt(
                "UPDATE devplat_webhook_deliveries n SET status = 'CANCELLED', completed_at = $1, \
                   updated_at = $1, failure_category = 'DELIVERED_BY_CALLING' \
                  FROM devplat_webhook_events ne, devplat_webhook_deliveries c, devplat_webhook_events ce \
                 WHERE n.origin = 'NATIVE' AND n.replay_of IS NULL AND n.status IN ('PENDING', 'RETRY_SCHEDULED') \
                   AND NOT EXISTS (SELECT 1 FROM devplat_webhook_attempts a WHERE a.delivery_id = n.id) \
                   AND ne.event_id = n.event_id AND ne.origin = 'NATIVE' \
                   AND c.origin = 'MIGRATED_FROM_CALLING' AND c.replay_of IS NULL \
                   AND c.subscription_id = n.subscription_id \
                   AND c.status IN ('SUCCEEDED', 'FAILED', 'CANCELLED') \
                   AND c.failure_category IS DISTINCT FROM 'DUPLICATE_OF_NATIVE' \
                   AND ce.event_id = c.event_id AND ce.producer = ne.producer \
                   AND ce.source_event_id = ne.source_event_id",
                vec![now.into()],
            ))
            .await?
            .rows_affected();
        set_phase(&txn, "CUT_OVER", self.operator, now).await?;
        let summary = json!({
            "released": released,
            "mirrorsCancelledForNative": natives_won,
            "nativesCancelledForMirror": natives_cancelled,
            "nativesDeliveredByCalling": delivered_by_calling,
            "runId": run.as_ref().map(|r| r.id),
        });
        self.audit(
            &txn,
            "takeover",
            run.as_ref().map(|r| r.id),
            summary.clone(),
        )
        .await?;
        txn.commit().await?;
        Ok(json!({ "phase": "CUT_OVER", "result": summary }))
    }

    /// Rollback export: every subscription changed here since Calling last had it (a newer
    /// version), and every subscription created here, is pushed back to Calling; a newer
    /// signing secret travels only transfer-sealed for the rollback direction.
    pub async fn rollback_export(&self) -> Result<Value, ApiError> {
        let ring = self.ring()?;
        let transfer = self.transfer()?;
        #[derive(Debug, FromQueryResult)]
        struct Row {
            id: Uuid,
            tenant_id: Uuid,
            application_id: Uuid,
            environment_id: Uuid,
            url: String,
            description: Option<String>,
            event_types: Vec<String>,
            status: String,
            secret_state: String,
            secret_sealed: Option<String>,
            secret_version: i32,
            version: i64,
            created_at: DateTime<Utc>,
            updated_at: DateTime<Utc>,
            deleted_at: Option<DateTime<Utc>>,
            mapped: bool,
            calling_secret_version: Option<i32>,
        }
        let rows = Row::find_by_statement(stmt(
            "SELECT s.id, s.tenant_id, s.application_id, s.environment_id, s.url, s.description, \
                    s.event_types, s.status, s.secret_state, s.secret_sealed, s.secret_version, s.version, \
                    s.created_at, s.updated_at, s.deleted_at, (m.subscription_id IS NOT NULL) AS mapped, \
                    m.calling_secret_version \
               FROM devplat_webhook_subscriptions s \
               LEFT JOIN devplat_webhook_migration_subscriptions m ON m.subscription_id = s.id \
              WHERE (m.subscription_id IS NOT NULL AND s.version > m.calling_version) \
                 OR (m.subscription_id IS NULL AND s.origin = 'NATIVE' AND s.status <> 'DELETED') \
              ORDER BY s.id",
            vec![],
        ))
        .all(self.db)
        .await?;
        let mut items = Vec::new();
        for r in &rows {
            // A rotation-required subscription keeps the status Calling can serve (it is
            // never re-enabled without a secret); its secret is never sent (none exists).
            let secret = (r.secret_state == "VALID"
                && (!r.mapped || r.secret_version > r.calling_secret_version.unwrap_or(0)))
            .then_some(r.secret_sealed.as_deref())
            .flatten()
            .and_then(|sealed| Plain::open(ring, sealed, &r.id.to_string()))
            .map(|plain| {
                json!({
                    "transferKeyId": transfer.current_kid(),
                    "transferSealed": transfer.seal(plain.expose(), &transfer_owner(ROLLBACK_PURPOSE, r.id)),
                })
            });
            items.push((
                r,
                json!({
                    "webhookId": r.id, "tenantId": r.tenant_id, "applicationId": r.application_id,
                    "environmentId": r.environment_id, "url": r.url, "description": r.description,
                    "eventTypes": r.event_types, "status": r.status, "secretVersion": r.secret_version,
                    "version": r.version, "createdAt": super::exact(r.created_at),
                    "updatedAt": super::exact(r.updated_at), "deletedAt": super::exact_opt(r.deleted_at),
                    "secret": secret,
                }),
            ));
        }
        let (mut applied, mut unchanged) = (0i64, 0i64);
        let mut conflicts = Vec::new();
        for chunk in items.chunks(100) {
            let out = self
                .calling
                .rollback_subscriptions(chunk.iter().map(|(_, v)| v.clone()).collect())
                .await
                .map_err(source_error)?;
            applied += out["applied"].as_i64().unwrap_or(0);
            unchanged += out["unchanged"].as_i64().unwrap_or(0);
            let refused: HashSet<String> = out["conflicts"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|c| c["webhookId"].as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            conflicts.extend(out["conflicts"].as_array().cloned().unwrap_or_default());
            let now = self.now();
            for (r, _) in chunk
                .iter()
                .filter(|(r, _)| !refused.contains(&r.id.to_string()))
            {
                // Calling now has this version: a later import treats it as known.
                self.db
                    .execute(stmt(
                        "INSERT INTO devplat_webhook_migration_subscriptions (subscription_id, \
                           calling_version, calling_status, calling_secret_version, imported_version, \
                           secret_outcome, imported_at, updated_at) \
                         VALUES ($1, $2, $3, $4, $2, 'PUSHED_TO_CALLING', $5, $5) \
                         ON CONFLICT (subscription_id) DO UPDATE SET calling_version = EXCLUDED.calling_version, \
                           calling_status = EXCLUDED.calling_status, \
                           calling_secret_version = GREATEST(devplat_webhook_migration_subscriptions.calling_secret_version, \
                             CASE WHEN $6 THEN EXCLUDED.calling_secret_version ELSE 0 END), \
                           imported_version = EXCLUDED.imported_version, updated_at = EXCLUDED.updated_at",
                        vec![
                            r.id.into(),
                            r.version.into(),
                            r.status.clone().into(),
                            r.secret_version.into(),
                            now.into(),
                            (r.secret_state == "VALID").into(),
                        ],
                    ))
                    .await?;
            }
        }
        let summary = json!({
            "candidates": items.len(), "applied": applied, "unchanged": unchanged,
            "conflicts": conflicts.len(),
        });
        self.audit(self.db, "rollback_exported", None, summary.clone())
            .await?;
        Ok(
            json!({ "candidates": items.len(), "applied": applied, "unchanged": unchanged, "conflicts": conflicts }),
        )
    }

    /// The rollback of the delivery switch: moves to ROLLED_BACK (nothing is sent here any
    /// more), settles at Calling every open delivery this platform already delivered, and
    /// hands every other open delivery back to Calling. Refused while Calling already
    /// delivers again or while a claim here is still live (retry once it expired).
    pub async fn rollback(&self, opts: RollbackOptions) -> Result<Value, ApiError> {
        if !matches!(
            phase(self.db).await?.as_deref(),
            Some("CUT_OVER") | Some("ROLLED_BACK")
        ) {
            return Err(ApiError(ErrorCode::InvalidState));
        }
        let calling = self.calling_inventory().await?;
        if flag(&calling, "/flags/deliveryEnabled") != Some(false) {
            return Err(ApiError(ErrorCode::InvalidState));
        }
        let now = self.now();
        set_phase(self.db, "ROLLED_BACK", self.operator, now).await?;
        let live: i64 = self
            .db
            .query_one(stmt(
                "SELECT count(*) FROM devplat_webhook_deliveries WHERE status = 'DELIVERING' AND lease_until > $1",
                vec![now.into()],
            ))
            .await?
            .ok_or(ApiError(ErrorCode::InternalError))?
            .try_get_by_index(0)?;
        if live > 0 {
            return Err(ApiError(ErrorCode::InvalidState));
        }
        let open: Vec<CallingDelivery> = self.calling_all(Entity::Deliveries, true).await?;
        #[derive(Debug, FromQueryResult)]
        struct Local {
            id: Uuid,
            subscription_id: Uuid,
            producer: String,
            source_event_id: String,
            status: String,
            origin: String,
            replay_of: Option<Uuid>,
        }
        let local = Local::find_by_statement(stmt(
            "SELECT d.id, d.subscription_id, e.producer, e.source_event_id, d.status, d.origin, d.replay_of \
               FROM devplat_webhook_deliveries d JOIN devplat_webhook_events e ON e.event_id = d.event_id \
              WHERE d.status <> 'SHADOW'",
            vec![],
        ))
        .all(self.db)
        .await?;
        let by_id: HashMap<Uuid, &Local> = local.iter().map(|l| (l.id, l)).collect();
        let delivered = |s: &str| matches!(s, "SUCCEEDED" | "FAILED");
        let native_delivered: HashSet<Pair> = local
            .iter()
            .filter(|l| l.origin == "NATIVE" && l.replay_of.is_none() && delivered(&l.status))
            .map(|l| {
                (
                    l.subscription_id,
                    l.producer.clone(),
                    l.source_event_id.clone(),
                )
            })
            .collect();
        let mut settle = Vec::new();
        let mut calling_open_pairs: HashSet<Pair> = HashSet::new();
        let mut calling_open_ids: HashSet<Uuid> = HashSet::new();
        for c in &open {
            let pair = (c.webhook_id, c.producer.clone(), c.source_event_id.clone());
            let done_here = by_id
                .get(&c.delivery_id)
                .is_some_and(|l| matches!(l.status.as_str(), "SUCCEEDED" | "FAILED" | "CANCELLED"))
                || (c.replay_of.is_none() && native_delivered.contains(&pair));
            if done_here {
                settle.push(c.delivery_id);
            } else {
                calling_open_ids.insert(c.delivery_id);
                if c.replay_of.is_none() {
                    calling_open_pairs.insert(pair);
                }
            }
        }
        let mut settled = 0i64;
        for chunk in settle.chunks(500) {
            let out = self
                .calling
                .settle_deliveries(chunk)
                .await
                .map_err(source_error)?;
            settled += out["settled"].as_i64().unwrap_or(0);
        }
        // Open deliveries here: handed back when Calling will deliver them, else orphans.
        let mut handed_back = Vec::new();
        let mut orphans = Vec::new();
        for l in local.iter().filter(|l| {
            matches!(
                l.status.as_str(),
                "PENDING" | "RETRY_SCHEDULED" | "DELIVERING"
            )
        }) {
            let pair = (
                l.subscription_id,
                l.producer.clone(),
                l.source_event_id.clone(),
            );
            if calling_open_ids.contains(&l.id)
                || (l.replay_of.is_none() && calling_open_pairs.contains(&pair))
            {
                handed_back.push(l.id);
            } else {
                orphans.push(l.id);
            }
        }
        if !orphans.is_empty() && !opts.accept_orphans {
            self.audit(
                self.db,
                "rollback_blocked",
                None,
                json!({ "settledInCalling": settled, "orphans": orphans.len() }),
            )
            .await?;
            return Ok(json!({
                "phase": "ROLLED_BACK", "completed": false, "settledInCalling": settled,
                "handedBack": 0, "orphanCount": orphans.len(),
                "orphans": orphans.iter().take(MAX_LISTED).collect::<Vec<_>>(),
            }));
        }
        let txn = self.db.begin().await?;
        let cancelled = txn
            .execute(stmt(
                "UPDATE devplat_webhook_deliveries SET status = 'CANCELLED', lease_until = NULL, \
                   migration_hold = FALSE, completed_at = $2, updated_at = $2, \
                   failure_category = 'HANDED_BACK_TO_CALLING' \
                 WHERE id = ANY($1) AND status IN ('PENDING', 'RETRY_SCHEDULED', 'DELIVERING')",
                vec![handed_back.clone().into(), now.into()],
            ))
            .await?
            .rows_affected();
        let summary = json!({
            "settledInCalling": settled, "handedBack": cancelled, "orphanCount": orphans.len(),
        });
        self.audit(&txn, "rolled_back", None, summary).await?;
        txn.commit().await?;
        Ok(json!({
            "phase": "ROLLED_BACK", "completed": true, "settledInCalling": settled,
            "handedBack": cancelled, "orphanCount": orphans.len(),
            "orphans": orphans.iter().take(MAX_LISTED).collect::<Vec<_>>(),
        }))
    }

    /// The migration status: phase, the latest run, migrated / held / rotation counts.
    pub async fn status(&self) -> Result<Value, ApiError> {
        let row = self
            .db
            .query_one(stmt(
                "SELECT (SELECT count(*) FROM devplat_webhook_migration_subscriptions), \
                        (SELECT count(*) FROM devplat_webhook_subscriptions WHERE secret_state = 'ROTATION_REQUIRED'), \
                        (SELECT count(*) FROM devplat_webhook_deliveries WHERE migration_hold), \
                        (SELECT count(*) FROM devplat_webhook_events WHERE origin = 'MIGRATED_FROM_CALLING'), \
                        (SELECT count(*) FROM devplat_webhook_deliveries WHERE origin = 'MIGRATED_FROM_CALLING')",
                vec![],
            ))
            .await?
            .ok_or(ApiError(ErrorCode::InternalError))?;
        let n = |i| row.try_get_by_index::<i64>(i);
        Ok(json!({
            "phase": phase(self.db).await?,
            "deliveryMode": self.webhooks.settings().mode.as_str(),
            "mutationsFrozen": self.webhooks.settings().mutations_frozen,
            "transferKeyId": self.webhooks.settings().transfer_key.as_deref().map(|k| k.current_kid()),
            "migratedSubscriptions": n(0)?,
            "rotationRequired": n(1)?,
            "heldDeliveries": n(2)?,
            "migratedEvents": n(3)?,
            "migratedDeliveries": n(4)?,
            "unacknowledgedRotations": self.unacknowledged_rotations().await?,
            "latestRun": latest_run(self.db).await?.map(|r| r.view()),
        }))
    }
}
