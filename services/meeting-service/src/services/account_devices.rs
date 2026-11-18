//! Account-wide linked devices: one primary mobile, several web devices, web-only accounts.
//!
//! The Account authority (this service) owns the device policy. A *device* is one browser
//! profile or one mobile app installation, identified by a client-generated installation
//! id and bound to a device-generated Ed25519 key. A sign-in (`account_sessions`) belongs
//! to at most one device; tabs of one browser profile share the profile's sign-in, so they
//! are one device.
//!
//! States (per account):
//! * `WEB_ONLY` — no primary mobile; trusted web devices work on their own;
//! * `MOBILE_PRIMARY` — exactly one ACTIVE mobile (database-enforced), plus web devices;
//! * `DEVICE_REPLACEMENT_PENDING` — a newly signed-in mobile waits for approval by a
//!   trusted device or an email step-up. Its sign-in is refused everywhere except the
//!   device endpoints until the replacement completes;
//! * a device or sign-in that is `REVOKED` has no authority (refused on every request).
//!
//! Trust is what lets a device approve other devices and enroll a personal E2EE
//! (Messaging) device. A device becomes trusted by: bootstrap (the first device of an
//! account that never had a trusted device), approval from a trusted device (the approver
//! types the comparison code shown on the new device and signs with its own device key),
//! or an email step-up code (account recovery; proves control of the account mailbox).
//! A sign-in code alone gives a usable but untrusted web sign-in (Meetings and
//! Calling keep working; E2EE enrollment and approvals do not).
//!
//! Every proof is an Ed25519 signature over a domain-separated statement with a purpose,
//! the device key thumbprint, a purpose-specific context, a timestamp (±window) and a
//! single-use nonce recorded in PostgreSQL (replay refused). Nothing here classifies a
//! device from the User-Agent.
//!
//! Sessions that never declared a device (every sign-in made before this change, and
//! clients that do not declare) stay *unclassified*: they keep working exactly as before
//! and do not count against the device limits.
use base64::Engine as _;
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, VerifyingKey};
use rand::Rng;
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend, Statement, TransactionTrait};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::{ApiError, ApiResult, ErrorCode},
    models::users,
    services::account_security::{self as security, ActorKind, Outcome, SecurityEvent},
    state::AppState,
};

pub const DEFAULT_MAX_WEB_DEVICES: i64 = 4;
const PROOF_DOMAIN: &str = "cfmeet-account-device-proof:v1";
const INSTALLATION_DOMAIN: &str = "cfmeet-account-installation:v1";
const MAX_LINKS_PER_HOUR: i64 = 10;
const MAX_LINK_ATTEMPTS: i32 = 5;
const EMAIL_RESEND_SECS: i64 = 60;

/// Backend device policy (environment; no client hard-codes these values).
#[derive(Debug, Clone)]
pub struct DevicePolicy {
    /// `ACCOUNT_MAX_WEB_DEVICES` (default 4, bounded 1-20): active web devices per account.
    pub max_web_devices: i64,
    /// `ACCOUNT_DEVICE_LINK_TTL_SECS` (default 300, bounded 60-900).
    pub link_ttl_secs: i64,
    /// `ACCOUNT_DEVICE_PROOF_WINDOW_SECS` (default 300, bounded 30-900).
    pub proof_window_secs: i64,
    /// `ACCOUNT_DEVICE_EMAIL_STEP_UP` (default on): an emailed code can approve a link
    /// (web-only bootstrap after device loss, mobile replacement without a trusted device).
    pub email_step_up: bool,
    /// `ACCOUNT_E2EE_REQUIRES_TRUSTED_DEVICE` (default on): personal E2EE device enrollment
    /// through the Messaging relay needs a trusted account device.
    pub e2ee_requires_trusted_device: bool,
    /// `ACCOUNT_DEVICE_DECLARATION_REQUIRED_FROM` (RFC 3339; unset = compatibility mode):
    /// from this instant a sign-in must declare its device (`POST /api/v2/me/devices/current`)
    /// within its declaration window, or every authenticated request and refresh of it is
    /// refused with 403 `DEVICE_DECLARATION_REQUIRED`. Legacy (unclassified) sign-ins then
    /// end; nothing is deleted. Set it once every client declares at sign-in.
    pub declaration_required_from: Option<chrono::DateTime<chrono::Utc>>,
    /// How long after sign-in a device can still be declared (the re-authentication window,
    /// `ACCOUNT_REAUTH_MAX_AGE_SECS`); set by the account policy.
    pub declaration_window_secs: i64,
}

impl DevicePolicy {
    /// Whether an unclassified sign-in authenticated at `authenticated_at` is refused at `now`.
    pub fn unclassified_refused(
        &self,
        authenticated_at: chrono::DateTime<chrono::Utc>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> bool {
        self.declaration_required_from.is_some_and(|from| {
            now >= from
                && now - authenticated_at > chrono::Duration::seconds(self.declaration_window_secs)
        })
    }
}

/// 403 `DEVICE_DECLARATION_REQUIRED`.
pub fn declaration_required() -> ApiError {
    ApiError::domain(
        ErrorCode::DeviceDeclarationRequired,
        "Sign in again and register this device (POST /api/v2/me/devices/current right after signing in)",
    )
}

impl DevicePolicy {
    pub fn from_lookup(get: &impl Fn(&str) -> Option<String>) -> Self {
        let num = |k: &str, d: i64, lo: i64, hi: i64| {
            get(k)
                .and_then(|v| v.trim().parse::<i64>().ok())
                .unwrap_or(d)
                .clamp(lo, hi)
        };
        let flag = |k: &str| {
            !matches!(
                get(k)
                    .unwrap_or_default()
                    .trim()
                    .to_ascii_lowercase()
                    .as_str(),
                "false" | "0" | "off" | "no"
            )
        };
        Self {
            max_web_devices: num("ACCOUNT_MAX_WEB_DEVICES", DEFAULT_MAX_WEB_DEVICES, 1, 20),
            link_ttl_secs: num("ACCOUNT_DEVICE_LINK_TTL_SECS", 300, 60, 900),
            proof_window_secs: num("ACCOUNT_DEVICE_PROOF_WINDOW_SECS", 300, 30, 900),
            email_step_up: flag("ACCOUNT_DEVICE_EMAIL_STEP_UP"),
            e2ee_requires_trusted_device: flag("ACCOUNT_E2EE_REQUIRES_TRUSTED_DEVICE"),
            declaration_required_from: get("ACCOUNT_DEVICE_DECLARATION_REQUIRED_FROM")
                .filter(|v| !v.trim().is_empty())
                .and_then(|v| match chrono::DateTime::parse_from_rfc3339(v.trim()) {
                    Ok(t) => Some(t.with_timezone(&chrono::Utc)),
                    Err(_) => {
                        tracing::error!(
                            "ACCOUNT_DEVICE_DECLARATION_REQUIRED_FROM is not RFC 3339; device declaration is not enforced"
                        );
                        None
                    }
                }),
            declaration_window_secs: 600,
        }
    }
}

impl Default for DevicePolicy {
    fn default() -> Self {
        Self::from_lookup(&|_| None)
    }
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// Proof of possession of a device key: an Ed25519 signature (base64url, 64 bytes) over
/// `cfmeet-account-device-proof:v1\n{purpose}\n{keyThumbprint}\n{context}\n{issuedAt}\n{nonce}`
/// where `keyThumbprint` is the lowercase hex SHA-256 of the 32-byte public key and the
/// context is defined per operation (see each endpoint). `issuedAt` is Unix seconds
/// (accepted within the policy window); `nonce` is 16-64 random bytes, base64url, never
/// reused.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeviceProof {
    pub issued_at: i64,
    pub nonce: String,
    pub signature: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProofPurpose {
    Declare,
    Refresh,
    LinkRequest,
    LinkApprove,
    LinkComplete,
    Revoke,
}

impl ProofPurpose {
    pub fn as_str(self) -> &'static str {
        match self {
            ProofPurpose::Declare => "DECLARE",
            ProofPurpose::Refresh => "REFRESH",
            ProofPurpose::LinkRequest => "LINK_REQUEST",
            ProofPurpose::LinkApprove => "LINK_APPROVE",
            ProofPurpose::LinkComplete => "LINK_COMPLETE",
            ProofPurpose::Revoke => "REVOKE",
        }
    }
}

/// `POST /api/v2/me/devices/current` body. Proof purpose `DECLARE`, context = this
/// sign-in's `sessionId` (from `GET /api/v2/me/sessions`, `current: true`).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeclareDeviceRequest {
    /// Random id generated once per installation / browser profile and kept with the key
    /// (16-128 characters of `[A-Za-z0-9_-]`). Never derived from hardware identifiers.
    pub installation_id: String,
    /// `WEB` or `MOBILE`.
    pub device_class: String,
    /// `WEB` (with `WEB`), `IOS` or `ANDROID` (with `MOBILE`).
    pub platform: String,
    /// Optional user-visible name (1-80 characters, no control characters).
    pub label: Option<String>,
    /// Ed25519 public key, base64url (32 bytes), generated on the device; the private key
    /// never leaves it (non-extractable Web Crypto key / platform keystore).
    pub public_key: String,
    pub proof: DeviceProof,
}

