//! ADR 0068 (brief §1): API keys issued or rotated by the HQ admin bridge are delivered by
//! Platform Access itself, straight to the partner — never through HQ.
//!
//! HQ staff never see a partner secret and HQ never stores or relays one, so a bridge
//! `identity.key.issue` / `identity.key.rotate` cannot answer the secret the way the
//! console and the Developer Platform do. Instead, in the SAME transaction as the
//! issuance, its audit row and its admin-operations record:
//!
//! * the new key's secret is sealed (XChaCha20-Poly1305, fresh random 192-bit nonce)
//!   under a key derived from the credential pepper of the current version
//!   (`HMAC-SHA-256(pepper_v, "cfmeet.platform-access.credential-delivery-key.v1")`, the
//!   same derivation pattern as the OAuth pairwise key — no new deployment secret), with
//!   additional data binding the ciphertext to its delivery, its key and the pepper
//!   version; the clear secret is dropped (zeroized) when the transaction commits;
//! * a `platform_credential_deliveries` row holds the sealed copy as `PENDING` for
//!   [`DELIVERY_TTL_DAYS`] days, channel [`CHANNEL_CONSOLE_REVEAL_ONCE`].
//!
//! The partner reveals it ONCE in their own Developer Platform console, which reaches
//! Platform Access through delegation (`POST /internal/v1/delegation/environments/{id}/
//! credential-deliveries/{deliveryRef}/reveal`, a named human member only): the reveal
//! answers the secret and destroys the sealed copy in the same transaction (`REVEALED`,
//! audited `CREDENTIAL_DELIVERY_REVEALED`). Expiry, a revoked key or a copy that no
//! longer opens (its pepper retired) destroys it too (`EXPIRED`, the minute sweep
//! [`purge_expired`] and the reveal itself). Nothing is ever resent: an expired delivery
//! is re-issued by a new command.
//!
//! The bridge answer, the ledger, the audit row and every log line carry the delivery
//! REFERENCE only (ids, channel, state, times) — never the secret, the sealed copy, the
//! nonce or the derived key.

use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    XChaCha20Poly1305, XNonce,
};
use chrono::{DateTime, Duration, Utc};
use rand::{rngs::OsRng, RngCore};
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait, Value as DbValue};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;
use zeroize::Zeroizing;

use super::{
    bridge,
    credentials::{CredentialEffectiveState, IssueCredential, IssuedCredential, RotateCredential},
    operator::{
        self, credential_entry, AuditEntry, AuditTargetKind, OperatorAction, OperatorContext,
    },
    verifier::VerifierKeyring,
    PlatformError, PlatformResult, PlatformService,
};
use crate::domain::{CredentialId, EnvironmentId, IssuedSecret, ServiceAccountId};

/// `delivery` of every owner-delivered key.
pub const DELIVERY: &str = "OWNER_DELIVERED";
/// The only channel Platform Access offers: one reveal in the partner's own console.
pub const CHANNEL_CONSOLE_REVEAL_ONCE: &str = "CONSOLE_REVEAL_ONCE";
/// How long a sealed copy waits for its reveal before it is destroyed.
pub const DELIVERY_TTL_DAYS: i64 = 7;
/// The most pending deliveries one environment lists.
pub const PENDING_LIST_MAX: u64 = 100;

const KEY_LABEL: &[u8] = b"cfmeet.platform-access.credential-delivery-key.v1";
const AAD_LABEL: &str = "cfmeet.platform-access.credential-delivery.v1";
const NONCE_LEN: usize = 24;

fn stmt(sql: &str, values: Vec<DbValue>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

fn aad(delivery: Uuid, credential: Uuid, key_version: i32) -> Vec<u8> {
    format!("{AAD_LABEL}|{delivery}|{credential}|{key_version}").into_bytes()
}

fn cipher(keyring: &VerifierKeyring, key_version: i32) -> Option<XChaCha20Poly1305> {
    let key = Zeroizing::new(keyring.compute_with(key_version, KEY_LABEL)?);
    XChaCha20Poly1305::new_from_slice(key.as_ref()).ok()
}

/// A sealed secret as stored. `Debug` shows lengths only.
struct Sealed {
    bytes: Vec<u8>,
    nonce: [u8; NONCE_LEN],
}

impl std::fmt::Debug for Sealed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sealed")
            .field("len", &self.bytes.len())
            .finish()
    }
}

