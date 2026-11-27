//! Owner-delivered organization exports (ADR 0068 phase A4; the owner decision agreed with
//! HQ on 2026-11-21: HQ staff never see exported personal data, and HQ never stores or
//! relays it).
//!
//! HQ asks, through the admin bridge (`organizations.organization.export`, R4, two
//! people), for an organization's export. Organizations then:
//!
//! 1. builds the SAME document as the existing export
//!    ([`crate::http::authorization::export_document`]; it carries invitation addresses);
//! 2. keeps only an encrypted copy ([`ExportVault`]: XChaCha20-Poly1305, 192-bit random nonce,
//!    additional data bound to the export and its organization, keys from the key FILE
//!    `ORGANIZATIONS_EXPORT_KEYS_FILE`) for [`EXPORT_TTL_DAYS`] days, plus its size and the
//!    SHA-256 of the clear document;
//! 3. delivers it to the organization's ACTIVE OWNERs (at most [`MAX_RECIPIENTS`]): each
//!    owner gets a single-use download link valid until the export expires, by email
//!    (`DOWNLOAD_LINK_EMAIL`). Organizations never holds an email address: the notice is
//!    an `organizations.organization.export_ready` event of the Notification Service
//!    (I1), addressed to the owner's account (`cfmeet-user:<id>`) — the Notification
//!    Service alone resolves the account's verified address from the account authority
//!    and renders the link — with the link's token HPKE-sealed to the Notification
//!    Service key (`CF_NOTIFICATIONS_SEALING_KEY`), like the Meeting Service's invitation
//!    links. Organizations stores only the token's SHA-256; the sealed notice is kept only
//!    until the Notification Service acknowledged it.
//! 4. The link works only for its owner after sign-in: the download
//!    (`POST /internal/v1/organizations/{id}/exports/{exportId}/download`, relayed with the
//!    verified acting user) needs the token, the acting user to be the link's recipient and
//!    still an ACTIVE OWNER, and an unused, unexpired link. It is used once; expiry means
//!    HQ issues the command again. Nothing is ever resent.
//!
//! The bridge (and every log, audit event and ledger row) only ever sees the reference:
//! export id, state (`READY` → `DELIVERED` | `EXPIRED` | `FAILED`), size, SHA-256, expiry,
//! the delivery references and states. Never the document, a token, a sealed part, a URL
//! or an address (`recipientMasked` is always `null`: Organizations has no address to
//! mask).
//!
//! **Availability.** The notice type is not yet in the Notification Service catalogue
//! (`packages/notification-client`): until it is, `ORGANIZATIONS_EXPORT_DELIVERY_ENABLED`
//! stays off (the service refuses to start with it on) and the command answers 409
//! `EXPORT_DELIVERY_UNAVAILABLE`, building and storing nothing.
//!
//! **Maintenance** ([`maintain`], every worker tick): links past expiry end `EXPIRED`;
//! unused exports past expiry end `EXPIRED`; the encrypted copy is purged at expiry, when
//! no owner could be notified (`FAILED`), or as soon as every link is settled; rows are
//! deleted [`ROW_RETENTION_DAYS`] days after creation. **Relay** ([`relay_notices`], only
//! with `CF_NOTIFICATIONS_BASE_URL`): pending notices are posted to I1 with the
//! publication relay's classification, backoff and attempt limit.

use std::{collections::HashMap, fmt, path::Path, sync::Arc};

use base64::Engine;
use cf_notification_client::{
    EnvelopeBuilder, EnvelopeError, Producer, RouteRef, SealedPlaintext, SealingKey,
    ROUTING_PROTOCOL,
};
use chacha20poly1305::{
    aead::{Aead, Payload},
    KeyInit, XChaCha20Poly1305, XNonce,
};
use chrono::{DateTime, Duration, Utc};
use rand::RngCore;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, FromQueryResult, TransactionTrait};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    db,
    notification_publications::{
        account_ref, backoff, IngestResult, NotificationEventSink, PublishFailure, MAX_ATTEMPTS,
        MAX_BATCH,
    },
    store::rfc3339,
};

/// The Notification Service event that tells an owner their download link.
pub const EXPORT_READY_TYPE: &str = "organizations.organization.export_ready";
/// How long the encrypted copy and the links live.
pub const EXPORT_TTL_DAYS: i64 = 7;
/// When export and delivery rows (references only by then) are deleted.
pub const ROW_RETENTION_DAYS: i64 = 90;
/// Most owners one export is delivered to (oldest memberships first).
pub const MAX_RECIPIENTS: i64 = 20;
/// The bridge resource's `delivery`.
pub const DELIVERY: &str = "OWNER_DELIVERED";
/// The bridge resource's `deliveryChannel`.
pub const CHANNEL: &str = "DOWNLOAD_LINK_EMAIL";
/// Additional data of the encrypted copy: `<prefix><export id>|<organization id>`.
const AAD_PREFIX: &[u8] = b"cfmeet.organizations/export/v1|";

