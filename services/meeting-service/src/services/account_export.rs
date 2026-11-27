//! ADR 0068 A7 — `accounts.user.export`: a personal-data export that HQ staff request and
//! the Meeting Service delivers to the account OWNER only (owner decision and design agreed
//! with HQ, 2026-11-21). HQ never sees, stores or relays the exported data.
//!
//! 1. [`request`] (the bridge command, R4): in one transaction the key is claimed, the
//!    account locked at HQ's expected `account_version`, an `account_data_exports` row
//!    recorded REQUESTED (the owner's VERIFIED address only masked) and the command
//!    recorded in `meeting_admin_operations`. Nothing else is answered than a reference.
//! 2. [`process`] (spawned right after the commit, retried by the [`sweep`] worker): builds
//!    the same document `GET /api/v2/me/data-export` answers, SEALS it
//!    (XChaCha20-Poly1305, a key derived from the service's sealing secret
//!    `MFA_ENCRYPTION_KEY` / `JWT_SECRET`, bound to the export and the account), stores only
//!    the sealed copy, the size and the SHA-256 of the document, mints a single-use download
//!    token (stored as its SHA-256 only) and, in the same transaction, queues ONE email to
//!    the owner's verified address with the link (READY).
//! 3. [`download`] (`GET /api/v2/me/data-export?exportId=…` with the token in the
//!    `X-CF-Data-Export-Token` header): the owner, signed in again (a session-bound sign-in
//!    within the re-authentication window), presents the token once; the copy is opened, answered and purged in the same transaction
//!    (DELIVERED). A used, foreign, wrong or expired link is one `404`.
//! 4. Retention: [`RETENTION`] (7 days) after the request the copy and the token are purged
//!    (EXPIRED); the copy of an account deleted meanwhile is purged at once. Expiry means
//!    the command is re-issued; nothing is ever resent.
//!
//! Logs, the operations ledger and the answers never contain the document, the token, the
//! link or the clear address.

use base64::Engine;
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    XChaCha20Poly1305, XNonce,
};
use chrono::{DateTime, Duration, Utc};
use hmac::{Hmac, Mac};
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    error::{ApiError, ApiResult},
    services::{
        account_security::{self as security, ActorKind, Outcome, SecurityEvent},
        admin_operations::{self as ledger, subject, BridgeCommand},
        email_delivery::{self, category, NewEmail},
    },
    state::AppState,
};

/// How long a requested export can be downloaded before it is purged.
pub const RETENTION: Duration = Duration::days(7);
/// The only delivery channel of an export.
pub const DELIVERY_CHANNEL: &str = "DOWNLOAD_LINK_EMAIL";
/// Builds attempted before an export is given up (FAILED).
const MAX_ATTEMPTS: i32 = 5;
/// How long one build may hold an export before the worker retries it.
const LEASE_MINUTES: i64 = 10;

/// The export states (`ck_account_data_exports_state`).
pub mod state {
    pub const REQUESTED: &str = "REQUESTED";
    pub const READY: &str = "READY";
    pub const DELIVERED: &str = "DELIVERED";
    pub const EXPIRED: &str = "EXPIRED";
    pub const FAILED: &str = "FAILED";
}

fn stmt(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

// ---------------------------------------------------------------------------
// Sealing
// ---------------------------------------------------------------------------

const KEY_LABEL: &[u8] = b"cfmeet.account-export.key.v1";
const AAD_LABEL: &[u8] = b"cfmeet.account-export.v1|";
const NONCE_LEN: usize = 24;

/// The export sealing key: HMAC-SHA256 of a fixed label under the service sealing secret
/// (`MFA_ENCRYPTION_KEY`, else `JWT_SECRET`); its id is that secret's key id, so a rotated
/// secret keeps opening exports sealed before the rotation (`MFA_PREVIOUS_ENCRYPTION_KEYS`
/// / `JWT_PREVIOUS_SECRETS`).
pub struct ExportKey {
    key: Zeroizing<[u8; 32]>,
    kid: String,
}

impl std::fmt::Debug for ExportKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExportKey")
            .field("kid", &self.kid)
            .finish_non_exhaustive()
    }
}

