//! Part VI §55 unified usage facts (Appendix D).
//!
//! A [`UsageFact`] is one immutable, provider-independent consumption fact of an
//! application environment, produced by the domain service that owns it. It carries the
//! authoritative tenant / application / environment ids, a SAFE resource reference (an
//! opaque `kind:uuid`, never a provider id, number, address or content), a quantity in
//! the type's canonical [`Unit`], the producer's stable `source_fact_id` and the
//! canonical payload [`UsageFact::digest`]. The Developer Platform deduplicates on
//! `(producer, source_fact_id)` and refuses a replay whose digest differs.
//!
//! It is NOT a price, rate, invoice or balance: quantities are billing-neutral.

use std::collections::BTreeMap;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[cfg(feature = "exporter")]
pub mod exporter;

/// Producers (service ids).
pub mod producers {
    pub const API_GATEWAY: &str = "cfmeet.api-gateway";
    pub const MEETING_SERVICE: &str = "cfmeet.meeting-service";
    pub const CALLING: &str = "cfmeet.calling";
    pub const NUMBERING: &str = "cfmeet.numbering";
    pub const SMS_GATEWAY: &str = "cfmeet.sms-gateway";
    pub const MESSAGING: &str = "cfmeet.messaging";
}

/// The canonical unit of a usage type's quantity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Operation,
    Millisecond,
    Segment,
    /// Stored bytes × seconds over the metering interval.
    ByteSecond,
}

impl Unit {
    pub fn as_str(&self) -> &'static str {
        match self {
            Unit::Operation => "OPERATION",
            Unit::Millisecond => "MILLISECOND",
            Unit::Segment => "SEGMENT",
            Unit::ByteSecond => "BYTE_SECOND",
        }
    }
}

/// One entry of the usage catalogue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageTypeDefinition {
    pub name: &'static str,
    pub unit: Unit,
    /// Services allowed to produce it (the authenticated caller, never a body claim).
    pub producers: &'static [&'static str],
    /// Whether the fact describes a metering interval (`periodStart` / `periodEnd`).
    pub interval: bool,
    /// Dimension keys a fact may carry.
    pub dimensions: &'static [&'static str],
    /// Dimension keys never shown to customers (provider-independent cost evidence).
    pub internal_dimensions: &'static [&'static str],
}

use producers::*;

/// The eight Part VI usage types (Appendix D).
pub const CATALOGUE: &[UsageTypeDefinition] = &[
    UsageTypeDefinition {
        name: "API_OPERATION",
        unit: Unit::Operation,
        producers: &[API_GATEWAY],
        interval: false,
        dimensions: &[
            "domain",
            "operationId",
            "statusClass",
            "credentialId",
            "serviceAccountId",
        ],
        internal_dimensions: &[],
    },
    UsageTypeDefinition {
        name: "MEETING_PARTICIPANT_MINUTE",
        unit: Unit::Millisecond,
        producers: &[MEETING_SERVICE],
        interval: false,
        dimensions: &["conferenceId", "participantKind"],
        internal_dimensions: &[],
    },
    UsageTypeDefinition {
        name: "CALL_MINUTE_APP",
        unit: Unit::Millisecond,
        producers: &[CALLING],
        interval: false,
        dimensions: &["callId"],
        internal_dimensions: &[],
    },
    UsageTypeDefinition {
        name: "CALL_MINUTE_PUBLIC_NETWORK",
        unit: Unit::Millisecond,
        producers: &[CALLING],
        interval: false,
        dimensions: &[
            "callId",
            "routeClass",
            "destinationCountry",
            "billableSeconds",
            "costEvidence",
            "costMinor",
            "costCurrency",
        ],
        internal_dimensions: &["costMinor", "costCurrency"],
    },
    UsageTypeDefinition {
        name: "NUMBER_ACTIVE",
        unit: Unit::Millisecond,
        producers: &[NUMBERING],
        interval: true,
        dimensions: &["numberType", "countryClass", "capabilityClass"],
        internal_dimensions: &[],
    },
    UsageTypeDefinition {
        name: "SMS_SEGMENT",
        unit: Unit::Segment,
        producers: &[SMS_GATEWAY],
        interval: false,
        dimensions: &["direction", "countryClass", "deliveryCategory"],
        internal_dimensions: &[],
    },
    UsageTypeDefinition {
        name: "RECORDING_MINUTE",
        unit: Unit::Millisecond,
        producers: &[MEETING_SERVICE, CALLING],
        interval: false,
        dimensions: &["recordingDomain"],
        internal_dimensions: &[],
    },
    UsageTypeDefinition {
        name: "STORAGE_BYTE_TIME",
        unit: Unit::ByteSecond,
        producers: &[MEETING_SERVICE, CALLING, MESSAGING],
        interval: true,
        dimensions: &["storageDomain", "storedBytes"],
        internal_dimensions: &[],
    },
];