/// One logical device as shown to its owner.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeviceView {
    pub device_id: Uuid,
    /// `WEB` or `MOBILE`.
    pub device_class: String,
    /// `WEB`, `IOS` or `ANDROID`.
    pub platform: String,
    pub label: Option<String>,
    /// `ACTIVE`, `REPLACEMENT_PENDING` or `REVOKED`.
    pub state: String,
    /// `TRUSTED` or `UNTRUSTED`.
    pub trust: String,
    /// `BOOTSTRAP`, `DEVICE_APPROVAL` or `EMAIL_STEP_UP` (trusted devices only).
    pub trusted_via: Option<String>,
    /// True for the account's ACTIVE mobile.
    pub primary: bool,
    /// Lowercase hex SHA-256 of the device public key (compare across devices).
    pub key_thumbprint: String,
    /// Open sign-ins of this device (normally 1).
    pub open_sessions: i64,
    /// True when the request was made from this device.
    pub current: bool,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub trusted_at: Option<DateTime<Utc>>,
    pub promoted_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoke_reason: Option<String>,
    pub replaced_by_device_id: Option<Uuid>,
}

/// The account's device policy and state.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DevicePolicyView {
    /// `WEB_ONLY`, `MOBILE_PRIMARY` or `DEVICE_REPLACEMENT_PENDING` (a new mobile waits).
    pub account_state: String,
    /// Always 1: at most one primary mobile.
    pub max_primary_mobiles: i64,
    pub max_web_devices: i64,
    pub active_web_devices: i64,
    pub primary_mobile_device_id: Option<Uuid>,
    pub pending_mobile_device_ids: Vec<Uuid>,
    /// Ways a new device can be approved: `DEVICE_APPROVAL` (when a trusted device exists)
    /// and `EMAIL_STEP_UP` (when enabled).
    pub linking_methods: Vec<String>,
    pub trusted_devices: i64,
    /// Personal E2EE device enrollment needs a trusted device.
    pub e2ee_requires_trusted_device: bool,
    /// The current sign-in: its device (absent while unclassified).
    pub current_device_id: Option<Uuid>,
    /// `UNCLASSIFIED` (sign-in has declared no device), `TRUSTED`, `UNTRUSTED` or
    /// `REPLACEMENT_PENDING`.
    pub current_device_status: String,
    pub proof_window_secs: i64,
    pub link_ttl_secs: i64,
}

/// A linking challenge created by a new device.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeviceLinkView {
    pub link_id: Uuid,
    /// The requesting (new) device.
    pub device_id: Uuid,
    pub device_class: String,
    pub platform: String,
    pub label: Option<String>,
    /// `PENDING`, `APPROVED`, `COMPLETED`, `CANCELLED`, `EXPIRED` or `REJECTED`.
    pub status: String,
    /// `DEVICE_APPROVAL` or `EMAIL_STEP_UP` once approved.
    pub method: Option<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    /// Shown ONLY to the requesting device (and in its QR payload); a trusted device must
    /// type or scan it to approve. Never listed to other devices.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comparison_code: Option<String>,
    /// `cfmeet-device-link:v1:{linkId}:{comparisonCode}` for an optional QR code (requesting
    /// device only). A copy of it is useless without a trusted device's key and session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qr_payload: Option<String>,
}

/// Result of a completed link or declaration.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeviceOutcome {
    pub device: DeviceView,
    /// The primary mobile this completion replaced (revoked), if any.
    pub replaced_device_id: Option<Uuid>,
    /// Sign-ins ended by this operation (replaced mobile, superseded sign-ins).
    pub ended_sessions: i64,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn err(code: ErrorCode, msg: &str) -> ApiError {
    ApiError::domain(code, msg)
}

fn validation(msg: &str) -> ApiError {
    err(ErrorCode::ValidationError, msg)
}

fn b64(s: &str) -> Option<Vec<u8>> {
    let t = s.trim().trim_end_matches('=');
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(t)
        .ok()
}

pub fn thumbprint(public_key: &[u8]) -> String {
    hex::encode(Sha256::digest(public_key))
}

pub fn installation_digest(user_id: Uuid, installation_id: &str) -> String {
    let mut h = Sha256::new();
    h.update(INSTALLATION_DOMAIN.as_bytes());
    h.update(b"|");
    h.update(user_id.as_bytes());
    h.update(b"|");
    h.update(installation_id.as_bytes());
    hex::encode(h.finalize())
}

/// The exact statement a device signs.
pub fn proof_statement(
    purpose: ProofPurpose,
    key_thumbprint: &str,
    context: &str,
    issued_at: i64,
    nonce: &str,
) -> String {
    format!(
        "{PROOF_DOMAIN}\n{}\n{key_thumbprint}\n{context}\n{issued_at}\n{nonce}",
        purpose.as_str()
    )
}

fn proof_invalid() -> ApiError {
    err(
        ErrorCode::DeviceProofInvalid,
        "The device proof is invalid, expired or was already used",
    )
}

fn parse_public_key(encoded: &str) -> ApiResult<(Vec<u8>, VerifyingKey)> {
    let bytes = b64(encoded).ok_or_else(|| validation("publicKey must be base64url"))?;
    let arr: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| validation("publicKey must be a 32-byte Ed25519 key"))?;
    let key = VerifyingKey::from_bytes(&arr)
        .map_err(|_| validation("publicKey is not a valid Ed25519 key"))?;
    if key.is_weak() {
        return Err(validation("publicKey is a weak Ed25519 key"));
    }
    Ok((bytes, key))
}

/// Verify a proof of possession and consume its nonce (single use, replay refused).
pub async fn verify_proof<C: ConnectionTrait>(
    db: &C,
    policy: &DevicePolicy,
    public_key: &[u8],
    purpose: ProofPurpose,
    context: &str,
    proof: &DeviceProof,
) -> ApiResult<()> {
    let now = Utc::now().timestamp();
    if (now - proof.issued_at).abs() > policy.proof_window_secs {
        return Err(proof_invalid());
    }
    let nonce = b64(&proof.nonce).ok_or_else(proof_invalid)?;
    if !(16..=64).contains(&nonce.len()) {
        return Err(proof_invalid());
    }
    let sig_bytes = b64(&proof.signature).ok_or_else(proof_invalid)?;
    let sig_arr: [u8; 64] = sig_bytes
        .as_slice()
        .try_into()
        .map_err(|_| proof_invalid())?;
    let arr: [u8; 32] = public_key.try_into().map_err(|_| proof_invalid())?;
    let key = VerifyingKey::from_bytes(&arr).map_err(|_| proof_invalid())?;
    let tp = thumbprint(public_key);
    let statement = proof_statement(purpose, &tp, context, proof.issued_at, proof.nonce.trim());
    key.verify_strict(statement.as_bytes(), &Signature::from_bytes(&sig_arr))
        .map_err(|_| proof_invalid())?;
    // Single use: the digest binds key and nonce; kept for longer than the window.
    let digest = hex::encode(Sha256::digest(format!("{tp}|{}", hex::encode(&nonce))));
    db.execute(Statement::from_string(
        DbBackend::Postgres,
        "DELETE FROM account_device_proof_nonces WHERE ctid IN \
         (SELECT ctid FROM account_device_proof_nonces WHERE expires_at < now() LIMIT 200)",
    ))
    .await?;
    let inserted = db
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO account_device_proof_nonces (digest, expires_at) \
             VALUES ($1, now() + make_interval(secs => $2)) ON CONFLICT DO NOTHING",
            [
                digest.into(),
                ((policy.proof_window_secs * 2 + 60) as f64).into(),
            ],
        ))
        .await?;
    if inserted.rows_affected() != 1 {
        return Err(proof_invalid());
    }
    Ok(())
}

async fn lock_account(txn: &DatabaseTransaction, user_id: Uuid) -> ApiResult<()> {
    txn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT pg_advisory_xact_lock(hashtextextended('cfmeet-account-devices:' || $1::text, 0))",
        [user_id.into()],
    ))
    .await?;
    Ok(())
}

#[derive(Debug, Clone)]
struct DeviceRow {
    id: Uuid,
    device_class: String,
    platform: String,
    label: Option<String>,
    public_key: Vec<u8>,
    key_thumbprint: String,
    trust: String,
    trusted_via: Option<String>,
    state: String,
    created_at: DateTime<Utc>,
    last_seen_at: DateTime<Utc>,
    trusted_at: Option<DateTime<Utc>>,
    promoted_at: Option<DateTime<Utc>>,
    revoked_at: Option<DateTime<Utc>>,
    revoke_reason: Option<String>,
    replaced_by_device_id: Option<Uuid>,
    open_sessions: i64,
}

const DEVICE_COLUMNS: &str = "d.id, d.user_id, d.device_class, d.platform, d.label, d.public_key, \
     d.key_thumbprint, d.trust, d.trusted_via, d.state, d.created_at, d.last_seen_at, \
     d.trusted_at, d.promoted_at, d.revoked_at, d.revoke_reason, d.replaced_by_device_id, \
     (SELECT count(*) FROM account_sessions s WHERE s.device_id = d.id \
        AND s.revoked_at IS NULL AND s.absolute_expires_at > now()) AS open_sessions";

