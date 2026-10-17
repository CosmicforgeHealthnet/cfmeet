//! Structured capability limitations and the claims Numbering never makes.
//!
//! Limitations are a small, closed JSON vocabulary (catalog entries and capability
//! records share it). Free-form promises are refused: CF Meet never states that a number
//! is accepted by WhatsApp, a bank, a social platform or any OTP / third-party
//! verification service — compatibility varies by service and number type, and every
//! voice / SMS capability carries [`EXTERNAL_VERIFICATION_NOTICE`].

use serde_json::{json, Map, Value};

use super::{Capability, DomainError, DomainResult};

/// The fixed wording attached to every voice / SMS capability.
pub const EXTERNAL_VERIFICATION_NOTICE: &str =
    "Supports voice and SMS where provisioned; third-party verification compatibility varies by service and number type.";

/// Words that state or imply acceptance by a third party.
const CLAIM_WORDS: &[&str] = &[
    "guarantee",
    "guaranteed",
    "whatsapp",
    "otp",
    "bank",
    "banking",
    "telegram",
    "signal",
    "facebook",
    "instagram",
    "google",
    "always accepted",
    "accepted by",
];

fn refuse(message: &str) -> DomainError {
    DomainError::precondition("LIMITATION_NOT_ALLOWED", message.to_string())
}

