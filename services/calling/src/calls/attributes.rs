//! Bounded, typed, privacy-safe key/value data.
//!
//! Every free-form JSON the call domain persists — authorization policy and limits
//! snapshots, route decision snapshots, event payloads — goes through [`SafeAttributes`]:
//!
//! * at most [`SafeAttributes::MAX_ENTRIES`] flat entries; no nested objects;
//! * snake_case keys of at most 63 characters, never naming secrets, credentials, contact
//!   data or provider / carrier identifiers;
//! * values are booleans, integers, finite numbers, short strings or short string lists;
//! * strings are bounded, contain no control characters and never look like a telephone
//!   number (`+<digit>`) or a `tel:` / `sip:` URI.
//!
//! The same rules are expressed by the shared contract `cfmeet.calling.call-common`
//! (`SafeEventData`), so every persisted payload is publishable as is.

use std::collections::BTreeMap;

use serde_json::{Map, Number, Value};

/// Key fragments that are never accepted (secrets, credentials, contact data, provider /
/// carrier identifiers). Mirrored by the `SafeEventData` contract.
pub const FORBIDDEN_KEY_FRAGMENTS: &[&str] = &[
    "password",
    "secret",
    "token",
    "credential",
    "api_key",
    "apikey",
    "phone",
    "msisdn",
    "e164",
    "contact",
    "provider",
    "carrier",
    "vendor",
    "sip",
    "pstn",
    "dialog",
    "room_sid",
];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AttributeError {
    #[error("too many entries (at most {0})")]
    TooManyEntries(usize),
    #[error("invalid key")]
    InvalidKey,
    #[error("key names protected or provider data")]
    ForbiddenKey,
    #[error("invalid value")]
    InvalidValue,
    #[error("value looks like contact data")]
    ContactLikeValue,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SafeValue {
    Bool(bool),
    Integer(i64),
    Number(f64),
    Text(String),
    TextList(Vec<String>),
}

impl From<bool> for SafeValue {
    fn from(v: bool) -> Self {
        SafeValue::Bool(v)
    }
}

impl From<i64> for SafeValue {
    fn from(v: i64) -> Self {
        SafeValue::Integer(v)
    }
}

impl From<i32> for SafeValue {
    fn from(v: i32) -> Self {
        SafeValue::Integer(v.into())
    }
}

impl From<usize> for SafeValue {
    fn from(v: usize) -> Self {
        SafeValue::Integer(i64::try_from(v).unwrap_or(i64::MAX))
    }
}

impl From<&str> for SafeValue {
    fn from(v: &str) -> Self {
        SafeValue::Text(v.to_string())
    }
}

impl From<String> for SafeValue {
    fn from(v: String) -> Self {
        SafeValue::Text(v)
    }
}

impl From<Vec<String>> for SafeValue {
    fn from(v: Vec<String>) -> Self {
        SafeValue::TextList(v)
    }
}

/// Whether `value` looks like a telephone number or a telephony URI.
pub fn looks_like_contact(value: &str) -> bool {
    let bytes = value.as_bytes();
    let plus_digit = bytes
        .windows(2)
        .any(|w| w[0] == b'+' && w[1].is_ascii_digit());
    let lower = value.trim_start().to_ascii_lowercase();
    plus_digit
        || ["tel:", "sip:", "sips:"]
            .iter()
            .any(|p| lower.starts_with(p))
}

pub fn validate_key(key: &str) -> Result<(), AttributeError> {
    let bytes = key.as_bytes();
    let well_formed = !bytes.is_empty()
        && bytes.len() <= SafeAttributes::MAX_KEY_LEN
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_');
    if !well_formed {
        return Err(AttributeError::InvalidKey);
    }
    if FORBIDDEN_KEY_FRAGMENTS.iter().any(|f| key.contains(f)) {
        return Err(AttributeError::ForbiddenKey);
    }
    Ok(())
}

fn validate_text(value: &str) -> Result<(), AttributeError> {
    if value.chars().count() > SafeAttributes::MAX_TEXT_CHARS || value.chars().any(char::is_control)
    {
        return Err(AttributeError::InvalidValue);
    }
    if looks_like_contact(value) {
        return Err(AttributeError::ContactLikeValue);
    }
    Ok(())
}

fn validate_value(value: &SafeValue) -> Result<(), AttributeError> {
    match value {
        SafeValue::Bool(_) | SafeValue::Integer(_) => Ok(()),
        SafeValue::Number(n) if n.is_finite() => Ok(()),
        SafeValue::Number(_) => Err(AttributeError::InvalidValue),
        SafeValue::Text(s) => validate_text(s),
        SafeValue::TextList(items) => {
            if items.len() > SafeAttributes::MAX_LIST_ITEMS {
                return Err(AttributeError::InvalidValue);
            }
            items.iter().try_for_each(|s| validate_text(s))
        }
    }
}

/// Bounded, flat, privacy-safe attribute map (see the module documentation).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SafeAttributes(BTreeMap<String, SafeValue>);

impl SafeAttributes {
    pub const MAX_ENTRIES: usize = 32;
    pub const MAX_KEY_LEN: usize = 63;
    pub const MAX_TEXT_CHARS: usize = 256;
    pub const MAX_LIST_ITEMS: usize = 16;

    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn get(&self, key: &str) -> Option<&SafeValue> {
        self.0.get(key)
    }

    /// Adds or replaces an entry after validating key and value.
    pub fn insert(
        &mut self,
        key: &str,
        value: impl Into<SafeValue>,
    ) -> Result<&mut Self, AttributeError> {
        let value = value.into();
        validate_key(key)?;
        validate_value(&value)?;
        if !self.0.contains_key(key) && self.0.len() >= Self::MAX_ENTRIES {
            return Err(AttributeError::TooManyEntries(Self::MAX_ENTRIES));
        }
        self.0.insert(key.to_string(), value);
        Ok(self)
    }

    /// Builder form of [`insert`](Self::insert).
    pub fn with(mut self, key: &str, value: impl Into<SafeValue>) -> Result<Self, AttributeError> {
        self.insert(key, value)?;
        Ok(self)
    }

    /// Parses and validates an untrusted JSON object.
    pub fn from_json(value: &Value) -> Result<Self, AttributeError> {
        let object = value.as_object().ok_or(AttributeError::InvalidValue)?;
        if object.len() > Self::MAX_ENTRIES {
            return Err(AttributeError::TooManyEntries(Self::MAX_ENTRIES));
        }
        let mut out = Self::new();
        for (key, v) in object {
            let value = match v {
                Value::Bool(b) => SafeValue::Bool(*b),
                Value::Number(n) => match n.as_i64() {
                    Some(i) => SafeValue::Integer(i),
                    None => SafeValue::Number(n.as_f64().ok_or(AttributeError::InvalidValue)?),
                },
                Value::String(s) => SafeValue::Text(s.clone()),
                Value::Array(items) => SafeValue::TextList(
                    items
                        .iter()
                        .map(|i| {
                            i.as_str()
                                .map(str::to_string)
                                .ok_or(AttributeError::InvalidValue)
                        })
                        .collect::<Result<_, _>>()?,
                ),
                Value::Null | Value::Object(_) => return Err(AttributeError::InvalidValue),
            };
            out.insert(key, value)?;
        }
        Ok(out)
    }

    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        for (k, v) in &self.0 {
            let value = match v {
                SafeValue::Bool(b) => Value::Bool(*b),
                SafeValue::Integer(i) => Value::Number((*i).into()),
                SafeValue::Number(n) => Number::from_f64(*n)
                    .map(Value::Number)
                    .unwrap_or(Value::Null),
                SafeValue::Text(s) => Value::String(s.clone()),
                SafeValue::TextList(items) => {
                    Value::Array(items.iter().cloned().map(Value::String).collect())
                }
            };
            map.insert(k.clone(), value);
        }
        Value::Object(map)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn accepts_bounded_flat_data() {
        let attrs = SafeAttributes::new()
            .with("action", "ACCEPT")
            .unwrap()
            .with("session_count", 2)
            .unwrap()
            .with("evidence_complete", true)
            .unwrap()
            .with(
                "session_ids",
                vec!["6f1c2b9e-3a4d-4c5e-8f70-112233445566".to_string()],
            )
            .unwrap();
        let round = SafeAttributes::from_json(&attrs.to_json()).unwrap();
        assert_eq!(round, attrs);
    }

    #[test]
    fn refuses_protected_keys_and_contact_like_values() {
        for key in [
            "phone",
            "caller_phone_number",
            "provider_call_id",
            "carrier_status",
            "api_key",
            "auth_token",
            "sip_dialog",
            "contact",
            "room_sid",
        ] {
            assert!(SafeAttributes::new().with(key, "x").is_err(), "{key}");
        }
        for bad in ["Has-Caps", "1st", "", &"k".repeat(64), "a.b"] {
            assert_eq!(
                SafeAttributes::new().with(bad, "x").unwrap_err(),
                AttributeError::InvalidKey,
                "{bad}"
            );
        }
        for value in [
            "+2348012345678",
            "tel:0801",
            "SIP:alice@example.org",
            "x +1",
        ] {
            assert_eq!(
                SafeAttributes::new().with("note", value).unwrap_err(),
                AttributeError::ContactLikeValue
            );
        }
        assert!(SafeAttributes::new().with("note", "a\nb").is_err());
        assert!(SafeAttributes::new().with("note", "x".repeat(257)).is_err());
        assert!(SafeAttributes::new()
            .with("n", SafeValue::Number(f64::NAN))
            .is_err());
    }

    #[test]
    fn is_bounded_and_flat() {
        let mut attrs = SafeAttributes::new();
        for i in 0..SafeAttributes::MAX_ENTRIES {
            attrs.insert(&format!("k{i}"), 1).unwrap();
        }
        assert!(attrs.insert("one_more", 1).is_err());
        assert!(SafeAttributes::from_json(&json!({"nested": {"a": 1}})).is_err());
        assert!(SafeAttributes::from_json(&json!({"nil": null})).is_err());
        assert!(SafeAttributes::from_json(&json!([1, 2])).is_err());
        let list: Vec<String> = (0..17).map(|i| i.to_string()).collect();
        assert!(SafeAttributes::new().with("ids", list).is_err());
    }
}