fn device_row(r: &sea_orm::QueryResult) -> Result<DeviceRow, sea_orm::DbErr> {
    Ok(DeviceRow {
        id: r.try_get("", "id")?,
        device_class: r.try_get("", "device_class")?,
        platform: r.try_get("", "platform")?,
        label: r.try_get("", "label")?,
        public_key: r.try_get("", "public_key")?,
        key_thumbprint: r.try_get("", "key_thumbprint")?,
        trust: r.try_get("", "trust")?,
        trusted_via: r.try_get("", "trusted_via")?,
        state: r.try_get("", "state")?,
        created_at: r.try_get("", "created_at")?,
        last_seen_at: r.try_get("", "last_seen_at")?,
        trusted_at: r.try_get("", "trusted_at")?,
        promoted_at: r.try_get("", "promoted_at")?,
        revoked_at: r.try_get("", "revoked_at")?,
        revoke_reason: r.try_get("", "revoke_reason")?,
        replaced_by_device_id: r.try_get("", "replaced_by_device_id")?,
        open_sessions: r.try_get("", "open_sessions")?,
    })
}

impl DeviceRow {
    fn view(&self, current: Option<Uuid>) -> DeviceView {
        DeviceView {
            device_id: self.id,
            device_class: self.device_class.clone(),
            platform: self.platform.clone(),
            label: self.label.clone(),
            state: self.state.clone(),
            trust: self.trust.clone(),
            trusted_via: self.trusted_via.clone(),
            primary: self.device_class == "MOBILE" && self.state == "ACTIVE",
            key_thumbprint: self.key_thumbprint.clone(),
            open_sessions: self.open_sessions,
            current: current == Some(self.id),
            created_at: self.created_at,
            last_seen_at: self.last_seen_at,
            trusted_at: self.trusted_at,
            promoted_at: self.promoted_at,
            revoked_at: self.revoked_at,
            revoke_reason: self.revoke_reason.clone(),
            replaced_by_device_id: self.replaced_by_device_id,
        }
    }
    fn trusted_and_active(&self) -> bool {
        self.trust == "TRUSTED" && self.state == "ACTIVE"
    }
}

async fn find_device<C: ConnectionTrait>(
    db: &C,
    user_id: Uuid,
    device_id: Uuid,
    for_update: bool,
) -> ApiResult<Option<DeviceRow>> {
    let sql = format!(
        "SELECT {DEVICE_COLUMNS} FROM account_devices d WHERE d.id = $1 AND d.user_id = $2{}",
        if for_update { " FOR UPDATE" } else { "" }
    );
    let row = db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            [device_id.into(), user_id.into()],
        ))
        .await?;
    Ok(row.as_ref().map(device_row).transpose()?)
}

/// The device of a sign-in (None: unclassified).
pub async fn session_device_id<C: ConnectionTrait>(
    db: &C,
    session_id: Uuid,
) -> ApiResult<Option<Uuid>> {
    Ok(db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT device_id FROM account_sessions WHERE id = $1",
            [session_id.into()],
        ))
        .await?
        .map(|r| r.try_get::<Option<Uuid>>("", "device_id"))
        .transpose()?
        .flatten())
}

/// Gap audit A5: whether the sign-in `session_id` of `user_id` runs on an ACTIVE, TRUSTED
/// device of the account (a password-only or undeclared sign-in does not).
pub async fn session_on_trusted_device<C: ConnectionTrait>(
    db: &C,
    user_id: Uuid,
    session_id: Option<Uuid>,
) -> ApiResult<bool> {
    let Some(session_id) = session_id else {
        return Ok(false);
    };
    let Some(device) = session_device_id(db, session_id).await? else {
        return Ok(false);
    };
    Ok(find_device(db, user_id, device, false)
        .await?
        .is_some_and(|d| d.trusted_and_active()))
}

/// Gap audit A5: whether the sign-in `session_id` belongs to an ACTIVE, TRUSTED device.
pub async fn session_belongs_to_trusted_device<C: ConnectionTrait>(
    db: &C,
    user_id: Uuid,
    session_id: Uuid,
) -> ApiResult<bool> {
    session_on_trusted_device(db, user_id, Some(session_id)).await
}

/// Gap audit A5: whether the account has at least one ACTIVE, TRUSTED device.
pub async fn has_trusted_device<C: ConnectionTrait>(db: &C, user_id: Uuid) -> ApiResult<bool> {
    Ok(db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT 1 AS x FROM account_devices WHERE user_id = $1 AND trust = 'TRUSTED' AND state = 'ACTIVE' LIMIT 1",
            [user_id.into()],
        ))
        .await?
        .is_some())
}

/// The calling sign-in and its device, for the device endpoints.
#[derive(Debug, Clone)]
pub struct DeviceCaller {
    pub user: users::Model,
    pub session_id: Uuid,
    pub device_id: Option<Uuid>,
    pub recently_authenticated: bool,
}

async fn caller_device(state: &AppState, caller: &DeviceCaller) -> ApiResult<DeviceRow> {
    let id = caller.device_id.ok_or_else(|| {
        err(
            ErrorCode::DeviceNotFound,
            "This sign-in has not declared a device (POST /api/v2/me/devices/current)",
        )
    })?;
    let d = find_device(&state.db, caller.user.id, id, false)
        .await?
        .ok_or_else(|| err(ErrorCode::DeviceNotFound, "Device not found"))?;
    if d.state == "REVOKED" {
        return Err(security_revoked());
    }
    Ok(d)
}

/// Authenticate a bearer for the device endpoints: a registered, active account and an
/// open sign-in. Unlike every other verifier it also accepts the sign-in of a mobile whose
/// primary-mobile replacement is pending, so that mobile can complete or cancel it.
pub async fn resolve_caller(state: &AppState, token: &str) -> ApiResult<DeviceCaller> {
    use sea_orm::EntityTrait;
    let unauthenticated =
        || ApiError::domain(ErrorCode::AuthenticationRequired, "Sign in to continue");
    let claims = state
        .jwt_service
        .verify_token(token)
        .map_err(|_| unauthenticated())?;
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| unauthenticated())?;
    let user: users::Model = users::Entity::find_by_id(user_id)
        .one(&state.db)
        .await?
        .ok_or_else(unauthenticated)?;
    if matches!(
        user.status,
        users::UserStatus::Inactive | users::UserStatus::Suspended
    ) || user.role == users::UserRole::Guest
    {
        return Err(unauthenticated());
    }
    let auth = security::authorize_user_token_allow_pending(
        state,
        &claims,
        user.id,
        crate::services::auth::TokenKind::Access,
    )
    .await?;
    // Devices belong to sign-ins: a pre-session token cannot use them.
    let session_id = auth.session_id.ok_or_else(|| {
        err(
            ErrorCode::DeviceDeclarationRefused,
            "Sign in again to manage devices",
        )
    })?;
    let device_id = session_device_id(&state.db, session_id).await?;
    let recently_authenticated = state
        .auth_policy
        .recently_authenticated(&claims, Utc::now().timestamp());
    Ok(DeviceCaller {
        user,
        session_id,
        device_id,
        recently_authenticated,
    })
}

fn security_revoked() -> ApiError {
    ApiError::Unauthorized("Credentials have been revoked".into())
}

fn require_trusted(d: &DeviceRow) -> ApiResult<()> {
    if !d.trusted_and_active() {
        return Err(err(
            ErrorCode::DeviceNotTrusted,
            "This operation needs a trusted device of this account",
        ));
    }
    Ok(())
}

/// Revoke the open sign-ins of a device; returns their ids for the downstream fan-out.
async fn end_device_sessions(
    txn: &DatabaseTransaction,
    device_id: Uuid,
    reason: &str,
    except: Option<Uuid>,
) -> ApiResult<Vec<Uuid>> {
    let rows = txn
        .query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE account_sessions SET revoked_at = now(), revoke_reason = $2 \
             WHERE device_id = $1 AND revoked_at IS NULL AND ($3::uuid IS NULL OR id <> $3) \
             RETURNING id",
            [device_id.into(), reason.into(), except.into()],
        ))
        .await?;
    rows.iter()
        .map(|r| r.try_get::<Uuid>("", "id").map_err(ApiError::from))
        .collect()
}

/// Revoke the account's open UNCLASSIFIED sign-ins (no declared device). They cannot be
/// attributed to a device, so when a device is replaced or revoked they may belong to it:
/// they end too (the classified sign-ins of every other device are untouched), so a
/// legacy sign-in on a replaced or lost phone never outlives the replacement.
async fn end_unclassified_sessions(
    txn: &DatabaseTransaction,
    user_id: Uuid,
    reason: &str,
    except: Option<Uuid>,
) -> ApiResult<Vec<Uuid>> {
    let rows = txn
        .query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE account_sessions SET revoked_at = now(), revoke_reason = $2 \
             WHERE user_id = $1 AND device_id IS NULL AND revoked_at IS NULL \
               AND ($3::uuid IS NULL OR id <> $3) \
             RETURNING id",
            [user_id.into(), reason.into(), except.into()],
        ))
        .await?;
    rows.iter()
        .map(|r| r.try_get::<Uuid>("", "id").map_err(ApiError::from))
        .collect()
}