fn upper_snake(s: &str) -> bool {
    (2..=64).contains(&s.len())
        && s.as_bytes()[0].is_ascii_uppercase()
        && s.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

/// Validates a limitations object against the closed vocabulary.
///
/// | Key | Value |
/// |---|---|
/// | `destinationCountries` | ISO 3166-1 alpha-2 codes (max 250) |
/// | `throughputPerSecond` | integer 1–1000 |
/// | `contentRestrictions` | UPPER_SNAKE codes (max 32) |
/// | `registrationRequired` | boolean |
/// | `inboundSenderTypes` | `SHORT_CODE` / `LONG_CODE` / `ALPHANUMERIC` |
/// | `externalVerification` | `{ "compatibility": "VARIES" \| "NOT_SUPPORTED", "guaranteed": false }` |
/// | `note` | ≤ 200 characters, no acceptance claim |
pub fn validate(limitations: &Value) -> DomainResult<()> {
    let Some(map) = limitations.as_object() else {
        return Err(DomainError::Validation(
            "limitations are a JSON object".into(),
        ));
    };
    if limitations.to_string().len() > 4_000 {
        return Err(DomainError::Validation(
            "limitations are bounded (4 KB)".into(),
        ));
    }
    for (key, value) in map {
        match key.as_str() {
            "destinationCountries" => {
                let ok = value.as_array().is_some_and(|a| {
                    a.len() <= 250
                        && a.iter().all(|c| {
                            c.as_str().is_some_and(|c| {
                                c.len() == 2 && c.bytes().all(|b| b.is_ascii_uppercase())
                            })
                        })
                });
                if !ok {
                    return Err(DomainError::Validation(
                        "destinationCountries are ISO 3166-1 alpha-2 codes".into(),
                    ));
                }
            }
            "throughputPerSecond" => {
                if !value.as_i64().is_some_and(|n| (1..=1000).contains(&n)) {
                    return Err(DomainError::Validation(
                        "throughputPerSecond is 1–1000".into(),
                    ));
                }
            }
            "contentRestrictions" => {
                let ok = value.as_array().is_some_and(|a| {
                    a.len() <= 32 && a.iter().all(|c| c.as_str().is_some_and(upper_snake))
                });
                if !ok {
                    return Err(DomainError::Validation(
                        "contentRestrictions are UPPER_SNAKE codes".into(),
                    ));
                }
            }
            "registrationRequired" => {
                if !value.is_boolean() {
                    return Err(DomainError::Validation(
                        "registrationRequired is a boolean".into(),
                    ));
                }
            }
            "inboundSenderTypes" => {
                let ok = value.as_array().is_some_and(|a| {
                    a.iter().all(|c| {
                        matches!(
                            c.as_str(),
                            Some("SHORT_CODE" | "LONG_CODE" | "ALPHANUMERIC")
                        )
                    })
                });
                if !ok {
                    return Err(DomainError::Validation(
                        "inboundSenderTypes are SHORT_CODE / LONG_CODE / ALPHANUMERIC".into(),
                    ));
                }
            }
            "externalVerification" => {
                let ok = value.as_object().is_some_and(|o| {
                    o.len() == 2
                        && matches!(
                            o.get("compatibility").and_then(Value::as_str),
                            Some("VARIES" | "NOT_SUPPORTED")
                        )
                        && o.get("guaranteed") == Some(&Value::Bool(false))
                });
                if !ok {
                    return Err(refuse(
                        "third-party verification compatibility is never guaranteed (VARIES or NOT_SUPPORTED, guaranteed: false)",
                    ));
                }
            }
            "note" => {
                let Some(note) = value.as_str() else {
                    return Err(DomainError::Validation("note is a string".into()));
                };
                if note.chars().count() > 200 || note.chars().any(char::is_control) {
                    return Err(DomainError::Validation(
                        "note is at most 200 characters".into(),
                    ));
                }
                let lower = note.to_lowercase();
                if CLAIM_WORDS.iter().any(|w| lower.contains(w)) {
                    return Err(refuse(
                        "notes never claim acceptance by third-party services",
                    ));
                }
            }
            _ => return Err(refuse("unknown limitation key")),
        }
    }
    Ok(())
}

/// The limitations a resolved capability carries: the recorded ones, plus — for voice and
/// SMS — the fixed external-verification notice (never a guarantee).
pub fn effective(capability: Capability, catalog: &Value, record: &Value) -> Value {
    let mut out = Map::new();
    for source in [catalog, record] {
        if let Some(map) = source.as_object() {
            for (k, v) in map {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    if matches!(
        capability,
        Capability::VoiceInbound
            | Capability::VoiceOutbound
            | Capability::SmsInbound
            | Capability::SmsOutbound
            | Capability::MmsRich
    ) {
        out.insert(
            "externalVerification".into(),
            json!({ "compatibility": out.get("externalVerification")
                .and_then(|v| v.get("compatibility")).cloned().unwrap_or(json!("VARIES")),
                "guaranteed": false }),
        );
        out.insert("notice".into(), json!(EXTERNAL_VERIFICATION_NOTICE));
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guarantees_and_third_party_claims_are_refused() {
        for bad in [
            json!({ "externalVerification": { "compatibility": "VARIES", "guaranteed": true } }),
            json!({ "note": "Works with WhatsApp" }),
            json!({ "note": "OTP accepted everywhere" }),
            json!({ "note": "Guaranteed delivery" }),
            json!({ "otpGuaranteed": true }),
            json!({ "whatsapp": "supported" }),
        ] {
            assert_eq!(
                validate(&bad).unwrap_err().code(),
                "LIMITATION_NOT_ALLOWED",
                "{bad}"
            );
        }
        validate(&json!({
            "destinationCountries": ["NG", "GH"],
            "throughputPerSecond": 10,
            "registrationRequired": true,
            "externalVerification": { "compatibility": "VARIES", "guaranteed": false },
            "note": "Sender registration takes up to five days",
        }))
        .unwrap();
        let e = effective(
            Capability::SmsOutbound,
            &json!({}),
            &json!({ "throughputPerSecond": 5 }),
        );
        assert_eq!(e["notice"], EXTERNAL_VERIFICATION_NOTICE);
        assert_eq!(e["externalVerification"]["guaranteed"], false);
        assert!(
            effective(Capability::MobileNumberClass, &json!({}), &json!({}))
                .get("notice")
                .is_none()
        );
    }
}
