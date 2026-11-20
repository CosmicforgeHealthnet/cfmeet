//! Sealing codes, invitation tokens and invitee addresses to the Notification Service
//! (Document 4 §5.1, P4, S-19), and (contract `cfmeet.notifications.common` 1.2.0) the
//! account-notice facts an email shows but no event may carry in clear: a masked address
//! (`maskedAddress`) and the request's client label and masked network (`requestClient`,
//! `requestNetwork`).
//!
//! HPKE (RFC 9180) base mode with DHKEM(X25519, HKDF-SHA256), HKDF-SHA256 and
//! AES-256-GCM. `info` is fixed ([`HPKE_INFO`]); the AAD binds the sealed part to its
//! envelope (`<producer>|<eventId>|<eventType>`), so a sealed part copied into another
//! event does not open. Producers can only seal: opening is behind the `open` feature,
//! enabled only by the Notification Service renderer. Key rotation uses `kid`
//! (`notif-hpke-YYYY-MM[-suffix]`); the service keeps the previous private key until
//! every event sealed to it has reached a final state.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use hpke::{
    aead::AesGcm256, kdf::HkdfSha256, kem::X25519HkdfSha256, Deserializable, Kem as _, OpModeS,
    Serializable,
};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

type Kem = X25519HkdfSha256;

/// HPKE `info` for every sealed part (domain separation, versioned).
pub const HPKE_INFO: &[u8] = b"cfmeet.notifications.sealed.v1";
/// Maximum plaintext size of a sealed part.
pub const MAX_SEALED_PLAINTEXT: usize = 4096;

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum SealError {
    #[error("invalid key id (expected notif-hpke-YYYY-MM[-suffix])")]
    InvalidKid,
    #[error("invalid public or private key")]
    InvalidKey,
    #[error("the sealed plaintext is empty, malformed or too large")]
    InvalidPlaintext,
    #[error("the sealed part was sealed to another key id")]
    WrongKid,
    #[error("the sealed part does not open (wrong key, envelope or tampered)")]
    OpenFailed,
}

/// Validates a key id: `notif-hpke-YYYY-MM` with an optional `-[a-z0-9]{1,8}` suffix.
pub fn valid_kid(kid: &str) -> bool {
    let Some(rest) = kid.strip_prefix("notif-hpke-") else {
        return false;
    };
    let b = rest.as_bytes();
    if b.len() < 7 {
        return false;
    }
    let date_ok = b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit);
    if !date_ok {
        return false;
    }
    match &rest[7..] {
        "" => true,
        s => s.strip_prefix('-').is_some_and(|x| {
            (1..=8).contains(&x.len())
                && x.bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        }),
    }
}

/// The service's public sealing key, as published to producers (key id + 32-byte X25519
/// public key). Public by nature.
#[derive(Clone)]
pub struct SealingKey {
    kid: String,
    public: <Kem as hpke::Kem>::PublicKey,
}

impl std::fmt::Debug for SealingKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SealingKey")
            .field("kid", &self.kid)
            .finish_non_exhaustive()
    }
}

impl SealingKey {
    pub fn from_bytes(kid: &str, public: &[u8]) -> Result<Self, SealError> {
        if !valid_kid(kid) {
            return Err(SealError::InvalidKid);
        }
        let public =
            <Kem as hpke::Kem>::PublicKey::from_bytes(public).map_err(|_| SealError::InvalidKey)?;
        Ok(Self {
            kid: kid.to_string(),
            public,
        })
    }

    /// `<kid>:<base64url public key>` (the form release tooling distributes).
    pub fn parse(text: &str) -> Result<Self, SealError> {
        let (kid, key) = text.trim().split_once(':').ok_or(SealError::InvalidKey)?;
        let bytes = B64.decode(key).map_err(|_| SealError::InvalidKey)?;
        Self::from_bytes(kid, &bytes)
    }

    pub fn kid(&self) -> &str {
        &self.kid
    }

    pub fn public_bytes(&self) -> Vec<u8> {
        self.public.to_bytes().to_vec()
    }
}

/// What a sealed part contains. Never logged: `Debug` is redacted, memory is zeroized.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
pub struct SealedPlaintext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// A masked address as Meeting shows it (`n***@example.com`): the new address in the
    /// notice to the previous address of an email change.
    #[serde(
        rename = "maskedAddress",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub masked_address: Option<String>,
    /// The shortened client label of the request an account notice reports.
    #[serde(
        rename = "requestClient",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub request_client: Option<String>,
    /// The masked network prefix of that request (`102.89.x.x`, `2001:db8:x:x`).
    #[serde(
        rename = "requestNetwork",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub request_network: Option<String>,
}

