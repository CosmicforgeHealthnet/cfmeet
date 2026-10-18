//! Messaging devices and their PUBLIC MLS material (Part V C.4).
//!
//! Registration (two steps, both authorized afresh):
//!
//! 1. [`create_challenge`] — for an acting principal that Platform Access resolves ACTIVE
//!    in the credential's own environment: a single-use challenge with a server nonce and
//!    a SERVER-assigned device id, bound to credential, principal and environment, valid
//!    for minutes, rate-limited per principal.
//! 2. [`register`] — the device answers with a *registration KeyPackage*: credential
//!    identity = the assigned device id, leaf `application_id` = the challenge binding,
//!    both covered by the RFC 9420 signatures OpenMLS verifies. Possession of the device
//!    key and freshness are therefore proven without any custom signature. Consumes the
//!    challenge, enforces the active-device limit (per-principal advisory lock), stores
//!    the device signature PUBLIC key (unique for ever) and appends a transparency leaf —
//!    all in one transaction with its audit row and outbox event. Replaying the same
//!    answer returns the same device (idempotent); anything else is refused.
//!
//! KeyPackages ([`publish_key_packages`]): validated, bound to the device and its key,
//! never last-resort or registration packages, unique by KeyPackageRef (replay
//! prevention), bounded per device. [`claim_key_packages`] reserves one package per
//! active device with `FOR UPDATE SKIP LOCKED` (single use under concurrency) for the
//! membership workstream. [`revoke`] is terminal and closes the device's packages.
//!
//! Nothing here sees a private key: there is none on the server.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use cf_messaging_crypto::{
    binding,
    key_package::{validate_key_package, ValidatedKeyPackage},
    profile::{CIPHERSUITE_NAME, PROTOCOL_ID},
};
use chrono::{DateTime, SecondsFormat, Utc};
use rand::RngCore;
use sea_orm::{ConnectionTrait, DatabaseTransaction, FromQueryResult, TransactionTrait};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    audit::{self, AuditEntry},
    db,
    domain::{current_context, RequestContext},
    error::{Error, ErrorCode, Result},
    events::outbox::{self, EventScope, NewEvent},
    http::middleware::Acting,
    identity::TrustedContext,
    limits::{self, Window},
    state::AppState,
    transparency::{self, EntryKind, LeafEntry},
};

/// Most KeyPackages accepted in one publication request.
pub const MAX_KEY_PACKAGES_PER_REQUEST: usize = 20;

fn request_context() -> RequestContext {
    current_context().unwrap_or_else(RequestContext::generate)
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Micros, true)
}

fn require_enabled(state: &AppState) -> Result<()> {
    if state.devices.enabled {
        Ok(())
    } else {
        Err(Error::reason(
            ErrorCode::FeatureDisabled,
            "E2EE_DEVICES_DISABLED",
        ))
    }
}

fn event_scope(ctx: &TrustedContext) -> EventScope {
    EventScope {
        tenant_id: ctx.scope.tenant_id,
        application_id: ctx.scope.application_id,
        environment_id: ctx.scope.environment_id,
    }
}

fn unix(t: DateTime<Utc>) -> u64 {
    t.timestamp().max(0) as u64
}

fn decode_b64(value: &str, what: &'static str) -> Result<Vec<u8>> {
    if value.len() > 24 * 1024 {
        return Err(Error::reason(
            ErrorCode::KeyPackageRejected,
            "KEY_PACKAGE_TOO_LARGE",
        ));
    }
    URL_SAFE_NO_PAD
        .decode(value.trim_end_matches('='))
        .map_err(|_| Error::validation(format!("{what} must be base64url")))
}

fn validated(state: &AppState, bytes: &[u8]) -> Result<ValidatedKeyPackage> {
    validate_key_package(bytes, unix(state.now()), &state.devices.lifetime_policy()).map_err(|e| {
        crate::metrics::inc(
            "messaging_key_packages_rejected_total",
            &[("reason", e.code())],
        );
        Error::reason(ErrorCode::KeyPackageRejected, e.code())
    })
}

// ---------------------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeView {
    pub challenge_id: Uuid,
    pub device_id: Uuid,
    pub principal_id: Uuid,
    /// 64 lower-case hex characters.
    pub nonce: String,
    /// The exact `application_id` the registration KeyPackage's leaf must carry.
    pub registration_binding: String,
    /// The exact `BasicCredential` identity of every KeyPackage of this device.
    pub credential_identity: String,
    pub protocol: &'static str,
    pub ciphersuite: &'static str,
    pub expires_at: String,
}

