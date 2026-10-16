//! Rotation of the deployment key that seals webhook signing secrets (Part H §74.12).
//!
//! Every subscription records the id of the key its secret is sealed under
//! (`webhook_subscriptions.secret_key_id`; NULL = a legacy `v1.` seal, attributed to the
//! declared legacy key id). Rotation, without downtime:
//!
//! 1. configure the new key as current and the old one as previous (both open; new seals
//!    use the current key) and restart;
//! 2. `platform_admin webhooks reseal --batch N` re-seals every row not under the current
//!    key, in bounded batches (`FOR UPDATE SKIP LOCKED`, keyset by id: idempotent,
//!    retryable, safe next to running instances);
//! 3. `platform_admin webhooks key-status` shows zero rows under the old key; remove it
//!    and restart.
//!
//! Startup refuses a key ring that cannot open every stored seal
//! ([`unknown_key_ids`]); at delivery time an unopenable seal is refused and counted,
//! never sent unsigned. The decrypted signing secret is never logged or returned here.

use cf_webhooks::secret_box::{SecretBox, SecretBoxError};
use sea_orm::{ConnectionTrait, DbErr, FromQueryResult, TransactionTrait};
use serde::Serialize;
use uuid::Uuid;

use super::{stmt, DbResultExt, WebhookResult, WebhookService};

/// Subscriptions sealed under one key id.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct KeyCount {
    /// The stored key id; `None` for legacy `v1.` seals.
    pub key_id: Option<String>,
    /// The key id the seals resolve to (legacy → the declared legacy key id).
    pub resolved_key_id: String,
    /// `CURRENT`, `PREVIOUS` or `UNKNOWN` (not configured: these rows cannot be opened).
    pub role: &'static str,
    pub subscriptions: i64,
}

/// Which keys are configured and how many subscriptions each one seals.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyStatus {
    pub current_key_id: String,
    pub previous_key_id: Option<String>,
    pub legacy_key_id: String,
    pub keys: Vec<KeyCount>,
    /// Rows not yet under the current key (what `reseal` would process).
    pub pending_reseal: i64,
}

/// One reseal batch.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResealReport {
    pub resealed: u64,
    /// Rows whose seal no configured key opens (left untouched, counted).
    pub unopenable: u64,
    /// Keyset cursor: the last row examined (`None` when the batch was empty).
    pub last_id: Option<Uuid>,
}

#[derive(Debug, FromQueryResult)]
struct KeyRow {
    secret_key_id: Option<String>,
    subscriptions: i64,
}

fn role(secret_box: &SecretBox, kid: &str) -> &'static str {
    if secret_box.current_kid() == kid {
        "CURRENT"
    } else if secret_box.previous_kid() == Some(kid) {
        "PREVIOUS"
    } else {
        "UNKNOWN"
    }
}

/// Key ids referenced by stored seals that the configured ring cannot open (startup
/// refuses to serve with any).
pub async fn unknown_key_ids<C: ConnectionTrait>(
    db: &C,
    secret_box: &SecretBox,
) -> Result<Vec<String>, DbErr> {
    let rows = KeyRow::find_by_statement(stmt(
        "SELECT secret_key_id, count(*) AS subscriptions FROM webhook_subscriptions \
          GROUP BY secret_key_id",
        vec![],
    ))
    .all(db)
    .await?;
    let mut unknown: Vec<String> = rows
        .into_iter()
        .map(|r| match r.secret_key_id {
            Some(kid) => kid,
            None => secret_box.legacy_kid().to_string(),
        })
        .filter(|kid| !secret_box.knows(kid))
        .collect();
    unknown.sort();
    unknown.dedup();
    Ok(unknown)
}

#[derive(Debug, FromQueryResult)]
struct SealedRow {
    id: Uuid,
    secret_sealed: String,
}

impl WebhookService {
    /// Counts of subscriptions per sealing key (no secret, no seal is returned).
    pub async fn key_status(&self) -> WebhookResult<KeyStatus> {
        let secret_box = self.secret_box()?;
        let rows = KeyRow::find_by_statement(stmt(
            "SELECT secret_key_id, count(*) AS subscriptions FROM webhook_subscriptions \
              GROUP BY secret_key_id ORDER BY secret_key_id NULLS FIRST",
            vec![],
        ))
        .all(self.db())
        .await
        .db()?;
        let mut pending = 0;
        let keys = rows
            .into_iter()
            .map(|r| {
                let resolved = r
                    .secret_key_id
                    .clone()
                    .unwrap_or_else(|| secret_box.legacy_kid().to_string());
                if r.secret_key_id.as_deref() != Some(secret_box.current_kid()) {
                    pending += r.subscriptions;
                }
                KeyCount {
                    role: role(secret_box, &resolved),
                    key_id: r.secret_key_id,
                    resolved_key_id: resolved,
                    subscriptions: r.subscriptions,
                }
            })
            .collect();
        Ok(KeyStatus {
            current_key_id: secret_box.current_kid().to_string(),
            previous_key_id: secret_box.previous_kid().map(str::to_string),
            legacy_key_id: secret_box.legacy_kid().to_string(),
            keys,
            pending_reseal: pending,
        })
    }

