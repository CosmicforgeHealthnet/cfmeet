//! Input validation (contract §3 resource limits). Errors never echo the value.

use chrono::NaiveDate;
use serde::{Deserialize, Deserializer};

use crate::error::ApiError;

/// `Option<Option<T>>` for PATCH fields: absent → `None`, `null` → `Some(None)`.
pub fn double_option<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

/// Contract slug: `^[a-z0-9](?:[a-z0-9-]{1,61}[a-z0-9])$` (3–63 characters).
pub fn valid_slug(slug: &str) -> bool {
    let b = slug.as_bytes();
    let alnum = |c: &u8| c.is_ascii_lowercase() || c.is_ascii_digit();
    (3..=63).contains(&b.len())
        && alnum(&b[0])
        && alnum(&b[b.len() - 1])
        && b.iter().all(|c| alnum(c) || *c == b'-')
}

/// Legacy Meeting Service slug (imports only): `^[a-z0-9][a-z0-9-]*$`, at most 100.
pub fn valid_legacy_slug(slug: &str) -> bool {
    let b = slug.as_bytes();
    !b.is_empty()
        && b.len() <= 100
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

fn no_control(s: &str) -> bool {
    !s.chars().any(|c| c.is_control() && c != '\n')
}

/// A display name: trimmed, 1–200 characters, no control characters.
pub fn name(value: &str) -> Result<String, ApiError> {
    let trimmed = value.trim();
    let n = trimmed.chars().count();
    if !(1..=200).contains(&n) || trimmed.contains('\n') || !no_control(trimmed) {
        return Err(ApiError::validation("name must be 1-200 characters"));
    }
    Ok(trimmed.to_string())
}

pub fn slug(value: &str) -> Result<String, ApiError> {
    if valid_slug(value) {
        Ok(value.to_string())
    } else {
        Err(ApiError::validation(
            "slug must be 3-63 lowercase letters, digits or '-', starting and ending alphanumeric",
        ))
    }
}

/// Normalizes an email address (trimmed, lower-case) and checks its shape.
pub fn normalize_email(value: &str) -> Option<String> {
    let email = value.trim().to_lowercase();
    if email.is_empty()
        || email.len() > 254
        || email.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return None;
    }
    let (local, domain) = email.split_once('@')?;
    if local.is_empty()
        || local.len() > 64
        || domain.contains('@')
        || !domain.contains('.')
        || domain.starts_with('.')
        || domain.ends_with('.')
        || domain.starts_with('-')
    {
        return None;
    }
    Some(email)
}

/// `a***@example.com` (never the full address).
pub fn email_hint(normalized: &str) -> String {
    let (local, domain) = normalized.split_once('@').unwrap_or((normalized, ""));
    let first: String = local.chars().take(1).collect();
    format!("{first}***@{domain}")
}

fn bounded(
    field: &'static str,
    value: Option<String>,
    max: usize,
) -> Result<Option<String>, ApiError> {
    match value {
        None => Ok(None),
        Some(v) => {
            let t = v.trim().to_string();
            if t.is_empty() {
                return Ok(None);
            }
            if t.chars().count() > max || !no_control(&t) {
                return Err(ApiError::validation(format!(
                    "{field} must be at most {max} characters"
                )));
            }
            Ok(Some(t))
        }
    }
}

/// `profile.legalRegistration` as submitted.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LegalRegistrationInput {
    #[serde(default)]
    pub registered_name: Option<String>,
    #[serde(default)]
    pub registration_number: Option<String>,
    #[serde(default)]
    pub incorporated_on: Option<String>,
    #[serde(default)]
    pub verification: Option<String>,
}

/// `profile` as submitted (create: absent = null; PATCH: absent = unchanged).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileInput {
    #[serde(default, deserialize_with = "double_option")]
    pub description: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub organization_type: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub industry: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub country_code: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub city: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub operating_address: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub logo_ref: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub public_contact_email: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub legal_registration: Option<Option<LegalRegistrationInput>>,
}

/// The stored profile values.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileValues {
    pub description: Option<String>,
    pub organization_type: Option<String>,
    pub industry: Option<String>,
    pub country_code: Option<String>,
    pub city: Option<String>,
    pub operating_address: Option<String>,
    pub logo_ref: Option<String>,
    pub public_contact_email: Option<String>,
    pub legal: Option<LegalValues>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegalValues {
    pub registered_name: Option<String>,
    pub registration_number: Option<String>,
    pub incorporated_on: Option<NaiveDate>,
    /// `SELF_DECLARED` or `VERIFIED`.
    pub verification: String,
}

