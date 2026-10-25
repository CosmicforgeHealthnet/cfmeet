//! Section 53 §9.1 / §19.3 — the controlled migration of customer webhooks from Calling
//! (operator tooling; the production migration and cutover are operator decisions, see
//! `docs/phases/phase-06/webhook-migration-runbook.md`).
//!
//! ```text
//! Calling export (webhook-migration.export)            Developer Platform (this module)
//!   inventory / digest / keyset pages  ─────────────▶   inventory (dry run, writes nothing)
//!                                                       import    (resumable runs, idempotent)
//!                                                       reconcile (digests + record drift)
//!                                                       shadow-report (dual ingestion parity)
//!                                                       cutover-check (preconditions)
//!                                                       takeover  (gate: CUT_OVER)
//!   rollback routes (webhook-migration.import) ◀──────  rollback-export / rollback
//! ```
//!
//! **Custody.** A signing secret arrives only sealed under the migration transfer key
//! (`DEVELOPER_PLATFORM_WEBHOOK_MIGRATION_TRANSFER_KEY`, Calling holds the same key under
//! its own variable name) with associated data bound to the subscription id and the
//! direction ([`transfer_owner`]); it is opened in memory (zeroized on drop), checked
//! against Calling's keyed fingerprint and sealed again under this deployment's key ring —
//! `cf_webhooks::secret_box` only, no cryptography of our own. A secret Calling could not
//! open, or a transfer seal that does not open here (wrong key, tampering, fingerprint
//! mismatch), makes the subscription `ROTATION_REQUIRED`: imported DISABLED
//! (`SECRET_ROTATION_REQUIRED`) without any secret; only a customer rotation enables it
//! again. A substitute secret is never generated.
//!
//! **History.** Events, deliveries and attempts keep Calling's ids and carry origin
//! `MIGRATED_FROM_CALLING`; a delivery still open at Calling is a held mirror
//! (`migration_hold`) — never claimed, shadowed or sent — until the takeover.
//!
//! **Cutover gate** ([`super::worker::MIGRATION_GATE`]): while the migration is
//! `PREPARING`, deliveries to migrated subscriptions are recorded but never sent (Calling
//! still delivers them); the takeover verifies that Calling stopped sending (its delivery
//! flag off, no live claim, the final import taken after that), releases the held mirrors,
//! de-duplicates them against native deliveries and moves to `CUT_OVER`. A rollback moves
//! to `ROLLED_BACK`: nothing is sent here any more.

pub mod cutover;
pub mod http;
pub mod import;
pub mod port;
pub mod reconcile;

use cf_webhooks::secret_box::SecretBox;
use chrono::{DateTime, SecondsFormat, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr};
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;
use uuid::Uuid;

use self::port::{CallingMigrationPort, SourceError};
use super::WebhookService;
use crate::{
    control::stmt,
    http::error::{ApiError, ErrorCode},
    platform_access::PlatformAccessPort,
};

/// Associated-data purpose of a Calling → Developer Platform transfer seal (Calling's
/// `webhooks::ownership_migration::EXPORT_PURPOSE`).
pub const EXPORT_PURPOSE: &str = "webhook-migration|calling-to-developer-platform";
/// Associated-data purpose of a Developer Platform → Calling (rollback) transfer seal.
pub const ROLLBACK_PURPOSE: &str = "webhook-migration|developer-platform-to-calling";
/// Origin of imported records.
pub const ORIGIN_MIGRATED: &str = "MIGRATED_FROM_CALLING";
/// `disabled_reason` of a subscription awaiting a customer secret rotation.
pub const ROTATION_REASON: &str = "SECRET_ROTATION_REQUIRED";
/// Most conflicts kept on a run / in a report.
pub const MAX_LISTED: usize = 200;

/// The associated-data owner of a transfer seal: the subscription id AND the direction.
pub fn transfer_owner(purpose: &str, subscription: Uuid) -> String {
    format!("{purpose}|{subscription}")
}

/// Calling's keyed fingerprint of a signing secret: the `CF-Webhook-Signature` of a fixed
/// probe (what any delivery already discloses).
pub fn secret_fingerprint(secret: &str, subscription: Uuid) -> String {
    cf_webhooks::signing::sign(
        secret.as_bytes(),
        0,
        &format!("webhook-migration-fingerprint|{subscription}"),
        b"cfmeet",
    )
}

/// RFC 3339 with microseconds (PostgreSQL precision; the canonical digest form).
pub fn exact(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Micros, true)
}

