//! The Account CRITICAL lane (Notification Service roadmap N12; Document 4 §9.2, §9.5,
//! §9.8, §10.4; Document 5 §4.5 / §4.6): one-time codes and account security notices.
//!
//! **Enqueue** ([`enqueue`]). Every legacy enqueue point of a CRITICAL category hands its
//! rendered legacy email and its catalogue event (if any) here:
//!
//! | Enqueue point (after PR #97, passwordless sign-in) | Ledger category / key | Catalogue event |
//! |---|---|---|
//! | `EmailService::send_sign_in_code` | `AUTH_VERIFICATION`, `otp:signin:…` | `account.sign-in-code.requested` (sealed code, magic-link token and address; one email principal, no subject) |
//! | `EmailService::send_email_change_code` | `AUTH_VERIFICATION`, `otp:emailchange:…` | `account.email-change-code.requested` (sealed code and new address; one email principal, ACCOUNT subject) |
//! | `EmailService::send_step_up_code` | `SECURITY_DEVICE_STEP_UP`, `otp:stepup:…` | `account.step-up-code.requested` (sealed code; `purpose`) |
//! | `email::device_step_up_code_on` (device links) | `SECURITY_DEVICE_STEP_UP`, `device-step-up:…` | `account.device-step-up-code.requested` |
//! | `email::device_notice_on` (device linked) | `NOTIFICATION`, `device-linked:…` | `account.security.device-linked` |
//! | `EmailService::send_welcome_email` | `AUTH_WELCOME`, `welcome:…` | `account.welcome.requested` (sealed address; one email principal, ACCOUNT subject) |
//! | `EmailService::send_new_sign_in_alert` | `NOTIFICATION`, `new-sign-in:…` | `account.security.new-sign-in` (sealed address, client label and masked network; ACCOUNT subject) |
//! | `EmailService::send_email_in_use_notice` | `NOTIFICATION`, `email-in-use:…` | `account.security.email-in-use` (sealed address; no subject) |
//! | `email::email_changed_notices_on` (both notices, self-service and support) | `NOTIFICATION`, `email-changed:{old,new}:…` | `account.security.email-changed` (sealed address; `OLD` also the masked new address and the undo token; ACCOUNT subject) |
//!
//! The last four rows are the fence category `ACCOUNT_NOTICES` (owner decision A, the N12
//! follow-up): until then they were sent only through the legacy ledger.
//!
//! (`AUTH_PASSWORD_RESET` and `SECURITY_PASSWORD_CHANGED` have no producer since #97.)
//! The category's routing mode is read first ([`fence::current_mode`]):
//! * `LEGACY_ONLY` (or no fence history): today's call, `email_delivery::enqueue`, byte for
//!   byte — nothing else is written;
//! * otherwise ONE transaction records the sticky decision (keyed by the legacy
//!   idempotency key) and writes **either** the legacy email (`OLD`, plus a shadow copy in
//!   `SHADOW`) **or** one `account_notification_outbox` row (`NEW`): the envelope with the
//!   code HPKE-sealed to the service key (`CF_NOTIFICATIONS_SEALING_KEY`), and
//!   `fallback_sealed`, the exact legacy email sealed to the Account fallback key
//!   (`CF_ACCOUNT_FALLBACK_KEY_FILE`, XChaCha20-Poly1305, bound
//!   to the decision id). A message that cannot travel `NEW` (no catalogue event, no
//!   fallback key, no sealing key for a code, no valid envelope) is decided `OLD`
//!   (`UNPUBLISHABLE`). `FORCED`
//!   decides every new message `OLD` ([`fence::route_message`]).
//!
//! **Relay** ([`relay_due`], [`run`], the PW1 worker): posts PENDING rows to I1 as
//! `cfmeet.meeting-service` (shadow copies and publications apart); `ACCEPTED` /
//! `DUPLICATE` / `HELD` acknowledge the row (`ns_acked_at`, §9.5); a refusal fails the
//! decision over at once; transport failures retry within seconds. Before posting, each
//! pass:
//! * expires open rows whose message expired (never sent by either path);
//! * **fails over** (§10.4): a `NEW` decision that was **never leased** is rerouted to the
//!   legacy ledger (`AUTO_FAILOVER`) — after [`AUTO_FAILOVER_BUDGET`] (60 s) in `AUTO`,
//!   immediately in `FORCED`, never in `OFF` — in one row-locked transaction that writes
//!   the legacy email opened from `fallback_sealed`: the same code, the same idempotency
//!   key, the same rendering as `LEGACY_ONLY`.
//!
//! **Exactly one sender.** The Notification Service sends a CRITICAL email only under an
//! X6 lease recorded in the decision row ([`claim_lease`]), inside a 20 s provider deadline
//! (lease 30 s), and reports it with X7 ([`complete_send`]). Lease grants and failovers
//! serialize on the decision row (`FOR UPDATE`): a grant needs `route = NEW`; an automatic
//! failover needs `lease_count = 0` and no live lease (re-checked under the lock, and by
//! the F3 trigger). Whichever commits first wins, so a never-leased decision fails over
//! with no possible duplicate (every later X6 answers `REVOKED`), and a leased one is never
//! failed over automatically: if its lease lapsed without X7 (the service crashed mid-send)
//! only the operator's break-glass reroute under `FORCED` sends it again
//! ([`operator_reroute`], recorded `REROUTED_POSSIBLE_DUPLICATE`, Document 4 §9.11). A
//! completed decision is terminal.
//!
//! | Variable | Meaning |
//! |---|---|
//! | `CF_ACCOUNT_FALLBACK_KEY_FILE` | file holding the 32-byte Account fallback key (base64); self-tested at startup. Unset: no CRITICAL message is decided `NEW` (`UNPUBLISHABLE`) |
//! | `CF_NOTIFICATIONS_SEALING_KEY` | the service's public HPKE key (codes are sealed to it); unset: code messages stay `OLD` |
//! | `CF_NOTIFICATIONS_BASE_URL` (+ signing identity) | I1 target of the relay; unset: rows wait and fail over after 60 s |
//!
//! Nothing here logs codes, addresses or content.

use std::{collections::HashMap, path::Path, sync::OnceLock, time::Duration as StdDuration};

use base64::Engine;
use cf_notification_client::{EnvelopeBuilder, Producer, RouteRef, SealedPlaintext, SealingKey};
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    XChaCha20Poly1305, XNonce,
};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DbBackend, DbErr, FromQueryResult, Statement,
    TransactionTrait,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::{
    config::email::valid_address,
    error::{ApiError, ApiResult},
    services::{
        email_delivery::{self, category, Enqueued, NewEmail},
        notification_fence::{
            self as fence, CriticalReroute, DecisionReason, LeaseGrant, MeetingCategory, Route,
            RoutingMode,
        },
        notification_owner::email_principal_ref,
        notification_publications::{IngestResult, NotificationEventSink, PublishFailure},
        ops_metrics::{inc, OPS_METRICS as OM},
    },
    templates::email::RequestDetails,
};

/// A never-leased `NEW` decision fails over to the legacy path after this long in `AUTO`
/// (Document 4 §10.4: no `ACCEPTED` from I1, or no X6 lease, within 60 s).
pub const AUTO_FAILOVER_BUDGET: Duration = Duration::seconds(60);
/// How long a CRITICAL decision is kept (then 30 more days before the purge may delete it).
pub const DECISION_LIFETIME: Duration = Duration::days(7);
/// How long a settled outbox row is kept after `settled_at`.
pub const RETENTION: Duration = Duration::days(30);
/// Longest X6 lease (contract `leaseSeconds` 5–30; the service's provider deadline is 20 s).
pub const MAX_LEASE_SECONDS: u32 = 30;
pub const MIN_LEASE_SECONDS: u32 = 5;
/// Most events in one I1 batch and most decisions in one X6 / X7 request.
pub const MAX_BATCH: usize = 100;

/// The catalogue event types of the CRITICAL lane (producer `cfmeet.meeting-service`).
pub mod event_type {
    pub const VERIFICATION_CODE: &str = "account.verification-code.requested";
    pub const PASSWORD_RESET_CODE: &str = "account.password-reset-code.requested";
    pub const DEVICE_STEP_UP_CODE: &str = "account.device-step-up-code.requested";
    pub const PASSWORD_CHANGED: &str = "account.security.password-changed";
    pub const DEVICE_LINKED: &str = "account.security.device-linked";
    pub const SIGN_IN_CODE: &str = "account.sign-in-code.requested";
    pub const EMAIL_CHANGE_CODE: &str = "account.email-change-code.requested";
    pub const STEP_UP_CODE: &str = "account.step-up-code.requested";
    pub const WELCOME: &str = "account.welcome.requested";
    pub const NEW_SIGN_IN: &str = "account.security.new-sign-in";
    pub const EMAIL_IN_USE: &str = "account.security.email-in-use";
    pub const EMAIL_CHANGED: &str = "account.security.email-changed";
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Micros, true)
}

fn stmt(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

// ------------------------------------------------------------------------ fallback key

/// The legacy email of one CRITICAL message, exactly as `LEGACY_ONLY` would have recorded
/// it. Sealed into `fallback_sealed`; never logged (`Debug` is redacted), zeroized on drop.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FallbackEmail {
    pub category: String,
    pub idempotency_key: String,
    pub to: String,
    pub to_name: Option<String>,
    pub subject: String,
    pub html: String,
    pub text: String,
    #[zeroize(skip)]
    pub expires_at: Option<DateTime<Utc>>,
}

impl std::fmt::Debug for FallbackEmail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FallbackEmail")
            .field("category", &self.category)
            .finish_non_exhaustive()
    }
}

/// The ledger categories a fallback email may carry (`NOTIFICATION`: device and account
/// notices; `AUTH_WELCOME`: the welcome email; `OPERATIONS_ACCEPTANCE`: the fallback drill).
fn ledger_category(value: &str) -> Option<&'static str> {
    [
        category::AUTH_VERIFICATION,
        category::AUTH_WELCOME,
        category::AUTH_PASSWORD_RESET,
        category::SECURITY_DEVICE_STEP_UP,
        category::SECURITY_PASSWORD_CHANGED,
        category::NOTIFICATION,
        category::OPERATIONS_ACCEPTANCE,
    ]
    .into_iter()
    .find(|c| *c == value)
}

impl FallbackEmail {
    pub fn from_new(email: &NewEmail) -> Option<Self> {
        Some(Self {
            category: email.category.to_string(),
            idempotency_key: email.idempotency_key.clone()?,
            to: email.to.clone(),
            to_name: email.to_name.clone(),
            subject: email.subject.clone(),
            html: email.html.clone(),
            text: email.text.clone(),
            expires_at: email.expires_at,
        })
    }