impl ExportKey {
    pub fn derive(secret: &str) -> Self {
        let mut mac =
            <Hmac<Sha256> as Mac>::new_from_slice(secret.as_bytes()).expect("any key length");
        mac.update(KEY_LABEL);
        let mut key = Zeroizing::new([0u8; 32]);
        key.copy_from_slice(&mac.finalize().into_bytes());
        Self {
            key,
            kid: crate::services::auth::key_id(secret),
        }
    }

    /// The key exports are sealed with now.
    pub fn current() -> Self {
        Self::derive(&crate::services::mfa::key())
    }

    /// The key a sealed export names (the current one or one still accepted after a
    /// rotation).
    pub fn by_id(kid: &str) -> Option<Self> {
        std::iter::once(crate::services::mfa::key())
            .chain(crate::services::mfa::previous_keys())
            .find(|k| crate::services::auth::key_id(k) == kid)
            .map(|k| Self::derive(&k))
    }

    pub fn key_id(&self) -> &str {
        &self.kid
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new_from_slice(&self.key[..]).expect("a 32-byte key")
    }

    fn aad(export_id: Uuid, user_id: Uuid) -> Vec<u8> {
        let mut aad = AAD_LABEL.to_vec();
        aad.extend_from_slice(export_id.as_bytes());
        aad.extend_from_slice(user_id.as_bytes());
        aad
    }

    /// `nonce (24) ‖ ciphertext`, bound to the export and the account.
    pub fn seal(&self, export_id: Uuid, user_id: Uuid, plaintext: &[u8]) -> ApiResult<Vec<u8>> {
        let nonce: [u8; NONCE_LEN] = rand::random();
        let aad = Self::aad(export_id, user_id);
        let ct = self
            .cipher()
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad: &aad,
                },
            )
            .map_err(|_| ApiError::InternalError("the export cannot be sealed".into()))?;
        let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        Ok(out)
    }

    /// The document; `Err` for another key, export or account, or a tampered blob.
    pub fn open(
        &self,
        export_id: Uuid,
        user_id: Uuid,
        sealed: &[u8],
    ) -> ApiResult<Zeroizing<Vec<u8>>> {
        let bad = || ApiError::InternalError("the export does not open".into());
        if sealed.len() < NONCE_LEN + 16 {
            return Err(bad());
        }
        let aad = Self::aad(export_id, user_id);
        self.cipher()
            .decrypt(
                XNonce::from_slice(&sealed[..NONCE_LEN]),
                Payload {
                    msg: &sealed[NONCE_LEN..],
                    aad: &aad,
                },
            )
            .map(Zeroizing::new)
            .map_err(|_| bad())
    }
}

// ---------------------------------------------------------------------------
// Tokens and links
// ---------------------------------------------------------------------------

/// A new single-use download token (256 random bits, base64url).
fn new_token() -> Zeroizing<String> {
    Zeroizing::new(
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>()),
    )
}

/// The stored form of a token (SHA-256, lower-case hex).
pub fn token_digest(token: &str) -> String {
    hex::encode(Sha256::digest(token.trim().as_bytes()))
}

/// The web app's download page; it signs the owner in again and sends the token to
/// `GET /api/v2/me/data-export?exportId=…` (header `X-CF-Data-Export-Token`).
pub fn download_link(app_url: &str, export_id: Uuid, token: &str) -> String {
    format!(
        "{}/settings/data-export/download?export={export_id}&token={token}",
        app_url.trim_end_matches('/')
    )
}

// ---------------------------------------------------------------------------
// Request (the bridge command)
// ---------------------------------------------------------------------------