pub(crate) fn exact_opt(at: Option<DateTime<Utc>>) -> Value {
    at.map(|t| Value::String(exact(t))).unwrap_or(Value::Null)
}

/// A secret opened for re-sealing: zeroized on drop, no `Debug` / `Display` output.
pub(crate) struct Plain(SecretString);

impl Plain {
    pub(crate) fn open(ring: &SecretBox, sealed: &str, owner: &str) -> Option<Plain> {
        ring.open(sealed, owner)
            .ok()
            .map(|s| Plain(SecretString::from(s)))
    }

    pub(crate) fn expose(&self) -> &str {
        self.0.expose_secret()
    }
}

/// What every migration command works with.
pub struct Migration<'a> {
    pub db: &'a DatabaseConnection,
    pub webhooks: &'a WebhookService,
    pub calling: &'a dyn CallingMigrationPort,
    pub platform_access: &'a dyn PlatformAccessPort,
    /// The named human operator (`x-cf-operator`).
    pub operator: &'a str,
    pub request_id: String,
    pub correlation: cf_service_auth::CorrelationId,
}

impl Migration<'_> {
    pub fn now(&self) -> DateTime<Utc> {
        self.webhooks.now()
    }

    /// This deployment's sealing key ring (503 without it).
    pub fn ring(&self) -> Result<&SecretBox, ApiError> {
        self.webhooks.key_ring()
    }

    /// The migration transfer key (503 CAPABILITY_UNAVAILABLE without it).
    pub fn transfer(&self) -> Result<&SecretBox, ApiError> {
        self.webhooks
            .settings()
            .transfer_key
            .as_deref()
            .ok_or(ApiError(ErrorCode::CapabilityUnavailable))
    }

    /// One append-only audit row (codes and counts only).
    pub async fn audit<C: ConnectionTrait>(
        &self,
        db: &C,
        action: &str,
        run_id: Option<Uuid>,
        summary: Value,
    ) -> Result<(), ApiError> {
        db.execute(stmt(
            "INSERT INTO devplat_webhook_migration_audit (id, operator, action, run_id, summary, \
               request_id, occurred_at) VALUES ($1, $2, $3, $4, $5, $6, $7)",
            vec![
                Uuid::new_v4().into(),
                self.operator.into(),
                format!("webhook_migration.{action}").into(),
                run_id.into(),
                summary.into(),
                self.request_id.clone().into(),
                self.now().into(),
            ],
        ))
        .await?;
        tracing::info!(operator = %self.operator, action, "webhook migration command");
        Ok(())
    }

    /// Calling's inventory, or 503.
    pub async fn calling_inventory(&self) -> Result<Value, ApiError> {
        self.calling.inventory().await.map_err(source_error)
    }
}

/// A Calling failure as a bounded API error (logged without detail beyond the code).
pub fn source_error(e: SourceError) -> ApiError {
    tracing::warn!(code = e.code(), error = %e, "webhook migration: Calling export unavailable");
    ApiError(match e {
        SourceError::NotConfigured => ErrorCode::CapabilityUnavailable,
        _ => ErrorCode::ServiceUnavailable,
    })
}

/// The cutover phase (`None`: no migration ever started).
pub async fn phase<C: ConnectionTrait>(db: &C) -> Result<Option<String>, DbErr> {
    Ok(
        match db
            .query_one(stmt(
                "SELECT phase FROM devplat_webhook_migration_state WHERE id = 1",
                vec![],
            ))
            .await?
        {
            Some(r) => Some(r.try_get_by_index::<String>(0)?),
            None => None,
        },
    )
}

/// Sets the cutover phase.
pub async fn set_phase<C: ConnectionTrait>(
    db: &C,
    phase: &str,
    operator: &str,
    now: DateTime<Utc>,
) -> Result<(), DbErr> {
    db.execute(stmt(
        "INSERT INTO devplat_webhook_migration_state (id, phase, changed_by, changed_at) \
         VALUES (1, $1, $2, $3) \
         ON CONFLICT (id) DO UPDATE SET phase = EXCLUDED.phase, changed_by = EXCLUDED.changed_by, \
           changed_at = EXCLUDED.changed_at",
        vec![phase.into(), operator.into(), now.into()],
    ))
    .await?;
    Ok(())
}

/// A bounded list entry `{entity, id, reason}`.
pub(crate) fn listed(entity: &str, id: impl ToString, reason: &str) -> Value {
    serde_json::json!({ "entity": entity, "id": id.to_string(), "reason": reason })
}