    pub fn to_new(&self) -> Option<NewEmail> {
        Some(NewEmail {
            category: ledger_category(&self.category)?,
            idempotency_key: Some(self.idempotency_key.clone()),
            to: self.to.clone(),
            to_name: self.to_name.clone(),
            subject: self.subject.clone(),
            html: self.html.clone(),
            text: self.text.clone(),
            expires_at: self.expires_at,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FallbackError {
    #[error("the Account fallback key is invalid (expected 32 bytes, base64)")]
    InvalidKey,
    #[error("the fallback email cannot be sealed")]
    Seal,
    #[error("the fallback email does not open (another key, another decision, or tampered)")]
    Open,
}

const FALLBACK_MAGIC: &[u8; 4] = b"CFX1";
const FALLBACK_AAD: &[u8] = b"cfmeet.account.critical-fallback.v1|";

/// The Account fallback key (XChaCha20-Poly1305, 256-bit key; random 192-bit nonces).
/// Document 4 §9.2 names AES-256-GCM; the repository reserves `aes-gcm` to the Contact
/// Vault (service boundary guard), and the property needed — an AEAD only Meeting can
/// open — is the same. Only Meeting holds it: the Notification Service
/// can never open `fallback_sealed`, and Meeting can never open the service's sealed part.
/// A blob is `CFX1 ‖ key id (8) ‖ nonce (24) ‖ ciphertext`, its AAD binds it to the
/// decision id, so a blob copied to another decision does not open.
pub struct FallbackKey {
    key: Zeroizing<[u8; 32]>,
    kid: [u8; 8],
}

impl std::fmt::Debug for FallbackKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FallbackKey")
            .field("kid", &hex::encode(self.kid))
            .finish_non_exhaustive()
    }
}

impl FallbackKey {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, FallbackError> {
        let key: [u8; 32] = bytes.try_into().map_err(|_| FallbackError::InvalidKey)?;
        let digest = Sha256::new()
            .chain_update(b"cfmeet.account.critical-fallback.kid|")
            .chain_update(key)
            .finalize();
        let mut kid = [0u8; 8];
        kid.copy_from_slice(&digest[..8]);
        Ok(Self {
            key: Zeroizing::new(key),
            kid,
        })
    }

    /// A base64 (standard or URL-safe, padded or not) 32-byte key; surrounding whitespace
    /// is ignored.
    pub fn parse(text: &str) -> Result<Self, FallbackError> {
        let text = text.trim().trim_end_matches('=');
        let bytes = Zeroizing::new(
            base64::engine::general_purpose::STANDARD_NO_PAD
                .decode(text)
                .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(text))
                .map_err(|_| FallbackError::InvalidKey)?,
        );
        Self::from_bytes(&bytes)
    }

    pub fn from_file(path: &Path) -> Result<Self, FallbackError> {
        let text =
            Zeroizing::new(std::fs::read_to_string(path).map_err(|_| FallbackError::InvalidKey)?);
        Self::parse(&text)
    }

    /// A fresh random key (tests and tooling).
    pub fn generate() -> Self {
        let bytes = Zeroizing::new(rand::random::<[u8; 32]>());
        Self::from_bytes(&bytes[..]).expect("32 bytes")
    }

    /// The key as base64 (tooling that provisions the key file).
    pub fn to_base64(&self) -> Zeroizing<String> {
        Zeroizing::new(base64::engine::general_purpose::STANDARD.encode(&self.key[..]))
    }

    pub fn key_id(&self) -> String {
        hex::encode(self.kid)
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new_from_slice(&self.key[..]).expect("a 32-byte key")
    }

    fn aad(decision_id: Uuid) -> Vec<u8> {
        let mut aad = FALLBACK_AAD.to_vec();
        aad.extend_from_slice(decision_id.as_bytes());
        aad
    }

    pub fn seal(&self, decision_id: Uuid, email: &FallbackEmail) -> Result<Vec<u8>, FallbackError> {
        let plaintext = Zeroizing::new(serde_json::to_vec(email).map_err(|_| FallbackError::Seal)?);
        let nonce_bytes: [u8; 24] = rand::random();
        let aad = Self::aad(decision_id);
        let ct = self
            .cipher()
            .encrypt(
                XNonce::from_slice(&nonce_bytes),
                Payload {
                    msg: &plaintext,
                    aad: &aad,
                },
            )
            .map_err(|_| FallbackError::Seal)?;
        let mut out = Vec::with_capacity(4 + 8 + 24 + ct.len());
        out.extend_from_slice(FALLBACK_MAGIC);
        out.extend_from_slice(&self.kid);
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ct);
        Ok(out)
    }

    pub fn open(&self, decision_id: Uuid, sealed: &[u8]) -> Result<FallbackEmail, FallbackError> {
        if sealed.len() < 4 + 8 + 24 + 16
            || &sealed[..4] != FALLBACK_MAGIC
            || sealed[4..12] != self.kid
        {
            return Err(FallbackError::Open);
        }
        let aad = Self::aad(decision_id);
        let plaintext = Zeroizing::new(
            self.cipher()
                .decrypt(
                    XNonce::from_slice(&sealed[12..36]),
                    Payload {
                        msg: &sealed[36..],
                        aad: &aad,
                    },
                )
                .map_err(|_| FallbackError::Open)?,
        );
        serde_json::from_slice(&plaintext).map_err(|_| FallbackError::Open)
    }

    /// Readiness invariant 4 (§10.4): a probe seals and opens, and a tampered or rebound
    /// blob does not open.
    pub fn self_test(&self) -> Result<(), FallbackError> {
        let probe = FallbackEmail {
            category: category::OPERATIONS_ACCEPTANCE.into(),
            idempotency_key: "fallback-self-test".into(),
            to: "self-test@cfmeet.invalid".into(),
            to_name: None,
            subject: "self-test".into(),
            html: "<p>self-test</p>".into(),
            text: "self-test".into(),
            expires_at: None,
        };
        let id = Uuid::new_v4();
        let mut sealed = self.seal(id, &probe)?;
        if self.open(id, &sealed)? != probe || self.open(Uuid::new_v4(), &sealed).is_ok() {
            return Err(FallbackError::Open);
        }
        let last = sealed.len() - 1;
        sealed[last] ^= 1;
        if self.open(id, &sealed).is_ok() {
            return Err(FallbackError::Open);
        }
        Ok(())
    }
}

// ------------------------------------------------------------------------------- keys

/// The keys of the CRITICAL lane.
#[derive(Debug, Default)]
pub struct CriticalKeys {
    /// The service's public HPKE key (codes are sealed to it).
    pub sealing: Option<SealingKey>,
    /// The Account fallback key (`fallback_sealed`).
    pub fallback: Option<FallbackKey>,
}

static INSTALLED: OnceLock<CriticalKeys> = OnceLock::new();

/// Installs the process's CRITICAL-lane keys (once; `false` when already installed).
pub fn install(keys: CriticalKeys) -> bool {
    INSTALLED.set(keys).is_ok()
}

/// The installed keys (`None`: never installed, as with no key configured).
pub fn installed() -> Option<&'static CriticalKeys> {
    INSTALLED.get()
}

/// Whether the installed fallback key passed its self-test (reported by `/health` as
/// `criticalFallbackKey`; release tooling refuses to route a CRITICAL category `NEW`
/// without it on every replica).
pub fn fallback_ready() -> bool {
    installed().is_some_and(|k| k.fallback.is_some())
}

/// The keys from the environment: `CF_ACCOUNT_FALLBACK_KEY_FILE` (self-tested; `Err`:
/// configured but unusable — startup fails closed) and the already-parsed sealing key.
pub fn keys_from_env(sealing: Option<SealingKey>) -> Result<CriticalKeys, String> {
    let fallback = match std::env::var("CF_ACCOUNT_FALLBACK_KEY_FILE")
        .ok()
        .filter(|v| !v.trim().is_empty())
    {
        None => None,
        Some(path) => {
            let key = FallbackKey::from_file(Path::new(path.trim()))
                .map_err(|e| format!("CF_ACCOUNT_FALLBACK_KEY_FILE: {e}"))?;
            key.self_test()
                .map_err(|e| format!("CF_ACCOUNT_FALLBACK_KEY_FILE self-test failed: {e}"))?;
            Some(key)
        }
    };
    Ok(CriticalKeys { sealing, fallback })
}

// ------------------------------------------------------------------------------ event

/// The catalogue event of one CRITICAL message (ids and bounded facts only; the code, the
/// magic-link or undo token, an address, a masked address and a request's client label and
/// network are sealed to the service key and never stored in clear).
#[derive(Clone)]
pub struct CriticalEvent {
    pub category: MeetingCategory,
    /// `None`: the message has no catalogue event (it is routed, and always decided
    /// `OLD` / `UNPUBLISHABLE` outside `LEGACY_ONLY`).
    pub event_type: Option<&'static str>,
    /// The account (`Uuid::nil()` for a sign-in code, whose address may have none).
    pub account_id: Uuid,
    /// Whether the envelope names the account as its `ACCOUNT` subject (never for a
    /// sign-in code: the address may have no account, and the event must not reveal one).
    pub subject: bool,
    /// `None`: the recipient is the account (`cfmeet-user:<account>`, address via X1).
    /// `Some`: the recipient is one email principal ([`email_principal_ref`]) and this
    /// address is sealed (a sign-in code, or the new address of an email change).
    pub address: Option<Zeroizing<String>>,
    /// Exactly the catalogue's data keys.
    pub data: Vec<(&'static str, Value)>,
    /// The one-time code (code events only).
    pub code: Option<Zeroizing<String>>,
    /// The magic-link token (sign-in codes) or the undo token (email-changed, `OLD`).
    pub token: Option<Zeroizing<String>>,
    /// The masked new address (email-changed, `OLD`).
    pub masked_address: Option<Zeroizing<String>>,
    /// The request's client label and masked network (new sign-in alert).
    pub request_client: Option<Zeroizing<String>>,
    pub request_network: Option<Zeroizing<String>>,
}

impl std::fmt::Debug for CriticalEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CriticalEvent")
            .field("category", &self.category)
            .field("event_type", &self.event_type)
            .finish_non_exhaustive()
    }
}

impl CriticalEvent {
    /// A CRITICAL message the catalogue cannot carry. It is still routed — the guard
    /// trigger requires a decision once its category leaves `LEGACY_ONLY` — and always
    /// stays on the legacy path (`UNPUBLISHABLE`). Since contract 1.1.0 every enqueue point
    /// has a catalogue event; this remains for a sign-in code the catalogue cannot carry
    /// exactly as its legacy email shows it ([`Self::sign_in_code`]).
    pub fn not_in_catalogue(category: MeetingCategory) -> Self {
        Self {
            category,
            event_type: None,
            account_id: Uuid::nil(),
            subject: false,
            address: None,
            data: Vec::new(),
            code: None,
            token: None,
            masked_address: None,
            request_client: None,
            request_network: None,
        }
    }