impl std::fmt::Debug for SealedPlaintext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let r = |v: &Option<String>| v.as_ref().map(|_| "[REDACTED]");
        f.debug_struct("SealedPlaintext")
            .field("code", &r(&self.code))
            .field("token", &r(&self.token))
            .field("address", &r(&self.address))
            .field("masked_address", &r(&self.masked_address))
            .field("request_client", &r(&self.request_client))
            .field("request_network", &r(&self.request_network))
            .finish()
    }
}

fn code_ok(s: &str) -> bool {
    (4..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn token_ok(s: &str) -> bool {
    (16..=512).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'~' | b'-'))
}

fn address_ok(s: &str) -> bool {
    match s.split_once('@') {
        Some((local, domain)) => {
            s.len() <= 254
                && (1..=64).contains(&local.len())
                && (1..=253).contains(&domain.len())
                && !domain.contains('@')
                && !s.chars().any(char::is_whitespace)
        }
        None => false,
    }
}

/// `n***@example.com` (Meeting `services::email::mask_email`): at most one character of
/// the local part, then `***@` and the domain; never a full address.
pub fn masked_address_ok(s: &str) -> bool {
    let Some((local, domain)) = s.split_once('@') else {
        return false;
    };
    let Some(first) = local.strip_suffix("***") else {
        return false;
    };
    s.len() <= 260
        && first.chars().count() <= 1
        && !first.contains(['@', '*'])
        && (1..=253).contains(&domain.len())
        && !domain.contains('@')
        && !s.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// A client label: 1–80 characters, no control characters.
pub fn request_client_ok(s: &str) -> bool {
    (1..=80).contains(&s.chars().count()) && !s.chars().any(char::is_control)
}

/// A masked network prefix (`passwordless::mask_address`): hex digits, `.`, `:` and at
/// least one `x`, 3–64 bytes; never a full address.
pub fn request_network_ok(s: &str) -> bool {
    (3..=64).contains(&s.len())
        && s.contains('x')
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() || matches!(b, b'.' | b':' | b'x'))
}

impl SealedPlaintext {
    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }

    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    pub fn with_address(mut self, address: impl Into<String>) -> Self {
        self.address = Some(address.into());
        self
    }

    pub fn with_masked_address(mut self, masked: impl Into<String>) -> Self {
        self.masked_address = Some(masked.into());
        self
    }

    pub fn with_request_client(mut self, client: impl Into<String>) -> Self {
        self.request_client = Some(client.into());
        self
    }

    pub fn with_request_network(mut self, network: impl Into<String>) -> Self {
        self.request_network = Some(network.into());
        self
    }

    /// The fields present (for the catalogue's sealed allowance check).
    pub fn fields(&self) -> Vec<crate::catalogue::SealedField> {
        use crate::catalogue::SealedField::*;
        let mut out = Vec::new();
        if self.code.is_some() {
            out.push(Code);
        }
        if self.token.is_some() {
            out.push(Token);
        }
        if self.address.is_some() {
            out.push(Address);
        }
        if self.masked_address.is_some() {
            out.push(MaskedAddress);
        }
        if self.request_client.is_some() {
            out.push(RequestClient);
        }
        if self.request_network.is_some() {
            out.push(RequestNetwork);
        }
        out
    }

    /// Same value rules as the contract's `SealedPlaintext`.
    pub fn validate(&self) -> Result<(), SealError> {
        let ok = !self.fields().is_empty()
            && self.code.as_deref().is_none_or(code_ok)
            && self.token.as_deref().is_none_or(token_ok)
            && self.address.as_deref().is_none_or(address_ok)
            && self.masked_address.as_deref().is_none_or(masked_address_ok)
            && self.request_client.as_deref().is_none_or(request_client_ok)
            && self
                .request_network
                .as_deref()
                .is_none_or(request_network_ok);
        if ok {
            Ok(())
        } else {
            Err(SealError::InvalidPlaintext)
        }
    }
}

/// The wire form of a sealed part (`sealed` in the envelope).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sealed {
    pub kid: String,
    /// base64url (no padding) of the encapsulated key.
    pub enc: String,
    /// base64url (no padding) of ciphertext || tag.
    pub ct: String,
}

/// The AAD binding a sealed part to its envelope.
pub fn aad(producer: &str, event_id: &str, event_type: &str) -> Vec<u8> {
    format!("{producer}|{event_id}|{event_type}").into_bytes()
}