/// `accounts.user.export` (R4): records the export REQUESTED and the command, in one
/// transaction at HQ's expected `account_version`; the build starts after the commit.
/// Refused (nothing done): no such account (404), a deleted, disabled or guest account
/// (`STATE_CONFLICT`: the owner could not sign in to download it), an account without a
/// verified address (`STATE_CONFLICT`: nowhere to deliver it), a stale version (409).
pub async fn request(app: &AppState, user_id: Uuid, cmd: &BridgeCommand) -> ApiResult<Uuid> {
    let txn = app.db.begin().await?;
    ledger::claim(&txn, cmd).await?;
    let version = ledger::account_version(&txn, user_id, true)
        .await?
        .ok_or_else(|| ApiError::NotFound("User not found".into()))?;
    ledger::check_version(cmd, version)?;
    let account = txn
        .query_one(stmt(
            "SELECT email, status::text AS status, role::text AS role, \
                    (email_verified_at IS NOT NULL OR auth_type::text = 'google') AS verified, \
                    (email LIKE 'deleted+%@deleted.invalid') AS erased \
               FROM users WHERE id = $1",
            vec![user_id.into()],
        ))
        .await?
        .ok_or_else(|| ApiError::NotFound("User not found".into()))?;
    let email: String = account.try_get("", "email")?;
    let status: String = account.try_get("", "status")?;
    let role: String = account.try_get("", "role")?;
    if account.try_get::<bool>("", "erased")? || status != "active" || role == "guest" {
        return Err(ApiError::Conflict(
            "The account owner cannot sign in to download an export".into(),
        ));
    }
    if !account.try_get::<bool>("", "verified")? {
        return Err(ApiError::Conflict(
            "The account has no verified address to deliver the export to".into(),
        ));
    }
    let export_id = Uuid::new_v4();
    let now = Utc::now();
    txn.execute(stmt(
        "INSERT INTO account_data_exports (id, user_id, idempotency_key, delivery_ref, state, delivery_channel, \
           recipient_masked, requested_by, requested_at, expires_at, version) \
         VALUES ($1, $2, $3, $4, 'REQUESTED', $5, $6, $7, $8, $9, 1)",
        vec![
            export_id.into(),
            user_id.into(),
            cmd.key.into(),
            Uuid::new_v4().into(),
            DELIVERY_CHANNEL.into(),
            crate::services::admin_operator::mask_email(&email).into(),
            cmd.operator.clone().into(),
            now.into(),
            (now + RETENTION).into(),
        ],
    ))
    .await?;
    security::record_event(
        &txn,
        SecurityEvent::new("ADMIN_DATA_EXPORT_REQUESTED", Outcome::Success)
            .user(user_id)
            .actor(ActorKind::Admin, None)
            .detail(json!({
                "exportId": export_id, "deliveryChannel": DELIVERY_CHANNEL,
                "operator": cmd.operator(), "approver": cmd.approver(),
            })),
    )
    .await?;
    ledger::record(&txn, cmd, subject::USER, user_id, state::REQUESTED, Some(1)).await?;
    txn.commit().await?;
    let app = app.clone();
    tokio::spawn(async move {
        if let Err(e) = process(&app, export_id).await {
            tracing::warn!(%export_id, "account export build deferred to the worker: {e}");
        }
    });
    Ok(export_id)
}

// ---------------------------------------------------------------------------
// Build and deliver
// ---------------------------------------------------------------------------

async fn fail<C: ConnectionTrait>(db: &C, export_id: Uuid, code: &str) -> ApiResult<()> {
    db.execute(stmt(
        "UPDATE account_data_exports SET state = 'FAILED', failure_code = $2, sealed = NULL, \
                token_digest = NULL, lease_until = NULL, purged_at = now(), version = version + 1 \
          WHERE id = $1 AND state IN ('REQUESTED', 'READY')",
        vec![export_id.into(), code.into()],
    ))
    .await?;
    tracing::warn!(%export_id, code, "account export failed");
    Ok(())
}