    /// An event of the account: its `cfmeet-user` recipient and `ACCOUNT` subject.
    fn of_account(
        category: MeetingCategory,
        event_type: &'static str,
        account_id: Uuid,
        data: Vec<(&'static str, Value)>,
        code: Option<&str>,
    ) -> Self {
        Self {
            category,
            event_type: Some(event_type),
            account_id,
            subject: true,
            address: None,
            data,
            code: code.map(|c| Zeroizing::new(c.to_string())),
            token: None,
            masked_address: None,
            request_client: None,
            request_network: None,
        }
    }

    /// Passwordless sign-in / sign-up code (PR #97) to `address`, which may have no account
    /// yet: one email principal, no subject; the code, the magic-link token and the address
    /// are sealed, and the request facts the legacy email shows (`details`) travel as data.
    /// A message the catalogue cannot carry exactly as the legacy email shows it (no magic
    /// link, request facts outside the contract's bounds) stays on the legacy path
    /// (`UNPUBLISHABLE`), so the service never renders a different email.
    pub fn sign_in_code(
        address: &str,
        code: &str,
        link_token: Option<&str>,
        ttl_minutes: i64,
        details: &RequestDetails,
    ) -> Self {
        let requested_at =
            chrono::NaiveDateTime::parse_from_str(&details.when_utc, "%Y-%m-%d %H:%M")
                .ok()
                .map(|t| t.and_utc().to_rfc3339_opts(SecondsFormat::Secs, true));
        let client_ok = details.client.as_deref().is_none_or(|c| {
            (1..=80).contains(&c.chars().count()) && !c.chars().any(char::is_control)
        });
        let network_ok = details.address.as_deref().is_none_or(masked_network);
        let (Some(token), Some(requested_at), true, true) =
            (link_token, requested_at, client_ok, network_ok)
        else {
            return Self::not_in_catalogue(MeetingCategory::AuthVerification);
        };
        let mut data = vec![
            ("requestedAt", json!(requested_at)),
            ("ttlMinutes", json!(ttl_minutes)),
        ];
        if let Some(client) = &details.client {
            data.push(("requestClient", json!(client)));
        }
        if let Some(network) = &details.address {
            data.push(("requestNetwork", json!(network)));
        }
        Self {
            category: MeetingCategory::AuthVerification,
            event_type: Some(event_type::SIGN_IN_CODE),
            account_id: Uuid::nil(),
            subject: false,
            address: Some(Zeroizing::new(address.to_string())),
            data,
            code: Some(Zeroizing::new(code.to_string())),
            token: Some(Zeroizing::new(token.to_string())),
            masked_address: None,
            request_client: None,
            request_network: None,
        }
    }

    /// The code proving ownership of the NEW address of an email change (PR #97): one
    /// email principal with the new address sealed, on behalf of the account (subject).
    pub fn email_change_code(
        account_id: Uuid,
        new_address: &str,
        code: &str,
        ttl_minutes: i64,
    ) -> Self {
        Self {
            address: Some(Zeroizing::new(new_address.to_string())),
            ..Self::of_account(
                MeetingCategory::AuthVerification,
                event_type::EMAIL_CHANGE_CODE,
                account_id,
                vec![("ttlMinutes", json!(ttl_minutes))],
                Some(code),
            )
        }
    }

    /// The action step-up code (PR #97) to the account's current address; `purpose` is the
    /// closed `StepUpPurpose` value (`DEACTIVATE_ACCOUNT`, `CHANGE_EMAIL`).
    pub fn step_up_code(
        account_id: Uuid,
        purpose: &'static str,
        code: &str,
        ttl_minutes: i64,
    ) -> Self {
        Self::of_account(
            MeetingCategory::SecurityDeviceStepUp,
            event_type::STEP_UP_CODE,
            account_id,
            vec![
                ("purpose", json!(purpose)),
                ("ttlMinutes", json!(ttl_minutes)),
            ],
            Some(code),
        )
    }

    pub fn verification_code(account_id: Uuid, code: &str, ttl_minutes: i64) -> Self {
        Self::of_account(
            MeetingCategory::AuthVerification,
            event_type::VERIFICATION_CODE,
            account_id,
            vec![("ttlMinutes", json!(ttl_minutes))],
            Some(code),
        )
    }

    pub fn password_reset_code(account_id: Uuid, code: &str, ttl_minutes: i64) -> Self {
        Self::of_account(
            MeetingCategory::AuthPasswordReset,
            event_type::PASSWORD_RESET_CODE,
            account_id,
            vec![("ttlMinutes", json!(ttl_minutes))],
            Some(code),
        )
    }

    pub fn device_step_up_code(
        account_id: Uuid,
        account_device_id: Uuid,
        code: &str,
        ttl_minutes: i64,
    ) -> Self {
        Self::of_account(
            MeetingCategory::SecurityDeviceStepUp,
            event_type::DEVICE_STEP_UP_CODE,
            account_id,
            vec![
                ("accountDeviceId", json!(account_device_id.to_string())),
                ("ttlMinutes", json!(ttl_minutes)),
            ],
            Some(code),
        )
    }

    pub fn password_changed(account_id: Uuid, changed_at: DateTime<Utc>) -> Self {
        Self::of_account(
            MeetingCategory::SecurityPasswordChanged,
            event_type::PASSWORD_CHANGED,
            account_id,
            vec![("changedAt", json!(rfc3339(changed_at)))],
            None,
        )
    }

    pub fn device_linked(account_id: Uuid, account_device_id: Uuid, link_id: Uuid) -> Self {
        Self::of_account(
            MeetingCategory::SecurityDeviceLinked,
            event_type::DEVICE_LINKED,
            account_id,
            vec![
                ("accountDeviceId", json!(account_device_id.to_string())),
                ("linkId", json!(link_id.to_string())),
            ],
            None,
        )
    }

    /// An account notice (`ACCOUNT_NOTICES`) to exactly `address`: one email principal
    /// with the address sealed (the legacy recipient, which a later change of the
    /// account's address cannot redirect), on behalf of `account` (the ACCOUNT subject:
    /// X1 authorizes the send and names the greeting) when there is one.
    fn notice(
        event_type: &'static str,
        account: Option<Uuid>,
        address: &str,
        data: Vec<(&'static str, Value)>,
    ) -> Self {
        Self {
            category: MeetingCategory::AccountNotices,
            event_type: Some(event_type),
            account_id: account.unwrap_or_else(Uuid::nil),
            subject: account.is_some(),
            address: Some(Zeroizing::new(address.to_string())),
            data,
            code: None,
            token: None,
            masked_address: None,
            request_client: None,
            request_network: None,
        }
    }

    /// The welcome email (`AUTH_WELCOME`, once per account).
    pub fn welcome(account_id: Uuid, address: &str) -> Self {
        Self::notice(event_type::WELCOME, Some(account_id), address, Vec::new())
    }

    /// The new sign-in alert: `requestedAt` as data; the client label and the masked
    /// network the email shows are sealed (never data). Request facts the catalogue cannot
    /// carry exactly as the legacy email shows them stay on the legacy path
    /// (`UNPUBLISHABLE`).
    pub fn new_sign_in(account_id: Uuid, address: &str, details: &RequestDetails) -> Self {
        let requested_at =
            chrono::NaiveDateTime::parse_from_str(&details.when_utc, "%Y-%m-%d %H:%M")
                .ok()
                .map(|t| t.and_utc().to_rfc3339_opts(SecondsFormat::Secs, true));
        let client_ok = details
            .client
            .as_deref()
            .is_none_or(cf_notification_client::seal::request_client_ok);
        let network_ok = details.address.as_deref().is_none_or(masked_network);
        let (Some(requested_at), true, true) = (requested_at, client_ok, network_ok) else {
            return Self::not_in_catalogue(MeetingCategory::AccountNotices);
        };
        Self {
            request_client: details.client.as_deref().map(|c| Zeroizing::new(c.into())),
            request_network: details.address.as_deref().map(|n| Zeroizing::new(n.into())),
            ..Self::notice(
                event_type::NEW_SIGN_IN,
                Some(account_id),
                address,
                vec![("requestedAt", json!(requested_at))],
            )
        }
    }

    /// The email-change attempt notice to an address that already has an account: no
    /// subject (the event never names the account).
    pub fn email_in_use(address: &str) -> Self {
        Self::notice(event_type::EMAIL_IN_USE, None, address, Vec::new())
    }

    /// The notice to the PREVIOUS address of an email change: the masked new address and,
    /// for a self-service change, the undo token, both sealed (the service rebuilds the
    /// undo link from `NOTIFICATIONS_APP_URL`). A masked address that is not the
    /// catalogue's form stays on the legacy path.
    pub fn email_changed_old(
        account_id: Uuid,
        old_address: &str,
        new_address_masked: &str,
        undo_token: Option<&str>,
        changed_at: DateTime<Utc>,
    ) -> Self {
        if !cf_notification_client::seal::masked_address_ok(new_address_masked) {
            return Self::not_in_catalogue(MeetingCategory::AccountNotices);
        }
        Self {
            masked_address: Some(Zeroizing::new(new_address_masked.to_string())),
            token: undo_token.map(|t| Zeroizing::new(t.to_string())),
            ..Self::notice(
                event_type::EMAIL_CHANGED,
                Some(account_id),
                old_address,
                vec![
                    ("changedAt", json!(minute(changed_at))),
                    ("side", json!("OLD")),
                ],
            )
        }
    }

    /// The notice to the NEW address of an email change.
    pub fn email_changed_new(
        account_id: Uuid,
        new_address: &str,
        changed_at: DateTime<Utc>,
    ) -> Self {
        Self::notice(
            event_type::EMAIL_CHANGED,
            Some(account_id),
            new_address,
            vec![
                ("changedAt", json!(minute(changed_at))),
                ("side", json!("NEW")),
            ],
        )
    }

    /// The sealed part of this event (`None`: it carries no secret).
    fn secret(&self) -> Option<SealedPlaintext> {
        if self.code.is_none()
            && self.token.is_none()
            && self.address.is_none()
            && self.masked_address.is_none()
            && self.request_client.is_none()
            && self.request_network.is_none()
        {
            return None;
        }
        let mut p = SealedPlaintext::default();
        if let Some(code) = &self.code {
            p = p.with_code(code.as_str());
        }
        if let Some(token) = &self.token {
            p = p.with_token(token.as_str());
        }
        if let Some(address) = &self.address {
            p = p.with_address(address.as_str());
        }
        if let Some(masked) = &self.masked_address {
            p = p.with_masked_address(masked.as_str());
        }
        if let Some(client) = &self.request_client {
            p = p.with_request_client(client.as_str());
        }
        if let Some(network) = &self.request_network {
            p = p.with_request_network(network.as_str());
        }
        Some(p)
    }
}

/// A masked network prefix as `passwordless::mask_address` writes it (`102.89.x.x`,
/// `2001:db8:x:x`; the contract's `requestNetwork`), never a full address.
fn masked_network(value: &str) -> bool {
    (3..=64).contains(&value.len())
        && value.contains('x')
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() || matches!(b, b'.' | b':' | b'x'))
}

/// `changedAt` of an email-changed notice: the minute the legacy email shows
/// (`%Y-%m-%d %H:%M`, UTC), as RFC 3339.
fn minute(at: DateTime<Utc>) -> String {
    format!("{}:00Z", at.format("%Y-%m-%dT%H:%M"))
}

/// The envelope event id of an outbox row.
pub fn event_id(row_id: Uuid) -> String {
    format!("evt_{}", row_id.simple())
}

/// Builds and validates the envelope of `event` (`Err`: it cannot be published — no
/// sealing key for a code, or an envelope the catalogue refuses).
pub fn build_envelope(
    event: &CriticalEvent,
    key: &str,
    row_id: Uuid,
    route: RouteRef,
    sealing: Option<&SealingKey>,
    expires_at: DateTime<Utc>,
    occurred_at: DateTime<Utc>,
) -> Result<Value, DecisionReason> {
    let event_type = event.event_type.ok_or(DecisionReason::Unpublishable)?;
    let mut builder = EnvelopeBuilder::new(
        Producer::MeetingService,
        event_type,
        &rfc3339(occurred_at),
        route,
    )
    .map_err(|_| DecisionReason::Unpublishable)?
    .event_id(event_id(row_id))
    .correlation_id(format!("corr_{}", row_id.simple()))
    .dedupe_key(key)
    .idempotency_key(key)
    .expires_at(rfc3339(expires_at));
    if event.subject {
        builder = builder.subject(event.account_id, None);
    }
    builder = match &event.address {
        Some(address) => builder.recipient(email_principal_ref(address)),
        None => builder.recipient(format!("cfmeet-user:{}", event.account_id)),
    };
    for (k, v) in &event.data {
        builder = builder.data(k, v.clone());
    }
    let built = match (event.secret(), sealing) {
        (Some(secret), Some(sealing)) => builder.secret(secret).build_sealed(sealing),
        (Some(_), None) => return Err(DecisionReason::Unpublishable),
        (None, _) => builder.build(),
    };
    built
        .map_err(|_| DecisionReason::Unpublishable)
        .and_then(|e| serde_json::to_value(e).map_err(|_| DecisionReason::Unpublishable))
}

// ---------------------------------------------------------------------------- enqueue

/// The legacy enqueue's own refusals, checked before anything is routed so a fenced
/// category answers exactly as `LEGACY_ONLY` would (suppression, which refuses only the
/// welcome email's suppressible `AUTH_WELCOME` among the CRITICAL messages, is checked in
/// [`enqueue`]).
fn legacy_precheck(email: &NewEmail) -> ApiResult<String> {
    if !valid_address(email.to.trim()) {
        return Err(ApiError::BadRequest(
            "Invalid recipient email address".into(),
        ));
    }
    if email
        .subject
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .is_empty()
    {
        return Err(ApiError::InternalError("Email subject is empty".into()));
    }
    email
        .idempotency_key
        .clone()
        .filter(|k| !k.is_empty())
        .ok_or_else(|| ApiError::InternalError("a CRITICAL email needs its idempotency key".into()))
}

/// Records one CRITICAL message (see the module documentation). `conn` may be the pool or
/// the caller's transaction (a security notice commits with the change it reports).
/// Returns the legacy ledger row (`OLD`) or the outbox row (`NEW`); `created = false` when
/// the key was already decided (sticky: nothing new is written).
pub async fn enqueue<C>(
    conn: &C,
    keys: Option<&CriticalKeys>,
    email: NewEmail,
    event: CriticalEvent,
) -> ApiResult<Enqueued>
where
    C: ConnectionTrait + TransactionTrait,
{
    match fence::current_mode(conn, event.category).await? {
        None | Some(RoutingMode::LegacyOnly) => return email_delivery::enqueue(conn, email).await,
        Some(_) => {}
    }
    let key = legacy_precheck(&email)?;
    if !email_delivery::UNSUPPRESSIBLE_CATEGORIES.contains(&email.category)
        && email_delivery::recipient_suppressed(
            conn,
            &email_delivery::recipient_hash(email.to.trim()),
        )
        .await?
    {
        inc(&OM.email_suppressed);
        return Err(ApiError::Conflict(
            email_delivery::RECIPIENT_SUPPRESSED.into(),
        ));
    }
    let now = Utc::now();
    let expires_at = email.expires_at.unwrap_or(now + Duration::days(2));
    let sealing = keys.and_then(|k| k.sealing.as_ref());
    let fallback = keys.and_then(|k| k.fallback.as_ref());
    let worst = RouteRef {
        decision_id: Uuid::max(),
        routing_version: i32::MAX,
        protocol: fence::ROUTING_PROTOCOL,
    };
    let publishable =
        build_envelope(&event, &key, Uuid::max(), worst, sealing, expires_at, now).map(|_| ());
    let capable_new = match (fallback, publishable) {
        (Some(_), Ok(())) => Ok(()),
        _ => Err(DecisionReason::Unpublishable),
    };

    let txn = conn.begin().await?;
    let decision = fence::route_message_capable(
        &txn,
        &key,
        event.category,
        now + DECISION_LIFETIME,
        capable_new,
    )
    .await?;
    let result = match (decision.route, decision.sticky) {
        // Old path (fresh or followed): the legacy row, idempotent on the key.
        (Route::Old, sticky) => {
            let enqueued = email_delivery::enqueue(&txn, email).await?;
            if !sticky {
                if decision.shadow {
                    if publishable.is_ok() {
                        insert_row(
                            &txn,
                            &event,
                            &key,
                            &decision,
                            sealing,
                            Vec::new(),
                            true,
                            expires_at,
                            now,
                        )
                        .await?;
                        inc(&OM.critical_shadowed);
                    } else {
                        inc(&OM.critical_unpublishable);
                    }
                } else if capable_new.is_err() {
                    inc(&OM.critical_unpublishable);
                }
            }
            enqueued
        }
        (Route::New, true) => {
            let row = txn
                .query_one(stmt(
                    "SELECT id FROM account_notification_outbox WHERE decision_id = $1",
                    vec![decision.decision_id.into()],
                ))
                .await?
                .map(|r| r.try_get::<Uuid>("", "id"))
                .transpose()?
                .unwrap_or(decision.decision_id);
            Enqueued {
                id: row,
                created: false,
            }
        }
        (Route::New, false) => {
            let fallback = fallback.ok_or_else(|| {
                ApiError::InternalError("NEW CRITICAL decision without a fallback key".into())
            })?;
            let sealed_email = FallbackEmail::from_new(&email)
                .ok_or_else(|| ApiError::InternalError("fallback email without a key".into()))?;
            let fallback_sealed = fallback
                .seal(decision.decision_id, &sealed_email)
                .map_err(|e| ApiError::InternalError(e.to_string()))?;
            let id = insert_row(
                &txn,
                &event,
                &key,
                &decision,
                sealing,
                fallback_sealed,
                false,
                expires_at,
                now,
            )
            .await?;
            inc(&OM.critical_enqueued_new);
            Enqueued { id, created: true }
        }
    };
    txn.commit().await?;
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
async fn insert_row<C: ConnectionTrait>(
    conn: &C,
    event: &CriticalEvent,
    key: &str,
    decision: &fence::RoutedMessage,
    sealing: Option<&SealingKey>,
    fallback_sealed: Vec<u8>,
    shadow: bool,
    expires_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> ApiResult<Uuid> {
    let id = Uuid::new_v4();
    let envelope = build_envelope(
        event,
        key,
        id,
        RouteRef {
            decision_id: decision.decision_id,
            routing_version: decision.routing_version,
            protocol: fence::ROUTING_PROTOCOL,
        },
        sealing,
        expires_at,
        now,
    )
    .map_err(|_| ApiError::InternalError("the CRITICAL envelope cannot be built".into()))?;
    conn.execute(stmt(
        "INSERT INTO account_notification_outbox
             (id, decision_id, event, fallback_sealed, state, shadow, next_attempt_at, created_at, expires_at)
         VALUES ($1, $2, $3, $4, 'PENDING', $5, $6, $6, $7)",
        vec![
            id.into(),
            decision.decision_id.into(),
            envelope.into(),
            fallback_sealed.into(),
            shadow.into(),
            now.into(),
            expires_at.into(),
        ],
    ))
    .await?;
    Ok(id)
}

// ---------------------------------------------------------------------------- failover

/// Outcome of one failover attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failover {
    /// Decision now `OLD` with this reason; the legacy email (the same code) is queued.
    Sent {
        reason: DecisionReason,
        delivery_id: Uuid,
    },
    /// A live lease: the service may be sending right now.
    WaitForLease,
    /// The lease rules refuse (leased without break-glass, completed, expired, `OFF`).
    Refused,
    /// No such CRITICAL decision.
    Unknown,
    /// `fallback_sealed` cannot be opened here (no / another key): nothing changed.
    FallbackUnavailable,
}

impl Failover {
    pub fn as_str(self) -> &'static str {
        match self {
            Failover::Sent { .. } => "REROUTED",
            Failover::WaitForLease => "WAIT_FOR_LEASE",
            Failover::Refused => "REFUSED",
            Failover::Unknown => "UNKNOWN",
            Failover::FallbackUnavailable => "FALLBACK_UNAVAILABLE",
        }
    }
}

/// §9.8 reroute of the CRITICAL decision keyed `key` on `txn`, writing the legacy email
/// from `fallback_sealed` in the same transaction. On `FallbackUnavailable` the caller must
/// roll back (the decision was already moved on `txn`).
async fn fail_over<C: ConnectionTrait>(
    txn: &C,
    key: &str,
    keys: Option<&CriticalKeys>,
    automatic: bool,
    now: DateTime<Utc>,
) -> Result<Failover, DbErr> {
    let (reason, decision_id) = match fence::reroute_critical(txn, key, automatic).await? {
        CriticalReroute::Rerouted {
            reason,
            decision_id,
        } => (reason, decision_id),
        CriticalReroute::WaitForLease => return Ok(Failover::WaitForLease),
        CriticalReroute::Refused => return Ok(Failover::Refused),
        CriticalReroute::NotCritical | CriticalReroute::NotFound => return Ok(Failover::Unknown),
    };
    let sealed: Vec<u8> = match txn
        .query_one(stmt(
            "SELECT fallback_sealed FROM account_notification_outbox WHERE decision_id = $1",
            vec![decision_id.into()],
        ))
        .await?
    {
        Some(r) => r.try_get("", "fallback_sealed")?,
        None => return Ok(Failover::FallbackUnavailable),
    };
    let Some(email) = keys
        .and_then(|k| k.fallback.as_ref())
        .and_then(|k| k.open(decision_id, &sealed).ok())
        .and_then(|e| e.to_new())
    else {
        inc(&OM.critical_fallback_unavailable);
        return Ok(Failover::FallbackUnavailable);
    };
    let enqueued = match email_delivery::enqueue(txn, email).await {
        Ok(e) => e,
        Err(ApiError::DatabaseError(e)) => return Err(e),
        Err(_) => {
            inc(&OM.critical_fallback_unavailable);
            return Ok(Failover::FallbackUnavailable);
        }
    };
    txn.execute(stmt(
        "UPDATE account_notification_outbox
            SET fallback_sealed = ''::bytea, event = event - 'sealed', settled_at = $2,
                last_error = $3
          WHERE decision_id = $1",
        vec![decision_id.into(), now.into(), reason.as_str().into()],
    ))
    .await?;
    inc(match reason {
        DecisionReason::AutoFailover => &OM.critical_auto_failover,
        DecisionReason::ReroutedPossibleDuplicate => &OM.critical_possible_duplicate,
        _ => &OM.critical_operator_reroutes,
    });
    tracing::warn!(%decision_id, reason = reason.as_str(),
        "CRITICAL notification rerouted to the legacy email path");
    Ok(Failover::Sent {
        reason,
        delivery_id: enqueued.id,
    })
}

/// One failover in its own transaction (committed only when the legacy email is queued).
async fn fail_over_txn(
    db: &DatabaseConnection,
    key: &str,
    keys: Option<&CriticalKeys>,
    automatic: bool,
    now: DateTime<Utc>,
) -> Result<Failover, DbErr> {
    let txn = db.begin().await?;
    let outcome = fail_over(&txn, key, keys, automatic, now).await?;
    if matches!(outcome, Failover::Sent { .. }) {
        txn.commit().await?;
    } else {
        txn.rollback().await?;
    }
    Ok(outcome)
}

/// The operator's reroute of one CRITICAL decision (§9.8 "manual"): a never-leased decision
/// is rerouted (`REROUTED_CRITICAL_LEASE`); a leased, lapsed, uncompleted one only under
/// `critical_fallback = FORCED` (break-glass, `REROUTED_POSSIBLE_DUPLICATE`, counted).
pub async fn operator_reroute(
    db: &DatabaseConnection,
    keys: Option<&CriticalKeys>,
    decision_id: Uuid,
    now: DateTime<Utc>,
) -> Result<Failover, DbErr> {
    let Some(key) = critical_key(db, decision_id).await? else {
        return Ok(Failover::Unknown);
    };
    fail_over_txn(db, &key, keys, false, now).await
}

/// ADR 0068 `meetings.critical-lane.change`: the same operator reroute of one CRITICAL
/// decision, by an HQ staff member through the admin bridge (R3, approved by a second
/// person), at HQ's expected `decision_version`. Only a reroute that happened
/// (`Failover::Sent`) commits, and it is recorded in the same transaction (`meeting_admin_operations`, `REROUTED`); every other outcome
/// changes nothing and is answered as a refusal by the caller.
pub async fn operator_reroute_bridge(
    db: &DatabaseConnection,
    keys: Option<&CriticalKeys>,
    decision_id: Uuid,
    now: DateTime<Utc>,
    cmd: &crate::services::admin_operations::BridgeCommand,
) -> ApiResult<Failover> {
    use crate::services::admin_operations;
    let Some(key) = critical_key(db, decision_id).await? else {
        return Ok(Failover::Unknown);
    };
    let version_of = |sql: &'static str| stmt(sql, vec![decision_id.into()]);
    let txn = db.begin().await?;
    admin_operations::claim(&txn, cmd).await?;
    let current: i64 = txn
        .query_one(version_of(
            "SELECT decision_version::bigint AS v FROM notification_route_decisions               WHERE decision_id = $1 FOR UPDATE",
        ))
        .await?
        .map(|r| r.try_get::<i64>("", "v"))
        .transpose()?
        .ok_or_else(|| ApiError::NotFound("No such decision".into()))?;
    admin_operations::check_version(cmd, current)?;
    let outcome = fail_over(&txn, &key, keys, false, now).await?;
    if matches!(outcome, Failover::Sent { .. }) {
        let version: Option<i64> = txn
            .query_one(version_of(
                "SELECT decision_version::bigint AS v FROM notification_route_decisions                   WHERE decision_id = $1",
            ))
            .await?
            .map(|r| r.try_get::<i64>("", "v"))
            .transpose()?;
        admin_operations::record(
            &txn,
            cmd,
            admin_operations::subject::CRITICAL_DECISION,
            decision_id,
            "REROUTED",
            version,
        )
        .await?;
        txn.commit().await?;
    } else {
        txn.rollback().await?;
    }
    Ok(outcome)
}

async fn critical_key<C: ConnectionTrait>(
    conn: &C,
    decision_id: Uuid,
) -> Result<Option<String>, DbErr> {
    conn.query_one(stmt(
        "SELECT d.idempotency_key FROM notification_route_decisions d
           JOIN account_notification_outbox o ON o.decision_id = d.decision_id AND NOT o.shadow
          WHERE d.decision_id = $1",
        vec![decision_id.into()],
    ))
    .await?
    .map(|r| r.try_get::<String>("", "idempotency_key"))
    .transpose()
}

// ------------------------------------------------------------------------------ X6 / X7

/// One X6 result (`claimCriticalSendLeases`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseAnswer {
    pub decision_id: Uuid,
    /// `GRANTED`, `REVOKED`, `EXPIRED` or `UNKNOWN`.
    pub outcome: &'static str,
    pub lease_until: Option<DateTime<Utc>>,
}