fn seal(
    keyring: &VerifierKeyring,
    key_version: i32,
    delivery: Uuid,
    credential: Uuid,
    value: &str,
) -> PlatformResult<Sealed> {
    let cipher =
        cipher(keyring, key_version).ok_or_else(|| internal("delivery key unavailable"))?;
    let mut nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);
    let bytes = cipher
        .encrypt(
            &XNonce::from(nonce),
            Payload {
                msg: value.as_bytes(),
                aad: &aad(delivery, credential, key_version),
            },
        )
        .map_err(|_| internal("sealing failed"))?;
    Ok(Sealed { bytes, nonce })
}

/// Opens a sealed copy (`None`: unknown pepper version, wrong binding or tampered).
fn open(
    keyring: &VerifierKeyring,
    key_version: i32,
    delivery: Uuid,
    credential: Uuid,
    sealed: &[u8],
    nonce: &[u8],
) -> Option<IssuedSecret> {
    let nonce: [u8; NONCE_LEN] = nonce.try_into().ok()?;
    let clear = Zeroizing::new(
        cipher(keyring, key_version)?
            .decrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: sealed,
                    aad: &aad(delivery, credential, key_version),
                },
            )
            .ok()?,
    );
    let text = std::str::from_utf8(&clear).ok()?;
    Some(IssuedSecret::new(text.to_string()))
}

fn internal(what: &str) -> PlatformError {
    PlatformError::Database {
        summary: what.to_string(),
        connectivity: false,
    }
}

/// A delivery waiting for its reveal, as the partner's console lists it (no secret).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingDelivery {
    pub delivery_ref: Uuid,
    pub delivery: &'static str,
    pub delivery_channel: String,
    pub state: String,
    pub credential_id: Uuid,
    pub public_id: String,
    pub service_account_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

impl PlatformService {
    /// Seals `issued`'s secret into a new PENDING delivery inside `txn`; returns its ref.
    async fn hold_for_reveal<C: ConnectionTrait>(
        &self,
        txn: &C,
        issued: &IssuedCredential,
        now: DateTime<Utc>,
    ) -> PlatformResult<Uuid> {
        let delivery = Uuid::new_v4();
        let credential = issued.credential.id.as_uuid();
        let key_version = self.keyring.current_version();
        let sealed = seal(
            &self.keyring,
            key_version,
            delivery,
            credential,
            issued.secret.expose_once(),
        )?;
        txn.execute(stmt(
            "INSERT INTO platform_credential_deliveries (id, credential_id, tenant_id, \
               environment_id, channel, state, sealed, nonce, key_version, created_at, expires_at) \
             VALUES ($1, $2, $3, $4, $5, 'PENDING', $6, $7, $8, $9, $10)",
            vec![
                delivery.into(),
                credential.into(),
                issued.credential.tenant_id.as_uuid().into(),
                issued.credential.environment_id.as_uuid().into(),
                CHANNEL_CONSOLE_REVEAL_ONCE.into(),
                sealed.bytes.clone().into(),
                sealed.nonce.to_vec().into(),
                key_version.into(),
                now.into(),
                (now + Duration::days(DELIVERY_TTL_DAYS)).into(),
            ],
        ))
        .await?;
        Ok(delivery)
    }