/// Builds, seals and delivers one REQUESTED export (a no-op for any other state, or while
/// another build holds it). A transient failure leaves it REQUESTED for the worker; after
/// [`MAX_ATTEMPTS`] it is FAILED.
pub async fn process(app: &AppState, export_id: Uuid) -> ApiResult<()> {
    let leased = app
        .db
        .query_one(stmt(
            "UPDATE account_data_exports SET lease_until = now() + make_interval(mins => $2), \
                    attempts = attempts + 1 \
              WHERE id = $1 AND state = 'REQUESTED' AND expires_at > now() \
                AND (lease_until IS NULL OR lease_until < now()) \
          RETURNING user_id, attempts",
            vec![export_id.into(), (LEASE_MINUTES as i32).into()],
        ))
        .await?;
    let Some(leased) = leased else {
        return Ok(());
    };
    let user_id: Uuid = leased.try_get("", "user_id")?;
    let attempts: i32 = leased.try_get("", "attempts")?;
    let document = match crate::services::account_erasure::export(app, user_id).await {
        Ok(d) => d,
        Err(ApiError::NotFound(_)) => return fail(&app.db, export_id, "ACCOUNT_NOT_FOUND").await,
        Err(e) => {
            if attempts >= MAX_ATTEMPTS {
                return fail(&app.db, export_id, "BUILD_FAILED").await;
            }
            app.db
                .execute(stmt(
                    "UPDATE account_data_exports SET lease_until = NULL WHERE id = $1 AND state = 'REQUESTED'",
                    vec![export_id.into()],
                ))
                .await?;
            return Err(e);
        }
    };
    let plaintext = Zeroizing::new(
        serde_json::to_vec_pretty(&document)
            .map_err(|_| ApiError::InternalError("the export cannot be serialized".into()))?,
    );
    drop(document);
    let sha256 = hex::encode(Sha256::digest(&plaintext[..]));
    let size = plaintext.len() as i64;
    let key = ExportKey::current();
    let sealed = key.seal(export_id, user_id, &plaintext)?;
    drop(plaintext);
    let token = new_token();

    let txn = app.db.begin().await?;
    let row = txn
        .query_one(stmt(
            "SELECT e.state, e.delivery_ref, e.expires_at, u.email, u.username, \
                    (u.email_verified_at IS NOT NULL OR u.auth_type::text = 'google') AS verified, \
                    (u.email LIKE 'deleted+%@deleted.invalid' OR u.status::text <> 'active') AS gone \
               FROM account_data_exports e JOIN users u ON u.id = e.user_id \
              WHERE e.id = $1 FOR UPDATE OF e",
            vec![export_id.into()],
        ))
        .await?;
    let Some(row) = row else {
        return Ok(());
    };
    if row.try_get::<String>("", "state")? != state::REQUESTED {
        return Ok(());
    }
    if row.try_get::<bool>("", "gone")? {
        fail(&txn, export_id, "ACCOUNT_UNAVAILABLE").await?;
        txn.commit().await?;
        return Ok(());
    }
    if !row.try_get::<bool>("", "verified")? {
        fail(&txn, export_id, "RECIPIENT_UNVERIFIED").await?;
        txn.commit().await?;
        return Ok(());
    }
    let delivery_ref: Uuid = row.try_get("", "delivery_ref")?;
    let expires_at: DateTime<Utc> = row.try_get("", "expires_at")?;
    let email: String = row.try_get("", "email")?;
    let username: String = row.try_get("", "username")?;
    txn.execute(stmt(
        "UPDATE account_data_exports SET state = 'READY', sealed = $2, key_id = $3, token_digest = $4, \
                size_bytes = $5, sha256 = $6, ready_at = now(), lease_until = NULL, version = version + 1 \
          WHERE id = $1",
        vec![
            export_id.into(),
            sealed.into(),
            key.key_id().to_string().into(),
            token_digest(&token).into(),
            size.into(),
            sha256.into(),
        ],
    ))
    .await?;
    let link = Zeroizing::new(download_link(
        app.email_service.app_url(),
        export_id,
        &token,
    ));
    let template = crate::templates::email::data_export_ready_email(
        &username,
        &link,
        &expires_at.format("%Y-%m-%d %H:%M").to_string(),
    );
    let enqueued = email_delivery::enqueue(
        &txn,
        NewEmail {
            category: category::NOTIFICATION,
            idempotency_key: Some(format!("account-export:{delivery_ref}")),
            to: email,
            to_name: Some(username),
            subject: "Your CF Meet data export is ready".into(),
            html: template.html,
            text: template.text,
            expires_at: Some(expires_at),
        },
    )
    .await;
    match enqueued {
        Ok(e) => {
            txn.execute(stmt(
                "UPDATE account_data_exports SET email_delivery_id = $2 WHERE id = $1",
                vec![export_id.into(), e.id.into()],
            ))
            .await?;
            txn.commit().await?;
            tracing::info!(%export_id, "account export ready; download link queued to the owner");
        }
        // The stored address cannot receive email: nothing could ever be delivered.
        Err(ApiError::BadRequest(_)) => {
            drop(txn);
            fail(&app.db, export_id, "RECIPIENT_UNUSABLE").await?;
        }
        Err(e) => return Err(e),
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Download (the owner)
// ---------------------------------------------------------------------------

fn invalid_link() -> ApiError {
    ApiError::NotFound("This download link is invalid, already used or expired".into())
}

/// The owner downloads their export once: the token must match a READY, unexpired export of
/// THIS account. The copy and the token are purged in the same transaction (DELIVERED).
pub async fn download(
    app: &AppState,
    user_id: Uuid,
    export_id: Uuid,
    token: &str,
) -> ApiResult<Zeroizing<Vec<u8>>> {
    let token = token.trim();
    if token.is_empty() || token.len() > 128 {
        return Err(invalid_link());
    }
    // Deployments without the admin-operations history have no exports at all.
    let present = app
        .db
        .query_one(stmt(
            "SELECT to_regclass('account_data_exports') IS NOT NULL AS present",
            vec![],
        ))
        .await?
        .map(|r| r.try_get::<bool>("", "present"))
        .transpose()?
        .unwrap_or(false);
    if !present {
        return Err(invalid_link());
    }
    let txn = app.db.begin().await?;
    let row = txn
        .query_one(stmt(
            "SELECT sealed, key_id FROM account_data_exports \
              WHERE id = $1 AND user_id = $2 AND token_digest = $3 AND state = 'READY' \
                AND expires_at > now() FOR UPDATE",
            vec![export_id.into(), user_id.into(), token_digest(token).into()],
        ))
        .await?
        .ok_or_else(invalid_link)?;
    let sealed: Vec<u8> = row.try_get("", "sealed")?;
    let kid: String = row.try_get("", "key_id")?;
    let opened = ExportKey::by_id(&kid)
        .ok_or_else(|| ApiError::InternalError("the export key is no longer available".into()))
        .and_then(|k| k.open(export_id, user_id, &sealed));
    let document = match opened {
        Ok(d) => d,
        Err(e) => {
            fail(&txn, export_id, "UNSEALABLE").await?;
            txn.commit().await?;
            return Err(e);
        }
    };
    txn.execute(stmt(
        "UPDATE account_data_exports SET state = 'DELIVERED', delivered_at = now(), sealed = NULL, \
                token_digest = NULL, purged_at = now(), version = version + 1 WHERE id = $1",
        vec![export_id.into()],
    ))
    .await?;
    security::record_event(
        &txn,
        SecurityEvent::new("DATA_EXPORT_DOWNLOADED", Outcome::Success)
            .by_user(user_id)
            .detail(json!({ "exportId": export_id })),
    )
    .await?;
    txn.commit().await?;
    Ok(document)
}

// ---------------------------------------------------------------------------
// Retention worker
// ---------------------------------------------------------------------------

/// What one sweep did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Swept {
    pub expired: u64,
    pub purged_deleted_accounts: u64,
    pub retried: usize,
}

/// Expires every export past its retention (copy and token purged), purges the open export
/// of an account deleted meanwhile, and retries REQUESTED exports whose build lapsed.
pub async fn sweep(app: &AppState) -> ApiResult<Swept> {
    let expired = app
        .db
        .execute(stmt(
            "UPDATE account_data_exports SET state = 'EXPIRED', sealed = NULL, token_digest = NULL, \
                    lease_until = NULL, purged_at = now(), version = version + 1 \
              WHERE state IN ('REQUESTED', 'READY') AND expires_at <= now()",
            vec![],
        ))
        .await?
        .rows_affected();
    let purged = app
        .db
        .execute(stmt(
            "UPDATE account_data_exports e SET state = 'FAILED', failure_code = 'ACCOUNT_DELETED', \
                    sealed = NULL, token_digest = NULL, lease_until = NULL, purged_at = now(), \
                    version = e.version + 1 \
               FROM users u \
              WHERE u.id = e.user_id AND e.state IN ('REQUESTED', 'READY') \
                AND u.email LIKE 'deleted+%@deleted.invalid'",
            vec![],
        ))
        .await?
        .rows_affected();
    let due = app
        .db
        .query_all(stmt(
            "SELECT id FROM account_data_exports WHERE state = 'REQUESTED' \
               AND (lease_until IS NULL OR lease_until < now()) \
               AND requested_at < now() - interval '1 minute' ORDER BY requested_at LIMIT 20",
            vec![],
        ))
        .await?;
    let mut retried = 0;
    for row in due {
        let id: Uuid = row.try_get("", "id")?;
        retried += 1;
        if let Err(e) = process(app, id).await {
            tracing::warn!(export_id = %id, "account export build failed again: {e}");
        }
    }
    Ok(Swept {
        expired,
        purged_deleted_accounts: purged,
        retried,
    })
}

/// The retention worker (started with the admin operator API).
pub fn spawn_worker(app: AppState, every: std::time::Duration) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            match sweep(&app).await {
                Ok(s) if s != Swept::default() => tracing::info!(
                    expired = s.expired,
                    purged = s.purged_deleted_accounts,
                    retried = s.retried,
                    "account export retention sweep"
                ),
                Ok(_) => {}
                Err(e) => tracing::warn!("account export retention sweep failed: {e}"),
            }
        }
    })
}