/// One X7 result (`completeCriticalSends`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionAnswer {
    pub decision_id: Uuid,
    /// `COMPLETED`, `ALREADY_COMPLETED`, `NO_LIVE_LEASE` or `UNKNOWN`.
    pub outcome: &'static str,
}

#[derive(Debug, FromQueryResult)]
struct LaneRow {
    idempotency_key: String,
    category: String,
    route: String,
    lease_count: i32,
    completed: bool,
    lease_live: bool,
    state: Option<String>,
    shadow: Option<bool>,
    expired: Option<bool>,
}

/// The decision row locked (`FOR UPDATE OF d`) with its outbox row (read, not locked).
async fn lock_lane_row<C: ConnectionTrait>(
    conn: &C,
    decision_id: Uuid,
) -> Result<Option<LaneRow>, DbErr> {
    LaneRow::find_by_statement(stmt(
        "SELECT d.idempotency_key, d.category, d.route, d.lease_count,
                d.ns_completed_at IS NOT NULL AS completed,
                COALESCE(d.send_lease_until > clock_timestamp(), false) AS lease_live,
                o.state, o.shadow, o.expires_at <= clock_timestamp() AS expired
           FROM notification_route_decisions d
           LEFT JOIN account_notification_outbox o ON o.decision_id = d.decision_id
          WHERE d.decision_id = $1
          FOR UPDATE OF d",
        vec![decision_id.into()],
    ))
    .one(conn)
    .await
}