#[derive(Debug, Clone, Serialize, FromQueryResult)]
pub struct DeviceRow {
    pub id: Uuid,
    pub principal_id: Uuid,
    pub status: String,
    pub signature_public_key: Vec<u8>,
    pub credential_identity: Vec<u8>,
    pub key_fingerprint: String,
    pub display_label: Option<String>,
    pub transparency_leaf_index: i64,
    pub revocation_leaf_index: Option<i64>,
    pub registered_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revocation_reason: Option<String>,
    pub version: i64,
}

const DEVICE_COLUMNS: &str = "id, principal_id, status, signature_public_key, credential_identity, \
     key_fingerprint, display_label, transparency_leaf_index, revocation_leaf_index, registered_at, \
     revoked_at, revocation_reason, version";

/// The public view of a device (contract `cfmeet.messaging.common` `Device`).
pub fn device_view(d: &DeviceRow) -> serde_json::Value {
    json!({
        "deviceId": d.id,
        "principalId": d.principal_id,
        "status": d.status,
        "protocol": PROTOCOL_ID,
        "ciphersuite": CIPHERSUITE_NAME,
        "signaturePublicKey": URL_SAFE_NO_PAD.encode(&d.signature_public_key),
        "credentialIdentity": String::from_utf8_lossy(&d.credential_identity),
        "keyFingerprint": d.key_fingerprint,
        "displayLabel": d.display_label,
        "registeredAt": rfc3339(d.registered_at),
        "revokedAt": d.revoked_at.map(rfc3339),
        "revocationReason": d.revocation_reason,
        "transparency": {
            "registrationLeafIndex": d.transparency_leaf_index,
            "revocationLeafIndex": d.revocation_leaf_index,
        },
        "version": d.version,
    })
}

// ---------------------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------------------

pub async fn create_challenge(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
) -> Result<ChallengeView> {
    require_enabled(state)?;
    let now = state.now();
    let key = format!(
        "challenge:{}:{}",
        ctx.scope.environment_id.simple(),
        acting.principal_id.simple()
    );
    if let Some(retry) = limits::hit(
        &state.db,
        &key,
        Window::Hour,
        state.devices.challenges_per_hour,
        now,
    )
    .await?
    {
        return Err(Error::RateLimited {
            retry_after_seconds: retry,
        });
    }
    let challenge_id = Uuid::new_v4();
    let device_id = Uuid::new_v4();
    let mut nonce = [0u8; binding::NONCE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let expires_at = now + state.devices.challenge_ttl;
    db::exec(
        &state.db,
        "INSERT INTO messaging_device_challenges (id, tenant_id, application_id, environment_id, \
           principal_id, credential_id, device_id, nonce, created_at, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        vec![
            challenge_id.into(),
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            ctx.scope.environment_id.into(),
            acting.principal_id.into(),
            ctx.credential_id.into(),
            device_id.into(),
            nonce.to_vec().into(),
            now.into(),
            expires_at.into(),
        ],
    )
    .await?;
    Ok(ChallengeView {
        challenge_id,
        device_id,
        principal_id: acting.principal_id,
        nonce: hex::encode(nonce),
        registration_binding: String::from_utf8(binding::registration_binding(
            challenge_id,
            &nonce,
        ))
        .expect("ascii"),
        credential_identity: String::from_utf8(binding::device_credential_identity(device_id))
            .expect("ascii"),
        protocol: PROTOCOL_ID,
        ciphersuite: CIPHERSUITE_NAME,
        expires_at: rfc3339(expires_at),
    })
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegisterRequest {
    pub challenge_id: Uuid,
    /// base64url of the registration KeyPackage (`MLSMessage`, wire format KeyPackage).
    pub key_package: String,
    #[serde(default)]
    pub display_label: Option<String>,
}

#[derive(FromQueryResult)]
struct ChallengeRow {
    principal_id: Uuid,
    credential_id: Uuid,
    device_id: Uuid,
    nonce: Vec<u8>,
    status: String,
    expires_at: DateTime<Utc>,
}

/// Outcome of a registration: the device and whether it was created now.
pub struct Registered {
    pub device: DeviceRow,
    pub created: bool,
}

fn challenge_invalid(reason: &'static str) -> Error {
    crate::metrics::inc(
        "messaging_device_registrations_refused_total",
        &[("reason", reason)],
    );
    Error::reason(ErrorCode::ChallengeInvalid, reason)
}

pub async fn register(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    request: RegisterRequest,
) -> Result<Registered> {
    require_enabled(state)?;
    let label = match request.display_label.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(l) if l.chars().count() <= 64 && !l.chars().any(char::is_control) => {
            Some(l.to_string())
        }
        Some(_) => {
            return Err(Error::validation(
                "displayLabel must be 1-64 printable characters",
            ))
        }
    };
    let bytes = decode_b64(&request.key_package, "keyPackage")?;
    let kp = validated(state, &bytes)?;
    let ctx = ctx.clone();
    let acting = acting.clone();
    let st = state.clone();
    let request_ctx = request_context();
    state
        .db
        .transaction::<_, Registered, Error>(move |txn| {
            Box::pin(async move {
                register_in(
                    &st,
                    txn,
                    &ctx,
                    &acting,
                    request.challenge_id,
                    kp,
                    label,
                    &request_ctx,
                )
                .await
            })
        })
        .await
        .map_err(Error::from)
}