/// Everything downstream of one ended sign-in: MANAGED session devices and the Hub
/// devices it linked (Messaging E2EE device incl. MLS pending removal, push, client
/// sessions; Calling endpoint incl. live invitations and signaling) through the durable
/// account outbox, and only this sign-in's meeting media. Never fails the caller: every
/// queue is durable and retried.
pub async fn session_effects(state: &AppState, user_id: Uuid, session_ids: &[Uuid]) {
    for sid in session_ids {
        // `session_ended` also ends this sign-in's meeting media.
        if let Err(e) = crate::services::communications::session_ended(state, user_id, *sid).await {
            tracing::error!(session_id = %sid, "queueing the device fan-out of a revoked sign-in failed: {e}");
        }
    }
}

/// Record that a sign-in opened a meeting media session (called in the join transaction).
pub async fn record_session_media<C: ConnectionTrait>(
    db: &C,
    account_session_id: Uuid,
    participant_session_id: Uuid,
    conference_id: Uuid,
) -> ApiResult<()> {
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO account_session_media (participant_session_id, account_session_id, conference_id) \
         VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
        [
            participant_session_id.into(),
            account_session_id.into(),
            conference_id.into(),
        ],
    ))
    .await?;
    Ok(())
}

/// End the live meeting media sessions opened by one sign-in, and queue their provider
/// removal. The logical participant, its role and its other device sessions are kept.
pub async fn end_session_media(state: &AppState, account_session_id: Uuid) -> ApiResult<u64> {
    use crate::services::meeting_events::{emit, event, OutboxRecord};
    let txn = state.db.begin().await?;
    let rows = txn
        .query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE participant_sessions ps SET state = 'TERMINATED', disconnected_at = now(), \
                    end_reason = 'ACCOUNT_SESSION_ENDED', updated_at = now() \
             FROM account_session_media m \
             WHERE m.account_session_id = $1 AND ps.id = m.participant_session_id \
               AND ps.state IN ('PENDING', 'CONNECTED') \
             RETURNING ps.id, ps.conference_id, ps.conference_participant_id, ps.media_identity",
            [account_session_id.into()],
        ))
        .await?;
    for r in &rows {
        let id: Uuid = r.try_get("", "id")?;
        let conference_id: Uuid = r.try_get("", "conference_id")?;
        let participant_id: Uuid = r.try_get("", "conference_participant_id")?;
        let media_identity: String = r.try_get("", "media_identity")?;
        emit(
            &txn,
            OutboxRecord {
                aggregate_type: "participant_session",
                aggregate_id: id,
                event_type: event::RTC_PARTICIPANT_REMOVE_REQUESTED,
                meeting_space_id: None,
                conference_id: Some(conference_id),
                payload: json!({
                    "conferenceId": conference_id,
                    "participantId": participant_id,
                    "participantSessionId": id,
                    "mediaIdentity": media_identity,
                    "reason": "ACCOUNT_SESSION_ENDED",
                }),
            },
        )
        .await?;
    }
    txn.commit().await?;
    Ok(rows.len() as u64)
}

// ---------------------------------------------------------------------------
// Policy and listing
// ---------------------------------------------------------------------------

pub async fn policy_view(state: &AppState, caller: &DeviceCaller) -> ApiResult<DevicePolicyView> {
    let p = &state.auth_policy.devices;
    let devices = list_rows(state, caller.user.id, false).await?;
    let primary = devices
        .iter()
        .find(|d| d.device_class == "MOBILE" && d.state == "ACTIVE")
        .map(|d| d.id);
    let pending: Vec<Uuid> = devices
        .iter()
        .filter(|d| d.state == "REPLACEMENT_PENDING")
        .map(|d| d.id)
        .collect();
    let trusted = devices.iter().filter(|d| d.trusted_and_active()).count() as i64;
    let web = devices
        .iter()
        .filter(|d| d.device_class == "WEB" && d.state == "ACTIVE" && d.open_sessions > 0)
        .count() as i64;
    let current = caller
        .device_id
        .and_then(|id| devices.iter().find(|d| d.id == id));
    let mut methods = vec![];
    if trusted > 0 {
        methods.push("DEVICE_APPROVAL".to_string());
    }
    if p.email_step_up {
        methods.push("EMAIL_STEP_UP".to_string());
    }
    Ok(DevicePolicyView {
        account_state: if !pending.is_empty() {
            "DEVICE_REPLACEMENT_PENDING"
        } else if primary.is_some() {
            "MOBILE_PRIMARY"
        } else {
            "WEB_ONLY"
        }
        .into(),
        max_primary_mobiles: 1,
        max_web_devices: p.max_web_devices,
        active_web_devices: web,
        primary_mobile_device_id: primary,
        pending_mobile_device_ids: pending,
        linking_methods: methods,
        trusted_devices: trusted,
        e2ee_requires_trusted_device: p.e2ee_requires_trusted_device,
        current_device_id: caller.device_id,
        current_device_status: match current {
            None => "UNCLASSIFIED",
            Some(d) if d.state == "REPLACEMENT_PENDING" => "REPLACEMENT_PENDING",
            Some(d) if d.trust == "TRUSTED" => "TRUSTED",
            Some(_) => "UNTRUSTED",
        }
        .into(),
        proof_window_secs: p.proof_window_secs,
        link_ttl_secs: p.link_ttl_secs,
    })
}

async fn list_rows(
    state: &AppState,
    user_id: Uuid,
    include_revoked: bool,
) -> ApiResult<Vec<DeviceRow>> {
    let rows = state
        .db
        .query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!(
                "SELECT {DEVICE_COLUMNS} FROM account_devices d WHERE d.user_id = $1 \
                 AND ($2 OR d.state <> 'REVOKED') ORDER BY d.created_at DESC LIMIT 100"
            ),
            [user_id.into(), include_revoked.into()],
        ))
        .await?;
    rows.iter()
        .map(device_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(ApiError::from)
}

pub async fn list_devices(
    state: &AppState,
    caller: &DeviceCaller,
    include_revoked: bool,
) -> ApiResult<Vec<DeviceView>> {
    Ok(list_rows(state, caller.user.id, include_revoked)
        .await?
        .iter()
        .map(|d| d.view(caller.device_id))
        .collect())
}

// ---------------------------------------------------------------------------
// Declaration
// ---------------------------------------------------------------------------

fn validate_declaration(req: &DeclareDeviceRequest) -> ApiResult<()> {
    let id = req.installation_id.trim();
    if !(16..=128).contains(&id.len())
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(validation(
            "installationId must be 16-128 characters of [A-Za-z0-9_-]",
        ));
    }
    match (req.device_class.as_str(), req.platform.as_str()) {
        ("WEB", "WEB") | ("MOBILE", "IOS") | ("MOBILE", "ANDROID") => {}
        _ => {
            return Err(validation(
                "deviceClass/platform must be WEB/WEB, MOBILE/IOS or MOBILE/ANDROID",
            ))
        }
    }
    if let Some(l) = &req.label {
        let n = l.chars().count();
        if !(1..=80).contains(&n) || l.chars().any(char::is_control) {
            return Err(validation(
                "label must be 1-80 characters without control characters",
            ));
        }
    }
    Ok(())
}