fn critical_lane(row: &LaneRow) -> bool {
    MeetingCategory::parse(&row.category).is_some_and(MeetingCategory::is_critical)
        && row.state.is_some()
        && row.shadow == Some(false)
}

/// X6 for one decision, in one row-locked transaction (§9.8 step 1):
/// * `GRANTED` — the route is still `NEW`, not completed, the message has not expired and
///   no lease is live: `send_lease_until = now + lease` (Meeting DB clock),
///   `lease_count + 1` (the acknowledgement is recorded too, if the relay had not yet);
/// * `REVOKED` — rerouted to the legacy path (or superseded / dead / already completed):
///   the service must never send it;
/// * `EXPIRED` — the code (message) expired: never sent by either path;
/// * `UNKNOWN` — no CRITICAL decision with this id, or another lease is live (never a
///   second concurrent lease: the service retries).
pub async fn claim_lease(
    db: &DatabaseConnection,
    decision_id: Uuid,
    lease: StdDuration,
) -> Result<LeaseAnswer, DbErr> {
    let answer = |outcome, lease_until| LeaseAnswer {
        decision_id,
        outcome,
        lease_until,
    };
    let txn = db.begin().await?;
    let Some(row) = lock_lane_row(&txn, decision_id).await? else {
        txn.rollback().await?;
        return Ok(answer("UNKNOWN", None));
    };
    let refused = if !critical_lane(&row) {
        Some("UNKNOWN")
    } else if row.route != "NEW"
        || row.completed
        || matches!(row.state.as_deref(), Some("SUPERSEDED" | "DEAD"))
    {
        Some("REVOKED")
    } else if row.expired == Some(true) || row.state.as_deref() == Some("EXPIRED") {
        Some("EXPIRED")
    } else if row.lease_live {
        inc(&OM.critical_lease_busy);
        Some("UNKNOWN")
    } else {
        None
    };
    if let Some(outcome) = refused {
        txn.rollback().await?;
        if outcome == "REVOKED" {
            inc(&OM.critical_lease_revoked);
        }
        return Ok(answer(outcome, None));
    }
    let lease = lease.min(StdDuration::from_secs(u64::from(MAX_LEASE_SECONDS)));
    match fence::grant_critical_lease(&txn, &row.idempotency_key, lease).await? {
        LeaseGrant::Granted { until, .. } => {
            txn.execute(stmt(
                "UPDATE notification_route_decisions SET ns_acked_at = clock_timestamp()
                  WHERE decision_id = $1 AND route = 'NEW' AND ns_acked_at IS NULL",
                vec![decision_id.into()],
            ))
            .await?;
            txn.commit().await?;
            inc(&OM.critical_leases_granted);
            Ok(answer("GRANTED", Some(until)))
        }
        LeaseGrant::Revoked | LeaseGrant::Completed => {
            txn.rollback().await?;
            Ok(answer("REVOKED", None))
        }
        LeaseGrant::Busy | LeaseGrant::NotCritical | LeaseGrant::NotFound => {
            txn.rollback().await?;
            Ok(answer("UNKNOWN", None))
        }
    }
}