// ---------------------------------------------------------------------------
// The reference HQ sees
// ---------------------------------------------------------------------------

const RESOURCE_SQL: &str =
    "SELECT id, user_id, delivery_ref, state, delivery_channel, recipient_masked, \
       size_bytes, sha256, email_delivery_id, failure_code, requested_at, ready_at, delivered_at, \
       expires_at, purged_at, version::bigint AS version FROM account_data_exports";

fn rfc(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn opt_rfc(t: Option<DateTime<Utc>>) -> Value {
    t.map(|t| Value::String(rfc(t))).unwrap_or(Value::Null)
}

/// The export as HQ sees it: a reference only (never the document, a token, a link, a
/// sealed blob or the clear address).
fn resource_of(r: &sea_orm::QueryResult) -> ApiResult<Value> {
    Ok(json!({
        "delivery": "OWNER_DELIVERED",
        "deliveryChannel": r.try_get::<String>("", "delivery_channel")?,
        "deliveryRef": r.try_get::<Uuid>("", "delivery_ref")?,
        "recipientMasked": r.try_get::<String>("", "recipient_masked")?,
        "expiresAt": rfc(r.try_get("", "expires_at")?),
        "exportId": r.try_get::<Uuid>("", "id")?,
        "userId": r.try_get::<Uuid>("", "user_id")?,
        "state": r.try_get::<String>("", "state")?,
        "sizeBytes": r.try_get::<Option<i64>>("", "size_bytes")?,
        "sha256": r.try_get::<Option<String>>("", "sha256")?,
        "emailDeliveryId": r.try_get::<Option<Uuid>>("", "email_delivery_id")?,
        "failureCode": r.try_get::<Option<String>>("", "failure_code")?,
        "requestedAt": rfc(r.try_get("", "requested_at")?),
        "readyAt": opt_rfc(r.try_get("", "ready_at")?),
        "deliveredAt": opt_rfc(r.try_get("", "delivered_at")?),
        "purgedAt": opt_rfc(r.try_get("", "purged_at")?),
        "version": r.try_get::<i64>("", "version")?,
    }))
}

/// The export recorded under a bridge `Idempotency-Key`.
pub async fn resource_by_key<C: ConnectionTrait>(db: &C, key: Uuid) -> ApiResult<Option<Value>> {
    db.query_one(stmt(
        &format!("{RESOURCE_SQL} WHERE idempotency_key = $1"),
        vec![key.into()],
    ))
    .await?
    .map(|r| resource_of(&r))
    .transpose()
}

/// One export by id.
pub async fn resource<C: ConnectionTrait>(db: &C, export_id: Uuid) -> ApiResult<Option<Value>> {
    db.query_one(stmt(
        &format!("{RESOURCE_SQL} WHERE id = $1"),
        vec![export_id.into()],
    ))
    .await?
    .map(|r| resource_of(&r))
    .transpose()
}

/// An account's exports, newest first; `after` is the `(requested_at, id)` of the last one
/// of the previous page. Returns at most `limit + 1` (the caller pages).
pub async fn list<C: ConnectionTrait>(
    db: &C,
    user_id: Uuid,
    after: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
) -> ApiResult<Vec<(DateTime<Utc>, Uuid, Value)>> {
    let (at, id) = match after {
        Some((a, i)) => (Some(a), Some(i)),
        None => (None, None),
    };
    let found = db
        .query_all(stmt(
            &format!(
                "{RESOURCE_SQL} WHERE user_id = $1 \
                   AND ($2::timestamptz IS NULL OR (requested_at, id) < ($2, $3::uuid)) \
                 ORDER BY requested_at DESC, id DESC LIMIT $4"
            ),
            vec![user_id.into(), at.into(), id.into(), (limit + 1).into()],
        ))
        .await?;
    found
        .iter()
        .map(|r| {
            Ok((
                r.try_get::<DateTime<Utc>>("", "requested_at")?,
                r.try_get::<Uuid>("", "id")?,
                resource_of(r)?,
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_exports_open_only_for_their_export_and_account() {
        let key = ExportKey::derive("secret-one");
        let (export, user) = (Uuid::new_v4(), Uuid::new_v4());
        let sealed = key.seal(export, user, b"{\"account\":{}}").unwrap();
        assert!(!sealed.windows(9).any(|w| w == b"\"account\""));
        assert_eq!(
            &key.open(export, user, &sealed).unwrap()[..],
            b"{\"account\":{}}"
        );
        assert!(key.open(Uuid::new_v4(), user, &sealed).is_err());
        assert!(key.open(export, Uuid::new_v4(), &sealed).is_err());
        assert!(ExportKey::derive("secret-two")
            .open(export, user, &sealed)
            .is_err());
        let mut tampered = sealed.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(key.open(export, user, &tampered).is_err());
        assert!(key.open(export, user, &sealed[..10]).is_err());
        assert_eq!(key.key_id(), crate::services::auth::key_id("secret-one"));
        assert!(!format!("{key:?}").contains("key:"));
    }

    #[test]
    fn tokens_are_random_and_stored_only_as_digests() {
        let (a, b) = (new_token(), new_token());
        assert_ne!(*a, *b);
        assert_eq!(a.len(), 43);
        let d = token_digest(&a);
        assert_eq!(d.len(), 64);
        assert!(!d.contains(a.as_str()));
        assert_eq!(token_digest(&format!(" {} ", a.as_str())), d);
        let id = Uuid::nil();
        assert_eq!(
            download_link("https://meet.example/", id, "tok"),
            format!("https://meet.example/settings/data-export/download?export={id}&token=tok")
        );
    }
}