pub fn export_ttl() -> Duration {
    Duration::days(EXPORT_TTL_DAYS)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

// --------------------------------------------------------------------------------- vault

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum VaultError {
    /// The key file is malformed (the reason never echoes a value).
    #[error("the export key file is invalid: {0}")]
    Malformed(&'static str),
    #[error("the export was sealed with a key that is no longer configured")]
    UnknownKey,
    #[error("the export could not be sealed")]
    Seal,
    #[error("the export does not open (wrong key, organization or tampered)")]
    Open,
}

struct VaultKey {
    id: String,
    key: [u8; 32],
}

impl Drop for VaultKey {
    fn drop(&mut self) {
        self.key.iter_mut().for_each(|b| *b = 0);
    }
}

/// The encrypted copy of one export.
pub struct SealedExport {
    pub key_id: String,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
}

/// The at-rest keys of exports. The FIRST key seals; every key opens (rotation: add the new
/// key first and keep the previous one for [`EXPORT_TTL_DAYS`] days).
pub struct ExportVault {
    keys: Vec<VaultKey>,
}

impl fmt::Debug for ExportVault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExportVault")
            .field(
                "key_ids",
                &self.keys.iter().map(|k| k.id.as_str()).collect::<Vec<_>>(),
            )
            .finish()
    }
}

fn valid_key_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

impl ExportVault {
    /// One key per line, `<key id>:<base64url of 32 random bytes>`; blank lines and lines
    /// starting with `#` are ignored.
    pub fn parse(text: &str) -> Result<Self, VaultError> {
        let mut keys: Vec<VaultKey> = Vec::new();
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (id, encoded) = line.split_once(':').ok_or(VaultError::Malformed(
                "expected <key id>:<base64url key> lines",
            ))?;
            if !valid_key_id(id) {
                return Err(VaultError::Malformed(
                    "a key id must be 1-64 characters [A-Za-z0-9._-]",
                ));
            }
            if keys.iter().any(|k| k.id == id) {
                return Err(VaultError::Malformed("a key id appears twice"));
            }
            let mut bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(encoded.trim_end_matches('='))
                .map_err(|_| VaultError::Malformed("a key is not base64url"))?;
            if bytes.len() != 32 {
                bytes.iter_mut().for_each(|b| *b = 0);
                return Err(VaultError::Malformed("a key must be 32 bytes"));
            }
            let mut key = [0u8; 32];
            key.copy_from_slice(&bytes);
            bytes.iter_mut().for_each(|b| *b = 0);
            keys.push(VaultKey {
                id: id.to_string(),
                key,
            });
        }
        if keys.is_empty() {
            return Err(VaultError::Malformed("no key"));
        }
        Ok(Self { keys })
    }

    pub fn from_file(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|_| "cannot read the key file")?;
        Self::parse(&text).map_err(|e| e.to_string())
    }

    /// The id of the sealing key.
    pub fn current_key_id(&self) -> &str {
        &self.keys[0].id
    }

    fn aad(export: Uuid, organization: Uuid) -> Vec<u8> {
        let mut aad = AAD_PREFIX.to_vec();
        aad.extend_from_slice(export.to_string().as_bytes());
        aad.push(b'|');
        aad.extend_from_slice(organization.to_string().as_bytes());
        aad
    }

    fn cipher(key: &VaultKey) -> Result<XChaCha20Poly1305, VaultError> {
        XChaCha20Poly1305::new_from_slice(&key.key).map_err(|_| VaultError::Seal)
    }

    /// Encrypts the clear document of `export` (of `organization`) with the current key.
    pub fn seal(
        &self,
        export: Uuid,
        organization: Uuid,
        clear: &[u8],
    ) -> Result<SealedExport, VaultError> {
        let key = &self.keys[0];
        let mut nonce = [0u8; 24];
        rand::thread_rng().fill_bytes(&mut nonce);
        let aad = Self::aad(export, organization);
        let ciphertext = Self::cipher(key)?
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: clear,
                    aad: &aad,
                },
            )
            .map_err(|_| VaultError::Seal)?;
        Ok(SealedExport {
            key_id: key.id.clone(),
            nonce: nonce.to_vec(),
            ciphertext,
        })
    }

    /// Decrypts a stored copy (refused for another export or organization, a tampered
    /// copy, or a key no longer configured).
    pub fn open(
        &self,
        key_id: &str,
        nonce: &[u8],
        ciphertext: &[u8],
        export: Uuid,
        organization: Uuid,
    ) -> Result<Vec<u8>, VaultError> {
        let key = self
            .keys
            .iter()
            .find(|k| k.id == key_id)
            .ok_or(VaultError::UnknownKey)?;
        let nonce: [u8; 24] = nonce.try_into().map_err(|_| VaultError::Open)?;
        let aad = Self::aad(export, organization);
        Self::cipher(key)?
            .decrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|_| VaultError::Open)
    }
}