#[allow(clippy::too_many_arguments)]
async fn register_in(
    state: &AppState,
    txn: &DatabaseTransaction,
    ctx: &TrustedContext,
    acting: &Acting,
    challenge_id: Uuid,
    kp: ValidatedKeyPackage,
    label: Option<String>,
    request: &RequestContext,
) -> Result<Registered> {
    let now = state.now();
    let challenge: ChallengeRow = db::one(
        txn,
        "SELECT principal_id, credential_id, device_id, nonce, status, expires_at \
         FROM messaging_device_challenges WHERE id = $1 AND tenant_id = $2 AND application_id = $3 \
           AND environment_id = $4 FOR UPDATE",
        vec![
            challenge_id.into(),
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            ctx.scope.environment_id.into(),
        ],
    )
    .await?
    .ok_or_else(|| challenge_invalid("UNKNOWN"))?;
    // Bound to the principal AND the credential that asked for it.
    if challenge.principal_id != acting.principal_id || challenge.credential_id != ctx.credential_id
    {
        return Err(challenge_invalid("BOUND_TO_ANOTHER_REQUEST"));
    }
    let nonce: [u8; binding::NONCE_LEN] = challenge
        .nonce
        .as_slice()
        .try_into()
        .map_err(|_| Error::code(ErrorCode::InternalError))?;
    let binding_ok = kp.device_id == challenge.device_id
        && kp
            .application_id
            .as_deref()
            .and_then(binding::parse_registration_binding)
            == Some((challenge_id, nonce));
    if challenge.status == "CONSUMED" {
        // Idempotent replay of the SAME answer returns the device; anything else is refused.
        let existing: Option<DeviceRow> = db::one(
            txn,
            &format!("SELECT {DEVICE_COLUMNS} FROM messaging_devices WHERE challenge_id = $1"),
            vec![challenge_id.into()],
        )
        .await?;
        return match existing {
            Some(d) if binding_ok && d.signature_public_key == kp.signature_key => Ok(Registered {
                device: d,
                created: false,
            }),
            _ => Err(challenge_invalid("CONSUMED")),
        };
    }
    if challenge.expires_at <= now {
        return Err(challenge_invalid("EXPIRED"));
    }
    if !binding_ok {
        return Err(challenge_invalid("BINDING_MISMATCH"));
    }
    // Serialize registrations of one principal: the active-device limit holds under races.
    db::exec(
        txn,
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 7))",
        vec![format!(
            "messaging-devices:{}:{}",
            ctx.scope.environment_id, acting.principal_id
        )
        .into()],
    )
    .await?;
    let active = db::scalar_i64(
        txn,
        "SELECT count(*) FROM messaging_devices WHERE environment_id = $1 AND principal_id = $2 AND status = 'ACTIVE'",
        vec![ctx.scope.environment_id.into(), acting.principal_id.into()],
    )
    .await?;
    if active >= state.devices.max_active_devices {
        return Err(Error::code(ErrorCode::DeviceLimitReached));
    }
    let in_use = db::scalar_i64(
        txn,
        "SELECT count(*) FROM messaging_devices WHERE signature_public_key = $1",
        vec![kp.signature_key.clone().into()],
    )
    .await?;
    if in_use > 0 {
        return Err(Error::reason(ErrorCode::Conflict, "SIGNATURE_KEY_IN_USE"));
    }
    let leaf = LeafEntry::new(
        EntryKind::DeviceRegistered,
        ctx.scope.tenant_id,
        ctx.scope.application_id,
        ctx.scope.environment_id,
        acting.principal_id,
        challenge.device_id,
        &kp.signature_key,
        &kp.credential_identity,
        now,
    );
    let leaf_index = transparency::append(txn, &leaf, now).await?;
    let fingerprint = hex::encode(Sha256::digest(&kp.signature_key));
    db::exec(
        txn,
        "INSERT INTO messaging_devices (id, tenant_id, application_id, environment_id, principal_id, status, \
           protocol, ciphersuite, signature_public_key, credential_identity, key_fingerprint, display_label, \
           registered_by_credential_id, challenge_id, transparency_leaf_index, registered_at) \
         VALUES ($1, $2, $3, $4, $5, 'ACTIVE', $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)",
        vec![
            challenge.device_id.into(),
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            ctx.scope.environment_id.into(),
            acting.principal_id.into(),
            PROTOCOL_ID.into(),
            CIPHERSUITE_NAME.into(),
            kp.signature_key.clone().into(),
            kp.credential_identity.clone().into(),
            fingerprint.clone().into(),
            label.into(),
            ctx.credential_id.into(),
            challenge_id.into(),
            (leaf_index as i64).into(),
            now.into(),
        ],
    )
    .await?;
    db::exec(
        txn,
        "UPDATE messaging_device_challenges SET status = 'CONSUMED', consumed_at = $2 WHERE id = $1",
        vec![challenge_id.into(), now.into()],
    )
    .await?;
    audit::record(
        txn,
        AuditEntry {
            ctx,
            principal_id: Some(acting.principal_id),
            action: "DEVICE_REGISTERED",
            target_kind: "DEVICE",
            target_id: challenge.device_id,
            detail: json!({ "key_fingerprint": fingerprint, "leaf_index": leaf_index }),
        },
        request,
        now,
    )
    .await?;
    outbox::enqueue(
        txn,
        NewEvent {
            event_type: "messaging.device.registered",
            aggregate_type: "DEVICE",
            aggregate_id: challenge.device_id,
            scope: event_scope(ctx),
            payload: json!({
                "device_id": challenge.device_id,
                "principal_id": acting.principal_id,
                "key_fingerprint": fingerprint,
                "transparency_leaf_index": leaf_index,
            }),
        },
        request,
        now,
    )
    .await?;
    crate::metrics::inc("messaging_devices_registered_total", &[]);
    let device = load_device(txn, ctx, challenge.device_id, false)
        .await?
        .ok_or(Error::code(ErrorCode::InternalError))?;
    Ok(Registered {
        device,
        created: true,
    })
}

