//! ADR 0064 install events: the projection of Platform Access's `APP_GRANT` change-feed
//! entries (`POST /internal/v1/identity/changes`, `platform-identity.read`) into
//! `devplat_app_installations`, which the install routing pass of event ingestion reads
//! ([`crate::webhooks::ingest`]), and the app lifecycle events it emits to the installed
//! app's own subscriptions:
//!
//! * a grant first seen ACTIVE → `app.installed`;
//! * ACTIVE → REVOKED by an uninstall (`UNINSTALLED` / `USER_REVOKED` / `ADMIN_REVOKED`
//!   or no reason) → `app.uninstalled`; by anything else (refresh-token reuse, app
//!   suspension, …) → `app.grant_revoked`.
//!
//! The worker runs it every pass when `DEVELOPER_PLATFORM_INSTALL_EVENTS_ENABLED`. The
//! cursor (`devplat_feed_cursors`) advances in the transaction that applies the page, so a
//! crash re-applies at most one page (idempotent: entries older than the stored sequence of
//! a grant are ignored, lifecycle events are deduplicated by their derived source id).
//!
//! **Snapshot** (camelCase): `id`, `appId`, `target` (`USER` | `ORGANIZATION`),
//! `organizationId?`, `principalId` (the user / bot principal), `status` (`ACTIVE` |
//! `REVOKED`), `scopes` (array or space-separated), `revokeReason?` (Platform Access:
//! `UNINSTALLED`, `APP_SUSPENDED`, `CODE_REUSE`, `REFRESH_REUSE`), and the install target's
//! FIRST_PARTY scope as `tenantId` / `applicationId` / `environmentId` (Platform Access's
//! `AppGrantSnapshot`). An account id is never stored.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, TransactionTrait};
use serde_json::{Map, Value};
use uuid::Uuid;

use super::port::{OAuthReply, PlatformOAuthPort};
use crate::{
    control::stmt,
    http::error::{ApiError, ErrorCode},
    webhooks::{
        catalog::PRODUCER_DEVELOPER_PLATFORM,
        ingest::{deliver_to_app, AppDelivery},
        DeliveryMode, WebhookService,
    },
};

/// The cursor name of Platform Access's identity change feed.
pub const FEED: &str = "platform-access.identity";
/// Entries per feed page (contract: at most 500).
pub const PAGE: u32 = 500;
/// Pages applied per worker pass.
pub const MAX_PAGES: usize = 20;

/// One parsed `APP_GRANT` snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct GrantSnapshot {
    pub grant_id: Uuid,
    pub app_id: Uuid,
    pub target_kind: String,
    pub organization_id: Option<Uuid>,
    pub principal_id: Option<Uuid>,
    pub target: Option<(Uuid, Uuid, Uuid)>,
    pub scopes: Vec<String>,
    pub status: String,
    pub revoked_reason: Option<String>,
}

fn uuid_at(v: &Value, k: &str) -> Option<Uuid> {
    v.get(k)
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
}

fn code(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.as_bytes()[0].is_ascii_uppercase()
        && s.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

impl GrantSnapshot {
    /// Parses a snapshot (`None`: malformed — skipped and logged by the caller).
    pub fn parse(s: &Value) -> Option<Self> {
        let target_kind = s.get("target")?.as_str()?;
        if !matches!(target_kind, "USER" | "ORGANIZATION") {
            return None;
        }
        let status = s.get("status")?.as_str()?;
        if !matches!(status, "ACTIVE" | "REVOKED") {
            return None;
        }
        let nested = s.get("targetScope");
        let target = match nested {
            Some(n) => uuid_at(n, "tenantId")
                .zip(uuid_at(n, "applicationId"))
                .zip(uuid_at(n, "environmentId"))
                .map(|((t, a), e)| (t, a, e)),
            None => uuid_at(s, "tenantId")
                .or_else(|| uuid_at(s, "targetTenantId"))
                .zip(uuid_at(s, "applicationId").or_else(|| uuid_at(s, "targetApplicationId")))
                .zip(uuid_at(s, "environmentId").or_else(|| uuid_at(s, "targetEnvironmentId")))
                .map(|((t, a), e)| (t, a, e)),
        };
        let scopes: Vec<String> = match s.get("scopes") {
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
            Some(Value::String(t)) => t.split_whitespace().map(str::to_string).collect(),
            _ => vec![],
        };
        if scopes.len() > 32 || scopes.iter().any(|x| x.len() > 64) {
            return None;
        }
        Some(Self {
            grant_id: uuid_at(s, "id")?,
            app_id: uuid_at(s, "appId")?,
            target_kind: target_kind.to_string(),
            organization_id: uuid_at(s, "organizationId"),
            principal_id: uuid_at(s, "principalId"),
            target,
            scopes,
            status: status.to_string(),
            revoked_reason: s
                .get("revokeReason")
                .or_else(|| s.get("revokedReason"))
                .and_then(Value::as_str)
                .filter(|r| code(r))
                .map(str::to_string),
        })
    }

    /// The lifecycle event of moving from `previous` (None: first seen) to this state.
    pub fn lifecycle_event(&self, previous: Option<&str>) -> Option<&'static str> {
        match (previous, self.status.as_str()) {
            (None | Some("REVOKED"), "ACTIVE") => Some("app.installed"),
            (Some("ACTIVE"), "REVOKED") => Some(match self.revoked_reason.as_deref() {
                None | Some("UNINSTALLED" | "USER_REVOKED" | "ADMIN_REVOKED") => "app.uninstalled",
                Some(_) => "app.grant_revoked",
            }),
            _ => None,
        }
    }
}