// ------------------------------------------------------------------------------- notices

/// What one owner's notice says (the token only travels sealed).
pub struct ExportNotice<'a> {
    pub delivery_id: Uuid,
    pub export_id: Uuid,
    pub organization_id: Uuid,
    pub recipient: Uuid,
    pub token: &'a str,
    pub occurred_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

fn derived(delivery: Uuid, purpose: &str) -> [u8; 16] {
    let mut h = Sha256::new();
    h.update(b"cfmeet.organizations/export-notice/");
    h.update(purpose.as_bytes());
    h.update(b"/");
    h.update(delivery.as_bytes());
    let digest = h.finalize();
    let mut out = [0u8; 16];
    out.copy_from_slice(&digest[..16]);
    out
}

/// The notice's envelope `eventId` (deterministic per delivery: a re-post is a DUPLICATE).
pub fn notice_event_id(delivery: Uuid) -> String {
    format!("evt_{}", hex::encode(derived(delivery, "event")))
}

fn notice_decision_id(delivery: Uuid) -> Uuid {
    uuid::Builder::from_random_bytes(derived(delivery, "decision")).into_uuid()
}

/// Builds the notices of an export.
pub trait ExportNotices: Send + Sync {
    /// Whether notices can be built at all (`Err(reason)`: the command is refused).
    fn available(&self) -> Result<(), &'static str> {
        Ok(())
    }

    /// The I1 envelope telling `recipient` their link, the token sealed. `Err(reason)`:
    /// nothing can be delivered (the command is refused and nothing is stored).
    fn notice(&self, n: &ExportNotice<'_>) -> Result<Value, &'static str>;
}

/// The Notification Service notice (`organizations.organization.export_ready`), the token
/// sealed to the service key.
pub struct NotificationExportNotices {
    sealing: SealingKey,
}

impl NotificationExportNotices {
    pub fn new(sealing: SealingKey) -> Self {
        Self { sealing }
    }
}

impl ExportNotices for NotificationExportNotices {
    fn available(&self) -> Result<(), &'static str> {
        match cf_notification_client::spec(EXPORT_READY_TYPE) {
            Some(_) => Ok(()),
            None => Err("NOT_IN_CATALOGUE"),
        }
    }

    fn notice(&self, n: &ExportNotice<'_>) -> Result<Value, &'static str> {
        let event_id = notice_event_id(n.delivery_id);
        let route = RouteRef {
            decision_id: notice_decision_id(n.delivery_id),
            routing_version: crate::notification_publications::ROUTING_VERSION,
            protocol: ROUTING_PROTOCOL,
        };
        let envelope = EnvelopeBuilder::new(
            Producer::Organizations,
            EXPORT_READY_TYPE,
            &rfc3339(n.occurred_at),
            route,
        )
        .map_err(|e| match e {
            EnvelopeError::UnknownEventType => "NOT_IN_CATALOGUE",
            _ => "ENVELOPE_INVALID",
        })?
        .event_id(event_id.clone())
        .correlation_id(format!("corr_{}", n.export_id.simple()))
        .subject(n.export_id, Some(1))
        .dedupe_key(format!("{EXPORT_READY_TYPE}:{}", n.delivery_id))
        .idempotency_key(event_id)
        .expires_at(rfc3339(n.expires_at))
        .data("organizationId", n.organization_id.to_string())
        .data("exportId", n.export_id.to_string())
        .data("deliveryId", n.delivery_id.to_string())
        .data("expiresAt", rfc3339(n.expires_at))
        .recipient(account_ref(n.recipient))
        .secret(SealedPlaintext::default().with_token(n.token))
        .build_sealed(&self.sealing)
        .map_err(|_| "ENVELOPE_INVALID")?;
        serde_json::to_value(envelope).map_err(|_| "ENVELOPE_INVALID")
    }
}

/// Everything the export command needs (`None` in the state: the command is refused).
pub struct ExportDelivery {
    pub vault: ExportVault,
    pub notices: Arc<dyn ExportNotices>,
}

impl fmt::Debug for ExportDelivery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExportDelivery")
            .field("vault", &self.vault)
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------------- rows

pub const EXPORT_COLUMNS: &str = "id, organization_id, state, version, size_bytes, sha256, recipients, created_at, updated_at, expires_at, delivered_at, purged_at";

#[derive(Debug, Clone, FromQueryResult)]
pub struct ExportRow {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub state: String,
    pub version: i64,
    pub size_bytes: i64,
    pub sha256: String,
    pub recipients: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub delivered_at: Option<DateTime<Utc>>,
    pub purged_at: Option<DateTime<Utc>>,
}

impl ExportRow {
    /// The state HQ sees: an unused export past its expiry is `EXPIRED` even before the
    /// maintenance pass recorded it.
    pub fn effective_state(&self, now: DateTime<Utc>) -> &str {
        match self.state.as_str() {
            "READY" | "REQUESTED" if self.expires_at <= now => "EXPIRED",
            s => s,
        }
    }
}

