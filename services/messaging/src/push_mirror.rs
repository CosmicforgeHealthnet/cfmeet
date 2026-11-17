//! The push-registration facade towards the Notification Service (roadmap N9, Phase 4.1,
//! D-1b "copy path"; Document 5 §6 4.1).
//!
//! Off by default (`MESSAGING_PUSH_REGISTRATION_MIRROR` unset or `off`): Messaging behaves
//! exactly as before. With `on`:
//!
//! * **Copy.** At startup, and then every `MESSAGING_PUSH_REGISTRATION_MIRROR_INTERVAL_
//!   SECONDS` (default 60; a full pass every 6 h, incremental passes on `updated_at` in
//!   between), every push registration of CF Meet's personal scope
//!   (`CF_COMMUNICATIONS_PERSONAL_*`) is mirrored to the service with I13
//!   (`POST /internal/v1/push/registrations/sync`, permission
//!   `notifications-push-registration.sync`). The service writes only its own database
//!   and Messaging only reads its own: each service keeps its own store.
//! * **Facade.** Every registration write (register, unregister, device revocation) is
//!   mirrored right after it commits (best effort, bounded; the next pass repairs a
//!   missed one). Messaging's own registration rows, push jobs and legacy dispatcher are
//!   untouched: what Messaging sends is decided only by the routing fence
//!   (`MESSAGE_WAKEUPS`), never by this module.
//! * **First-party only.** Only registrations of an ACTIVE device whose principal is an
//!   ACTIVE NATIVE_CF_USER with a `cfmeet-user:<uuid>` Platform Access reference are sent
//!   as enabled; every other registration of the scope is sent as a removal. Other
//!   scopes (platform tenants) are never read.
//! * **Secrets.** The handle travels only in the authenticated I13 body; it is never
//!   logged, counted or kept in memory beyond one batch. Reports and logs carry counts.

use std::{collections::HashMap, sync::Arc, time::Duration as StdDuration};

use async_trait::async_trait;
use axum::http::Method;
use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient},
    OutboundServiceRequest, ServiceId,
};
use chrono::{DateTime, Duration, Utc};
use sea_orm::FromQueryResult;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    db,
    error::{Error, ErrorCode, Result},
    identity::{EnvironmentKind, OwnerScope},
    state::AppState,
};

/// I13, nested under the Notification Service's internal base URL.
pub const SYNC_PATH: &str = "/internal/v1/push/registrations/sync";
/// Most registrations in one I13 request (contract `maxItems`).
pub const BATCH: usize = 100;
/// A full copy pass at least this often (incremental passes in between).
pub const FULL_PASS_EVERY: Duration = Duration::hours(6);
/// Bound of one post-commit facade call.
pub const FACADE_DEADLINE: StdDuration = StdDuration::from_secs(5);
/// Counter of mirrored registrations (`outcome`: the I13 outcome class, or `failed`).
pub const METRIC: &str = "messaging_push_registration_mirror_total";

/// The switch: `None` = off (today's behaviour), `Some(interval)` = on.
pub fn interval_from_env(
    get: impl Fn(&str) -> Option<String>,
) -> std::result::Result<Option<StdDuration>, String> {
    match get("MESSAGING_PUSH_REGISTRATION_MIRROR")
        .as_deref()
        .map(str::trim)
    {
        None | Some("") | Some("off") => return Ok(None),
        Some("on") => {}
        Some(_) => return Err("MESSAGING_PUSH_REGISTRATION_MIRROR must be off or on".into()),
    }
    let secs = match get("MESSAGING_PUSH_REGISTRATION_MIRROR_INTERVAL_SECONDS") {
        None => 60,
        Some(v) => v
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|s| (10..=3600).contains(s))
            .ok_or("MESSAGING_PUSH_REGISTRATION_MIRROR_INTERVAL_SECONDS must be 10-3600")?,
    };
    Ok(Some(StdDuration::from_secs(secs)))
}

/// One I13 answer.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncResult {
    pub device_id: Uuid,
    pub outcome: String,
    #[serde(default)]
    pub push_target_ref: Option<Uuid>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncFailure {
    /// No Notification Service target configured.
    NotConfigured,
    /// Transport failure, 5xx, 401 / 403 / 408 / 429: retried by the next pass.
    Unavailable,
    /// A contract refusal of the batch (other 4xx).
    Rejected,
}