/// What one sync did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncReport {
    pub pages: usize,
    pub applied: u64,
    pub skipped: u64,
    pub deliveries: u64,
}

async fn cursor<C: ConnectionTrait>(db: &C) -> Result<i64, ApiError> {
    Ok(db
        .query_one(stmt(
            "SELECT after_sequence FROM devplat_feed_cursors WHERE feed = $1",
            vec![FEED.into()],
        ))
        .await?
        .map(|r| r.try_get::<i64>("", "after_sequence"))
        .transpose()?
        .unwrap_or(0))
}

/// Applies one change (`sequence`) of `snapshot`; returns the deliveries created.
async fn apply<C: ConnectionTrait>(
    db: &C,
    g: &GrantSnapshot,
    sequence: i64,
    revision: i64,
    occurred_at: DateTime<Utc>,
    webhooks: Option<&WebhookService>,
) -> Result<Option<u64>, ApiError> {
    let previous = db
        .query_one(stmt(
            "SELECT status, feed_sequence FROM devplat_app_installations WHERE grant_id = $1 FOR UPDATE",
            vec![g.grant_id.into()],
        ))
        .await?;
    let previous = match previous {
        Some(r) => {
            let seq: i64 = r.try_get("", "feed_sequence")?;
            if seq >= sequence {
                return Ok(None);
            }
            Some(r.try_get::<String>("", "status")?)
        }
        None => None,
    };
    let now = Utc::now();
    let (tt, ta, te) = g.target.unzip3();
    db.execute(stmt(
        "INSERT INTO devplat_app_installations (grant_id, app_id, target_kind, organization_id, \
           principal_id, target_tenant_id, target_application_id, target_environment_id, scopes, status, \
           revoked_reason, revision, feed_sequence, installed_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $14) \
         ON CONFLICT (grant_id) DO UPDATE SET app_id = EXCLUDED.app_id, target_kind = EXCLUDED.target_kind, \
           organization_id = EXCLUDED.organization_id, principal_id = EXCLUDED.principal_id, \
           target_tenant_id = EXCLUDED.target_tenant_id, target_application_id = EXCLUDED.target_application_id, \
           target_environment_id = EXCLUDED.target_environment_id, scopes = EXCLUDED.scopes, \
           status = EXCLUDED.status, revoked_reason = EXCLUDED.revoked_reason, revision = EXCLUDED.revision, \
           feed_sequence = EXCLUDED.feed_sequence, updated_at = EXCLUDED.updated_at",
        vec![
            g.grant_id.into(),
            g.app_id.into(),
            g.target_kind.clone().into(),
            g.organization_id.into(),
            g.principal_id.into(),
            tt.into(),
            ta.into(),
            te.into(),
            g.scopes.clone().into(),
            g.status.clone().into(),
            g.revoked_reason.clone().into(),
            revision.into(),
            sequence.into(),
            now.into(),
        ],
    ))
    .await?;
    let (Some(event_type), Some(w)) = (g.lifecycle_event(previous.as_deref()), webhooks) else {
        return Ok(Some(0));
    };
    if !w.settings().enabled {
        return Ok(Some(0));
    }
    let mut data = Map::new();
    data.insert(
        "installationId".into(),
        Value::String(g.grant_id.to_string()),
    );
    data.insert("appId".into(), Value::String(g.app_id.to_string()));
    data.insert("targetKind".into(), Value::String(g.target_kind.clone()));
    if let Some(o) = g.organization_id {
        data.insert("organizationId".into(), Value::String(o.to_string()));
    }
    if event_type != "app.installed" {
        if let Some(r) = &g.revoked_reason {
            data.insert("reasonCode".into(), Value::String(r.clone()));
        }
    }
    let grant = g.grant_id.to_string();
    let seq = sequence.to_string();
    let n = deliver_to_app(
        db,
        AppDelivery {
            app_id: g.app_id,
            producer: PRODUCER_DEVELOPER_PLATFORM,
            source_key: &["app-grant", &grant, event_type, &seq],
            event_type,
            data,
            occurred_at,
            correlation_id: None,
        },
        w.settings().mode == DeliveryMode::Shadow,
        w.now(),
    )
    .await?;
    Ok(Some(n))
}

trait Unzip3 {
    fn unzip3(self) -> (Option<Uuid>, Option<Uuid>, Option<Uuid>);
}

impl Unzip3 for Option<(Uuid, Uuid, Uuid)> {
    fn unzip3(self) -> (Option<Uuid>, Option<Uuid>, Option<Uuid>) {
        match self {
            Some((a, b, c)) => (Some(a), Some(b), Some(c)),
            None => (None, None, None),
        }
    }
}