pub fn definition(usage_type: &str) -> Option<&'static UsageTypeDefinition> {
    CATALOGUE.iter().find(|d| d.name == usage_type)
}

/// Every usage type name.
pub fn usage_types() -> impl Iterator<Item = &'static str> {
    CATALOGUE.iter().map(|d| d.name)
}

/// Upper bound of one fact's quantity.
pub const MAX_QUANTITY: u64 = 1_000_000_000_000_000;
/// Upper bound of facts in one ingestion request.
pub const MAX_BATCH: usize = 500;

/// One usage fact (wire format, camelCase).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UsageFact {
    pub source_fact_id: String,
    pub usage_type: String,
    pub unit: String,
    pub quantity: u64,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_kind: Option<String>,
    pub resource_ref: String,
    pub occurred_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period_start: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period_end: Option<DateTime<Utc>>,
    #[serde(default)]
    pub dimensions: BTreeMap<String, Value>,
    #[serde(default)]
    pub digest: String,
}

/// Why a fact is refused (stable codes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    MalformedSourceFactId,
    UnknownUsageType,
    WrongUnit,
    ProducerNotAllowed,
    MalformedResourceRef,
    QuantityOutOfRange,
    InvalidPeriod,
    InvalidEnvironmentKind,
    InvalidDimensions,
    DigestMismatch,
}

impl Refusal {
    pub fn code(&self) -> &'static str {
        match self {
            Refusal::MalformedSourceFactId => "MALFORMED_SOURCE_FACT_ID",
            Refusal::UnknownUsageType => "UNKNOWN_USAGE_TYPE",
            Refusal::WrongUnit => "WRONG_UNIT",
            Refusal::ProducerNotAllowed => "PRODUCER_NOT_ALLOWED",
            Refusal::MalformedResourceRef => "MALFORMED_RESOURCE_REF",
            Refusal::QuantityOutOfRange => "QUANTITY_OUT_OF_RANGE",
            Refusal::InvalidPeriod => "INVALID_PERIOD",
            Refusal::InvalidEnvironmentKind => "INVALID_ENVIRONMENT_KIND",
            Refusal::InvalidDimensions => "INVALID_DIMENSIONS",
            Refusal::DigestMismatch => "DIGEST_MISMATCH",
        }
    }
}

/// `^[A-Za-z0-9][A-Za-z0-9._:-]{0,159}$`.
pub fn valid_ref(value: &str) -> bool {
    let b = value.as_bytes();
    (1..=160).contains(&b.len())
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || b"._:-".contains(c))
}

fn valid_dimension_value(v: &Value) -> bool {
    match v {
        Value::String(s) => {
            (1..=64).contains(&s.len())
                && s.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"._:-".contains(&c))
        }
        Value::Number(n) => n.as_u64().is_some(),
        Value::Bool(_) => true,
        _ => false,
    }
}

fn ts(t: &DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Micros, true)
}