pub const DELIVERY_COLUMNS: &str =
    "id, export_id, recipient_user_id, state, created_at, expires_at, sent_at, claimed_at";

#[derive(Debug, Clone, FromQueryResult)]
pub struct DeliveryRow {
    pub id: Uuid,
    pub export_id: Uuid,
    pub recipient_user_id: Uuid,
    pub state: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub sent_at: Option<DateTime<Utc>>,
    pub claimed_at: Option<DateTime<Utc>>,
}

impl DeliveryRow {
    pub fn effective_state(&self, now: DateTime<Utc>) -> &str {
        match self.state.as_str() {
            "PENDING" | "SENT" if self.expires_at <= now => "EXPIRED",
            s => s,
        }
    }
}

fn opt_time(t: Option<DateTime<Utc>>) -> Value {
    t.map(rfc3339).map(Value::String).unwrap_or(Value::Null)
}

/// The reference HQ sees (the bridge command's `resource` and the operator read): ids,
/// states, size, digest and times only.
pub fn resource(e: &ExportRow, deliveries: &[DeliveryRow], now: DateTime<Utc>) -> Value {
    json!({
        "organizationId": e.organization_id,
        "exportId": e.id,
        "state": e.effective_state(now),
        "delivery": DELIVERY,
        "deliveryChannel": CHANNEL,
        "deliveryRef": deliveries.first().map(|d| d.id),
        "recipientMasked": Value::Null,
        "recipientCount": e.recipients,
        "deliveries": deliveries
            .iter()
            .map(|d| json!({
                "deliveryRef": d.id,
                "state": d.effective_state(now),
                "sentAt": opt_time(d.sent_at),
                "claimedAt": opt_time(d.claimed_at),
            }))
            .collect::<Vec<_>>(),
        "sizeBytes": e.size_bytes,
        "sha256": e.sha256,
        "createdAt": rfc3339(e.created_at),
        "expiresAt": rfc3339(e.expires_at),
        "deliveredAt": opt_time(e.delivered_at),
        "purgedAt": opt_time(e.purged_at),
    })
}

/// One export of `organization` (optionally `FOR UPDATE`).
pub async fn load<C: ConnectionTrait>(
    c: &C,
    organization: Uuid,
    export: Uuid,
    lock: bool,
) -> Result<Option<ExportRow>, DbErr> {
    db::one(
        c,
        &format!(
            "SELECT {EXPORT_COLUMNS} FROM organization_exports WHERE id = $1 AND organization_id = $2{}",
            if lock { " FOR UPDATE" } else { "" }
        ),
        vec![export.into(), organization.into()],
    )
    .await
}

/// An export by id, whatever its organization (replay of a bridge command).
pub async fn by_id<C: ConnectionTrait>(c: &C, export: Uuid) -> Result<Option<ExportRow>, DbErr> {
    db::one(
        c,
        &format!("SELECT {EXPORT_COLUMNS} FROM organization_exports WHERE id = $1"),
        vec![export.into()],
    )
    .await
}

/// The deliveries of an export (id order: the first is the resource's `deliveryRef`).
pub async fn deliveries<C: ConnectionTrait>(
    c: &C,
    export: Uuid,
) -> Result<Vec<DeliveryRow>, DbErr> {
    db::all(
        c,
        &format!(
            "SELECT {DELIVERY_COLUMNS} FROM organization_export_deliveries WHERE export_id = $1 ORDER BY id"
        ),
        vec![export.into()],
    )
    .await
}

#[derive(FromQueryResult)]
struct UserRow {
    user_id: Uuid,
}

/// The ACTIVE OWNERs an export is delivered to (oldest memberships first, at most
/// [`MAX_RECIPIENTS`]).
pub async fn active_owners<C: ConnectionTrait>(
    c: &C,
    organization: Uuid,
) -> Result<Vec<Uuid>, DbErr> {
    Ok(db::all::<UserRow, _>(
        c,
        "SELECT user_id FROM organization_memberships \
          WHERE organization_id = $1 AND role = 'OWNER' AND status = 'ACTIVE' \
          ORDER BY created_at, id LIMIT $2",
        vec![organization.into(), MAX_RECIPIENTS.into()],
    )
    .await?
    .into_iter()
    .map(|r| r.user_id)
    .collect())
}

/// A new export, sealed and ready to insert.
pub struct NewExport<'a> {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub idempotency_key: Uuid,
    pub operator_ref: &'a str,
    pub approver_ref: &'a str,
    pub approval_ref: &'a str,
    pub size_bytes: i64,
    pub sha256: &'a str,
    pub sealed: &'a SealedExport,
    pub recipients: i32,
    pub now: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