    /// Re-seals up to `limit` subscriptions (id greater than `after`) that are not under
    /// the current key. Rows locked by a concurrent reseal are skipped (SKIP LOCKED), rows
    /// no configured key opens are counted and left unchanged. Idempotent: a row already
    /// under the current key is never selected.
    pub async fn reseal_batch(
        &self,
        after: Option<Uuid>,
        limit: u64,
    ) -> WebhookResult<ResealReport> {
        let secret_box = self.secret_box()?;
        let current = secret_box.current_kid().to_string();
        let txn = self.db().begin().await.db()?;
        let rows = SealedRow::find_by_statement(stmt(
            "SELECT id, secret_sealed FROM webhook_subscriptions \
              WHERE secret_key_id IS DISTINCT FROM $1 AND ($2::uuid IS NULL OR id > $2) \
              ORDER BY id LIMIT $3 FOR UPDATE SKIP LOCKED",
            vec![
                current.clone().into(),
                after.into(),
                (limit.clamp(1, 1000) as i64).into(),
            ],
        ))
        .all(&txn)
        .await
        .db()?;
        let mut report = ResealReport {
            last_id: rows.last().map(|r| r.id),
            ..Default::default()
        };
        for row in rows {
            let owner = row.id.to_string();
            let secret = match secret_box.open(&row.secret_sealed, &owner) {
                Ok(secret) => secret,
                Err(error) => {
                    report.unopenable += 1;
                    let reason = match error {
                        SecretBoxError::UnknownKey => "UNKNOWN_KEY",
                        _ => "UNOPENABLE",
                    };
                    crate::metrics::inc("calling_webhook_reseals_total", &[("result", reason)]);
                    tracing::warn!(webhook_id = %row.id, reason, "webhook secret could not be re-sealed");
                    continue;
                }
            };
            txn.execute(stmt(
                "UPDATE webhook_subscriptions SET secret_sealed = $2, secret_key_id = $3 \
                  WHERE id = $1 AND secret_sealed = $4",
                vec![
                    row.id.into(),
                    secret_box.seal(&secret, &owner).into(),
                    current.clone().into(),
                    row.secret_sealed.into(),
                ],
            ))
            .await
            .db()?;
            report.resealed += 1;
            crate::metrics::inc("calling_webhook_reseals_total", &[("result", "RESEALED")]);
        }
        txn.commit().await.db()?;
        tracing::info!(
            resealed = report.resealed,
            unopenable = report.unopenable,
            current_key_id = %current,
            "webhook secret reseal batch"
        );
        Ok(report)
    }

    /// The operator command `platform_admin webhooks reseal`: runs [`reseal_batch`]
    /// (Self::reseal_batch) until no row is left, then writes ONE operator-audit row
    /// (`WEBHOOK_KEYS_RESEALED`, target the deployment key ring) whose detail holds the
    /// counts and the batch size only — never a key id's material, a secret or a seal.
    /// Each batch commits on its own (bounded, idempotent), so the audit row follows the
    /// work it describes; a failure part-way returns before the audit row, and re-running
    /// the command finishes (and audits) the remaining rows.
    pub async fn operator_reseal_all(
        &self,
        op: &crate::platform::operator::OperatorContext,
        batch: u64,
    ) -> WebhookResult<ResealSummary> {
        use crate::platform::operator::{self, AuditEntry, AuditTargetKind, OperatorAction};
        let batch = batch.clamp(1, 1000);
        let mut summary = ResealSummary {
            batch_size: batch,
            ..Default::default()
        };
        let mut after = None;
        loop {
            let report = self.reseal_batch(after, batch).await?;
            summary.resealed += report.resealed;
            summary.unopenable += report.unopenable;
            summary.batches += 1;
            match report.last_id {
                Some(last) => after = Some(last),
                None => break,
            }
        }
        let entry = AuditEntry {
            action: OperatorAction::WebhookKeysResealed,
            target_kind: AuditTargetKind::WebhookKeyRing,
            target_id: None,
            tenant_id: None,
            application_id: None,
            environment_id: None,
            detail: serde_json::json!({
                "resealed": summary.resealed,
                "unopenable": summary.unopenable,
                "batches": summary.batches,
                "batchSize": summary.batch_size,
            }),
        };
        let txn = self.db().begin().await.db()?;
        operator::record(&txn, op, &entry, self.now())
            .await
            .map_err(|error| {
                tracing::error!(%error, "webhook reseal could not be audited");
                crate::error::CallingError::Internal
            })?;
        txn.commit().await.db()?;
        Ok(summary)
    }
}

/// A complete reseal run (counts only).
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ResealSummary {
    pub resealed: u64,
    pub unopenable: u64,
    pub batches: u64,
    pub batch_size: u64,
}