/// Bind the calling sign-in to a device (new or the same installation signing in again).
pub async fn declare(
    state: &AppState,
    caller: &DeviceCaller,
    req: DeclareDeviceRequest,
) -> ApiResult<DeviceOutcome> {
    validate_declaration(&req)?;
    let policy = &state.auth_policy.devices;
    let (public_key, _) = parse_public_key(&req.public_key)?;
    let tp = thumbprint(&public_key);
    let user = &caller.user;
    if let Some(existing) = caller.device_id {
        // Idempotent for the same key; a sign-in never changes device.
        let d = find_device(&state.db, user.id, existing, false)
            .await?
            .ok_or_else(|| err(ErrorCode::DeviceNotFound, "Device not found"))?;
        if d.key_thumbprint == tp && d.state != "REVOKED" {
            return Ok(DeviceOutcome {
                device: d.view(Some(d.id)),
                replaced_device_id: None,
                ended_sessions: 0,
            });
        }
        return Err(err(
            ErrorCode::DeviceDeclarationRefused,
            "This sign-in already belongs to another device",
        ));
    }
    if !caller.recently_authenticated {
        return Err(err(
            ErrorCode::DeviceDeclarationRefused,
            "Declare the device right after signing in on it (sign in again)",
        ));
    }
    verify_proof(
        &state.db,
        policy,
        &public_key,
        ProofPurpose::Declare,
        &caller.session_id.to_string(),
        &req.proof,
    )
    .await?;
    let digest = installation_digest(user.id, req.installation_id.trim());

    let txn = state.db.begin().await?;
    lock_account(&txn, user.id).await?;
    let same_installation = txn
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!(
                "SELECT {DEVICE_COLUMNS} FROM account_devices d \
                 WHERE d.user_id = $1 AND d.installation_digest = $2 AND d.state <> 'REVOKED' FOR UPDATE"
            ),
            [user.id.into(), digest.clone().into()],
        ))
        .await?
        .as_ref()
        .map(device_row)
        .transpose()?;
    let mut superseded = vec![];
    let mut bootstrapped = false;
    let device_id = match same_installation {
        Some(d) => {
            // The same installation signing in again: rotate, never a new device.
            if d.key_thumbprint != tp {
                return Err(err(
                    ErrorCode::DeviceKeyInUse,
                    "This installation is bound to another key; revoke it from a trusted device first",
                ));
            }
            superseded =
                end_device_sessions(&txn, d.id, "SUPERSEDED", Some(caller.session_id)).await?;
            txn.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE account_devices SET last_seen_at = now(), updated_at = now(), \
                        label = COALESCE($2, label) WHERE id = $1",
                [d.id.into(), req.label.clone().into()],
            ))
            .await?;
            d.id
        }
        None => {
            let key_taken = txn
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT 1 AS x FROM account_devices WHERE key_thumbprint = $1 AND state <> 'REVOKED'",
                    [tp.clone().into()],
                ))
                .await?
                .is_some();
            if key_taken {
                return Err(err(
                    ErrorCode::DeviceKeyInUse,
                    "This device key is already bound to another device",
                ));
            }
            let ever_trusted = txn
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT 1 AS x FROM account_devices WHERE user_id = $1 AND trust = 'TRUSTED' LIMIT 1",
                    [user.id.into()],
                ))
                .await?
                .is_some();
            let bootstrap = !ever_trusted;
            let mobile = req.device_class == "MOBILE";
            if !mobile {
                let web: i64 = txn
                    .query_one(Statement::from_sql_and_values(
                        DbBackend::Postgres,
                        "SELECT count(*) AS n FROM account_devices d WHERE d.user_id = $1 \
                           AND d.device_class = 'WEB' AND d.state = 'ACTIVE' AND EXISTS ( \
                             SELECT 1 FROM account_sessions s WHERE s.device_id = d.id \
                               AND s.revoked_at IS NULL AND s.absolute_expires_at > now())",
                        [user.id.into()],
                    ))
                    .await?
                    .map(|r| r.try_get::<i64>("", "n"))
                    .transpose()?
                    .unwrap_or(0);
                if web >= policy.max_web_devices {
                    security::record_event(
                        &txn,
                        SecurityEvent::new("DEVICE_DECLARED", Outcome::Denied)
                            .by_user(user.id)
                            .session(caller.session_id)
                            .detail(json!({"reason": "WEB_DEVICE_LIMIT_REACHED", "limit": policy.max_web_devices})),
                    )
                    .await?;
                    txn.commit().await?;
                    return Err(err(
                        ErrorCode::WebDeviceLimitReached,
                        &format!(
                            "This account already has {} active web devices; revoke one from a trusted device first",
                            policy.max_web_devices
                        ),
                    ));
                }
            }
            let (trust, state_value) = match (mobile, bootstrap) {
                (_, true) => ("TRUSTED", "ACTIVE"),
                (false, false) => ("UNTRUSTED", "ACTIVE"),
                (true, false) => ("UNTRUSTED", "REPLACEMENT_PENDING"),
            };
            bootstrapped = trust == "TRUSTED";
            let id = Uuid::new_v4();
            txn.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO account_devices (id, user_id, installation_digest, device_class, platform, \
                    label, public_key, key_thumbprint, trust, trusted_via, trusted_at, state, promoted_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, \
                    CASE WHEN $9 = 'TRUSTED' THEN 'BOOTSTRAP' END, \
                    CASE WHEN $9 = 'TRUSTED' THEN now() END, $10, \
                    CASE WHEN $4 = 'MOBILE' AND $10 = 'ACTIVE' THEN now() END)",
                [
                    id.into(),
                    user.id.into(),
                    digest.into(),
                    req.device_class.clone().into(),
                    req.platform.clone().into(),
                    req.label.clone().into(),
                    public_key.clone().into(),
                    tp.clone().into(),
                    trust.into(),
                    state_value.into(),
                ],
            ))
            .await?;
            security::record_event(
                &txn,
                SecurityEvent::new("DEVICE_DECLARED", Outcome::Success)
                    .by_user(user.id)
                    .session(caller.session_id)
                    .detail(json!({
                        "deviceId": id, "deviceClass": req.device_class, "platform": req.platform,
                        "trust": trust, "state": state_value, "bootstrap": bootstrap,
                    })),
            )
            .await?;
            if mobile && state_value == "ACTIVE" {
                security::record_event(
                    &txn,
                    SecurityEvent::new("PRIMARY_MOBILE_ASSIGNED", Outcome::Success)
                        .by_user(user.id)
                        .session(caller.session_id)
                        .detail(json!({"deviceId": id, "via": "BOOTSTRAP"})),
                )
                .await?;
            }
            id
        }
    };
    let bound = txn
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE account_sessions SET device_id = $2 \
             WHERE id = $1 AND user_id = $3 AND device_id IS NULL AND revoked_at IS NULL",
            [caller.session_id.into(), device_id.into(), user.id.into()],
        ))
        .await?;
    if bound.rows_affected() != 1 {
        return Err(err(
            ErrorCode::DeviceDeclarationRefused,
            "This sign-in is no longer eligible to declare a device",
        ));
    }
    txn.commit().await?;
    session_effects(state, user.id, &superseded).await;
    if bootstrapped {
        // Gap audit A6: the first trusted device of the account is announced to the owner.
        crate::services::account_security::notify_security(
            state,
            user.id,
            crate::services::email::SecurityNotice::FirstDeviceTrusted,
            &device_id.to_string(),
        )
        .await;
    }
    let d = find_device(&state.db, user.id, device_id, false)
        .await?
        .ok_or_else(|| ApiError::InternalError("declared device vanished".into()))?;
    Ok(DeviceOutcome {
        device: d.view(Some(d.id)),
        replaced_device_id: None,
        ended_sessions: superseded.len() as i64,
    })
}

// ---------------------------------------------------------------------------
// Linking
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProofOnlyRequest {
    pub proof: DeviceProof,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApproveLinkRequest {
    /// The 6-digit code shown on the new device (typed or scanned from its QR code).
    pub comparison_code: String,
    /// Purpose `LINK_APPROVE`, signed by the APPROVING device; context =
    /// `{linkId}|{requesting device keyThumbprint}` (shown by `GET /api/v2/me/device-links`).
    pub proof: DeviceProof,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompleteLinkRequest {
    /// Purpose `LINK_COMPLETE`, signed by the NEW device; context = `{linkId}`.
    pub proof: DeviceProof,
    /// Required only for the email step-up (6 digits from the email).
    pub email_code: Option<String>,
}

/// A pending link as listed to a trusted device (no comparison code).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PendingLinkView {
    #[serde(flatten)]
    pub link: DeviceLinkView,
    /// Thumbprint of the requesting device's key (part of the approval proof context).
    pub key_thumbprint: String,
}

fn six_digits() -> String {
    format!("{:06}", rand::thread_rng().gen_range(0..1_000_000u32))
}

fn code_digest(link_id: Uuid, code: &str) -> String {
    hex::encode(Sha256::digest(format!(
        "cfmeet-device-link-email:v1|{link_id}|{code}"
    )))
}

fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

#[derive(Debug, Clone)]
struct LinkRow {
    id: Uuid,
    device_id: Uuid,
    comparison_code: String,
    status: String,
    method: Option<String>,
    email_code_digest: Option<String>,
    email_code_sent_at: Option<DateTime<Utc>>,
    attempts: i32,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    expired: bool,
}

async fn find_link(txn: &DatabaseTransaction, user_id: Uuid, link_id: Uuid) -> ApiResult<LinkRow> {
    let r = txn
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT id, device_id, comparison_code, status, method, email_code_digest, \
                    email_code_sent_at, email_attempts, created_at, expires_at, \
                    expires_at <= now() AS expired \
             FROM account_device_links WHERE id = $1 AND user_id = $2 FOR UPDATE",
            [link_id.into(), user_id.into()],
        ))
        .await?
        .ok_or_else(link_invalid)?;
    Ok(LinkRow {
        id: r.try_get("", "id")?,
        device_id: r.try_get("", "device_id")?,
        comparison_code: r.try_get("", "comparison_code")?,
        status: r.try_get("", "status")?,
        method: r.try_get("", "method")?,
        email_code_digest: r.try_get("", "email_code_digest")?,
        email_code_sent_at: r.try_get("", "email_code_sent_at")?,
        attempts: r.try_get("", "email_attempts")?,
        created_at: r.try_get("", "created_at")?,
        expires_at: r.try_get("", "expires_at")?,
        expired: r.try_get("", "expired")?,
    })
}

fn link_invalid() -> ApiError {
    err(
        ErrorCode::DeviceLinkInvalid,
        "The linking request does not exist, has expired or is no longer open",
    )
}

async fn finish_link(txn: &DatabaseTransaction, link_id: Uuid, status: &str) -> ApiResult<()> {
    txn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE account_device_links SET status = $2, finished_at = now() WHERE id = $1",
        [link_id.into(), status.into()],
    ))
    .await?;
    Ok(())
}