/// Where mirrored registrations go (I13).
#[async_trait]
pub trait PushRegistrationSink: Send + Sync {
    async fn sync(
        &self,
        registrations: &[Value],
    ) -> std::result::Result<Vec<SyncResult>, SyncFailure>;
}

/// The service-authenticated I13 of the Notification Service.
pub struct NotificationServiceRegistrations {
    client: Arc<InternalServiceClient>,
}

impl NotificationServiceRegistrations {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }
}

#[derive(Deserialize)]
struct SyncAnswer {
    results: Vec<SyncResult>,
}

#[async_trait]
impl PushRegistrationSink for NotificationServiceRegistrations {
    async fn sync(
        &self,
        registrations: &[Value],
    ) -> std::result::Result<Vec<SyncResult>, SyncFailure> {
        let request = OutboundServiceRequest::new_operation(ServiceId::notifications());
        match self
            .client
            .exchange_json(
                &request,
                Method::POST,
                SYNC_PATH,
                Some(&json!({ "registrations": registrations })),
            )
            .await
        {
            Ok(r) if r.status() == 200 => r
                .json::<SyncAnswer>()
                .map(|a| a.results)
                .map_err(|_| SyncFailure::Unavailable),
            Ok(r) if r.status() >= 500 || matches!(r.status(), 401 | 403 | 408 | 429) => {
                Err(SyncFailure::Unavailable)
            }
            Ok(_) => Err(SyncFailure::Rejected),
            Err(InternalCallError::UnknownTarget(_)) => Err(SyncFailure::NotConfigured),
            Err(_) => Err(SyncFailure::Unavailable),
        }
    }
}

/// The facade of one deployment: CF Meet's personal scope and the I13 sink.
pub struct PushMirror {
    pub scope: cf_communications_policy::Scope,
    pub sink: Arc<dyn PushRegistrationSink>,
}

/// Counts of one mirror call or pass (never a handle).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MirrorReport {
    /// Registrations read.
    pub scanned: usize,
    /// Sent as enabled (first-party, ACTIVE device, enabled registration).
    pub enabled: usize,
    /// Sent as removals (disabled, revoked, or not a CF Meet account).
    pub removals: usize,
    /// I13 outcome class → count.
    pub outcomes: HashMap<String, usize>,
}

impl MirrorReport {
    fn absorb(&mut self, other: MirrorReport) {
        self.scanned += other.scanned;
        self.enabled += other.enabled;
        self.removals += other.removals;
        for (k, v) in other.outcomes {
            *self.outcomes.entry(k).or_default() += v;
        }
    }

    pub fn count(&self, outcome: &str) -> usize {
        self.outcomes.get(outcome).copied().unwrap_or(0)
    }
}

#[derive(FromQueryResult)]
struct RegistrationRow {
    device_id: Uuid,
    principal_id: Uuid,
    platform: String,
    push_handle: String,
    enabled: bool,
    device_active: bool,
}

#[derive(FromQueryResult)]
struct AuthorityRow {
    environment_kind: String,
    service_account_id: Uuid,
}

#[derive(FromQueryResult)]
struct DeviceRow {
    device_id: Uuid,
}