/// A device of the caller's environment, optionally row-locked.
pub async fn load_device<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    device_id: Uuid,
    lock: bool,
) -> Result<Option<DeviceRow>> {
    Ok(db::one(
        db,
        &format!(
            "SELECT {DEVICE_COLUMNS} FROM messaging_devices WHERE id = $1 AND tenant_id = $2 \
               AND application_id = $3 AND environment_id = $4{}",
            if lock { " FOR UPDATE" } else { "" }
        ),
        vec![
            device_id.into(),
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            ctx.scope.environment_id.into(),
        ],
    )
    .await?)
}

/// The acting principal's own device (another principal's or scope's device is NOT_FOUND).
async fn own_device<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    acting: &Acting,
    device_id: Uuid,
) -> Result<DeviceRow> {
    match load_device(db, ctx, device_id, true).await? {
        Some(d) if d.principal_id == acting.principal_id => Ok(d),
        _ => Err(Error::code(ErrorCode::NotFound)),
    }
}

// ---------------------------------------------------------------------------------------
// KeyPackages
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublishRequest {
    pub key_packages: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishedKeyPackage {
    /// base64url RFC 9420 KeyPackageRef.
    pub key_package_ref: String,
    pub not_after: String,
    /// `PUBLISHED` now, or `ALREADY_PUBLISHED` (idempotent re-submission).
    pub outcome: &'static str,
}

pub async fn publish_key_packages(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    device_id: Uuid,
    request: PublishRequest,
) -> Result<Vec<PublishedKeyPackage>> {
    require_enabled(state)?;
    if request.key_packages.is_empty() || request.key_packages.len() > MAX_KEY_PACKAGES_PER_REQUEST
    {
        return Err(Error::validation(format!(
            "keyPackages must hold 1-{MAX_KEY_PACKAGES_PER_REQUEST} entries"
        )));
    }
    let mut packages = Vec::with_capacity(request.key_packages.len());
    for encoded in &request.key_packages {
        let kp = validated(state, &decode_b64(encoded, "keyPackages")?)?;
        if kp.last_resort {
            return Err(Error::reason(
                ErrorCode::KeyPackageRejected,
                "LAST_RESORT_REJECTED",
            ));
        }
        if kp.application_id.is_some() {
            // Registration packages carry a challenge binding and are never published.
            return Err(Error::reason(
                ErrorCode::KeyPackageRejected,
                "REGISTRATION_KEY_PACKAGE",
            ));
        }
        packages.push(kp);
    }
    let ctx = ctx.clone();
    let acting = acting.clone();
    let st = state.clone();
    let request_ctx = request_context();
    state
        .db
        .transaction::<_, Vec<PublishedKeyPackage>, Error>(move |txn| {
            Box::pin(async move {
                publish_in(&st, txn, &ctx, &acting, device_id, packages, &request_ctx).await
            })
        })
        .await
        .map_err(Error::from)
}

async fn publish_in(
    state: &AppState,
    txn: &DatabaseTransaction,
    ctx: &TrustedContext,
    acting: &Acting,
    device_id: Uuid,
    packages: Vec<ValidatedKeyPackage>,
    request: &RequestContext,
) -> Result<Vec<PublishedKeyPackage>> {
    let now = state.now();
    let device = own_device(txn, ctx, acting, device_id).await?;
    if device.status != "ACTIVE" {
        return Err(Error::code(ErrorCode::DeviceRevoked));
    }
    for kp in &packages {
        kp.require_device(device.id, &device.signature_public_key)
            .map_err(|e| Error::reason(ErrorCode::KeyPackageRejected, e.code()))?;
    }
    let available = db::scalar_i64(
        txn,
        "SELECT count(*) FROM messaging_key_packages WHERE device_id = $1 AND status = 'AVAILABLE' AND not_after > $2",
        vec![device.id.into(), now.into()],
    )
    .await?;
    let mut out = Vec::with_capacity(packages.len());
    let mut fresh = 0i64;
    for kp in packages {
        let not_before =
            DateTime::<Utc>::from_timestamp(kp.not_before as i64, 0).ok_or(Error::reason(
                ErrorCode::KeyPackageRejected,
                "KEY_PACKAGE_LIFETIME_REJECTED",
            ))?;
        let not_after =
            DateTime::<Utc>::from_timestamp(kp.not_after as i64, 0).ok_or(Error::reason(
                ErrorCode::KeyPackageRejected,
                "KEY_PACKAGE_LIFETIME_REJECTED",
            ))?;
        let inserted = db::exec(
            txn,
            "INSERT INTO messaging_key_packages (id, device_id, tenant_id, application_id, environment_id, \
               principal_id, key_package_ref, encoded, not_before, not_after, published_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) ON CONFLICT (key_package_ref) DO NOTHING",
            vec![
                Uuid::new_v4().into(),
                device.id.into(),
                ctx.scope.tenant_id.into(),
                ctx.scope.application_id.into(),
                ctx.scope.environment_id.into(),
                acting.principal_id.into(),
                kp.key_package_ref.clone().into(),
                kp.encoded.clone().into(),
                not_before.into(),
                not_after.into(),
                now.into(),
            ],
        )
        .await?;
        let outcome = if inserted == 1 {
            fresh += 1;
            "PUBLISHED"
        } else {
            // The ref includes the device key, so it can only be this device's package:
            // an idempotent re-submission (never a second acceptance).
            let owner: Option<Uuid> = db::one::<OwnerRow, _>(
                txn,
                "SELECT device_id FROM messaging_key_packages WHERE key_package_ref = $1",
                vec![kp.key_package_ref.clone().into()],
            )
            .await?
            .map(|r| r.device_id);
            if owner != Some(device.id) {
                return Err(Error::reason(ErrorCode::Conflict, "KEY_PACKAGE_REPLAYED"));
            }
            "ALREADY_PUBLISHED"
        };
        out.push(PublishedKeyPackage {
            key_package_ref: URL_SAFE_NO_PAD.encode(&kp.key_package_ref),
            not_after: rfc3339(not_after),
            outcome,
        });
    }
    if available + fresh > state.devices.max_key_packages_per_device {
        return Err(Error::code(ErrorCode::KeyPackageLimitReached));
    }
    if fresh > 0 {
        audit::record(
            txn,
            AuditEntry {
                ctx,
                principal_id: Some(acting.principal_id),
                action: "KEY_PACKAGES_PUBLISHED",
                target_kind: "DEVICE",
                target_id: device.id,
                detail: json!({ "count": fresh }),
            },
            request,
            now,
        )
        .await?;
        outbox::enqueue(
            txn,
            NewEvent {
                event_type: "messaging.device.key-packages-published",
                aggregate_type: "DEVICE",
                aggregate_id: device.id,
                scope: event_scope(ctx),
                payload: json!({ "device_id": device.id, "count": fresh, "available": available + fresh }),
            },
            request,
            now,
        )
        .await?;
    }
    Ok(out)
}

#[derive(FromQueryResult)]
struct OwnerRow {
    device_id: Uuid,
}

/// One reserved KeyPackage.
#[derive(Debug, Clone, FromQueryResult)]
pub struct ClaimedKeyPackage {
    pub id: Uuid,
    pub device_id: Uuid,
    pub key_package_ref: Vec<u8>,
    pub encoded: Vec<u8>,
}

/// Reserves one AVAILABLE KeyPackage of each ACTIVE device of `principal_id` under
/// `reservation_id`, earliest expiry first, skipping rows another transaction holds
/// (`FOR UPDATE SKIP LOCKED`): two concurrent claims never receive the same package.
/// Packages expiring within `min_remaining` are not handed out. For the membership
/// workstream (E), which authorizes the claimer; no public endpoint calls it yet.
pub async fn claim_key_packages<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    principal_id: Uuid,
    reservation_id: Uuid,
    now: DateTime<Utc>,
    min_remaining: chrono::Duration,
) -> Result<Vec<ClaimedKeyPackage>> {
    Ok(db::all(
        db,
        "WITH candidates AS ( \
           SELECT p.id FROM messaging_devices d \
           CROSS JOIN LATERAL ( \
             SELECT k.id FROM messaging_key_packages k \
              WHERE k.device_id = d.id AND k.status = 'AVAILABLE' \
                AND k.not_before <= $4 AND k.not_after > $5 \
              ORDER BY k.not_after, k.id LIMIT 1 \
              FOR UPDATE SKIP LOCKED) p \
            WHERE d.environment_id = $1 AND d.principal_id = $2 AND d.status = 'ACTIVE') \
         UPDATE messaging_key_packages k SET status = 'RESERVED', reservation_id = $3, reserved_at = $4 \
           FROM candidates c WHERE k.id = c.id AND k.status = 'AVAILABLE' \
         RETURNING k.id, k.device_id, k.key_package_ref, k.encoded",
        vec![
            environment_id.into(),
            principal_id.into(),
            reservation_id.into(),
            now.into(),
            (now + min_remaining).into(),
        ],
    )
    .await?)
}