fn link_view(l: &LinkRow, d: &DeviceRow, reveal: bool) -> DeviceLinkView {
    DeviceLinkView {
        link_id: l.id,
        device_id: d.id,
        device_class: d.device_class.clone(),
        platform: d.platform.clone(),
        label: d.label.clone(),
        status: if l.expired && matches!(l.status.as_str(), "PENDING" | "APPROVED") {
            "EXPIRED".into()
        } else {
            l.status.clone()
        },
        method: l.method.clone(),
        created_at: l.created_at,
        expires_at: l.expires_at,
        comparison_code: reveal.then(|| l.comparison_code.clone()),
        qr_payload: reveal.then(|| format!("cfmeet-device-link:v1:{}:{}", l.id, l.comparison_code)),
    }
}

/// A new (untrusted or pending) device asks to be linked. Proof `LINK_REQUEST`, context
/// = its `deviceId`.
pub async fn create_link(
    state: &AppState,
    caller: &DeviceCaller,
    req: ProofOnlyRequest,
) -> ApiResult<DeviceLinkView> {
    let policy = &state.auth_policy.devices;
    let d = caller_device(state, caller).await?;
    if d.trusted_and_active() {
        return Err(err(
            ErrorCode::DeviceDeclarationRefused,
            "This device is already trusted",
        ));
    }
    verify_proof(
        &state.db,
        policy,
        &d.public_key,
        ProofPurpose::LinkRequest,
        &d.id.to_string(),
        &req.proof,
    )
    .await?;
    let txn = state.db.begin().await?;
    lock_account(&txn, caller.user.id).await?;
    let recent: i64 = txn
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT count(*) AS n FROM account_device_links \
             WHERE user_id = $1 AND created_at > now() - interval '1 hour'",
            [caller.user.id.into()],
        ))
        .await?
        .map(|r| r.try_get::<i64>("", "n"))
        .transpose()?
        .unwrap_or(0);
    if recent >= MAX_LINKS_PER_HOUR {
        return Err(ApiError::rate_limited(
            "Too many device linking requests; try again later",
            3600,
        ));
    }
    txn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE account_device_links SET status = 'CANCELLED', finished_at = now() \
         WHERE device_id = $1 AND status IN ('PENDING', 'APPROVED')",
        [d.id.into()],
    ))
    .await?;
    let id = Uuid::new_v4();
    let code = six_digits();
    txn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO account_device_links (id, user_id, device_id, comparison_code, status, expires_at) \
         VALUES ($1, $2, $3, $4, 'PENDING', now() + make_interval(secs => $5))",
        [
            id.into(),
            caller.user.id.into(),
            d.id.into(),
            code.into(),
            (policy.link_ttl_secs as f64).into(),
        ],
    ))
    .await?;
    security::record_event(
        &txn,
        SecurityEvent::new("DEVICE_LINK_REQUESTED", Outcome::Success)
            .by_user(caller.user.id)
            .session(caller.session_id)
            .detail(json!({"linkId": id, "deviceId": d.id, "deviceClass": d.device_class})),
    )
    .await?;
    let l = find_link(&txn, caller.user.id, id).await?;
    txn.commit().await?;
    Ok(link_view(&l, &d, true))
}

/// The caller's own view of a link it requested (status polling), with its code.
pub async fn get_link(
    state: &AppState,
    caller: &DeviceCaller,
    link_id: Uuid,
) -> ApiResult<DeviceLinkView> {
    let d = caller_device(state, caller).await?;
    let txn = state.db.begin().await?;
    let l = find_link(&txn, caller.user.id, link_id).await?;
    txn.commit().await?;
    if l.device_id != d.id {
        return Err(link_invalid());
    }
    Ok(link_view(&l, &d, true))
}

/// Open link requests of the account, for a trusted device to approve.
pub async fn pending_links(
    state: &AppState,
    caller: &DeviceCaller,
) -> ApiResult<Vec<PendingLinkView>> {
    let me = caller_device(state, caller).await?;
    require_trusted(&me)?;
    let rows = state
        .db
        .query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT id FROM account_device_links WHERE user_id = $1 AND status = 'PENDING' \
               AND expires_at > now() ORDER BY created_at DESC LIMIT 20",
            [caller.user.id.into()],
        ))
        .await?;
    let txn = state.db.begin().await?;
    let mut out = vec![];
    for r in rows {
        let id: Uuid = r.try_get("", "id")?;
        let l = find_link(&txn, caller.user.id, id).await?;
        if let Some(d) = find_device(&txn, caller.user.id, l.device_id, false).await? {
            out.push(PendingLinkView {
                link: link_view(&l, &d, false),
                key_thumbprint: d.key_thumbprint,
            });
        }
    }
    txn.commit().await?;
    Ok(out)
}

/// A trusted device approves a link: it must present the comparison code shown on the new
/// device and sign with its own device key.
pub async fn approve_link(
    state: &AppState,
    caller: &DeviceCaller,
    link_id: Uuid,
    req: ApproveLinkRequest,
) -> ApiResult<DeviceLinkView> {
    let policy = &state.auth_policy.devices;
    let me = caller_device(state, caller).await?;
    require_trusted(&me)?;
    let txn = state.db.begin().await?;
    lock_account(&txn, caller.user.id).await?;
    let l = find_link(&txn, caller.user.id, link_id).await?;
    if l.status != "PENDING" || l.expired {
        return Err(link_invalid());
    }
    let target = find_device(&txn, caller.user.id, l.device_id, false)
        .await?
        .ok_or_else(link_invalid)?;
    if target.id == me.id || target.state == "REVOKED" {
        return Err(link_invalid());
    }
    verify_proof(
        &txn,
        policy,
        &me.public_key,
        ProofPurpose::LinkApprove,
        &format!("{}|{}", l.id, target.key_thumbprint),
        &req.proof,
    )
    .await?;
    if !ct_eq(req.comparison_code.trim(), &l.comparison_code) {
        let attempts = l.attempts + 1;
        txn.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE account_device_links SET email_attempts = $2, \
                    status = CASE WHEN $2 >= $3 THEN 'REJECTED' ELSE status END, \
                    finished_at = CASE WHEN $2 >= $3 THEN now() ELSE finished_at END \
             WHERE id = $1",
            [l.id.into(), attempts.into(), MAX_LINK_ATTEMPTS.into()],
        ))
        .await?;
        security::record_event(
            &txn,
            SecurityEvent::new("DEVICE_LINK_APPROVED", Outcome::Denied)
                .by_user(caller.user.id)
                .session(caller.session_id)
                .detail(json!({"linkId": l.id, "reason": "COMPARISON_CODE_MISMATCH", "attempts": attempts})),
        )
        .await?;
        txn.commit().await?;
        return Err(validation(
            "The comparison code does not match the new device",
        ));
    }
    txn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE account_device_links SET status = 'APPROVED', method = 'DEVICE_APPROVAL', \
                approved_by_device_id = $2, approved_at = now() WHERE id = $1",
        [l.id.into(), me.id.into()],
    ))
    .await?;
    security::record_event(
        &txn,
        SecurityEvent::new("DEVICE_LINK_APPROVED", Outcome::Success)
            .by_user(caller.user.id)
            .session(caller.session_id)
            .detail(json!({"linkId": l.id, "deviceId": target.id, "approverDeviceId": me.id})),
    )
    .await?;
    let l = find_link(&txn, caller.user.id, link_id).await?;
    txn.commit().await?;
    Ok(link_view(&l, &target, false))
}

/// A trusted device refuses a link.
pub async fn reject_link(
    state: &AppState,
    caller: &DeviceCaller,
    link_id: Uuid,
) -> ApiResult<DeviceLinkView> {
    let me = caller_device(state, caller).await?;
    require_trusted(&me)?;
    let txn = state.db.begin().await?;
    let l = find_link(&txn, caller.user.id, link_id).await?;
    if !matches!(l.status.as_str(), "PENDING" | "APPROVED") || l.expired {
        return Err(link_invalid());
    }
    let target = find_device(&txn, caller.user.id, l.device_id, false)
        .await?
        .ok_or_else(link_invalid)?;
    finish_link(&txn, l.id, "REJECTED").await?;
    security::record_event(
        &txn,
        SecurityEvent::new("DEVICE_LINK_REJECTED", Outcome::Success)
            .by_user(caller.user.id)
            .session(caller.session_id)
            .detail(json!({"linkId": l.id, "deviceId": target.id})),
    )
    .await?;
    let l = find_link(&txn, caller.user.id, link_id).await?;
    txn.commit().await?;
    Ok(link_view(&l, &target, false))
}