/// X7 for one decision (§9.8 step 3): a `NEW` decision sent under a granted lease becomes
/// terminal (`COMPLETED`; also when the lease lapsed after the provider accepted — the
/// send happened under it) and its outbox row drops its secrets. A decision rerouted to
/// the legacy path, or never leased, answers `NO_LIVE_LEASE` (and a completion reported
/// after a reroute is counted: it is the recorded possible duplicate of §9.11).
pub async fn complete_send(
    db: &DatabaseConnection,
    decision_id: Uuid,
) -> Result<CompletionAnswer, DbErr> {
    let answer = |outcome| CompletionAnswer {
        decision_id,
        outcome,
    };
    let txn = db.begin().await?;
    let Some(row) = lock_lane_row(&txn, decision_id).await? else {
        txn.rollback().await?;
        return Ok(answer("UNKNOWN"));
    };
    if !critical_lane(&row) {
        txn.rollback().await?;
        return Ok(answer("UNKNOWN"));
    }
    if row.completed {
        txn.rollback().await?;
        return Ok(answer("ALREADY_COMPLETED"));
    }
    if row.route != "NEW" || row.lease_count == 0 {
        txn.rollback().await?;
        if row.lease_count > 0 {
            inc(&OM.critical_completion_after_reroute);
        }
        return Ok(answer("NO_LIVE_LEASE"));
    }
    match fence::complete_critical_send(&txn, &row.idempotency_key).await? {
        fence::Completion::Completed => {
            txn.execute(stmt(
                "UPDATE account_notification_outbox
                    SET fallback_sealed = ''::bytea, event = event - 'sealed',
                        state = CASE WHEN state = 'PENDING' THEN 'ACKED' ELSE state END,
                        settled_at = COALESCE(settled_at, clock_timestamp())
                  WHERE decision_id = $1",
                vec![decision_id.into()],
            ))
            .await?;
            txn.commit().await?;
            inc(&OM.critical_completions);
            Ok(answer("COMPLETED"))
        }
        fence::Completion::AlreadyCompleted => {
            txn.rollback().await?;
            Ok(answer("ALREADY_COMPLETED"))
        }
        fence::Completion::Revoked | fence::Completion::NotLeased => {
            txn.rollback().await?;
            Ok(answer("NO_LIVE_LEASE"))
        }
        fence::Completion::NotFound => {
            txn.rollback().await?;
            Ok(answer("UNKNOWN"))
        }
    }
}

// ------------------------------------------------------------------------------- relay

/// One relay pass.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CriticalRelayReport {
    pub acked: usize,
    pub retried: usize,
    pub dead: usize,
    /// Rerouted to the legacy path by AUTO / FORCED (or after an I1 refusal).
    pub failed_over: usize,
    pub expired: u64,
    pub not_configured: usize,
    pub fallback_unavailable: usize,
}

fn retry_delay(attempts: i32) -> Duration {
    // Short: a CRITICAL message has 60 s before it fails over.
    Duration::seconds((1i64 << attempts.clamp(0, 4)).min(10))
}

fn reason_code(reason: Option<&str>, fallback: &'static str) -> String {
    reason
        .filter(|r| {
            (2..=64).contains(&r.len())
                && r.as_bytes()[0].is_ascii_uppercase()
                && r.bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        })
        .unwrap_or(fallback)
        .to_owned()
}

/// Ends open rows whose message expired (never sent by either path; secrets erased).
async fn expire_overdue(db: &DatabaseConnection, now: DateTime<Utc>) -> Result<u64, DbErr> {
    let n = db
        .execute(stmt(
            "UPDATE account_notification_outbox
                SET state = 'EXPIRED', last_error = 'EXPIRED', settled_at = $1,
                    fallback_sealed = ''::bytea, event = event - 'sealed'
              WHERE settled_at IS NULL AND state IN ('PENDING', 'ACKED') AND expires_at <= $1",
            vec![now.into()],
        ))
        .await?
        .rows_affected();
    for _ in 0..n {
        inc(&OM.critical_expired);
    }
    Ok(n)
}

/// The keys of `NEW` decisions that must fail over now: never leased, not completed, not
/// expired, and either `FORCED` or (`AUTO` and older than the budget). The lease rule is
/// re-checked under the row lock by [`fence::reroute_critical`].
async fn failover_candidates(
    db: &DatabaseConnection,
    now: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<String>, DbErr> {
    db.query_all(stmt(
        "SELECT d.idempotency_key
           FROM account_notification_outbox o
           JOIN notification_route_decisions d ON d.decision_id = o.decision_id
           JOIN notification_routing_state s ON s.category = d.category
          WHERE o.settled_at IS NULL AND NOT o.shadow AND o.state IN ('PENDING', 'ACKED')
            AND o.expires_at > $1
            AND d.route = 'NEW' AND d.ns_completed_at IS NULL AND d.lease_count = 0
            AND (s.critical_fallback = 'FORCED'
                 OR (s.critical_fallback = 'AUTO' AND d.decided_at <= $2))
          ORDER BY d.decided_at LIMIT $3",
        vec![
            now.into(),
            (now - AUTO_FAILOVER_BUDGET).into(),
            limit.into(),
        ],
    ))
    .await?
    .iter()
    .map(|r| r.try_get::<String>("", "idempotency_key"))
    .collect()
}

#[derive(Debug, FromQueryResult)]
struct PostRow {
    id: Uuid,
    decision_id: Uuid,
    idempotency_key: String,
    shadow: bool,
    event: Value,
    attempts: i32,
}

async fn settle_acked<C: ConnectionTrait>(
    txn: &C,
    row: &PostRow,
    outcome: &str,
    now: DateTime<Utc>,
) -> Result<(), DbErr> {
    // A shadow copy is done once the service recorded it; a publication keeps its fallback
    // until X7, a failover or expiry.
    txn.execute(stmt(
        "UPDATE account_notification_outbox
            SET state = 'ACKED', outcome = $2, last_error = NULL, attempts = attempts + 1,
                event = event - 'sealed',
                settled_at = CASE WHEN shadow THEN $3 ELSE settled_at END,
                fallback_sealed = CASE WHEN shadow THEN ''::bytea ELSE fallback_sealed END
          WHERE id = $1",
        vec![row.id.into(), outcome.into(), now.into()],
    ))
    .await?;
    if !row.shadow {
        // §9.5 / F5: responsibility moves to the service.
        txn.execute(stmt(
            "UPDATE notification_route_decisions SET ns_acked_at = $2
              WHERE decision_id = $1 AND route = 'NEW' AND ns_acked_at IS NULL",
            vec![row.decision_id.into(), now.into()],
        ))
        .await?;
    }
    inc(&OM.critical_acked);
    Ok(())
}

/// The service refused the event for good: a shadow copy is dead; a publication fails
/// over to the legacy path at once (it was never leased), or is dead (and alerted) when
/// `critical_fallback = OFF` forbids it.
async fn refused<C: ConnectionTrait>(
    txn: &C,
    row: &PostRow,
    keys: Option<&CriticalKeys>,
    outcome: Option<&str>,
    error: String,
    now: DateTime<Utc>,
    report: &mut CriticalRelayReport,
) -> Result<(), DbErr> {
    inc(&OM.critical_rejected);
    if !row.shadow {
        match fail_over(txn, &row.idempotency_key, keys, true, now).await? {
            Failover::Sent { .. } => {
                txn.execute(stmt(
                    "UPDATE account_notification_outbox SET outcome = $2, attempts = attempts + 1
                      WHERE id = $1",
                    vec![row.id.into(), outcome.map(str::to_owned).into()],
                ))
                .await?;
                report.failed_over += 1;
                return Ok(());
            }
            Failover::FallbackUnavailable => {
                // Nothing may change on this transaction's decision; retry later.
                return Err(DbErr::Custom("CRITICAL fallback unavailable".into()));
            }
            _ => {}
        }
    }
    txn.execute(stmt(
        "UPDATE account_notification_outbox
            SET state = 'DEAD', outcome = $2, last_error = $3, attempts = attempts + 1,
                settled_at = $4, event = event - 'sealed', fallback_sealed = ''::bytea
          WHERE id = $1",
        vec![
            row.id.into(),
            outcome.map(str::to_owned).into(),
            error.into(),
            now.into(),
        ],
    ))
    .await?;
    if !row.shadow {
        inc(&OM.critical_dead);
        tracing::error!(decision_id = %row.decision_id,
            "CRITICAL notification refused by the Notification Service and not failed over (critical_fallback = OFF)");
    }
    report.dead += 1;
    Ok(())
}

async fn retry<C: ConnectionTrait>(
    txn: &C,
    row: &PostRow,
    error: &str,
    counted: bool,
    now: DateTime<Utc>,
) -> Result<(), DbErr> {
    let attempts = row.attempts + i32::from(counted);
    let next = if counted {
        now + retry_delay(attempts)
    } else {
        now + Duration::seconds(5)
    };
    txn.execute(stmt(
        "UPDATE account_notification_outbox SET attempts = $2, last_error = $3, next_attempt_at = $4
          WHERE id = $1",
        vec![row.id.into(), attempts.into(), error.into(), next.into()],
    ))
    .await?;
    if counted {
        inc(&OM.critical_retried);
    }
    Ok(())
}

/// Posts one batch of due rows (all shadow copies or all publications).
async fn post_batch(
    db: &DatabaseConnection,
    sink: Option<&dyn NotificationEventSink>,
    keys: Option<&CriticalKeys>,
    shadow: bool,
    now: DateTime<Utc>,
    report: &mut CriticalRelayReport,
) -> Result<bool, DbErr> {
    let txn = db.begin().await?;
    // Decision and outbox rows are locked together, never waiting (lock order with X6 /
    // X7 and the failover sweep, which lock the decision first).
    let rows = PostRow::find_by_statement(stmt(
        "SELECT o.id, o.decision_id, d.idempotency_key, o.shadow, o.event, o.attempts
           FROM account_notification_outbox o
           JOIN notification_route_decisions d ON d.decision_id = o.decision_id
          WHERE o.state = 'PENDING' AND o.settled_at IS NULL AND o.shadow = $3
            AND o.next_attempt_at <= $1 AND o.expires_at > $1
          ORDER BY o.next_attempt_at, o.created_at LIMIT $2
          FOR UPDATE OF o, d SKIP LOCKED",
        vec![now.into(), (MAX_BATCH as i64).into(), shadow.into()],
    ))
    .all(&txn)
    .await?;
    if rows.is_empty() {
        txn.rollback().await?;
        return Ok(false);
    }
    let full = rows.len() == MAX_BATCH;
    let events: Vec<Value> = rows.iter().map(|r| r.event.clone()).collect();
    let answer = match sink {
        Some(sink) => sink.ingest(&events, shadow).await,
        None => Err(PublishFailure::NotConfigured),
    };
    let mut keep_going = full;
    match answer {
        Ok(results) => {
            let by_id: HashMap<&str, &IngestResult> =
                results.iter().map(|r| (r.event_id.as_str(), r)).collect();
            for row in &rows {
                let id = event_id(row.id);
                match by_id.get(id.as_str()) {
                    Some(r) if matches!(r.outcome.as_str(), "ACCEPTED" | "DUPLICATE" | "HELD") => {
                        settle_acked(&txn, row, &r.outcome, now).await?;
                        report.acked += 1;
                    }
                    Some(r) if r.outcome == "REJECTED" => {
                        let reason = reason_code(r.reason.as_deref(), "REJECTED");
                        refused(&txn, row, keys, Some("REJECTED"), reason, now, report).await?;
                    }
                    _ => {
                        retry(&txn, row, "NO_RESULT", true, now).await?;
                        report.retried += 1;
                    }
                }
            }
        }
        Err(PublishFailure::Rejected) => {
            for row in &rows {
                refused(
                    &txn,
                    row,
                    keys,
                    None,
                    "CONTRACT_REJECTED".into(),
                    now,
                    report,
                )
                .await?;
            }
        }
        Err(PublishFailure::Unavailable) => {
            for row in &rows {
                retry(&txn, row, "UNAVAILABLE", true, now).await?;
                report.retried += 1;
            }
            keep_going = false;
        }
        Err(PublishFailure::NotConfigured) => {
            for row in &rows {
                retry(&txn, row, "NOT_CONFIGURED", false, now).await?;
                report.not_configured += 1;
            }
            keep_going = false;
        }
    }
    txn.commit().await?;
    Ok(keep_going)
}

/// One relay pass (multi-instance safe): expiry, the AUTO / FORCED failover sweep, then
/// the I1 posts (`sink = None`: no Notification Service target; rows wait and fail over).
pub async fn relay_due(
    db: &DatabaseConnection,
    sink: Option<&dyn NotificationEventSink>,
    keys: Option<&CriticalKeys>,
    now: DateTime<Utc>,
) -> Result<CriticalRelayReport, DbErr> {
    let mut report = CriticalRelayReport {
        expired: expire_overdue(db, now).await?,
        ..Default::default()
    };
    for key in failover_candidates(db, now, 500).await? {
        match fail_over_txn(db, &key, keys, true, now).await? {
            Failover::Sent { .. } => report.failed_over += 1,
            Failover::FallbackUnavailable => report.fallback_unavailable += 1,
            _ => {}
        }
    }
    for shadow in [false, true] {
        for _ in 0..5 {
            match post_batch(db, sink, keys, shadow, now, &mut report).await {
                Ok(true) => continue,
                Ok(false) => break,
                Err(DbErr::Custom(e)) if e == "CRITICAL fallback unavailable" => {
                    report.fallback_unavailable += 1;
                    break;
                }
                Err(e) => return Err(e),
            }
        }
    }
    Ok(report)
}

fn undefined_object(e: &DbErr) -> bool {
    let runtime = match e {
        DbErr::Query(e) | DbErr::Exec(e) => e,
        _ => return false,
    };
    matches!(runtime, sea_orm::RuntimeErr::SqlxError(s)
        if matches!(s.as_database_error().and_then(|d| d.code()).as_deref(), Some("42P01" | "42703")))
}

/// Hourly maintenance: purges settled rows older than [`RETENTION`] and expired CRITICAL
/// decisions past their 30-day grace (never one an outbox row still references).
/// Returns (purged rows, purged decisions).
pub async fn maintain(
    db: &DatabaseConnection,
    batch: i64,
    now: DateTime<Utc>,
) -> Result<(u64, u64), DbErr> {
    let rows = db
        .execute(stmt(
            "DELETE FROM account_notification_outbox WHERE id IN (
                 SELECT id FROM account_notification_outbox
                  WHERE settled_at IS NOT NULL AND settled_at < $1 ORDER BY settled_at LIMIT $2)",
            vec![(now - RETENTION).into(), batch.into()],
        ))
        .await?
        .rows_affected();
    let decisions = db
        .execute(stmt(
            "DELETE FROM notification_route_decisions WHERE idempotency_key IN (
                 SELECT d.idempotency_key FROM notification_route_decisions d
                  WHERE d.category IN ('AUTH_VERIFICATION', 'AUTH_PASSWORD_RESET',
                          'SECURITY_DEVICE_STEP_UP', 'SECURITY_PASSWORD_CHANGED', 'SECURITY_DEVICE_LINKED',
                          'ACCOUNT_NOTICES')
                    AND d.expires_at < $1 AND d.expires_at <= now()
                    AND NOT EXISTS (SELECT 1 FROM account_notification_outbox o WHERE o.decision_id = d.decision_id)
                  ORDER BY d.expires_at LIMIT $2)",
            vec![(now - RETENTION).into(), batch.into()],
        ))
        .await?
        .rows_affected();
    Ok((rows, decisions))
}