/// Seals `plaintext` to `key` for the envelope identified by (producer, event id, type).
pub fn seal(
    key: &SealingKey,
    producer: &str,
    event_id: &str,
    event_type: &str,
    plaintext: &SealedPlaintext,
) -> Result<Sealed, SealError> {
    plaintext.validate()?;
    let mut bytes = serde_json::to_vec(plaintext).map_err(|_| SealError::InvalidPlaintext)?;
    if bytes.len() > MAX_SEALED_PLAINTEXT {
        bytes.zeroize();
        return Err(SealError::InvalidPlaintext);
    }
    let result = hpke::single_shot_seal::<AesGcm256, HkdfSha256, Kem>(
        &OpModeS::Base,
        &key.public,
        HPKE_INFO,
        &bytes,
        &aad(producer, event_id, event_type),
    );
    bytes.zeroize();
    let (enc, ct) = result.map_err(|_| SealError::InvalidKey)?;
    Ok(Sealed {
        kid: key.kid.clone(),
        enc: B64.encode(enc.to_bytes()),
        ct: B64.encode(ct),
    })
}

impl Sealed {
    /// The compact wire form `<kid>.<enc>.<ct>` (the `sealed` string of the push provider
    /// vault, contract `cfmeet.notifications.internal-api` 1.3.0). Neither the key id nor
    /// base64url contain `.`.
    pub fn to_compact(&self) -> String {
        format!("{}.{}.{}", self.kid, self.enc, self.ct)
    }

    /// Parses the compact form of [`to_compact`](Self::to_compact).
    pub fn parse_compact(text: &str) -> Result<Self, SealError> {
        let mut parts = text.trim().split('.');
        let (Some(kid), Some(enc), Some(ct), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(SealError::OpenFailed);
        };
        let b64 = |s: &str| {
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        };
        if !valid_kid(kid) {
            return Err(SealError::InvalidKid);
        }
        if !b64(enc) || !b64(ct) {
            return Err(SealError::OpenFailed);
        }
        Ok(Self {
            kid: kid.to_string(),
            enc: enc.to_string(),
            ct: ct.to_string(),
        })
    }
}

// ------------------------------------------------------------------------------------
// Partner push provider credentials (ADR 0063, N-P1)
// ------------------------------------------------------------------------------------

/// HPKE `info` of a sealed push provider credential (domain-separated from event parts).
pub const PUSH_CREDENTIAL_INFO: &[u8] = b"cfmeet.notifications.push-provider.v1";
/// Maximum size of a push provider credential (an APNs `.p8` key or an FCM
/// service-account JSON document).
pub const MAX_PUSH_CREDENTIAL_BYTES: usize = 16 * 1024;

/// The AAD binding a sealed push credential to its (tenant, application, environment):
/// a credential sealed for one scope never opens for another.
pub fn push_credential_aad(
    tenant_id: uuid::Uuid,
    application_id: uuid::Uuid,
    environment_id: uuid::Uuid,
) -> Vec<u8> {
    format!("cfmeet.push-provider|{tenant_id}|{application_id}|{environment_id}").into_bytes()
}