/// The new device asks for an email step-up code (account mailbox control).
pub async fn send_email_code(
    state: &AppState,
    caller: &DeviceCaller,
    link_id: Uuid,
) -> ApiResult<DeviceLinkView> {
    if !state.auth_policy.devices.email_step_up {
        return Err(err(
            ErrorCode::FeatureDisabled,
            "Email step-up is not enabled in this deployment",
        ));
    }
    let d = caller_device(state, caller).await?;
    let txn = state.db.begin().await?;
    let l = find_link(&txn, caller.user.id, link_id).await?;
    if l.device_id != d.id || l.status != "PENDING" || l.expired {
        return Err(link_invalid());
    }
    if l.email_code_sent_at
        .is_some_and(|t| (Utc::now() - t).num_seconds() < EMAIL_RESEND_SECS)
    {
        return Err(ApiError::rate_limited(
            "A code was sent recently; check your email",
            EMAIL_RESEND_SECS as u64,
        ));
    }
    let code = six_digits();
    txn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE account_device_links SET email_code_digest = $2, email_code_sent_at = now() WHERE id = $1",
        [l.id.into(), code_digest(l.id, &code).into()],
    ))
    .await?;
    security::record_event(
        &txn,
        SecurityEvent::new("DEVICE_STEP_UP_CODE_SENT", Outcome::Success)
            .by_user(caller.user.id)
            .session(caller.session_id)
            .detail(json!({"linkId": l.id, "deviceId": d.id})),
    )
    .await?;
    let what = if d.device_class == "MOBILE" {
        "make a new phone your primary CF Meet mobile (your previous phone will be signed out)"
    } else {
        "trust a new browser for your CF Meet account"
    };
    let template = crate::templates::email::notification_email(
        &caller.user.username,
        "Your CF Meet device verification code",
        &format!(
            "Your code is {code}. It confirms a request to {what}. It expires in {} minutes. \
             If you did not request this, do not share the code and sign out all devices under Security & devices.",
            (l.expires_at - Utc::now()).num_minutes().max(1)
        ),
    );
    // CRITICAL lane (N12): LEGACY_ONLY records exactly this ledger row; otherwise the
    // SECURITY_DEVICE_STEP_UP fence decides old ledger or Notification Service.
    crate::services::email::device_step_up_code_on(
        &txn,
        &caller.user.email,
        &caller.user.username,
        caller.user.id,
        d.id,
        format!(
            "device-step-up:{}:{}",
            l.id,
            &code_digest(l.id, &code)[..16]
        ),
        &code,
        template,
        l.expires_at,
    )
    .await?;
    let l = find_link(&txn, caller.user.id, link_id).await?;
    txn.commit().await?;
    Ok(link_view(&l, &d, true))
}

/// The new device completes its link (after approval, or with the email code). A web
/// device becomes trusted; a pending mobile becomes the primary mobile and the previous
/// primary mobile is revoked atomically (its sign-ins end; web devices are untouched).
pub async fn complete_link(
    state: &AppState,
    caller: &DeviceCaller,
    link_id: Uuid,
    req: CompleteLinkRequest,
) -> ApiResult<DeviceOutcome> {
    let policy = &state.auth_policy.devices;
    let d = caller_device(state, caller).await?;
    let txn = state.db.begin().await?;
    lock_account(&txn, caller.user.id).await?;
    let l = find_link(&txn, caller.user.id, link_id).await?;
    if l.device_id != d.id || l.expired || !matches!(l.status.as_str(), "PENDING" | "APPROVED") {
        return Err(link_invalid());
    }
    verify_proof(
        &txn,
        policy,
        &d.public_key,
        ProofPurpose::LinkComplete,
        &l.id.to_string(),
        &req.proof,
    )
    .await?;
    let method = if l.status == "APPROVED" {
        "DEVICE_APPROVAL"
    } else {
        let code = req.email_code.as_deref().map(str::trim).unwrap_or("");
        let ok = policy.email_step_up
            && l.email_code_digest
                .as_deref()
                .is_some_and(|dg| !code.is_empty() && ct_eq(dg, &code_digest(l.id, code)));
        if !ok {
            let attempts = l.attempts + 1;
            txn.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE account_device_links SET email_attempts = $2, \
                        status = CASE WHEN $2 >= $3 THEN 'REJECTED' ELSE status END, \
                        finished_at = CASE WHEN $2 >= $3 THEN now() ELSE finished_at END \
                 WHERE id = $1",
                [l.id.into(), attempts.into(), MAX_LINK_ATTEMPTS.into()],
            ))
            .await?;
            security::record_event(
                &txn,
                SecurityEvent::new("DEVICE_LINK_COMPLETED", Outcome::Denied)
                    .by_user(caller.user.id)
                    .session(caller.session_id)
                    .detail(
                        json!({"linkId": l.id, "reason": "NOT_APPROVED", "attempts": attempts}),
                    ),
            )
            .await?;
            txn.commit().await?;
            return Err(err(
                ErrorCode::DeviceNotTrusted,
                "The link is not approved yet (approve it from a trusted device or enter the emailed code)",
            ));
        }
        "EMAIL_STEP_UP"
    };
    let d = find_device(&txn, caller.user.id, d.id, true)
        .await?
        .ok_or_else(link_invalid)?;
    if d.state == "REVOKED" {
        return Err(security_revoked());
    }
    let mut replaced = None;
    let mut ended = vec![];
    if d.state == "REPLACEMENT_PENDING" {
        // Atomic primary-mobile replacement under the account lock: the previous primary
        // is revoked before this one becomes ACTIVE (unique index: one primary).
        let old = txn
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT id FROM account_devices WHERE user_id = $1 AND device_class = 'MOBILE' \
                   AND state = 'ACTIVE' FOR UPDATE",
                [caller.user.id.into()],
            ))
            .await?
            .map(|r| r.try_get::<Uuid>("", "id"))
            .transpose()?;
        if let Some(old) = old {
            ended.extend(end_device_sessions(&txn, old, "DEVICE_REPLACED", None).await?);
            ended.extend(
                end_unclassified_sessions(
                    &txn,
                    caller.user.id,
                    "DEVICE_REPLACED",
                    Some(caller.session_id),
                )
                .await?,
            );
            txn.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE account_devices SET state = 'REVOKED', revoked_at = now(), \
                        revoke_reason = 'DEVICE_REPLACED', replaced_by_device_id = $2, updated_at = now() \
                 WHERE id = $1",
                [old.into(), d.id.into()],
            ))
            .await?;
            txn.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE account_device_links SET status = 'CANCELLED', finished_at = now() \
                 WHERE device_id = $1 AND status IN ('PENDING', 'APPROVED')",
                [old.into()],
            ))
            .await?;
            replaced = Some(old);
        }
        txn.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE account_devices SET trust = 'TRUSTED', trusted_via = $2, trusted_at = now(), \
                    state = 'ACTIVE', promoted_at = now(), updated_at = now(), last_seen_at = now() \
             WHERE id = $1",
            [d.id.into(), method.into()],
        ))
        .await?;
        security::record_event(
            &txn,
            SecurityEvent::new("PRIMARY_MOBILE_REPLACED", Outcome::Success)
                .by_user(caller.user.id)
                .session(caller.session_id)
                .detail(json!({"deviceId": d.id, "replacedDeviceId": replaced, "via": method, "sessionsEnded": ended.len()})),
        )
        .await?;
    } else {
        txn.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE account_devices SET trust = 'TRUSTED', trusted_via = $2, trusted_at = now(), \
                    updated_at = now(), last_seen_at = now() WHERE id = $1",
            [d.id.into(), method.into()],
        ))
        .await?;
    }
    txn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE account_device_links SET status = 'COMPLETED', method = $2, finished_at = now() WHERE id = $1",
        [l.id.into(), method.into()],
    ))
    .await?;
    security::record_event(
        &txn,
        SecurityEvent::new("DEVICE_LINK_COMPLETED", Outcome::Success)
            .by_user(caller.user.id)
            .session(caller.session_id)
            .detail(json!({"linkId": l.id, "deviceId": d.id, "method": method})),
    )
    .await?;
    // N0-10 (S-9): the owner's security notice is recorded in the same transaction as the
    // link (deterministic key device-linked:<link id>), so it can never be lost.
    let (subject, message) = if replaced.is_some() || d.device_class == "MOBILE" {
        (
            "A new phone is now your primary CF Meet mobile",
            "A new phone was confirmed as the primary mobile of your CF Meet account. Any previous \
             phone was signed out; your trusted browsers stay signed in. If this was not you, \
             revoke the phone and sign out all devices under Security & devices.",
        )
    } else {
        (
            "A new browser was linked to your CF Meet account",
            "A new browser was confirmed as a trusted device of your CF Meet account. If this was \
             not you, revoke it and sign out all devices under Security & devices.",
        )
    };
    crate::services::email::tolerate_unsendable(
        crate::services::email::device_notice_on(
            &txn,
            &caller.user.email,
            &caller.user.username,
            caller.user.id,
            d.id,
            l.id,
            subject,
            message,
        )
        .await,
        caller.user.id,
        "device-linked",
    )?;
    txn.commit().await?;
    session_effects(state, caller.user.id, &ended).await;
    let d = find_device(&state.db, caller.user.id, d.id, false)
        .await?
        .ok_or_else(link_invalid)?;
    Ok(DeviceOutcome {
        device: d.view(Some(d.id)),
        replaced_device_id: replaced,
        ended_sessions: ended.len() as i64,
    })
}

/// The new device withdraws its own open link.
pub async fn cancel_link(
    state: &AppState,
    caller: &DeviceCaller,
    link_id: Uuid,
) -> ApiResult<DeviceLinkView> {
    let d = caller_device(state, caller).await?;
    let txn = state.db.begin().await?;
    let l = find_link(&txn, caller.user.id, link_id).await?;
    if l.device_id != d.id || !matches!(l.status.as_str(), "PENDING" | "APPROVED") {
        return Err(link_invalid());
    }
    finish_link(&txn, l.id, "CANCELLED").await?;
    let l = find_link(&txn, caller.user.id, link_id).await?;
    txn.commit().await?;
    Ok(link_view(&l, &d, true))
}