/// The authority references Platform Access is asked with: the latest client session of
/// the scope records them (every first-party device that registered push connected).
async fn owner_scope(
    state: &AppState,
    scope: cf_communications_policy::Scope,
) -> Result<OwnerScope> {
    let row: Option<AuthorityRow> = db::one(
        &state.db,
        "SELECT environment_kind, service_account_id FROM messaging_client_sessions \
          WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 ORDER BY created_at DESC LIMIT 1",
        vec![scope.tenant_id.into(), scope.application_id.into(), scope.environment_id.into()],
    )
    .await?;
    // Else the newest committed event of the scope (the references X3 also reads).
    let row = match row {
        Some(r) => Some(r),
        None => {
            db::one(
                &state.db,
                "SELECT payload->>'environment_kind' AS environment_kind, \
                        (payload->>'initiating_service_account_id')::uuid AS service_account_id \
                   FROM messaging_outbox_events \
                  WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 \
                    AND payload->>'environment_kind' IS NOT NULL \
                    AND payload->>'initiating_service_account_id' ~ '^[0-9a-f-]{36}$' \
                  ORDER BY sequence DESC LIMIT 1",
                vec![
                    scope.tenant_id.into(),
                    scope.application_id.into(),
                    scope.environment_id.into(),
                ],
            )
            .await?
        }
    };
    let row = row.ok_or(Error::code(ErrorCode::ServiceUnavailable))?;
    Ok(OwnerScope {
        tenant_id: scope.tenant_id,
        application_id: scope.application_id,
        environment_id: scope.environment_id,
        environment_kind: EnvironmentKind::parse(&row.environment_kind)
            .ok_or(Error::code(ErrorCode::ServiceUnavailable))?,
        service_account_id: row.service_account_id,
    })
}

/// Mirrors the registrations of `devices` (≤ [`BATCH`]) of the personal scope. `owner`:
/// the authority references of the request that changed them (else derived).
pub async fn mirror_devices(
    state: &AppState,
    mirror: &PushMirror,
    devices: &[Uuid],
    owner: Option<OwnerScope>,
) -> Result<MirrorReport> {
    let mut report = MirrorReport::default();
    if devices.is_empty() {
        return Ok(report);
    }
    let s = mirror.scope;
    let rows: Vec<RegistrationRow> = db::all(
        &state.db,
        "SELECT r.device_id, r.principal_id, r.platform, r.push_handle, r.enabled, (d.status = 'ACTIVE') AS device_active \
           FROM messaging_push_registrations r JOIN messaging_devices d ON d.id = r.device_id \
          WHERE r.tenant_id = $1 AND r.application_id = $2 AND r.environment_id = $3 AND r.device_id = ANY($4) \
          ORDER BY r.device_id",
        vec![s.tenant_id.into(), s.application_id.into(), s.environment_id.into(), devices.to_vec().into()],
    )
    .await?;
    report.scanned = rows.len();
    if rows.is_empty() {
        return Ok(report);
    }
    let live: Vec<Uuid> = rows
        .iter()
        .filter(|r| r.enabled && r.device_active)
        .map(|r| r.principal_id)
        .collect();
    let accounts: HashMap<Uuid, String> = if live.is_empty() {
        HashMap::new()
    } else {
        let owner = match owner.filter(|o| {
            (o.tenant_id, o.application_id, o.environment_id)
                == (s.tenant_id, s.application_id, s.environment_id)
        }) {
            Some(o) => o,
            None => owner_scope(state, s).await?,
        };
        let mut unique = live.clone();
        unique.sort();
        unique.dedup();
        state
            .principals
            .native_user_references(&owner, &unique)
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, "principal directory unavailable for the push-registration mirror");
                Error::code(ErrorCode::ServiceUnavailable)
            })?
            .into_iter()
            .filter_map(|(p, r)| crate::notifications::account_ref(&r).map(|r| (p, r)))
            .collect()
    };
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            let account = accounts.get(&r.principal_id).filter(|_| r.enabled && r.device_active);
            let mut v = json!({
                "tenantId": s.tenant_id, "applicationId": s.application_id, "environmentId": s.environment_id,
                "principalId": r.principal_id, "deviceId": r.device_id, "platform": r.platform,
                "enabled": account.is_some(),
            });
            if let Some(a) = account {
                v["accountRef"] = json!(a);
                v["handle"] = json!(r.push_handle);
            }
            v
        })
        .collect();
    report.enabled = items.iter().filter(|i| i["enabled"] == true).count();
    report.removals = items.len() - report.enabled;
    for chunk in items.chunks(BATCH) {
        match mirror.sink.sync(chunk).await {
            Ok(results) => {
                for r in results {
                    let class = match r.outcome.as_str() {
                        "CREATED" => "created",
                        "UPDATED" => "updated",
                        "UNCHANGED" => "unchanged",
                        "REMOVED" => "removed",
                        "ABSENT" => "absent",
                        "SUPERSEDED" => "superseded",
                        "HANDLE_IN_USE" => "handle_in_use",
                        "OUT_OF_SCOPE" => "out_of_scope",
                        _ => "invalid",
                    };
                    crate::metrics::inc(METRIC, &[("outcome", class)]);
                    *report.outcomes.entry(r.outcome).or_default() += 1;
                }
            }
            Err(f) => {
                crate::metrics::inc(METRIC, &[("outcome", "failed")]);
                tracing::warn!(failure = ?f, registrations = chunk.len(), "push-registration mirror batch not accepted (retried by the next pass)");
                return Err(Error::code(ErrorCode::ServiceUnavailable));
            }
        }
    }
    Ok(report)
}