/// The fingerprint the Developer Console keeps of a push credential: SHA-256 of the secret
/// exactly as sealed, lowercase hex.
pub fn push_credential_fingerprint(secret: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(secret.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Seals a partner push provider credential (the APNs `.p8` PEM or the FCM service-account
/// JSON) to the Notification Service for one (tenant, application, environment). The
/// Developer Console calls this on receipt and forwards only the result
/// ([`Sealed::to_compact`]) to `PUT /internal/v1/push/providers/...`; it never stores,
/// logs or returns the secret.
pub fn seal_push_credential(
    key: &SealingKey,
    tenant_id: uuid::Uuid,
    application_id: uuid::Uuid,
    environment_id: uuid::Uuid,
    secret: &str,
) -> Result<Sealed, SealError> {
    if secret.trim().is_empty() || secret.len() > MAX_PUSH_CREDENTIAL_BYTES {
        return Err(SealError::InvalidPlaintext);
    }
    let (enc, ct) = hpke::single_shot_seal::<AesGcm256, HkdfSha256, Kem>(
        &OpModeS::Base,
        &key.public,
        PUSH_CREDENTIAL_INFO,
        secret.as_bytes(),
        &push_credential_aad(tenant_id, application_id, environment_id),
    )
    .map_err(|_| SealError::InvalidKey)?;
    Ok(Sealed {
        kid: key.kid.clone(),
        enc: B64.encode(enc.to_bytes()),
        ct: B64.encode(ct),
    })
}

/// Generates a new service key pair: (`<kid>:<base64url public>`, private key bytes).
/// Used by release tooling / tests; the private key is written to a 0400 file and never
/// leaves the service.
pub fn generate_keypair(kid: &str) -> Result<(String, zeroize::Zeroizing<Vec<u8>>), SealError> {
    if !valid_kid(kid) {
        return Err(SealError::InvalidKid);
    }
    let (sk, pk) = Kem::gen_keypair();
    Ok((
        format!("{kid}:{}", B64.encode(pk.to_bytes())),
        zeroize::Zeroizing::new(sk.to_bytes().to_vec()),
    ))
}

#[cfg(feature = "open")]
pub use opening::{open, open_push_credential, OpeningKey};

#[cfg(feature = "open")]
mod opening {
    use super::*;
    use hpke::OpModeR;

    /// The service's private key (renderer only). Never logged.
    pub struct OpeningKey {
        kid: String,
        private: <Kem as hpke::Kem>::PrivateKey,
    }

    impl std::fmt::Debug for OpeningKey {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("OpeningKey")
                .field("kid", &self.kid)
                .finish_non_exhaustive()
        }
    }

    impl OpeningKey {
        pub fn from_bytes(kid: &str, private: &[u8]) -> Result<Self, SealError> {
            if !valid_kid(kid) {
                return Err(SealError::InvalidKid);
            }
            let private = <Kem as hpke::Kem>::PrivateKey::from_bytes(private)
                .map_err(|_| SealError::InvalidKey)?;
            Ok(Self {
                kid: kid.to_string(),
                private,
            })
        }

        pub fn kid(&self) -> &str {
            &self.kid
        }
    }

    /// Opens a sealed part of the envelope (producer, event id, type). Any change to the
    /// envelope identity, the key or the bytes fails closed.
    pub fn open(
        key: &OpeningKey,
        producer: &str,
        event_id: &str,
        event_type: &str,
        sealed: &Sealed,
    ) -> Result<SealedPlaintext, SealError> {
        if sealed.kid != key.kid {
            return Err(SealError::WrongKid);
        }
        let enc = B64.decode(&sealed.enc).map_err(|_| SealError::OpenFailed)?;
        let ct = B64.decode(&sealed.ct).map_err(|_| SealError::OpenFailed)?;
        let enc =
            <Kem as hpke::Kem>::EncappedKey::from_bytes(&enc).map_err(|_| SealError::OpenFailed)?;
        let mut plain = zeroize::Zeroizing::new(
            hpke::single_shot_open::<AesGcm256, HkdfSha256, Kem>(
                &OpModeR::Base,
                &key.private,
                &enc,
                HPKE_INFO,
                &ct,
                &aad(producer, event_id, event_type),
            )
            .map_err(|_| SealError::OpenFailed)?,
        );
        let value: SealedPlaintext =
            serde_json::from_slice(&plain).map_err(|_| SealError::InvalidPlaintext)?;
        plain.zeroize();
        value.validate()?;
        Ok(value)
    }

    /// Opens a sealed push provider credential of (tenant, application, environment). A
    /// credential sealed for another scope, to another key or tampered with fails closed.
    pub fn open_push_credential(
        key: &OpeningKey,
        tenant_id: uuid::Uuid,
        application_id: uuid::Uuid,
        environment_id: uuid::Uuid,
        sealed: &Sealed,
    ) -> Result<zeroize::Zeroizing<String>, SealError> {
        if sealed.kid != key.kid {
            return Err(SealError::WrongKid);
        }
        let enc = B64.decode(&sealed.enc).map_err(|_| SealError::OpenFailed)?;
        let ct = B64.decode(&sealed.ct).map_err(|_| SealError::OpenFailed)?;
        let enc =
            <Kem as hpke::Kem>::EncappedKey::from_bytes(&enc).map_err(|_| SealError::OpenFailed)?;
        let plain = zeroize::Zeroizing::new(
            hpke::single_shot_open::<AesGcm256, HkdfSha256, Kem>(
                &OpModeR::Base,
                &key.private,
                &enc,
                PUSH_CREDENTIAL_INFO,
                &ct,
                &push_credential_aad(tenant_id, application_id, environment_id),
            )
            .map_err(|_| SealError::OpenFailed)?,
        );
        if plain.is_empty() || plain.len() > MAX_PUSH_CREDENTIAL_BYTES {
            return Err(SealError::InvalidPlaintext);
        }
        let text = std::str::from_utf8(&plain).map_err(|_| SealError::InvalidPlaintext)?;
        Ok(zeroize::Zeroizing::new(text.to_string()))
    }
}