pub async fn insert_export<C: ConnectionTrait>(
    c: &C,
    n: &NewExport<'_>,
) -> Result<ExportRow, DbErr> {
    db::one(
        c,
        &format!(
            "INSERT INTO organization_exports (id, organization_id, state, version, idempotency_key, operator_ref, \
               approver_ref, approval_ref, size_bytes, sha256, key_id, nonce, ciphertext, recipients, created_at, \
               updated_at, expires_at) \
             VALUES ($1, $2, 'READY', 1, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $13, $14) \
             RETURNING {EXPORT_COLUMNS}"
        ),
        vec![
            n.id.into(),
            n.organization_id.into(),
            n.idempotency_key.into(),
            n.operator_ref.into(),
            n.approver_ref.into(),
            n.approval_ref.into(),
            n.size_bytes.into(),
            n.sha256.into(),
            n.sealed.key_id.clone().into(),
            n.sealed.nonce.clone().into(),
            n.sealed.ciphertext.clone().into(),
            n.recipients.into(),
            n.now.into(),
            n.expires_at.into(),
        ],
    )
    .await?
    .ok_or_else(|| DbErr::Custom("the export row was not returned".into()))
}

/// One owner's delivery: the token's digest and the sealed notice to relay.
pub struct NewDelivery<'a> {
    pub id: Uuid,
    pub export_id: Uuid,
    pub organization_id: Uuid,
    pub recipient: Uuid,
    pub token_hash: &'a str,
    pub notice: Value,
    pub now: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

pub async fn insert_delivery<C: ConnectionTrait>(c: &C, n: NewDelivery<'_>) -> Result<(), DbErr> {
    db::exec(
        c,
        "INSERT INTO organization_export_deliveries (id, export_id, organization_id, recipient_user_id, channel, \
           token_hash, state, notice_event_id, notice, attempts, next_attempt_at, created_at, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, 'PENDING', $7, $8, 0, $9, $9, $10)",
        vec![
            n.id.into(),
            n.export_id.into(),
            n.organization_id.into(),
            n.recipient.into(),
            CHANNEL.into(),
            n.token_hash.into(),
            notice_event_id(n.id).into(),
            n.notice.into(),
            n.now.into(),
            n.expires_at.into(),
        ],
    )
    .await?;
    Ok(())
}

// --------------------------------------------------------------------------------- relay

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ExportReport {
    /// Notices acknowledged by the Notification Service.
    pub sent: usize,
    pub retried: usize,
    /// Notices dead-lettered (rejected, contract refusal, attempts exhausted).
    pub failed: usize,
    /// Links and exports that expired this pass.
    pub expired: u64,
    /// Encrypted copies purged this pass.
    pub purged: u64,
    /// Rows deleted after [`ROW_RETENTION_DAYS`].
    pub deleted: u64,
}

#[derive(Debug, Clone, FromQueryResult)]
struct DueNotice {
    id: Uuid,
    export_id: Uuid,
    notice_event_id: String,
    notice: Value,
    attempts: i32,
}

fn failure_code(f: PublishFailure) -> &'static str {
    match f {
        PublishFailure::Unavailable => "UNAVAILABLE",
        PublishFailure::Rejected => "CONTRACT_REJECTED",
        PublishFailure::NotConfigured => "NOT_CONFIGURED",
    }
}

/// A machine reason from the service, kept only in its recorded shape.
fn reason_code(reason: Option<&str>) -> String {
    reason
        .filter(|r| {
            (2..=64).contains(&r.len())
                && r.as_bytes()[0].is_ascii_uppercase()
                && r.bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        })
        .unwrap_or("REJECTED")
        .to_owned()
}

async fn notice_sent<C: ConnectionTrait>(
    c: &C,
    row: &DueNotice,
    now: DateTime<Utc>,
) -> Result<(), DbErr> {
    db::exec(
        c,
        "UPDATE organization_export_deliveries SET state = 'SENT', notice = NULL, sent_at = $2, \
           attempts = attempts + 1, last_error = NULL WHERE id = $1 AND state = 'PENDING'",
        vec![row.id.into(), now.into()],
    )
    .await?;
    Ok(())
}

async fn notice_failed<C: ConnectionTrait>(
    c: &C,
    row: &DueNotice,
    error: &str,
    attempts: i32,
    now: DateTime<Utc>,
) -> Result<(), DbErr> {
    db::exec(
        c,
        "UPDATE organization_export_deliveries SET state = 'FAILED', notice = NULL, settled_at = $2, \
           attempts = $3, last_error = $4 WHERE id = $1 AND state = 'PENDING'",
        vec![row.id.into(), now.into(), attempts.into(), error.into()],
    )
    .await?;
    Ok(())
}

/// Schedules another attempt; returns whether the notice was dead-lettered instead.
async fn notice_retry<C: ConnectionTrait>(
    c: &C,
    row: &DueNotice,
    error: &str,
    counted: bool,
    now: DateTime<Utc>,
) -> Result<bool, DbErr> {
    let attempts = row.attempts + i32::from(counted);
    if attempts >= MAX_ATTEMPTS {
        notice_failed(c, row, "ATTEMPTS_EXHAUSTED", attempts, now).await?;
        return Ok(true);
    }
    let next = if counted {
        now + backoff(attempts)
    } else {
        now + Duration::seconds(60)
    };
    db::exec(
        c,
        "UPDATE organization_export_deliveries SET attempts = $2, last_error = $3, next_attempt_at = $4 WHERE id = $1",
        vec![row.id.into(), attempts.into(), error.into(), next.into()],
    )
    .await?;
    Ok(false)
}