/// The PW1 worker: a relay pass every `interval` (failover and expiry run whether or not
/// a Notification Service target is configured), maintenance hourly. A database without
/// the fence history is a no-op.
pub async fn run(
    db: DatabaseConnection,
    sink: Option<std::sync::Arc<dyn NotificationEventSink>>,
    interval: StdDuration,
) {
    let mut last_maintenance: Option<DateTime<Utc>> = None;
    loop {
        let now = Utc::now();
        match relay_due(&db, sink.as_deref(), installed(), now).await {
            Ok(_) => {}
            Err(e) if undefined_object(&e) => {}
            Err(e) => tracing::warn!("CRITICAL notification relay pass failed: {e}"),
        }
        if last_maintenance.is_none_or(|t| now - t >= Duration::hours(1)) {
            last_maintenance = Some(now);
            match maintain(&db, 500, now).await {
                Ok(_) => {}
                Err(e) if undefined_object(&e) => {}
                Err(e) => tracing::warn!("CRITICAL notification maintenance failed: {e}"),
            }
        }
        tokio::time::sleep(interval).await;
    }
}

// ------------------------------------------------------------------- operations / D-9

/// PII-free state of the CRITICAL lane (operations, certify and the D-9 gauges).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaneStatus {
    pub fallback_key: bool,
    /// Open (unsettled) publications by state.
    pub pending: i64,
    pub acked: i64,
    /// Open `NEW` decisions never leased / under a live lease / leased, lapsed, uncompleted.
    pub never_leased: i64,
    pub leased_live: i64,
    pub leased_uncompleted: i64,
    pub dead: i64,
    /// Age of the oldest open publication (seconds; 0 when none).
    pub oldest_open_seconds: i64,
    /// Decisions an operator may reroute under `FORCED` (at most 100, oldest first).
    pub leased_uncompleted_decisions: Vec<Uuid>,
}

pub async fn lane_status<C: ConnectionTrait>(conn: &C) -> Result<LaneStatus, DbErr> {
    let row = conn
        .query_one(stmt(
            "SELECT
                count(*) FILTER (WHERE o.state = 'PENDING') AS pending,
                count(*) FILTER (WHERE o.state = 'ACKED') AS acked,
                count(*) FILTER (WHERE d.lease_count = 0) AS never_leased,
                count(*) FILTER (WHERE d.send_lease_until > clock_timestamp()) AS leased_live,
                count(*) FILTER (WHERE d.lease_count > 0 AND d.ns_completed_at IS NULL
                                   AND COALESCE(d.send_lease_until <= clock_timestamp(), true)) AS leased_uncompleted,
                COALESCE(EXTRACT(EPOCH FROM clock_timestamp() - min(o.created_at)), 0)::bigint AS oldest
               FROM account_notification_outbox o
               JOIN notification_route_decisions d ON d.decision_id = o.decision_id
              WHERE o.settled_at IS NULL AND NOT o.shadow AND o.state IN ('PENDING', 'ACKED')
                AND d.route = 'NEW'",
            vec![],
        ))
        .await?;
    let get = |name: &str| -> i64 {
        row.as_ref()
            .and_then(|r| r.try_get::<i64>("", name).ok())
            .unwrap_or(0)
    };
    let dead = conn
        .query_one(stmt(
            "SELECT count(*) AS n FROM account_notification_outbox WHERE state = 'DEAD' AND NOT shadow",
            vec![],
        ))
        .await?
        .and_then(|r| r.try_get::<i64>("", "n").ok())
        .unwrap_or(0);
    let leased_uncompleted_decisions = conn
        .query_all(stmt(
            "SELECT d.decision_id FROM account_notification_outbox o
               JOIN notification_route_decisions d ON d.decision_id = o.decision_id
              WHERE o.settled_at IS NULL AND NOT o.shadow AND o.state IN ('PENDING', 'ACKED')
                AND d.route = 'NEW' AND d.lease_count > 0 AND d.ns_completed_at IS NULL
                AND d.send_lease_until <= clock_timestamp()
              ORDER BY d.decided_at LIMIT 100",
            vec![],
        ))
        .await?
        .iter()
        .filter_map(|r| r.try_get::<Uuid>("", "decision_id").ok())
        .collect();
    Ok(LaneStatus {
        fallback_key: fallback_ready(),
        pending: get("pending"),
        acked: get("acked"),
        never_leased: get("never_leased"),
        leased_live: get("leased_live"),
        leased_uncompleted: get("leased_uncompleted"),
        dead,
        oldest_open_seconds: get("oldest"),
        leased_uncompleted_decisions,
    })
}