// ---------------------------------------------------------------------------
// Revocation
// ---------------------------------------------------------------------------

/// Revoke one device. The device itself may always revoke itself (sign out and forget);
/// another device of the account needs a trusted device (proof `REVOKE`, context =
/// `{target deviceId}`, signed by the CALLING device). Every sign-in of the device ends
/// and every downstream credential it holds is revoked; other devices are untouched.
pub async fn revoke_device(
    state: &AppState,
    caller: &DeviceCaller,
    device_id: Uuid,
    proof: Option<DeviceProof>,
) -> ApiResult<DeviceOutcome> {
    let policy = &state.auth_policy.devices;
    let target = find_device(&state.db, caller.user.id, device_id, false)
        .await?
        .ok_or_else(|| err(ErrorCode::DeviceNotFound, "Device not found"))?;
    let own = caller.device_id == Some(device_id);
    if !own {
        let me = caller_device(state, caller).await?;
        require_trusted(&me)?;
        let proof = proof.ok_or(proof_invalid())?;
        verify_proof(
            &state.db,
            policy,
            &me.public_key,
            ProofPurpose::Revoke,
            &device_id.to_string(),
            &proof,
        )
        .await?;
    }
    let txn = state.db.begin().await?;
    lock_account(&txn, caller.user.id).await?;
    let current = find_device(&txn, caller.user.id, device_id, true)
        .await?
        .ok_or_else(|| err(ErrorCode::DeviceNotFound, "Device not found"))?;
    let mut ended = vec![];
    if current.state != "REVOKED" {
        ended = end_device_sessions(&txn, device_id, "DEVICE_REVOKED", None).await?;
        ended.extend(
            end_unclassified_sessions(
                &txn,
                caller.user.id,
                "DEVICE_REVOKED",
                Some(caller.session_id),
            )
            .await?,
        );
        txn.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE account_devices SET state = 'REVOKED', revoked_at = now(), \
                    revoke_reason = 'USER_REVOKED', updated_at = now() WHERE id = $1",
            [device_id.into()],
        ))
        .await?;
        txn.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE account_device_links SET status = 'CANCELLED', finished_at = now() \
             WHERE device_id = $1 AND status IN ('PENDING', 'APPROVED')",
            [device_id.into()],
        ))
        .await?;
        security::record_event(
            &txn,
            SecurityEvent::new("DEVICE_REVOKED", Outcome::Success)
                .by_user(caller.user.id)
                .session(caller.session_id)
                .detail(json!({
                    "deviceId": device_id, "deviceClass": target.device_class,
                    "wasPrimary": target.device_class == "MOBILE" && target.state == "ACTIVE",
                    "self": own, "sessionsEnded": ended.len(),
                })),
        )
        .await?;
    }
    txn.commit().await?;
    session_effects(state, caller.user.id, &ended).await;
    let d = find_device(&state.db, caller.user.id, device_id, false)
        .await?
        .ok_or_else(|| err(ErrorCode::DeviceNotFound, "Device not found"))?;
    Ok(DeviceOutcome {
        device: d.view(caller.device_id),
        replaced_device_id: None,
        ended_sessions: ended.len() as i64,
    })
}

/// Sign out everywhere: every live device of the account is revoked too (the sessions and
/// the epoch are handled by [`security::revoke_all`]).
pub async fn revoke_all_devices<C: ConnectionTrait>(db: &C, user_id: Uuid) -> ApiResult<u64> {
    let r = db
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE account_devices SET state = 'REVOKED', revoked_at = now(), \
                    revoke_reason = 'LOGOUT_ALL', updated_at = now() \
             WHERE user_id = $1 AND state <> 'REVOKED'",
            [user_id.into()],
        ))
        .await?;
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE account_device_links SET status = 'CANCELLED', finished_at = now() \
         WHERE user_id = $1 AND status IN ('PENDING', 'APPROVED')",
        [user_id.into()],
    ))
    .await?;
    Ok(r.rows_affected())
}

/// Whether a sign-in belongs to a trusted, active device (personal E2EE enrollment gate).
pub async fn session_is_trusted_device<C: ConnectionTrait>(
    db: &C,
    session_id: Uuid,
) -> ApiResult<bool> {
    Ok(db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT 1 AS x FROM account_sessions s JOIN account_devices d ON d.id = s.device_id \
             WHERE s.id = $1 AND s.revoked_at IS NULL AND d.trust = 'TRUSTED' AND d.state = 'ACTIVE'",
            [session_id.into()],
        ))
        .await?
        .is_some())
}

/// Refresh of a device-bound sign-in: the device must prove possession of its key
/// (purpose `REFRESH`, context = `{sessionId}`), so a stolen refresh token cannot be used
/// without the device. Unclassified sign-ins refresh as before.
pub async fn check_refresh_binding<C: ConnectionTrait>(
    db: &C,
    policy: &DevicePolicy,
    session_id: Uuid,
    proof: Option<&DeviceProof>,
) -> ApiResult<()> {
    let row = db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT d.public_key, d.state, s.authenticated_at FROM account_sessions s \
             LEFT JOIN account_devices d ON d.id = s.device_id WHERE s.id = $1",
            [session_id.into()],
        ))
        .await?;
    let Some(row) = row else { return Ok(()) };
    let key: Option<Vec<u8>> = row.try_get("", "public_key")?;
    let Some(key) = key else {
        // Unclassified sign-in: refreshes as before (compatibility), unless device
        // declaration is enforced and its declaration window has passed.
        let authenticated_at: chrono::DateTime<chrono::Utc> =
            row.try_get("", "authenticated_at")?;
        if policy.unclassified_refused(authenticated_at, chrono::Utc::now()) {
            return Err(declaration_required());
        }
        return Ok(());
    };
    let device_state: String = row.try_get("", "state")?;
    if device_state == "REVOKED" {
        return Err(security_revoked());
    }
    let proof = proof.ok_or_else(|| {
        err(
            ErrorCode::DeviceProofInvalid,
            "This sign-in is bound to a device: send deviceProof (purpose REFRESH)",
        )
    })?;
    verify_proof(
        db,
        policy,
        &key,
        ProofPurpose::Refresh,
        &session_id.to_string(),
        proof,
    )
    .await
}

/// Audit helper for refusals at the edge (best effort).
pub async fn audit_denied(
    state: &AppState,
    user_id: Uuid,
    session: Option<Uuid>,
    what: &'static str,
    reason: &str,
) {
    let mut e = SecurityEvent::new(what, Outcome::Denied)
        .user(user_id)
        .actor(ActorKind::User, Some(user_id))
        .detail(json!({ "reason": reason }));
    if let Some(s) = session {
        e = e.session(s);
    }
    security::record_event_best_effort(&state.db, e).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn policy_defaults_and_bounds() {
        let p = DevicePolicy::default();
        assert_eq!(p.max_web_devices, 4);
        assert!(p.email_step_up && p.e2ee_requires_trusted_device);
        let q = DevicePolicy::from_lookup(&|k| match k {
            "ACCOUNT_MAX_WEB_DEVICES" => Some("99".into()),
            "ACCOUNT_DEVICE_EMAIL_STEP_UP" => Some("off".into()),
            _ => None,
        });
        assert_eq!(q.max_web_devices, 20);
        assert!(!q.email_step_up);
    }

    #[test]
    fn statement_is_domain_separated_and_signable() {
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let pk = sk.verifying_key().to_bytes();
        let tp = thumbprint(&pk);
        let s = proof_statement(ProofPurpose::Declare, &tp, "ctx", 1, "bm9uY2U");
        assert!(s.starts_with("cfmeet-account-device-proof:v1\nDECLARE\n"));
        let sig = sk.sign(s.as_bytes());
        let key = VerifyingKey::from_bytes(&pk).unwrap();
        assert!(key.verify_strict(s.as_bytes(), &sig).is_ok());
        // Another purpose with the same signature fails.
        let other = proof_statement(ProofPurpose::Refresh, &tp, "ctx", 1, "bm9uY2U");
        assert!(key.verify_strict(other.as_bytes(), &sig).is_err());
    }

    #[test]
    fn installation_digest_is_per_account() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        assert_ne!(
            installation_digest(a, "installation-0001"),
            installation_digest(b, "installation-0001")
        );
    }

    #[test]
    fn declaration_validation() {
        let mut r = DeclareDeviceRequest {
            installation_id: "abcdefghijklmnop".into(),
            device_class: "WEB".into(),
            platform: "WEB".into(),
            label: None,
            public_key: String::new(),
            proof: DeviceProof {
                issued_at: 0,
                nonce: String::new(),
                signature: String::new(),
            },
        };
        assert!(validate_declaration(&r).is_ok());
        r.platform = "IOS".into();
        assert!(validate_declaration(&r).is_err());
        r.device_class = "MOBILE".into();
        assert!(validate_declaration(&r).is_ok());
        r.installation_id = "short".into();
        assert!(validate_declaration(&r).is_err());
    }
}