    /// `identity.key.issue` through the admin bridge: issues a key of an ACTIVE service
    /// account and holds its secret for one reveal in the partner's console; the audit
    /// row, the delivery and the bridge record commit together. `op` must carry the
    /// bridge command (subject `KEY_ISSUE`, locked at the service account's version).
    pub async fn bridge_issue_credential(
        &self,
        op: &OperatorContext,
        service_account_id: ServiceAccountId,
        request: IssueCredential,
    ) -> PlatformResult<Uuid> {
        let cmd = op
            .bridge()
            .ok_or_else(|| internal("owner-delivered issuance without a bridge command"))?;
        let txn = self.db.begin().await?;
        bridge::lock(&txn, cmd).await?;
        let issued = self
            .issue_credential_in(&txn, service_account_id, request)
            .await?;
        let now = self.clock.now();
        let delivery = self.hold_for_reveal(&txn, &issued, now).await?;
        let mut entry = credential_entry(OperatorAction::CredentialCreated, &issued.credential);
        entry.detail["deliveryRef"] = json!(delivery);
        entry.detail["deliveryChannel"] = json!(CHANNEL_CONSOLE_REVEAL_ONCE);
        operator::record(&txn, op, &entry, now).await?;
        bridge::record(&txn, cmd, Some(issued.credential.id.as_uuid()), None, now).await?;
        txn.commit().await?;
        tracing::info!(
            credential_id = %issued.credential.id,
            delivery_ref = %delivery,
            "owner-delivered API key issued; awaiting its reveal"
        );
        Ok(delivery)
    }

    /// `identity.key.rotate` through the admin bridge: rotates an ACTIVE key (the
    /// predecessor keeps working for the overlap) and holds the successor's secret for
    /// one reveal. `op` must carry the bridge command (subject `KEY_ROTATION`, locked at
    /// the rotated key's version).
    pub async fn bridge_rotate_credential(
        &self,
        op: &OperatorContext,
        credential_id: CredentialId,
        request: RotateCredential,
    ) -> PlatformResult<Uuid> {
        let cmd = op
            .bridge()
            .ok_or_else(|| internal("owner-delivered rotation without a bridge command"))?;
        let txn = self.db.begin().await?;
        bridge::lock(&txn, cmd).await?;
        let issued = self
            .rotate_credential_in(&txn, credential_id, request)
            .await?;
        let now = self.clock.now();
        let delivery = self.hold_for_reveal(&txn, &issued, now).await?;
        let mut entry = credential_entry(OperatorAction::CredentialRotated, &issued.credential);
        entry.target_id = Some(credential_id.as_uuid());
        entry.detail["successorCredentialId"] = json!(issued.credential.id);
        entry.detail["deliveryRef"] = json!(delivery);
        entry.detail["deliveryChannel"] = json!(CHANNEL_CONSOLE_REVEAL_ONCE);
        operator::record(&txn, op, &entry, now).await?;
        bridge::record(&txn, cmd, Some(issued.credential.id.as_uuid()), None, now).await?;
        txn.commit().await?;
        tracing::info!(
            credential_id = %issued.credential.id,
            rotated_from = %credential_id,
            delivery_ref = %delivery,
            "owner-delivered API key rotated; awaiting its reveal"
        );
        Ok(delivery)
    }

    /// The deliveries of `environment_id` still waiting for their reveal (oldest first).
    pub async fn pending_credential_deliveries(
        &self,
        environment_id: EnvironmentId,
    ) -> PlatformResult<Vec<PendingDelivery>> {
        let rows = self
            .db
            .query_all(stmt(
                "SELECT d.id, d.channel, d.state, d.credential_id, c.public_id, \
                   c.service_account_id, d.created_at, d.expires_at \
                 FROM platform_credential_deliveries d \
                 JOIN platform_api_credentials c ON c.id = d.credential_id \
                 WHERE d.environment_id = $1 AND d.state = 'PENDING' AND d.expires_at > $2 \
                   AND c.status <> 'REVOKED' \
                 ORDER BY d.created_at, d.id LIMIT $3",
                vec![
                    environment_id.as_uuid().into(),
                    self.clock.now().into(),
                    (PENDING_LIST_MAX as i64).into(),
                ],
            ))
            .await?;
        rows.iter()
            .map(|r| {
                Ok(PendingDelivery {
                    delivery_ref: r.try_get("", "id")?,
                    delivery: DELIVERY,
                    delivery_channel: r.try_get("", "channel")?,
                    state: r.try_get("", "state")?,
                    credential_id: r.try_get("", "credential_id")?,
                    public_id: r.try_get("", "public_id")?,
                    service_account_id: r.try_get("", "service_account_id")?,
                    created_at: r.try_get("", "created_at")?,
                    expires_at: r.try_get("", "expires_at")?,
                })
            })
            .collect()
    }