/// Ends as `FAILED` (purging the encrypted copy) every READY export among `exports` none
/// of whose owners could be notified.
async fn fail_undeliverable<C: ConnectionTrait>(
    c: &C,
    exports: &[Uuid],
    now: DateTime<Utc>,
) -> Result<u64, DbErr> {
    db::exec(
        c,
        "UPDATE organization_exports e SET state = 'FAILED', version = version + 1, updated_at = $2, \
                key_id = NULL, nonce = NULL, ciphertext = NULL, purged_at = $2 \
          WHERE e.id = ANY($1) AND e.state = 'READY' \
            AND NOT EXISTS (SELECT 1 FROM organization_export_deliveries d WHERE d.export_id = e.id AND d.state <> 'FAILED')",
        vec![exports.to_vec().into(), now.into()],
    )
    .await
}

/// One relay pass over at most `limit` due notices, in batches of ≤ [`MAX_BATCH`]
/// (rows locked `FOR UPDATE SKIP LOCKED` for their attempt and settled in the same
/// transaction; a crash mid-attempt re-posts the same envelope, answered `DUPLICATE`).
pub async fn relay_notices(
    db: &DatabaseConnection,
    sink: &dyn NotificationEventSink,
    limit: usize,
) -> Result<ExportReport, DbErr> {
    let mut report = ExportReport::default();
    let mut remaining = limit;
    while remaining > 0 {
        let batch = remaining.min(MAX_BATCH);
        let txn = db.begin().await?;
        let now = db::now(&txn).await?;
        let rows: Vec<DueNotice> = db::all(
            &txn,
            "SELECT id, export_id, notice_event_id, notice, attempts FROM organization_export_deliveries \
              WHERE state = 'PENDING' AND notice IS NOT NULL AND next_attempt_at <= $1 AND expires_at > $1 \
              ORDER BY next_attempt_at, id LIMIT $2 FOR UPDATE SKIP LOCKED",
            vec![now.into(), (batch as i64).into()],
        )
        .await?;
        if rows.is_empty() {
            txn.rollback().await?;
            break;
        }
        remaining = remaining.saturating_sub(rows.len());
        let full = rows.len() == batch;
        let events: Vec<Value> = rows.iter().map(|r| r.notice.clone()).collect();
        let mut stop = false;
        match sink.ingest(&events).await {
            Ok(results) => {
                let by_id: HashMap<&str, &IngestResult> =
                    results.iter().map(|r| (r.event_id.as_str(), r)).collect();
                for row in &rows {
                    match by_id.get(row.notice_event_id.as_str()) {
                        Some(r)
                            if matches!(r.outcome.as_str(), "ACCEPTED" | "DUPLICATE" | "HELD") =>
                        {
                            notice_sent(&txn, row, now).await?;
                            report.sent += 1;
                        }
                        Some(r) if r.outcome == "REJECTED" => {
                            let reason = reason_code(r.reason.as_deref());
                            tracing::warn!(delivery_id = %row.id, reason = %reason, "export notice rejected");
                            notice_failed(&txn, row, &reason, row.attempts + 1, now).await?;
                            report.failed += 1;
                        }
                        _ => {
                            if notice_retry(&txn, row, "NO_RESULT", true, now).await? {
                                report.failed += 1;
                            } else {
                                report.retried += 1;
                            }
                        }
                    }
                }
            }
            Err(f @ PublishFailure::Rejected) => {
                tracing::warn!(
                    notices = rows.len(),
                    "export notice batch refused by the contract"
                );
                for row in &rows {
                    notice_failed(&txn, row, failure_code(f), row.attempts + 1, now).await?;
                    report.failed += 1;
                }
                stop = true;
            }
            Err(f @ PublishFailure::Unavailable) => {
                for row in &rows {
                    if notice_retry(&txn, row, failure_code(f), true, now).await? {
                        report.failed += 1;
                    } else {
                        report.retried += 1;
                    }
                }
                stop = true;
            }
            Err(f @ PublishFailure::NotConfigured) => {
                for row in &rows {
                    notice_retry(&txn, row, failure_code(f), false, now).await?;
                }
                stop = true;
            }
        }
        let exports: Vec<Uuid> = rows.iter().map(|r| r.export_id).collect();
        report.purged += fail_undeliverable(&txn, &exports, now).await?;
        txn.commit().await?;
        if stop || !full {
            break;
        }
    }
    Ok(report)
}