impl ProfileValues {
    /// Applies `input` (validated) and returns the camelCase names of the changed fields.
    pub fn apply(&mut self, input: ProfileInput) -> Result<Vec<&'static str>, ApiError> {
        let before = self.clone();
        let mut changed = Vec::new();
        macro_rules! field {
            ($f:ident, $name:literal, $max:expr) => {
                if let Some(v) = input.$f {
                    self.$f = bounded($name, v, $max)?;
                    if self.$f != before.$f {
                        changed.push($name);
                    }
                }
            };
        }
        field!(description, "description", 2000);
        field!(organization_type, "organizationType", 60);
        field!(industry, "industry", 60);
        field!(city, "city", 120);
        field!(operating_address, "operatingAddress", 500);
        field!(logo_ref, "logoRef", 500);
        if let Some(v) = input.country_code {
            let v = bounded("countryCode", v, 2)?.map(|c| c.to_ascii_uppercase());
            if let Some(c) = &v {
                if c.len() != 2 || !c.bytes().all(|b| b.is_ascii_uppercase()) {
                    return Err(ApiError::validation(
                        "countryCode must be an ISO 3166-1 alpha-2 code",
                    ));
                }
            }
            self.country_code = v;
            if self.country_code != before.country_code {
                changed.push("countryCode");
            }
        }
        if let Some(v) = input.public_contact_email {
            self.public_contact_email = match bounded("publicContactEmail", v, 254)? {
                None => None,
                Some(e) => Some(normalize_email(&e).ok_or_else(|| {
                    ApiError::validation("publicContactEmail must be an email address")
                })?),
            };
            if self.public_contact_email != before.public_contact_email {
                changed.push("publicContactEmail");
            }
        }
        if let Some(v) = input.legal_registration {
            self.legal = match v {
                None => None,
                Some(l) => {
                    // Only an operator verifies a registration; a submitted registration
                    // is always SELF_DECLARED.
                    match l.verification.as_deref() {
                        None | Some("SELF_DECLARED") => {}
                        Some(_) => {
                            return Err(ApiError::validation(
                                "legalRegistration.verification can only be SELF_DECLARED",
                            ))
                        }
                    }
                    let incorporated_on = match l.incorporated_on {
                        None => None,
                        Some(d) => {
                            Some(NaiveDate::parse_from_str(&d, "%Y-%m-%d").map_err(|_| {
                                ApiError::validation(
                                    "legalRegistration.incorporatedOn must be YYYY-MM-DD",
                                )
                            })?)
                        }
                    };
                    Some(LegalValues {
                        registered_name: bounded(
                            "legalRegistration.registeredName",
                            l.registered_name,
                            200,
                        )?,
                        registration_number: bounded(
                            "legalRegistration.registrationNumber",
                            l.registration_number,
                            100,
                        )?,
                        incorporated_on,
                        verification: "SELF_DECLARED".to_string(),
                    })
                }
            };
            if self.legal != before.legal {
                changed.push("legalRegistration");
            }
        }
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_follow_the_contract_pattern() {
        for ok in ["acme", "acme-health", "a1b", "x".repeat(63).as_str()] {
            assert!(valid_slug(ok), "{ok}");
        }
        for bad in [
            "ab",
            "-acme",
            "acme-",
            "Acme",
            "acme_health",
            "x".repeat(64).as_str(),
            "",
        ] {
            assert!(!valid_slug(bad), "{bad}");
        }
        assert!(valid_legacy_slug("a"));
        assert!(valid_legacy_slug("acme-"));
        assert!(!valid_legacy_slug("-acme"));
    }

    #[test]
    fn emails_are_normalized_and_hinted() {
        assert_eq!(
            normalize_email("  Alice@Example.COM ").as_deref(),
            Some("alice@example.com")
        );
        for bad in [
            "",
            "alice",
            "a@b",
            "a b@example.com",
            "@example.com",
            "a@@example.com",
        ] {
            assert!(normalize_email(bad).is_none(), "{bad}");
        }
        assert_eq!(email_hint("alice@example.com"), "a***@example.com");
    }

    #[test]
    fn profile_patch_distinguishes_absent_from_null_and_forces_self_declared() {
        let mut p = ProfileValues::default();
        let input: ProfileInput = serde_json::from_value(serde_json::json!({
            "description": "Clinic network", "countryCode": "ng",
            "legalRegistration": {"registeredName": "Acme Ltd", "incorporatedOn": "2020-01-31"}
        }))
        .unwrap();
        let changed = p.apply(input).unwrap();
        assert_eq!(
            changed,
            vec!["description", "countryCode", "legalRegistration"]
        );
        assert_eq!(p.country_code.as_deref(), Some("NG"));
        assert_eq!(p.legal.as_ref().unwrap().verification, "SELF_DECLARED");
        let clear: ProfileInput =
            serde_json::from_value(serde_json::json!({"description": null})).unwrap();
        assert_eq!(p.apply(clear).unwrap(), vec!["description"]);
        assert!(p.description.is_none() && p.country_code.is_some());
        let verified: ProfileInput = serde_json::from_value(
            serde_json::json!({"legalRegistration": {"verification": "VERIFIED"}}),
        )
        .unwrap();
        assert!(p.apply(verified).is_err());
        assert!(
            serde_json::from_value::<ProfileInput>(serde_json::json!({"userId": "x"})).is_err()
        );
        let long: ProfileInput =
            serde_json::from_value(serde_json::json!({"industry": "x".repeat(61)})).unwrap();
        assert!(p.apply(long).is_err());
    }
}