    /// The ONE reveal of a pending delivery of `environment_id` by the partner (`op`: the
    /// delegated human member): answers the key and its secret and destroys the sealed
    /// copy in the same transaction (audited `CREDENTIAL_DELIVERY_REVEALED`). An unknown,
    /// already revealed or expired delivery is `NotFound`; a delivery whose key no longer
    /// authenticates (revoked, expired, superseded) or whose copy no longer opens is
    /// destroyed (`EXPIRED`) and `NotFound` too.
    pub async fn reveal_credential_delivery(
        &self,
        op: &OperatorContext,
        environment_id: EnvironmentId,
        delivery_ref: Uuid,
    ) -> PlatformResult<IssuedCredential> {
        let not_found = PlatformError::NotFound("credential delivery");
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        let Some(row) = txn
            .query_one(stmt(
                "SELECT credential_id, key_version, sealed, nonce, expires_at \
                 FROM platform_credential_deliveries \
                 WHERE id = $1 AND environment_id = $2 AND state = 'PENDING' FOR UPDATE",
                vec![delivery_ref.into(), environment_id.as_uuid().into()],
            ))
            .await?
        else {
            return Err(not_found);
        };
        let credential_id: Uuid = row.try_get("", "credential_id")?;
        let key_version: i32 = row.try_get("", "key_version")?;
        let expires_at: DateTime<Utc> = row.try_get("", "expires_at")?;
        let sealed: Vec<u8> = row.try_get("", "sealed")?;
        let nonce: Vec<u8> = row.try_get("", "nonce")?;
        let credential = self
            .credential_in(&txn, CredentialId::from_uuid(credential_id))
            .await?;
        let usable = now < expires_at
            && !matches!(
                credential.effective_state,
                CredentialEffectiveState::Revoked
                    | CredentialEffectiveState::Expired
                    | CredentialEffectiveState::Superseded
            );
        let opened = if usable {
            open(
                &self.keyring,
                key_version,
                delivery_ref,
                credential_id,
                &sealed,
                &nonce,
            )
        } else {
            None
        };
        let Some(secret) = opened else {
            txn.execute(stmt(
                "UPDATE platform_credential_deliveries SET state = 'EXPIRED', sealed = NULL, \
                   nonce = NULL, closed_at = $2 WHERE id = $1",
                vec![delivery_ref.into(), now.into()],
            ))
            .await?;
            txn.commit().await?;
            tracing::info!(
                delivery_ref = %delivery_ref,
                usable,
                "credential delivery closed without a reveal"
            );
            return Err(not_found);
        };
        txn.execute(stmt(
            "UPDATE platform_credential_deliveries SET state = 'REVEALED', sealed = NULL, \
               nonce = NULL, revealed_at = $2, revealed_by = $3, closed_at = $2 WHERE id = $1",
            vec![
                delivery_ref.into(),
                now.into(),
                op.operator().chars().take(100).collect::<String>().into(),
            ],
        ))
        .await?;
        let entry = AuditEntry::new(
            OperatorAction::CredentialDeliveryRevealed,
            AuditTargetKind::Credential,
            credential_id,
        )
        .scope(
            Some(credential.tenant_id.as_uuid()),
            Some(credential.application_id.as_uuid()),
            Some(credential.environment_id.as_uuid()),
        )
        .detail(json!({ "deliveryRef": delivery_ref, "publicId": credential.public_id }));
        operator::record(&txn, op, &entry, now).await?;
        txn.commit().await?;
        tracing::info!(
            credential_id = %credential_id,
            delivery_ref = %delivery_ref,
            "owner-delivered API key revealed"
        );
        Ok(IssuedCredential { credential, secret })
    }
}