/// One copy pass over the personal scope: every registration (`since = None`, the D-1b
/// copy) or those changed since `since`. Idempotent: the service answers UNCHANGED for
/// registrations already mirrored.
pub async fn reconcile(
    state: &AppState,
    mirror: &PushMirror,
    since: Option<DateTime<Utc>>,
) -> Result<MirrorReport> {
    let s = mirror.scope;
    let mut report = MirrorReport::default();
    let mut after = Uuid::nil();
    loop {
        let page: Vec<DeviceRow> = db::all(
            &state.db,
            "SELECT device_id FROM messaging_push_registrations \
              WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 AND device_id > $4 \
                AND ($5::timestamptz IS NULL OR updated_at >= $5) \
              ORDER BY device_id LIMIT 100",
            vec![
                s.tenant_id.into(),
                s.application_id.into(),
                s.environment_id.into(),
                after.into(),
                since.into(),
            ],
        )
        .await?;
        let Some(last) = page.last().map(|d| d.device_id) else {
            break;
        };
        let devices: Vec<Uuid> = page.iter().map(|d| d.device_id).collect();
        report.absorb(mirror_devices(state, mirror, &devices, None).await?);
        after = last;
        if page.len() < 100 {
            break;
        }
    }
    Ok(report)
}

/// The facade hook: mirrors one device's registration right after its write committed,
/// in the background (never delays or fails the caller's request).
pub fn after_change(state: &AppState, device_id: Uuid, owner: Option<OwnerScope>) {
    let Some(mirror) = state.push_mirror.clone() else {
        return;
    };
    let state = state.clone();
    tokio::spawn(async move {
        match tokio::time::timeout(
            FACADE_DEADLINE,
            mirror_devices(&state, &mirror, &[device_id], owner),
        )
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(_)) | Err(_) => tracing::warn!(
                "push-registration facade call failed; the next mirror pass repairs it"
            ),
        }
    });
}

/// The copy / repair loop (spawned only with the switch on).
pub async fn run(state: AppState, mirror: Arc<PushMirror>, interval: StdDuration) {
    let mut last_full: Option<DateTime<Utc>> = None;
    let mut last_start: Option<DateTime<Utc>> = None;
    loop {
        let started = state.now();
        let full = last_full.is_none_or(|t| started - t >= FULL_PASS_EVERY);
        // Overlap the previous pass generously: a write that committed while it ran is
        // picked up again (idempotent).
        let since = if full {
            None
        } else {
            last_start.map(|t| t - Duration::from_std(interval).unwrap_or(Duration::minutes(1)) * 2)
        };
        match reconcile(&state, &mirror, since).await {
            Ok(r) => {
                if full {
                    last_full = Some(started);
                }
                last_start = Some(started);
                if full || r.scanned > 0 {
                    tracing::info!(
                        full,
                        scanned = r.scanned,
                        enabled = r.enabled,
                        removals = r.removals,
                        created = r.count("CREATED"),
                        updated = r.count("UPDATED"),
                        unchanged = r.count("UNCHANGED"),
                        removed = r.count("REMOVED"),
                        superseded = r.count("SUPERSEDED"),
                        invalid = r.count("INVALID"),
                        out_of_scope = r.count("OUT_OF_SCOPE"),
                        "push-registration mirror pass"
                    );
                }
            }
            Err(_) => tracing::warn!(full, "push-registration mirror pass failed; retried"),
        }
        tokio::time::sleep(interval).await;
    }
}
