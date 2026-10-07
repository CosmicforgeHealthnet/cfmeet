//! Provider-independent call quality records (Part C §16).
//!
//! A record summarizes an observation window of one session, one leg or the whole Call as
//! a bounded set of named, finite numeric metrics. No provider payloads, no strings, no
//! contact data. Detailed quality analysis arrives later.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Number, Value};

use super::{
    attributes::validate_key,
    errors::CallError,
    ids::{CallId, CallLegId, CallQualityRecordId, CallSessionId},
};
use crate::models::call_quality_records;

/// Bounded numeric metrics: at most 32 entries, snake_case names (never naming secrets,
/// contact data or providers), finite values only.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(transparent)]
pub struct QualityMetrics(BTreeMap<String, f64>);

impl QualityMetrics {
    pub const MAX_ENTRIES: usize = 32;

    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, name: &str, value: f64) -> Result<&mut Self, CallError> {
        validate_key(name).map_err(|_| CallError::Validation("invalid quality metric name"))?;
        if !value.is_finite() {
            return Err(CallError::Validation(
                "quality metrics must be finite numbers",
            ));
        }
        if !self.0.contains_key(name) && self.0.len() >= Self::MAX_ENTRIES {
            return Err(CallError::Validation("too many quality metrics"));
        }
        self.0.insert(name.to_string(), value);
        Ok(self)
    }

    pub fn with(mut self, name: &str, value: f64) -> Result<Self, CallError> {
        self.insert(name, value)?;
        Ok(self)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<f64> {
        self.0.get(name).copied()
    }

    pub fn to_json(&self) -> Value {
        Value::Object(
            self.0
                .iter()
                .filter_map(|(k, v)| Number::from_f64(*v).map(|n| (k.clone(), Value::Number(n))))
                .collect(),
        )
    }

    fn from_json(value: &Value) -> Self {
        Self(
            value
                .as_object()
                .map(|o| {
                    o.iter()
                        .filter_map(|(k, v)| v.as_f64().map(|f| (k.clone(), f)))
                        .collect()
                })
                .unwrap_or_default(),
        )
    }
}

/// A quality observation to record.
#[derive(Debug, Clone)]
pub struct NewQualityRecord {
    pub session_id: Option<CallSessionId>,
    pub leg_id: Option<CallLegId>,
    pub observed_from: DateTime<Utc>,
    pub observed_to: DateTime<Utc>,
    pub metrics: QualityMetrics,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CallQualityRecord {
    pub id: CallQualityRecordId,
    pub call_id: CallId,
    pub session_id: Option<CallSessionId>,
    pub leg_id: Option<CallLegId>,
    pub observed_from: DateTime<Utc>,
    pub observed_to: DateTime<Utc>,
    pub metrics: QualityMetrics,
}

impl From<call_quality_records::Model> for CallQualityRecord {
    fn from(m: call_quality_records::Model) -> Self {
        Self {
            id: CallQualityRecordId::from_uuid(m.id),
            call_id: CallId::from_uuid(m.call_id),
            session_id: m.session_id.map(CallSessionId::from_uuid),
            leg_id: m.leg_id.map(CallLegId::from_uuid),
            observed_from: m.observed_from,
            observed_to: m.observed_to,
            metrics: QualityMetrics::from_json(&m.metrics),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_are_bounded_named_and_finite() {
        let m = QualityMetrics::new()
            .with("packet_loss_ratio", 0.02)
            .unwrap()
            .with("jitter_ms", 12.5)
            .unwrap();
        assert_eq!(m.len(), 2);
        assert!(QualityMetrics::new().with("jitter", f64::INFINITY).is_err());
        assert!(QualityMetrics::new().with("jitter", f64::NAN).is_err());
        for bad in ["Jitter", "provider_mos", "phone_quality", "", "a-b"] {
            assert!(QualityMetrics::new().with(bad, 1.0).is_err(), "{bad}");
        }
        let mut full = QualityMetrics::new();
        for i in 0..QualityMetrics::MAX_ENTRIES {
            full.insert(&format!("m{i}"), 1.0).unwrap();
        }
        assert!(full.insert("one_more", 1.0).is_err());
        assert_eq!(QualityMetrics::from_json(&m.to_json()), m);
    }
}