/// Destroys every sealed copy whose window has passed or whose key was revoked
/// (`EXPIRED`); the reference row stays. Run by the service's minute sweep.
pub async fn purge_expired<C: ConnectionTrait>(db: &C, now: DateTime<Utc>) -> PlatformResult<u64> {
    let purged = db
        .execute(stmt(
            "UPDATE platform_credential_deliveries d SET state = 'EXPIRED', sealed = NULL, \
               nonce = NULL, closed_at = $1 \
             WHERE d.state = 'PENDING' AND (d.expires_at <= $1 OR EXISTS ( \
               SELECT 1 FROM platform_api_credentials c \
               WHERE c.id = d.credential_id AND c.status = 'REVOKED'))",
            vec![now.into()],
        ))
        .await?
        .rows_affected();
    if purged > 0 {
        tracing::info!(purged, "expired credential deliveries destroyed");
    }
    Ok(purged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::verifier::CredentialPepper;

    const PEPPER_A: &str = "unit-pepper-A-0123456789abcdefghijklmnopqrstuvwxyz";
    const PEPPER_B: &str = "unit-pepper-B-zyxwvutsrqponmlkjihgfedcba9876543210";

    fn ring(current: i32, previous: bool) -> VerifierKeyring {
        VerifierKeyring::new(
            current,
            CredentialPepper::parse("P", if current == 2 { PEPPER_B } else { PEPPER_A }).unwrap(),
            previous.then(|| (1, CredentialPepper::parse("P", PEPPER_A).unwrap())),
        )
        .unwrap()
    }

    #[test]
    fn a_sealed_secret_opens_only_with_its_binding_and_pepper() {
        let keyring = ring(1, false);
        let (d, c) = (Uuid::new_v4(), Uuid::new_v4());
        let secret = "cfmk_test_fixture_0123456789abcdef_feedface";
        let sealed = seal(&keyring, 1, d, c, secret).unwrap();
        assert!(!sealed
            .bytes
            .windows(secret.len())
            .any(|w| w == secret.as_bytes()));
        assert!(!format!("{sealed:?}").contains("feedface"));
        let opened = open(&keyring, 1, d, c, &sealed.bytes, &sealed.nonce).unwrap();
        assert_eq!(opened.expose_once(), secret);
        // Another delivery, key or version, a tampered copy or a short nonce: nothing.
        assert!(open(&keyring, 1, Uuid::new_v4(), c, &sealed.bytes, &sealed.nonce).is_none());
        assert!(open(&keyring, 1, d, Uuid::new_v4(), &sealed.bytes, &sealed.nonce).is_none());
        assert!(open(&keyring, 2, d, c, &sealed.bytes, &sealed.nonce).is_none());
        let mut tampered = sealed.bytes.clone();
        tampered[0] ^= 1;
        assert!(open(&keyring, 1, d, c, &tampered, &sealed.nonce).is_none());
        assert!(open(&keyring, 1, d, c, &sealed.bytes, &sealed.nonce[..12]).is_none());
        // Encryption is randomized.
        assert_ne!(seal(&keyring, 1, d, c, secret).unwrap().bytes, sealed.bytes);
    }

    #[test]
    fn a_pepper_rotation_keeps_pending_copies_open_while_the_old_pepper_is_deployed() {
        let (d, c) = (Uuid::new_v4(), Uuid::new_v4());
        let sealed = seal(&ring(1, false), 1, d, c, "s3").unwrap();
        let rotated = ring(2, true);
        assert_eq!(
            open(&rotated, 1, d, c, &sealed.bytes, &sealed.nonce)
                .unwrap()
                .expose_once(),
            "s3"
        );
        // Once the old pepper is retired the copy never opens (the reveal then expires it).
        assert!(open(&ring(2, false), 1, d, c, &sealed.bytes, &sealed.nonce).is_none());
    }

    #[test]
    fn the_delivery_key_is_not_the_verifier_of_anything() {
        // Verifier messages are NUL-separated (family, tag, public id, secret); the label
        // has no NUL, so the derived key can never equal a stored verifier.
        assert!(!KEY_LABEL.contains(&0));
    }
}