/// The fallback drill (Document 4 §10.4 readiness invariants 1, 2 and 4;
/// `email-acceptance confirm=fallback`): one controlled `OPERATIONS_ACCEPTANCE` email that
/// travels exactly the failover path — sealed to the Account fallback key, opened, and
/// queued in the legacy ledger for the legacy worker and its own provider credentials.
/// `None`: no fallback key installed.
pub async fn fallback_drill<C: ConnectionTrait>(
    conn: &C,
    keys: Option<&CriticalKeys>,
    email: NewEmail,
) -> ApiResult<Option<Enqueued>> {
    let Some(key) = keys.and_then(|k| k.fallback.as_ref()) else {
        return Ok(None);
    };
    key.self_test()
        .map_err(|e| ApiError::InternalError(e.to_string()))?;
    let probe = Uuid::new_v4();
    let sealed_email = FallbackEmail::from_new(&email)
        .ok_or_else(|| ApiError::InternalError("drill email without a key".into()))?;
    let sealed = key
        .seal(probe, &sealed_email)
        .map_err(|e| ApiError::InternalError(e.to_string()))?;
    let opened = key
        .open(probe, &sealed)
        .map_err(|e| ApiError::InternalError(e.to_string()))?
        .to_new()
        .ok_or_else(|| ApiError::InternalError("drill category refused".into()))?;
    email_delivery::enqueue(conn, opened).await.map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn email() -> FallbackEmail {
        FallbackEmail {
            category: category::AUTH_VERIFICATION.into(),
            idempotency_key: "otp:verify:abc:def".into(),
            to: "ada@example.com".into(),
            to_name: Some("ada".into()),
            subject: "Your CF Meet verification code".into(),
            html: "<p>123456</p>".into(),
            text: "123456".into(),
            expires_at: DateTime::from_timestamp(1_800_000_000, 0),
        }
    }

    #[test]
    fn the_fallback_seal_is_bound_to_its_key_and_decision() {
        let key = FallbackKey::generate();
        key.self_test().unwrap();
        let d = Uuid::new_v4();
        let sealed = key.seal(d, &email()).unwrap();
        assert_eq!(key.open(d, &sealed).unwrap(), email());
        // Another decision, another key, a tampered blob: never opens.
        assert_eq!(key.open(Uuid::new_v4(), &sealed), Err(FallbackError::Open));
        assert_eq!(
            FallbackKey::generate().open(d, &sealed),
            Err(FallbackError::Open)
        );
        let mut t = sealed.clone();
        t[30] ^= 0x80;
        assert_eq!(key.open(d, &t), Err(FallbackError::Open));
        // The ciphertext does not carry the code or the address in clear.
        let hay = String::from_utf8_lossy(&sealed);
        assert!(!hay.contains("123456") && !hay.contains("ada@example.com"));
        // Round trip through the base64 key form.
        let same = FallbackKey::parse(&key.to_base64()).unwrap();
        assert_eq!(same.key_id(), key.key_id());
        assert_eq!(same.open(d, &sealed).unwrap(), email());
        assert!(FallbackKey::parse("c2hvcnQ=").is_err());
        assert!(!format!("{key:?}").contains(&*key.to_base64()));
        assert!(!format!("{:?}", email()).contains("123456"));
    }

    #[test]
    fn only_known_ledger_categories_are_restored() {
        let mut e = email();
        assert_eq!(e.to_new().unwrap().category, category::AUTH_VERIFICATION);
        e.category = "SOMETHING_ELSE".into();
        assert!(e.to_new().is_none());
    }

    fn details() -> RequestDetails {
        RequestDetails {
            client: Some("TestBrowser/1.0".into()),
            address: Some("102.89.x.x".into()),
            when_utc: "2026-11-17 12:00".into(),
        }
    }

    /// The sign-in code carries exactly the facts its legacy email shows; one the
    /// catalogue cannot carry exactly stays on the legacy path.
    #[test]
    fn sign_in_codes_carry_their_request_facts_or_stay_legacy() {
        let e = CriticalEvent::sign_in_code(
            "a@example.com",
            "123456",
            Some("tok_0123456789abcdef"),
            10,
            &details(),
        );
        assert_eq!(e.event_type, Some(event_type::SIGN_IN_CODE));
        let data: HashMap<&str, Value> = e.data.iter().cloned().collect();
        assert_eq!(data["requestedAt"], "2026-11-17T12:00:00Z");
        assert_eq!(data["requestClient"], "TestBrowser/1.0");
        assert_eq!(data["requestNetwork"], "102.89.x.x");
        assert_eq!(data["ttlMinutes"], 10);
        let bare = RequestDetails {
            client: None,
            address: None,
            ..details()
        };
        let e = CriticalEvent::sign_in_code(
            "a@example.com",
            "123456",
            Some("tok_0123456789abcdef"),
            10,
            &bare,
        );
        assert_eq!(e.data.len(), 2);
        assert!(masked_network("2001:db8:x:x") && !masked_network("102.89.12.7"));
        for odd in [
            RequestDetails {
                when_utc: "yesterday".into(),
                ..details()
            },
            RequestDetails {
                address: Some("102.89.12.7".into()),
                ..details()
            },
            RequestDetails {
                client: Some("x".repeat(81)),
                ..details()
            },
            RequestDetails {
                client: Some("a\u{7}b".into()),
                ..details()
            },
        ] {
            let e = CriticalEvent::sign_in_code(
                "a@example.com",
                "123456",
                Some("tok_0123456789abcdef"),
                10,
                &odd,
            );
            assert_eq!(e.event_type, None, "{odd:?}");
        }
        let e = CriticalEvent::sign_in_code("a@example.com", "123456", None, 10, &details());
        assert_eq!(
            e.event_type, None,
            "no magic link: not the catalogue's email"
        );
    }

    /// Every step-up purpose is a catalogue purpose (contract `StepUpPurpose`).
    #[test]
    fn step_up_purposes_are_the_catalogue_enum() {
        use crate::services::step_up::StepUpPurpose;
        let ours = [StepUpPurpose::DeactivateAccount, StepUpPurpose::ChangeEmail]
            .map(StepUpPurpose::as_str);
        assert_eq!(ours.as_slice(), cf_notification_client::STEP_UP_PURPOSES);
    }

    #[test]
    fn envelopes_follow_the_catalogue() {
        let (public, _) =
            cf_notification_client::seal::generate_keypair("notif-hpke-2026-11").unwrap();
        let sealing = SealingKey::parse(&public).unwrap();
        let account = Uuid::new_v4();
        let route = RouteRef {
            decision_id: Uuid::new_v4(),
            routing_version: 3,
            protocol: 1,
        };
        let now = Utc::now();
        let cases = [
            (
                CriticalEvent::verification_code(account, "123456", 1440),
                "otp:verify:aa:bb",
            ),
            (
                CriticalEvent::password_reset_code(account, "123456", 60),
                "otp:reset:aa:bb",
            ),
            (
                CriticalEvent::device_step_up_code(account, Uuid::new_v4(), "123456", 10),
                "device-step-up:x:y",
            ),
            (
                CriticalEvent::password_changed(account, now),
                "password-changed:x:1",
            ),
            (
                CriticalEvent::device_linked(account, Uuid::new_v4(), Uuid::new_v4()),
                &*format!("device-linked:{}", Uuid::new_v4()),
            ),
            (
                CriticalEvent::sign_in_code(
                    "Ada@Example.com",
                    "123456",
                    Some("0123456789abcdef0123456789abcdef"),
                    10,
                    &details(),
                ),
                "otp:signin:aa:bb",
            ),
            (
                CriticalEvent::email_change_code(account, "ada.new@example.com", "123456", 10),
                "otp:emailchange:aa:bb",
            ),
            (
                CriticalEvent::step_up_code(account, "DEACTIVATE_ACCOUNT", "123456", 10),
                "otp:stepup:aa:bb",
            ),
            (
                CriticalEvent::welcome(account, "ada@example.com"),
                &*format!("welcome:{account}"),
            ),
            (
                CriticalEvent::new_sign_in(account, "ada@example.com", &details()),
                &*format!("new-sign-in:{}", Uuid::new_v4()),
            ),
            (
                CriticalEvent::email_in_use("taken@example.com"),
                "email-in-use:aa:2026111712",
            ),
            (
                CriticalEvent::email_changed_old(
                    account,
                    "ada@example.com",
                    "n***@example.com",
                    Some("0123456789abcdef0123456789abcdef"),
                    now,
                ),
                &*format!("email-changed:old:{}", Uuid::new_v4()),
            ),
            (
                CriticalEvent::email_changed_new(account, "ada.new@example.com", now),
                &*format!("email-changed:new:{}", Uuid::new_v4()),
            ),
        ];
        for (event, key) in &cases {
            let v = build_envelope(
                event,
                key,
                Uuid::new_v4(),
                route.clone(),
                Some(&sealing),
                now + Duration::hours(1),
                now,
            )
            .unwrap_or_else(|_| panic!("{:?}", event.event_type));
            let env: cf_notification_client::Envelope = serde_json::from_value(v.clone()).unwrap();
            cf_notification_client::validate(&env, Some("cfmeet.meeting-service")).unwrap();
            match event.address.as_deref() {
                // A sealed address names its email principal, never the account.
                Some(address) => {
                    assert_eq!(v["recipients"][0]["ref"], email_principal_ref(address));
                    assert!(!v.to_string().to_ascii_lowercase().contains("example.com"));
                }
                None => assert_eq!(v["recipients"][0]["ref"], format!("cfmeet-user:{account}")),
            }
            assert_eq!(v["subject"].is_object(), event.subject);
            assert_eq!(
                v["subject"].is_object(),
                !key.starts_with("otp:signin:") && !key.starts_with("email-in-use:")
            );
            assert_eq!(v["delivery"]["priority"], "CRITICAL");
            assert_eq!(v["delivery"]["idempotencyKey"], *key);
            assert_eq!(v.get("sealed").is_some(), event.secret().is_some());
            assert!(!v.to_string().contains("123456"));
            assert!(!v.to_string().contains("0123456789abcdef0123456789abcdef"));
            // The account notices seal what their emails show (the sign-in code of PR #99
            // carries its request facts as data).
            if !key.starts_with("otp:signin:") {
                for clear in ["n***", "TestBrowser", "102.89"] {
                    assert!(!v.to_string().contains(clear), "{key}");
                }
            }
            // Nothing sealed travels without the service key.
            if event.secret().is_some() {
                assert_eq!(
                    build_envelope(event, key, Uuid::new_v4(), route, None, now, now),
                    Err(DecisionReason::Unpublishable)
                );
            }
            // A code never travels without the service key.
            if event.code.is_some() {
                assert_eq!(
                    build_envelope(event, key, Uuid::new_v4(), route.clone(), None, now, now),
                    Err(DecisionReason::Unpublishable)
                );
            }
        }
    }

    /// The account notices carry exactly what their legacy emails show, sealed; what the
    /// catalogue cannot carry exactly stays on the legacy path.
    #[test]
    fn account_notices_seal_what_their_emails_show_or_stay_legacy() {
        let account = Uuid::new_v4();
        let e = CriticalEvent::new_sign_in(account, "ada@example.com", &details());
        assert_eq!(e.category, MeetingCategory::AccountNotices);
        assert_eq!(e.event_type, Some(event_type::NEW_SIGN_IN));
        let data: HashMap<&str, Value> = e.data.iter().cloned().collect();
        assert_eq!(data.len(), 1);
        assert_eq!(data["requestedAt"], "2026-11-17T12:00:00Z");
        let p = e.secret().unwrap();
        assert_eq!(p.request_client.as_deref(), Some("TestBrowser/1.0"));
        assert_eq!(p.request_network.as_deref(), Some("102.89.x.x"));
        assert_eq!(p.address.as_deref(), Some("ada@example.com"));
        let bare = RequestDetails {
            client: None,
            address: None,
            ..details()
        };
        let p = CriticalEvent::new_sign_in(account, "ada@example.com", &bare)
            .secret()
            .unwrap();
        assert!(p.request_client.is_none() && p.request_network.is_none());
        for odd in [
            RequestDetails {
                when_utc: "yesterday".into(),
                ..details()
            },
            RequestDetails {
                address: Some("102.89.12.7".into()),
                ..details()
            },
            RequestDetails {
                client: Some("a\u{7}b".into()),
                ..details()
            },
        ] {
            let e = CriticalEvent::new_sign_in(account, "ada@example.com", &odd);
            assert_eq!(e.event_type, None, "{odd:?}");
            assert_eq!(e.category, MeetingCategory::AccountNotices);
        }
        let at = DateTime::parse_from_rfc3339("2026-11-17T12:34:56.789Z")
            .unwrap()
            .with_timezone(&Utc);
        let old = CriticalEvent::email_changed_old(account, "a@x.com", "n***@y.com", None, at);
        let data: HashMap<&str, Value> = old.data.iter().cloned().collect();
        assert_eq!(data["changedAt"], "2026-11-17T12:34:00Z");
        assert_eq!(data["side"], "OLD");
        assert!(
            old.token.is_none(),
            "the support-assisted change has no undo link"
        );
        assert_eq!(
            CriticalEvent::email_changed_old(account, "a@x.com", "nadia@y.com", None, at)
                .event_type,
            None,
            "a full address is never a masked one"
        );
        let e = CriticalEvent::email_in_use("taken@example.com");
        assert!(!e.subject);
        assert_eq!(e.account_id, Uuid::nil());
        assert!(e.category.is_critical());
    }
}