/// Expiry, purge and retention (see the module documentation). At most `batch` rows are
/// deleted per pass.
pub async fn maintain(db: &DatabaseConnection, batch: i64) -> Result<ExportReport, DbErr> {
    let now = db::now(db).await?;
    let links = db::exec(
        db,
        "UPDATE organization_export_deliveries SET state = 'EXPIRED', notice = NULL, settled_at = $1, \
                last_error = CASE WHEN state = 'PENDING' THEN 'EXPIRED' ELSE last_error END \
          WHERE state IN ('PENDING', 'SENT') AND expires_at <= $1",
        vec![now.into()],
    )
    .await?;
    let exports = db::exec(
        db,
        "UPDATE organization_exports SET state = 'EXPIRED', version = version + 1, updated_at = $1, \
                key_id = NULL, nonce = NULL, ciphertext = NULL, purged_at = $1 \
          WHERE state IN ('REQUESTED', 'READY') AND expires_at <= $1",
        vec![now.into()],
    )
    .await?;
    // A delivered export keeps its state; its copy goes at expiry or once every link is
    // settled (used, failed or expired).
    let settled = db::exec(
        db,
        "UPDATE organization_exports e SET key_id = NULL, nonce = NULL, ciphertext = NULL, purged_at = $1, updated_at = $1 \
          WHERE e.ciphertext IS NOT NULL AND e.state = 'DELIVERED' \
            AND (e.expires_at <= $1 OR NOT EXISTS (SELECT 1 FROM organization_export_deliveries d \
                                                  WHERE d.export_id = e.id AND d.state IN ('PENDING', 'SENT')))",
        vec![now.into()],
    )
    .await?;
    let deleted = db::exec(
        db,
        "DELETE FROM organization_exports WHERE id IN (SELECT id FROM organization_exports \
          WHERE created_at < $1 ORDER BY created_at LIMIT $2)",
        vec![
            (now - Duration::days(ROW_RETENTION_DAYS)).into(),
            batch.into(),
        ],
    )
    .await?;
    Ok(ExportReport {
        expired: links + exports,
        purged: exports + settled,
        deleted,
        ..ExportReport::default()
    })
}

