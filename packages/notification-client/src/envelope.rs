//! The `cfmeet.notifications.event` v1 envelope (Document 4 §5.1): builder and validator.
//!
//! [`validate`] enforces every envelope rule of the contract plus those JSON Schema cannot
//! express (total size, producer = authenticated issuer at I1). Producers build envelopes
//! only through [`EnvelopeBuilder`]; the Notification Service runs [`validate`] again at
//! ingest (I1) with the authenticated issuer.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::catalogue::{self, EventSpec, Priority, Producer};
use crate::routing::RouteRef;
use crate::seal::{self, Sealed, SealedPlaintext, SealingKey};

/// Envelope schema version.
pub const SCHEMA_VERSION: u8 = 1;
/// Size limit of an envelope without / with a sealed part.
pub const MAX_ENVELOPE_BYTES: usize = 4096;
pub const MAX_SEALED_ENVELOPE_BYTES: usize = 8192;
pub const MAX_RECIPIENTS: usize = 200;
pub const MAX_STRING: usize = 200;
pub const MAX_DATA_KEYS: usize = 16;

/// Substrings a `data` key may never contain (any case): such values travel only inside
/// `sealed` (mirrors Messaging's payload CHECK, `m20261106_000004_realtime.rs`).
pub const FORBIDDEN_DATA_KEY_PARTS: &[&str] = &[
    "secret",
    "private",
    "token",
    "ciphertext",
    "plaintext",
    "content",
    "body",
    "password",
    "email",
    "phone",
    "key",
    "code",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Scope {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Subject {
    pub kind: String,
    pub id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipient {
    #[serde(rename = "ref")]
    pub reference: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Delivery {
    pub priority: Priority,
    pub dedupe_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coalesce_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deliver_after: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_key: Option<String>,
}

/// A notification event envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Envelope {
    pub schema_version: u8,
    pub event_id: String,
    pub event_type: String,
    pub event_version: u8,
    pub producer: String,
    pub occurred_at: String,
    pub correlation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<Scope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<Subject>,
    pub recipients: Vec<Recipient>,
    pub delivery: Delivery,
    pub route: RouteRef,
    pub data: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sealed: Option<Sealed>,
}

/// Why an envelope is refused. Messages never include data values or sealed content.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnvelopeError {
    #[error("unsupported schemaVersion")]
    SchemaVersion,
    #[error("eventType is not in the catalogue")]
    UnknownEventType,
    #[error("producer is not allowed for this eventType")]
    ProducerNotAllowed,
    #[error("producer does not equal the authenticated issuer")]
    ProducerMismatch,
    #[error("priority does not match the catalogue for this eventType")]
    PriorityMismatch,
    #[error("invalid {0}")]
    Invalid(&'static str),
    #[error("data key `{0}` is not allowed")]
    DataKey(String),
    #[error("data key `{0}` is required")]
    MissingData(&'static str),
    #[error("invalid value for data key `{0}`")]
    DataValue(String),
    #[error("this eventType may not carry a sealed part")]
    SealedNotAllowed,
    #[error("this eventType requires a sealed part with {0}")]
    SealedRequired(&'static str),
    #[error("idempotencyKey is required for this eventType")]
    IdempotencyKeyRequired,
    #[error("idempotencyKey does not use today's key format for this eventType")]
    IdempotencyKeyFormat,
    #[error("envelope is larger than {0} bytes")]
    TooLarge(usize),
    #[error("seal: {0}")]
    Seal(#[from] seal::SealError),
}

fn is_lower_hex(s: &str) -> bool {
    s.bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn is_uuid(s: &str) -> bool {
    Uuid::parse_str(s).is_ok_and(|u| u.hyphenated().to_string() == s)
}

fn safe_string(s: &str) -> bool {
    s.chars().count() <= MAX_STRING && !s.chars().any(|c| c.is_control())
}

fn printable_key(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && s.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

/// RFC 3339 shape check (`YYYY-MM-DDTHH:MM:SS[.frac](Z|±HH:MM)`).
pub fn rfc3339(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() < 20 || b.len() > 35 {
        return false;
    }
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    if !(digits(0..4)
        && b[4] == b'-'
        && digits(5..7)
        && b[7] == b'-'
        && digits(8..10)
        && (b[10] == b'T' || b[10] == b't')
        && digits(11..13)
        && b[13] == b':'
        && digits(14..16)
        && b[16] == b':'
        && digits(17..19))
    {
        return false;
    }
    let mut i = 19;
    if b[i] == b'.' {
        i += 1;
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            return false;
        }
    }
    match &s[i..] {
        "Z" | "z" => true,
        tz => {
            let t = tz.as_bytes();
            t.len() == 6
                && (t[0] == b'+' || t[0] == b'-')
                && t[1..3].iter().all(u8::is_ascii_digit)
                && t[3] == b':'
                && t[4..6].iter().all(u8::is_ascii_digit)
        }
    }
}

/// Whether `r` is a valid recipient reference.
pub fn valid_recipient(r: &str) -> bool {
    if let Some(name) = r.strip_prefix("operator-group:") {
        return (1..=40).contains(&name.len())
            && name.as_bytes()[0].is_ascii_lowercase()
            && name.bytes().all(|b| b.is_ascii_lowercase() || b == b'-');
    }
    [
        "cfmeet-user:",
        "meeting-email-principal:",
        "platform-principal:",
    ]
    .iter()
    .any(|p| r.strip_prefix(p).is_some_and(is_uuid))
        || catalogue::parse_platform_principal(r).is_some()
}

/// Whether a `data` key is allowed by the envelope-wide rule (camelCase, ≤ 48, no
/// forbidden part in any case).
pub fn allowed_data_key(k: &str) -> bool {
    let b = k.as_bytes();
    let shape = (1..=48).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b.iter().all(u8::is_ascii_alphanumeric);
    let lower = k.to_ascii_lowercase();
    shape && !FORBIDDEN_DATA_KEY_PARTS.iter().any(|f| lower.contains(f))
}

fn data_value_ok(v: &Value) -> bool {
    match v {
        Value::String(s) => safe_string(s),
        Value::Number(n) => n.is_i64() || n.is_u64(),
        Value::Bool(_) | Value::Null => true,
        _ => false,
    }
}

/// Validates an envelope. `issuer` is the authenticated service identity at I1 (`None`
/// when a producer validates its own envelope before writing it to its outbox).
pub fn validate(env: &Envelope, issuer: Option<&str>) -> Result<&'static EventSpec, EnvelopeError> {
    use EnvelopeError::*;
    if env.schema_version != SCHEMA_VERSION {
        return Err(SchemaVersion);
    }
    let spec = catalogue::spec(&env.event_type).ok_or(UnknownEventType)?;
    let producer = Producer::parse(&env.producer).ok_or(ProducerNotAllowed)?;
    if !spec.allows_producer(producer) {
        return Err(ProducerNotAllowed);
    }
    if let Some(issuer) = issuer {
        if issuer != env.producer {
            return Err(ProducerMismatch);
        }
    }
    let id_ok = env.event_id.strip_prefix("evt_").map_or_else(
        || is_uuid(&env.event_id),
        |h| h.len() == 32 && is_lower_hex(h),
    );
    if !id_ok {
        return Err(Invalid("eventId"));
    }
    if !(1..=99).contains(&env.event_version) {
        return Err(Invalid("eventVersion"));
    }
    if !rfc3339(&env.occurred_at) {
        return Err(Invalid("occurredAt"));
    }
    if !env
        .correlation_id
        .strip_prefix("corr_")
        .is_some_and(|h| h.len() == 32 && is_lower_hex(h))
    {
        return Err(Invalid("correlationId"));
    }
    if let Some(subject) = &env.subject {
        if spec.subject_kind != Some(subject.kind.as_str())
            || subject.version.is_some_and(|v| v < 0)
        {
            return Err(Invalid("subject"));
        }
    }
    // Recipients: the control event has none; every other event has 1..=200.
    let count = env.recipients.len();
    if (spec.is_control() && count != 0)
        || (!spec.is_control() && !(1..=MAX_RECIPIENTS).contains(&count))
    {
        return Err(Invalid("recipients"));
    }
    for r in &env.recipients {
        if !valid_recipient(&r.reference) {
            return Err(Invalid("recipients"));
        }
        if producer == Producer::Alertmanager && !r.reference.starts_with("operator-group:") {
            return Err(Invalid("recipients"));
        }
    }
    // Partner end users (`platform-principal:<env>:<principal>`, ADR 0063 N-P4): only for
    // the metadata-only partner event types, never mixed with other recipients, and in
    // the envelope's scope when it names one.
    let partner: Vec<(Uuid, Uuid)> = env
        .recipients
        .iter()
        .filter_map(|r| catalogue::parse_platform_principal(&r.reference))
        .collect();
    if !partner.is_empty() {
        if partner.len() != count || !catalogue::PARTNER_EVENT_TYPES.contains(&spec.event_type) {
            return Err(Invalid("recipients"));
        }
        if let Some(scope) = &env.scope {
            if partner.iter().any(|(e, _)| *e != scope.environment_id) {
                return Err(Invalid("recipients"));
            }
        }
    }
    // A type that names its recipient kind has exactly one recipient of that kind (a
    // sealed address belongs to exactly one email principal).
    if let Some(kind) = spec.recipient {
        if count != 1 || !kind.matches(&env.recipients[0].reference) {
            return Err(Invalid("recipients"));
        }
    }
    // Delivery.
    let d = &env.delivery;
    if d.priority != spec.priority {
        return Err(PriorityMismatch);
    }
    if !printable_key(&d.dedupe_key, 200) {
        return Err(Invalid("delivery.dedupeKey"));
    }
    for (name, v, max) in [
        ("delivery.coalesceKey", &d.coalesce_key, 200),
        ("delivery.cancelKey", &d.cancel_key, 200),
    ] {
        if v.as_deref()
            .is_some_and(|s| s.len() > max || !safe_string(s))
        {
            return Err(Invalid(name));
        }
    }
    for (name, v) in [
        ("delivery.deliverAfter", &d.deliver_after),
        ("delivery.expiresAt", &d.expires_at),
    ] {
        if v.as_deref().is_some_and(|s| !rfc3339(s)) {
            return Err(Invalid(name));
        }
    }
    match (&d.idempotency_key, spec.idempotency_key) {
        (Some(k), _) if !printable_key(k, 160) => return Err(Invalid("delivery.idempotencyKey")),
        (None, Some(_)) => return Err(IdempotencyKeyRequired),
        (Some(k), Some(format)) if !format.matches(k) => return Err(IdempotencyKeyFormat),
        _ => {}
    }
    if spec.is_control() && d.cancel_key.as_deref().is_none_or(str::is_empty) {
        return Err(Invalid("delivery.cancelKey"));
    }
    // Route (protocol 1).
    if env.route.protocol != 1 || env.route.routing_version < 0 {
        return Err(Invalid("route"));
    }
    // Data: envelope-wide key rule, then the catalogue's key set.
    if env.data.len() > MAX_DATA_KEYS {
        return Err(Invalid("data"));
    }
    for (k, v) in &env.data {
        if !allowed_data_key(k) {
            return Err(DataKey(k.clone()));
        }
        if !spec.required_data.contains(&k.as_str()) && !spec.optional_data.contains(&k.as_str()) {
            return Err(DataKey(k.clone()));
        }
        if !data_value_ok(v) {
            return Err(DataValue(k.clone()));
        }
        if let Some((_, allowed)) = spec.closed_data.iter().find(|(key, _)| *key == k) {
            if !v.as_str().is_some_and(|s| allowed.contains(&s)) {
                return Err(DataValue(k.clone()));
            }
        }
    }
    if let Some(missing) = spec
        .required_data
        .iter()
        .find(|k| !env.data.contains_key(**k))
    {
        return Err(MissingData(missing));
    }
    // `account.security.email-changed`: the legacy key names the side (`email-changed:old:`
    // / `email-changed:new:`), and `side` must be that side.
    if spec.event_type == "account.security.email-changed" {
        let side = env.data.get("side").and_then(Value::as_str).unwrap_or("");
        let key_side = d
            .idempotency_key
            .as_deref()
            .and_then(|k| k.strip_prefix("email-changed:"))
            .and_then(|k| k.split_once(':'))
            .map(|(s, _)| s.to_ascii_uppercase());
        if key_side.as_deref() != Some(side) {
            return Err(DataValue("side".into()));
        }
    }
    // Sealed part: only for types that declare it, and then mandatory.
    match (&env.sealed, spec.sealed_allowed()) {
        (Some(_), false) => return Err(SealedNotAllowed),
        (None, true) => return Err(SealedRequired(spec.sealed[0].as_str())),
        (Some(s), true) => {
            if !seal::valid_kid(&s.kid) || s.enc.len() != 43 || !(22..=5462).contains(&s.ct.len()) {
                return Err(Invalid("sealed"));
            }
        }
        (None, false) => {}
    }
    // Size.
    let limit = if env.sealed.is_some() {
        MAX_SEALED_ENVELOPE_BYTES
    } else {
        MAX_ENVELOPE_BYTES
    };
    let size = serde_json::to_vec(env)
        .map(|v| v.len())
        .unwrap_or(usize::MAX);
    if size > limit {
        return Err(TooLarge(limit));
    }
    Ok(spec)
}

/// Builds a valid envelope; [`EnvelopeBuilder::build`] runs [`validate`].
#[derive(Debug, Clone)]
pub struct EnvelopeBuilder {
    spec: &'static EventSpec,
    env: Envelope,
    plaintext: Option<SealedPlaintext>,
}

impl EnvelopeBuilder {
    /// Starts an envelope of `event_type` from `producer`, with a fresh `eventId` and
    /// correlation id and the catalogue's priority.
    pub fn new(
        producer: Producer,
        event_type: &str,
        occurred_at: &str,
        route: RouteRef,
    ) -> Result<Self, EnvelopeError> {
        let spec = catalogue::spec(event_type).ok_or(EnvelopeError::UnknownEventType)?;
        if !spec.allows_producer(producer) {
            return Err(EnvelopeError::ProducerNotAllowed);
        }
        Ok(Self {
            spec,
            env: Envelope {
                schema_version: SCHEMA_VERSION,
                event_id: format!("evt_{}", Uuid::new_v4().simple()),
                event_type: spec.event_type.to_string(),
                event_version: 1,
                producer: producer.as_str().to_string(),
                occurred_at: occurred_at.to_string(),
                correlation_id: format!("corr_{}", Uuid::new_v4().simple()),
                scope: None,
                subject: None,
                recipients: Vec::new(),
                delivery: Delivery {
                    priority: spec.priority,
                    dedupe_key: String::new(),
                    idempotency_key: None,
                    coalesce_key: None,
                    deliver_after: None,
                    expires_at: None,
                    cancel_key: None,
                },
                route,
                data: BTreeMap::new(),
                sealed: None,
            },
            plaintext: None,
        })
    }

    /// The catalogue entry of this envelope.
    pub fn spec(&self) -> &'static EventSpec {
        self.spec
    }

    /// Uses an existing event id (an outbox row id, so a relay re-post is a DUPLICATE).
    pub fn event_id(mut self, id: impl Into<String>) -> Self {
        self.env.event_id = id.into();
        self
    }

    pub fn correlation_id(mut self, id: impl Into<String>) -> Self {
        self.env.correlation_id = id.into();
        self
    }

    pub fn event_version(mut self, v: u8) -> Self {
        self.env.event_version = v;
        self
    }

    pub fn scope(mut self, scope: Scope) -> Self {
        self.env.scope = Some(scope);
        self
    }

    pub fn subject(mut self, id: Uuid, version: Option<i64>) -> Self {
        if let Some(kind) = self.spec.subject_kind {
            self.env.subject = Some(Subject {
                kind: kind.to_string(),
                id,
                version,
            });
        }
        self
    }

    pub fn recipient(mut self, reference: impl Into<String>) -> Self {
        self.env.recipients.push(Recipient {
            reference: reference.into(),
        });
        self
    }

    pub fn dedupe_key(mut self, key: impl Into<String>) -> Self {
        self.env.delivery.dedupe_key = key.into();
        self
    }

    pub fn idempotency_key(mut self, key: impl Into<String>) -> Self {
        self.env.delivery.idempotency_key = Some(key.into());
        self
    }

    pub fn coalesce_key(mut self, key: impl Into<String>) -> Self {
        self.env.delivery.coalesce_key = Some(key.into());
        self
    }

    pub fn deliver_after(mut self, at: impl Into<String>) -> Self {
        self.env.delivery.deliver_after = Some(at.into());
        self
    }

    pub fn expires_at(mut self, at: impl Into<String>) -> Self {
        self.env.delivery.expires_at = Some(at.into());
        self
    }

    pub fn cancel_key(mut self, key: impl Into<String>) -> Self {
        self.env.delivery.cancel_key = Some(key.into());
        self
    }

    pub fn data(mut self, key: &str, value: impl Into<Value>) -> Self {
        self.env.data.insert(key.to_string(), value.into());
        self
    }

    /// Codes, tokens and addresses: sealed at [`build_sealed`](Self::build_sealed).
    pub fn secret(mut self, plaintext: SealedPlaintext) -> Self {
        self.plaintext = Some(plaintext);
        self
    }

    /// A one-time token and its delivery address, sealed at
    /// [`build_sealed`](Self::build_sealed). For producers that hand over a claim link and
    /// never handle the sealed form themselves.
    pub fn sealed_token_for(self, token: impl Into<String>, address: impl Into<String>) -> Self {
        self.secret(
            SealedPlaintext::default()
                .with_token(token)
                .with_address(address),
        )
    }

    /// Builds an envelope that carries no sealed part.
    pub fn build(self) -> Result<Envelope, EnvelopeError> {
        if self.plaintext.is_some() {
            return Err(EnvelopeError::Invalid(
                "secret given without a sealing key (use build_sealed)",
            ));
        }
        validate(&self.env, None)?;
        Ok(self.env)
    }

    /// Seals the secret to the service key and builds the envelope. The sealed fields must
    /// be exactly those the catalogue declares for this type.
    pub fn build_sealed(mut self, key: &SealingKey) -> Result<Envelope, EnvelopeError> {
        let plaintext = self.plaintext.take().ok_or(EnvelopeError::SealedRequired(
            self.spec.sealed.first().map_or("a secret", |f| f.as_str()),
        ))?;
        if !self.spec.sealed_allowed() {
            return Err(EnvelopeError::SealedNotAllowed);
        }
        let fields = plaintext.fields();
        if let Some(missing) = self.spec.sealed.iter().find(|f| !fields.contains(f)) {
            return Err(EnvelopeError::SealedRequired(missing.as_str()));
        }
        if fields.iter().any(|f| !self.spec.seals(*f)) {
            return Err(EnvelopeError::SealedNotAllowed);
        }
        // `account.security.email-changed`: the notice to the previous address carries the
        // masked new address (and the undo token of a self-service change); the notice to
        // the new address carries neither.
        if self.spec.event_type == "account.security.email-changed" {
            match self.env.data.get("side").and_then(Value::as_str) {
                Some("OLD") if plaintext.masked_address.is_none() => {
                    return Err(EnvelopeError::SealedRequired("maskedAddress"))
                }
                Some("NEW") if plaintext.masked_address.is_some() || plaintext.token.is_some() => {
                    return Err(EnvelopeError::SealedNotAllowed)
                }
                _ => {}
            }
        }
        self.env.sealed = Some(seal::seal(
            key,
            &self.env.producer,
            &self.env.event_id,
            &self.env.event_type,
            &plaintext,
        )?);
        validate(&self.env, None)?;
        Ok(self.env)
    }
}