impl UsageFact {
    /// The canonical payload digest: SHA-256 over a length-prefixed canonical encoding
    /// of every field but `digest` (timestamps at microsecond precision, dimensions in
    /// key order). Producer and Developer Platform compute it identically.
    pub fn compute_digest(&self) -> String {
        let mut h = Sha256::new();
        let mut part = |s: &str| {
            h.update((s.len() as u64).to_be_bytes());
            h.update(s.as_bytes());
        };
        part("cfmeet.usage-fact.v1");
        part(&self.source_fact_id);
        part(&self.usage_type);
        part(&self.unit);
        part(&self.quantity.to_string());
        part(&self.tenant_id.to_string());
        part(&self.application_id.to_string());
        part(&self.environment_id.to_string());
        part(self.environment_kind.as_deref().unwrap_or(""));
        part(&self.resource_ref);
        part(&ts(&self.occurred_at));
        part(&self.period_start.as_ref().map(ts).unwrap_or_default());
        part(&self.period_end.as_ref().map(ts).unwrap_or_default());
        for (k, v) in &self.dimensions {
            part(k);
            part(&v.to_string());
        }
        hex::encode(h.finalize())
    }

    /// Sets the digest (producers call this last).
    pub fn sealed(mut self) -> Self {
        self.digest = self.compute_digest();
        self
    }

    /// Truncates every timestamp to microseconds (the storage precision of both sides).
    pub fn normalized(mut self) -> Self {
        fn micros(t: DateTime<Utc>) -> DateTime<Utc> {
            DateTime::<Utc>::from_timestamp_micros(t.timestamp_micros()).unwrap_or(t)
        }
        self.occurred_at = micros(self.occurred_at);
        self.period_start = self.period_start.map(micros);
        self.period_end = self.period_end.map(micros);
        self
    }

    /// Validates the fact of `producer` against the catalogue (the digest included).
    pub fn validate(&self, producer: &str) -> Result<&'static UsageTypeDefinition, Refusal> {
        if !valid_ref(&self.source_fact_id) {
            return Err(Refusal::MalformedSourceFactId);
        }
        let def = definition(&self.usage_type).ok_or(Refusal::UnknownUsageType)?;
        if def.unit.as_str() != self.unit {
            return Err(Refusal::WrongUnit);
        }
        if !def.producers.contains(&producer) {
            return Err(Refusal::ProducerNotAllowed);
        }
        if !valid_ref(&self.resource_ref) {
            return Err(Refusal::MalformedResourceRef);
        }
        if self.quantity > MAX_QUANTITY {
            return Err(Refusal::QuantityOutOfRange);
        }
        match (self.period_start, self.period_end) {
            (Some(s), Some(e)) if e < s => return Err(Refusal::InvalidPeriod),
            (Some(_), Some(_)) => {}
            (None, None) if !def.interval => {}
            _ => return Err(Refusal::InvalidPeriod),
        }
        if let Some(k) = &self.environment_kind {
            if !matches!(k.as_str(), "SANDBOX" | "TEST" | "PRODUCTION") {
                return Err(Refusal::InvalidEnvironmentKind);
            }
        }
        if self.dimensions.len() > 12
            || self
                .dimensions
                .iter()
                .any(|(k, v)| !def.dimensions.contains(&k.as_str()) || !valid_dimension_value(v))
        {
            return Err(Refusal::InvalidDimensions);
        }
        if self.digest != self.compute_digest() {
            return Err(Refusal::DigestMismatch);
        }
        Ok(def)
    }

    /// The dimensions a customer may see (internal cost evidence removed).
    pub fn public_dimensions(usage_type: &str, dimensions: &Value) -> Value {
        let hidden = definition(usage_type)
            .map(|d| d.internal_dimensions)
            .unwrap_or(&[]);
        match dimensions {
            Value::Object(m) => Value::Object(
                m.iter()
                    .filter(|(k, _)| !hidden.contains(&k.as_str()))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            ),
            _ => Value::Object(Default::default()),
        }
    }
}