/// One worker pass: maintenance always; the notice relay only with a Notification Service
/// target (`CF_NOTIFICATIONS_BASE_URL`).
pub async fn run_once(
    db: &DatabaseConnection,
    sink: Option<&dyn NotificationEventSink>,
) -> Result<ExportReport, DbErr> {
    let maintained = maintain(db, 500).await?;
    let relayed = match sink {
        Some(s) => relay_notices(db, s, 200).await?,
        None => ExportReport::default(),
    };
    Ok(ExportReport {
        expired: maintained.expired,
        purged: maintained.purged + relayed.purged,
        deleted: maintained.deleted,
        ..relayed
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_line(id: &str, byte: u8) -> String {
        format!(
            "{id}:{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([byte; 32])
        )
    }

    #[test]
    fn the_vault_opens_only_its_own_export_of_its_own_organization() {
        let vault = ExportVault::parse(&format!(
            "# exports\n{}\n\n{}\n",
            key_line("exp-2026-11", 7),
            key_line("exp-2026-10", 9)
        ))
        .unwrap();
        assert_eq!(vault.current_key_id(), "exp-2026-11");
        let (export, org) = (Uuid::new_v4(), Uuid::new_v4());
        let clear = br#"{"invitations":[{"email_normalized":"a@example.com"}]}"#;
        let sealed = vault.seal(export, org, clear).unwrap();
        assert_eq!(sealed.key_id, "exp-2026-11");
        assert_eq!(sealed.nonce.len(), 24);
        assert!(!sealed.ciphertext.windows(7).any(|w| w == b"example"));
        let open = |k: &str, e: Uuid, o: Uuid, ct: &[u8]| vault.open(k, &sealed.nonce, ct, e, o);
        assert_eq!(
            open(&sealed.key_id, export, org, &sealed.ciphertext).unwrap(),
            clear
        );
        assert_eq!(
            open(&sealed.key_id, Uuid::new_v4(), org, &sealed.ciphertext),
            Err(VaultError::Open)
        );
        assert_eq!(
            open(&sealed.key_id, export, Uuid::new_v4(), &sealed.ciphertext),
            Err(VaultError::Open)
        );
        let mut tampered = sealed.ciphertext.clone();
        tampered[0] ^= 1;
        assert_eq!(
            open(&sealed.key_id, export, org, &tampered),
            Err(VaultError::Open)
        );
        assert_eq!(
            open("exp-2026-10", export, org, &sealed.ciphertext),
            Err(VaultError::Open)
        );
        assert_eq!(
            open("gone", export, org, &sealed.ciphertext),
            Err(VaultError::UnknownKey)
        );
        // Two seals of the same document never share a nonce.
        assert_ne!(vault.seal(export, org, clear).unwrap().nonce, sealed.nonce);
        // Debug shows key ids only.
        let debug = format!("{vault:?}");
        assert!(debug.contains("exp-2026-11") && !debug.contains("BwcH"));
    }

    #[test]
    fn malformed_key_files_are_refused_without_echoing_values() {
        for (text, why) in [
            ("", "no key"),
            ("# only a comment", "no key"),
            ("nokeyseparator", "lines"),
            ("bad id!:AAAA", "key id"),
            ("k:not base64 at all", "base64url"),
            ("k:AAAA", "32 bytes"),
        ] {
            let e = ExportVault::parse(text).unwrap_err().to_string();
            assert!(e.contains(why), "{text:?}: {e}");
            assert!(!e.contains("AAAA"));
        }
        let twice = format!("{}\n{}", key_line("k", 1), key_line("k", 2));
        assert!(ExportVault::parse(&twice)
            .unwrap_err()
            .to_string()
            .contains("twice"));
    }

    #[test]
    fn notices_are_deterministic_per_delivery() {
        let d = Uuid::new_v4();
        assert_eq!(notice_event_id(d), notice_event_id(d));
        assert_ne!(notice_event_id(d), notice_event_id(Uuid::new_v4()));
        let id = notice_event_id(d);
        assert!(id.starts_with("evt_") && id.len() == 36);
        assert_ne!(notice_decision_id(d), notice_decision_id(Uuid::new_v4()));
    }

    /// The Notification Service catalogue does not carry the notice type yet: the notice
    /// is refused (`NOT_IN_CATALOGUE`, so the command builds and stores nothing). Once it
    /// does, the notice must validate with the token sealed and nothing else carried.
    #[test]
    fn the_notice_needs_its_catalogue_type_and_seals_the_token() {
        let (kid_public, _private) =
            cf_notification_client::seal::generate_keypair("notif-hpke-2026-11").unwrap();
        let notices = NotificationExportNotices::new(SealingKey::parse(&kid_public).unwrap());
        let now = Utc::now();
        let n = ExportNotice {
            delivery_id: Uuid::new_v4(),
            export_id: Uuid::new_v4(),
            organization_id: Uuid::new_v4(),
            recipient: Uuid::new_v4(),
            token: "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG",
            occurred_at: now,
            expires_at: now + export_ttl(),
        };
        match cf_notification_client::spec(EXPORT_READY_TYPE) {
            None => {
                assert_eq!(notices.available(), Err("NOT_IN_CATALOGUE"));
                assert_eq!(notices.notice(&n), Err("NOT_IN_CATALOGUE"));
            }
            Some(_) => {
                notices.available().unwrap();
                let v = notices.notice(&n).unwrap();
                let text = v.to_string();
                assert!(!text.contains(n.token));
                assert!(v.get("sealed").is_some());
                assert_eq!(v["eventId"], notice_event_id(n.delivery_id));
            }
        }
    }

    #[test]
    fn the_reference_carries_no_document_token_url_or_address() {
        let now = Utc::now();
        let e = ExportRow {
            id: Uuid::new_v4(),
            organization_id: Uuid::new_v4(),
            state: "READY".into(),
            version: 1,
            size_bytes: 1234,
            sha256: "0".repeat(64),
            recipients: 2,
            created_at: now,
            updated_at: now,
            expires_at: now + export_ttl(),
            delivered_at: None,
            purged_at: None,
        };
        let d = |state: &str| DeliveryRow {
            id: Uuid::new_v4(),
            export_id: e.id,
            recipient_user_id: Uuid::new_v4(),
            state: state.into(),
            created_at: now,
            expires_at: e.expires_at,
            sent_at: None,
            claimed_at: None,
        };
        let ds = vec![d("PENDING"), d("SENT")];
        let v = resource(&e, &ds, now);
        assert_eq!(v["state"], "READY");
        assert_eq!(v["delivery"], "OWNER_DELIVERED");
        assert_eq!(v["deliveryChannel"], "DOWNLOAD_LINK_EMAIL");
        assert_eq!(v["deliveryRef"], json!(ds[0].id));
        assert!(v["recipientMasked"].is_null());
        assert_eq!(v["deliveries"].as_array().unwrap().len(), 2);
        let text = v.to_string();
        for forbidden in [
            "token",
            "url",
            "http",
            "@",
            "email",
            "userId",
            "recipientUser",
        ] {
            assert!(!text.contains(forbidden), "{forbidden} in {v:#}");
        }
        // Past expiry the reference reads EXPIRED before maintenance records it.
        let later = e.expires_at + Duration::seconds(1);
        assert_eq!(resource(&e, &ds, later)["state"], "EXPIRED");
        assert_eq!(
            resource(&e, &ds, later)["deliveries"][1]["state"],
            "EXPIRED"
        );
    }

    #[test]
    fn service_reasons_are_kept_only_in_their_machine_shape() {
        assert_eq!(reason_code(Some("NO_ADDRESS")), "NO_ADDRESS");
        assert_eq!(reason_code(Some("a@b.c")), "REJECTED");
        assert_eq!(reason_code(None), "REJECTED");
    }
}