/// Follows the feed until drained (at most [`MAX_PAGES`] pages).
pub async fn sync_once(
    db: &DatabaseConnection,
    port: &dyn PlatformOAuthPort,
    webhooks: Option<&WebhookService>,
) -> Result<SyncReport, ApiError> {
    let mut report = SyncReport::default();
    let correlation = cf_service_auth::CorrelationId::generate();
    for _ in 0..MAX_PAGES {
        let after = cursor(db).await?;
        let page = match port.identity_changes(after, PAGE, &correlation).await {
            Ok(OAuthReply::Ok { body, .. }) => body,
            Ok(OAuthReply::Refused { status, code }) => {
                tracing::warn!(status, %code, "identity change feed refused");
                return Err(ApiError(ErrorCode::ServiceUnavailable));
            }
            Err(e) => {
                tracing::warn!(error = %e, "identity change feed unavailable");
                return Err(ApiError(ErrorCode::ServiceUnavailable));
            }
        };
        let changes = page
            .get("changes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let next = page
            .get("nextAfter")
            .and_then(Value::as_i64)
            .filter(|n| *n >= after)
            .ok_or(ApiError(ErrorCode::ServiceUnavailable))?;
        let more = page
            .get("hasMore")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let txn = db.begin().await?;
        for c in &changes {
            if c.get("entityKind").and_then(Value::as_str) != Some("APP_GRANT") {
                continue;
            }
            let sequence = c.get("sequence").and_then(Value::as_i64).unwrap_or(0);
            let revision = c.get("revision").and_then(Value::as_i64).unwrap_or(0);
            let occurred_at = c
                .get("occurredAt")
                .and_then(Value::as_str)
                .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                .map(|t| t.with_timezone(&Utc))
                .unwrap_or_else(Utc::now);
            let Some(g) = c.get("snapshot").and_then(GrantSnapshot::parse) else {
                tracing::warn!(sequence, "malformed APP_GRANT change skipped");
                report.skipped += 1;
                continue;
            };
            match apply(&txn, &g, sequence, revision, occurred_at, webhooks).await? {
                Some(n) => {
                    report.applied += 1;
                    report.deliveries += n;
                }
                None => report.skipped += 1,
            }
        }
        txn.execute(stmt(
            "INSERT INTO devplat_feed_cursors (feed, after_sequence, updated_at) VALUES ($1, $2, $3) \
             ON CONFLICT (feed) DO UPDATE SET after_sequence = EXCLUDED.after_sequence, \
               updated_at = EXCLUDED.updated_at",
            vec![FEED.into(), next.into(), Utc::now().into()],
        ))
        .await?;
        txn.commit().await?;
        report.pages += 1;
        if !more || changes.is_empty() {
            break;
        }
    }
    Ok(report)
}

/// The worker step (logged; a failure is retried next pass).
pub async fn sync_pass(
    db: &DatabaseConnection,
    port: &Arc<dyn PlatformOAuthPort>,
    webhooks: Option<&WebhookService>,
) -> Option<SyncReport> {
    match sync_once(db, port.as_ref(), webhooks).await {
        Ok(r) => {
            if r.applied + r.skipped > 0 {
                tracing::info!(
                    pages = r.pages,
                    applied = r.applied,
                    skipped = r.skipped,
                    deliveries = r.deliveries,
                    "app grant projection pass"
                );
            }
            Some(r)
        }
        Err(e) => {
            tracing::warn!(code = e.0.as_str(), "app grant projection pass failed");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn snapshots_parse_and_name_their_lifecycle() {
        let t = Uuid::new_v4();
        let s = json!({ "id": Uuid::new_v4(), "appId": Uuid::new_v4(), "target": "ORGANIZATION",
            "organizationId": Uuid::new_v4(), "principalId": Uuid::new_v4(), "status": "ACTIVE",
            "scopes": "chat:read:bot chat:write:bot", "accountId": Uuid::new_v4(),
            "targetScope": { "tenantId": t, "applicationId": Uuid::new_v4(), "environmentId": Uuid::new_v4() } });
        let g = GrantSnapshot::parse(&s).unwrap();
        assert_eq!(g.target.unwrap().0, t);
        assert_eq!(g.scopes, ["chat:read:bot", "chat:write:bot"]);
        assert_eq!(g.lifecycle_event(None), Some("app.installed"));
        assert_eq!(g.lifecycle_event(Some("ACTIVE")), None);
        let mut r = g.clone();
        r.status = "REVOKED".into();
        assert_eq!(r.lifecycle_event(Some("ACTIVE")), Some("app.uninstalled"));
        r.revoked_reason = Some("REFRESH_TOKEN_REUSE".into());
        assert_eq!(r.lifecycle_event(Some("ACTIVE")), Some("app.grant_revoked"));
        assert_eq!(r.lifecycle_event(None), None);
        assert!(GrantSnapshot::parse(&json!({ "id": "x" })).is_none());
    }
}