/// Byte-seconds / milliseconds an interval `[from, until)` overlaps `[start, end)`.
pub fn overlap_seconds(
    from: DateTime<Utc>,
    until: Option<DateTime<Utc>>,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> i64 {
    let lo = from.max(start);
    let hi = until.unwrap_or(end).min(end);
    (hi - lo).num_seconds().max(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fact() -> UsageFact {
        UsageFact {
            source_fact_id: "sms:7c9e6679-7425-40de-944b-e07fc1f90ae7".into(),
            usage_type: "SMS_SEGMENT".into(),
            unit: "SEGMENT".into(),
            quantity: 2,
            tenant_id: Uuid::new_v4(),
            application_id: Uuid::new_v4(),
            environment_id: Uuid::new_v4(),
            environment_kind: Some("SANDBOX".into()),
            resource_ref: "sms:7c9e6679-7425-40de-944b-e07fc1f90ae7".into(),
            occurred_at: Utc::now(),
            period_start: None,
            period_end: None,
            dimensions: [("direction".to_string(), json!("OUTBOUND"))].into(),
            digest: String::new(),
        }
        .normalized()
        .sealed()
    }

    #[test]
    fn the_catalogue_has_the_eight_part_vi_types() {
        let names: Vec<_> = usage_types().collect();
        assert_eq!(
            names,
            [
                "API_OPERATION",
                "MEETING_PARTICIPANT_MINUTE",
                "CALL_MINUTE_APP",
                "CALL_MINUTE_PUBLIC_NETWORK",
                "NUMBER_ACTIVE",
                "SMS_SEGMENT",
                "RECORDING_MINUTE",
                "STORAGE_BYTE_TIME"
            ]
        );
    }

    #[test]
    fn a_valid_fact_passes_and_its_digest_is_stable_across_the_wire() {
        let f = fact();
        assert!(f.validate(producers::SMS_GATEWAY).is_ok());
        let wire = serde_json::to_string(&f).unwrap();
        let back: UsageFact = serde_json::from_str(&wire).unwrap();
        assert_eq!(back.compute_digest(), f.digest);
    }

    #[test]
    fn producers_types_units_refs_dimensions_and_digests_are_enforced() {
        let f = fact();
        assert_eq!(
            f.validate(producers::CALLING),
            Err(Refusal::ProducerNotAllowed)
        );
        let mut g = f.clone();
        g.quantity = 3;
        assert_eq!(
            g.validate(producers::SMS_GATEWAY),
            Err(Refusal::DigestMismatch)
        );
        let mut g = f.clone();
        g.unit = "MILLISECOND".into();
        assert_eq!(g.validate(producers::SMS_GATEWAY), Err(Refusal::WrongUnit));
        let mut g = f.clone();
        g.resource_ref = "+15551234567".into();
        assert_eq!(
            g.sealed().validate(producers::SMS_GATEWAY),
            Err(Refusal::MalformedResourceRef)
        );
        let mut g = f.clone();
        g.dimensions.insert("body".into(), json!("hello"));
        assert_eq!(
            g.sealed().validate(producers::SMS_GATEWAY),
            Err(Refusal::InvalidDimensions)
        );
        let mut g = f.clone();
        g.dimensions.insert("direction".into(), json!("has space"));
        assert_eq!(
            g.sealed().validate(producers::SMS_GATEWAY),
            Err(Refusal::InvalidDimensions)
        );
        let mut g = f;
        g.usage_type = "STORAGE_BYTE_TIME".into();
        g.unit = "BYTE_SECOND".into();
        g.dimensions.clear();
        // Interval types need their metering period.
        assert_eq!(
            g.sealed().validate(producers::MESSAGING),
            Err(Refusal::InvalidPeriod)
        );
    }

    #[test]
    fn cost_evidence_is_internal() {
        let d = json!({ "routeClass": "PSTN", "costMinor": 12, "costCurrency": "USD" });
        assert_eq!(
            UsageFact::public_dimensions("CALL_MINUTE_PUBLIC_NETWORK", &d),
            json!({ "routeClass": "PSTN" })
        );
    }

    #[test]
    fn interval_overlap_is_clamped() {
        let d = |h: i64| {
            DateTime::parse_from_rfc3339("2026-10-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc)
                + chrono::Duration::hours(h)
        };
        assert_eq!(overlap_seconds(d(-5), None, d(0), d(24)), 86_400);
        assert_eq!(overlap_seconds(d(6), Some(d(12)), d(0), d(24)), 21_600);
        assert_eq!(overlap_seconds(d(30), None, d(0), d(24)), 0);
    }
}