// ---------------------------------------------------------------------------------------
// Revocation and directory
// ---------------------------------------------------------------------------------------

pub const REVOCATION_REASONS: &[&str] = &[
    "USER_REQUESTED",
    "LOST_OR_STOLEN",
    "COMPROMISED",
    "REPLACED",
    "ADMINISTRATIVE",
];

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RevokeRequest {
    pub reason: String,
}

pub async fn revoke(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    device_id: Uuid,
    request: RevokeRequest,
) -> Result<DeviceRow> {
    let reason = REVOCATION_REASONS
        .iter()
        .copied()
        .find(|r| *r == request.reason)
        .ok_or_else(|| Error::validation("reason must be one of USER_REQUESTED, LOST_OR_STOLEN, COMPROMISED, REPLACED, ADMINISTRATIVE"))?;
    let ctx = ctx.clone();
    let acting = acting.clone();
    let st = state.clone();
    let request_ctx = request_context();
    state
        .db
        .transaction::<_, DeviceRow, Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let device = own_device(txn, &ctx, &acting, device_id).await?;
                if device.status == "REVOKED" {
                    return Ok(device);
                }
                let leaf = LeafEntry::new(
                    EntryKind::DeviceRevoked,
                    ctx.scope.tenant_id,
                    ctx.scope.application_id,
                    ctx.scope.environment_id,
                    acting.principal_id,
                    device.id,
                    &device.signature_public_key,
                    &device.credential_identity,
                    now,
                );
                let leaf_index = transparency::append(txn, &leaf, now).await?;
                db::exec(
                    txn,
                    "UPDATE messaging_devices SET status = 'REVOKED', revoked_at = $2, revocation_reason = $3, \
                       revocation_leaf_index = $4, version = version + 1 WHERE id = $1",
                    vec![device.id.into(), now.into(), reason.into(), (leaf_index as i64).into()],
                )
                .await?;
                let closed = db::exec(
                    txn,
                    "UPDATE messaging_key_packages SET status = 'REVOKED', closed_at = $2 \
                      WHERE device_id = $1 AND status IN ('AVAILABLE', 'RESERVED')",
                    vec![device.id.into(), now.into()],
                )
                .await?;
                audit::record(
                    txn,
                    AuditEntry {
                        ctx: &ctx,
                        principal_id: Some(acting.principal_id),
                        action: "DEVICE_REVOKED",
                        target_kind: "DEVICE",
                        target_id: device.id,
                        detail: json!({ "reason": reason, "closed_key_packages": closed, "leaf_index": leaf_index }),
                    },
                    &request_ctx,
                    now,
                )
                .await?;
                outbox::enqueue(
                    txn,
                    NewEvent {
                        event_type: "messaging.device.revoked",
                        aggregate_type: "DEVICE",
                        aggregate_id: device.id,
                        scope: event_scope(&ctx),
                        payload: json!({
                            "device_id": device.id,
                            "principal_id": acting.principal_id,
                            "reason": reason,
                            "transparency_leaf_index": leaf_index,
                        }),
                    },
                    &request_ctx,
                    now,
                )
                .await?;
                // The revoked device must leave every E2EE group of its principal: those
                // groups accept only a removal commit until one is accepted.
                crate::mls::device_revoked(txn, ctx.scope.environment_id, acting.principal_id, device.id, now)
                    .await?;
                // Its client sessions end and it receives no further notifications; the
                // gateways close its connections when the event below is published.
                crate::realtime::sessions::revoke_device_sessions(txn, device.id, now).await?;
                crate::realtime::push::disable_device(txn, device.id, now).await?;
                crate::metrics::inc("messaging_devices_revoked_total", &[]);
                load_device(txn, &ctx, device.id, false)
                    .await?
                    .ok_or(Error::code(ErrorCode::InternalError))
            })
        })
        .await
        .map_err(Error::from)
}

/// Counts one directory / proof lookup of the credential (anti-enumeration bound).
pub async fn directory_lookup_allowed(state: &AppState, ctx: &TrustedContext) -> Result<()> {
    let key = format!("directory:{}", ctx.credential_id.simple());
    match limits::hit(
        &state.db,
        &key,
        Window::Minute,
        state.devices.directory_lookups_per_minute,
        state.now(),
    )
    .await?
    {
        Some(retry) => Err(Error::RateLimited {
            retry_after_seconds: retry,
        }),
        None => Ok(()),
    }
}

/// The ACTIVE devices of a principal of the caller's environment.
pub async fn active_devices(
    state: &AppState,
    ctx: &TrustedContext,
    principal_id: Uuid,
) -> Result<Vec<DeviceRow>> {
    Ok(db::all(
        &state.db,
        &format!(
            "SELECT {DEVICE_COLUMNS} FROM messaging_devices WHERE tenant_id = $1 AND application_id = $2 \
               AND environment_id = $3 AND principal_id = $4 AND status = 'ACTIVE' ORDER BY registered_at, id LIMIT 100"
        ),
        vec![
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            ctx.scope.environment_id.into(),
            principal_id.into(),
        ],
    )
    .await?)
}
