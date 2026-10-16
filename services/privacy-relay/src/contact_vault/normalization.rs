//! E.164 canonicalization and the masked display (§31).
//!
//! International number parsing is delegated to `phonenumber` (a port of Google's
//! libphonenumber and its metadata); nothing here parses numbering plans by hand. This
//! module only screens the raw input shape (digits and common separators, so URI schemes,
//! extensions and letters never reach the parser), asks the library for a VALID number,
//! and derives the canonical E.164 form, the country and a bounded masked display.
//!
//! The raw input and the canonical value are secrets: [`RawContactValue`] and
//! [`CanonicalNumber`] zeroize on drop, have redacted `Debug`, no `Display` and no
//! `Serialize`, and every error is a fixed code without the value.

use std::fmt;

use phonenumber::{country, Mode};
use serde::{Deserialize, Deserializer};
use zeroize::Zeroizing;

/// Longest accepted raw input (an E.164 number has at most 15 digits).
const MAX_RAW_LEN: usize = 32;

/// A raw telephone value exactly as a client sent it. Deserializable (it is the Contact
/// Vault's one raw input), never serializable or printable.
pub struct RawContactValue(Zeroizing<String>);

impl RawContactValue {
    pub fn new(value: impl Into<String>) -> Self {
        Self(Zeroizing::new(value.into()))
    }

    fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for RawContactValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RawContactValue([REDACTED])")
    }
}

impl<'de> Deserialize<'de> for RawContactValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // The deserializer's error never contains the value (fixed message).
        let value = String::deserialize(deserializer)
            .map_err(|_| serde::de::Error::custom("telephoneNumber must be a string"))?;
        Ok(Self(Zeroizing::new(value)))
    }
}

/// A canonical E.164 number (`+<country code><national number>`). Only the Contact
/// Vault's encryption, fingerprint and execution-resolution paths ever see it.
pub struct CanonicalNumber {
    e164: Zeroizing<String>,
    country_iso2: Option<String>,
    calling_code: u16,
}

impl CanonicalNumber {
    /// The canonical value (encryption / fingerprint / execution-resolution input only).
    pub(crate) fn expose_e164(&self) -> &str {
        &self.e164
    }

    pub fn country_iso2(&self) -> Option<&str> {
        self.country_iso2.as_deref()
    }

    /// Re-hydrates a decrypted canonical value (already validated when it was stored).
    pub(crate) fn from_decrypted(e164: Zeroizing<String>) -> Result<Self, NormalizationError> {
        let parsed = parse_screened(&e164, None)?;
        Ok(parsed)
    }

    /// Bounded masked display: `+<country code>` + six bullets + at most the last four
    /// digits of the national number, and fewer for short numbers, so that at least six
    /// national digits always stay hidden. The bullet count is constant (no length leak).
    pub fn masked_display(&self) -> String {
        let code = self.calling_code.to_string();
        let national = &self.e164[1 + code.len()..];
        let shown = national.len().saturating_sub(6).min(4);
        format!(
            "+{code}{}{}",
            "\u{2022}".repeat(6),
            &national[national.len() - shown..]
        )
    }
}

impl fmt::Debug for CanonicalNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CanonicalNumber")
            .field("e164", &"[REDACTED]")
            .field("country_iso2", &self.country_iso2)
            .finish()
    }
}

/// Why a value is not a protectable telephone number. Never carries the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NormalizationError {
    #[error("the telephone number is empty or too long")]
    Length,
    #[error("the telephone number contains unsupported characters (URIs, extensions and letters are not accepted)")]
    Characters,
    #[error("defaultCountry must be an ISO 3166-1 alpha-2 country code")]
    DefaultCountry,
    #[error("the telephone number cannot be canonicalized (add the international prefix or defaultCountry)")]
    Unparseable,
    #[error("the telephone number is not a valid number")]
    Invalid,
    #[error("telephone number extensions are not supported")]
    Extension,
}

impl NormalizationError {
    /// Fixed, safe message for API responses.
    pub fn message(&self) -> &'static str {
        match self {
            NormalizationError::Length => "telephoneNumber is empty or too long.",
            NormalizationError::Characters => {
                "telephoneNumber may only contain digits, spaces, '+', '-', '.', '(' and ')'."
            }
            NormalizationError::DefaultCountry => {
                "defaultCountry must be an ISO 3166-1 alpha-2 country code."
            }
            NormalizationError::Unparseable => {
                "telephoneNumber cannot be canonicalized; send E.164 (+…) or a defaultCountry."
            }
            NormalizationError::Invalid => "telephoneNumber is not a valid telephone number.",
            NormalizationError::Extension => "Telephone number extensions are not supported.",
        }
    }
}

/// Canonicalizes a client-supplied number. `default_country` (ISO 3166-1 alpha-2) is
/// only used for numbers written without the international prefix.
pub fn normalize(
    raw: &RawContactValue,
    default_country: Option<&str>,
) -> Result<CanonicalNumber, NormalizationError> {
    let country = match default_country {
        None => None,
        Some(c) => {
            if c.len() != 2 || !c.bytes().all(|b| b.is_ascii_uppercase()) {
                return Err(NormalizationError::DefaultCountry);
            }
            Some(
                c.parse::<country::Id>()
                    .map_err(|_| NormalizationError::DefaultCountry)?,
            )
        }
    };
    parse_screened(raw.expose(), country)
}

/// A platform-owned PUBLIC relay number (temporary relay inventory, §34), canonicalized
/// with the same parser. Not a protected contact: the relay number is CF Meet's own public
/// identity, stored in the inventory (never in Calling, events or contracts).
pub struct PublicRelayNumber {
    pub e164: String,
    pub country_iso2: String,
    /// Bounded masked display for operator output.
    pub masked: String,
}

impl fmt::Debug for PublicRelayNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PublicRelayNumber")
            .field("masked", &self.masked)
            .field("country_iso2", &self.country_iso2)
            .finish()
    }
}

/// Canonicalizes an operator-supplied relay number (international form required; the
/// country must be determinable).
pub fn canonical_public_number(raw: &str) -> Result<PublicRelayNumber, NormalizationError> {
    if !raw.trim().starts_with('+') {
        return Err(NormalizationError::Unparseable);
    }
    let n = parse_screened(raw, None)?;
    let country_iso2 = n
        .country_iso2()
        .ok_or(NormalizationError::Unparseable)?
        .to_string();
    Ok(PublicRelayNumber {
        e164: n.e164.to_string(),
        masked: n.masked_display(),
        country_iso2,
    })
}

/// Masked display of a stored canonical relay number.
pub fn mask_public_number(e164: &str) -> String {
    match parse_screened(e164, None) {
        Ok(n) => n.masked_display(),
        Err(_) => "+\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}".to_string(),
    }
}

fn parse_screened(
    input: &str,
    country: Option<country::Id>,
) -> Result<CanonicalNumber, NormalizationError> {
    let trimmed = input.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_RAW_LEN {
        return Err(NormalizationError::Length);
    }
    // Shape screen: no scheme (tel:, sip:), no extension markers (x, ext, ;, #, ,), no
    // letters or other characters; at most one leading '+'.
    let plus_ok = !trimmed[1..].contains('+');
    if !plus_ok
        || !trimmed
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '+' | ' ' | '-' | '.' | '(' | ')'))
    {
        return Err(NormalizationError::Characters);
    }
    let digits = trimmed.chars().filter(char::is_ascii_digit).count();
    if !(3..=17).contains(&digits) {
        return Err(NormalizationError::Length);
    }
    let number =
        phonenumber::parse(country, trimmed).map_err(|_| NormalizationError::Unparseable)?;
    if number.extension().is_some() {
        return Err(NormalizationError::Extension);
    }
    if !number.is_valid() {
        return Err(NormalizationError::Invalid);
    }
    let e164 = Zeroizing::new(number.format().mode(Mode::E164).to_string());
    // E.164: '+' then 2..=15 digits.
    let ok = e164.len() >= 3
        && e164.len() <= 16
        && e164.starts_with('+')
        && e164[1..].bytes().all(|b| b.is_ascii_digit());
    if !ok {
        return Err(NormalizationError::Invalid);
    }
    Ok(CanonicalNumber {
        country_iso2: region_of(&number),
        calling_code: number.code().value(),
        e164,
    })
}

/// ISO 3166-1 alpha-2 region of a valid number, or None when the library's metadata
/// cannot determine one unambiguously (non-geographic codes, ambiguous shared codes).
/// Shared calling codes (+1, +7, +39, +44, …) are resolved with the national number
/// INCLUDING its significant leading zeros (the library's own lookup drops them).
fn region_of(number: &phonenumber::PhoneNumber) -> Option<String> {
    use phonenumber::metadata::DATABASE;
    if let Some(id) = number.country().id() {
        return Some(id.as_ref().to_string());
    }
    let code = number.code().value();
    let national = format!(
        "{}{}",
        "0".repeat(number.national().zeros() as usize),
        number.national().value()
    );
    let regions: Vec<&str> = DATABASE
        .region(&code)?
        .into_iter()
        .filter(|r| r.len() == 2)
        .collect();
    for region in &regions {
        let Some(meta) = DATABASE.by_id(*region) else {
            continue;
        };
        if let Some(leading) = meta.leading_digits() {
            if leading.find(&national).is_some_and(|m| m.start() == 0) {
                return Some(region.to_string());
            }
        }
    }
    let candidates: Vec<&str> = regions
        .into_iter()
        .filter(|r| {
            DATABASE.by_id(*r).is_some_and(|m| {
                m.leading_digits().is_none() && m.descriptors().general().is_match(&national)
            })
        })
        .collect();
    match candidates.as_slice() {
        [one] => Some(one.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canon(raw: &str, country: Option<&str>) -> Result<CanonicalNumber, NormalizationError> {
        normalize(&RawContactValue::new(raw), country)
    }

    #[test]
    fn valid_numbers_canonicalize_to_e164() {
        for (raw, country, e164, iso) in [
            ("+39 02 1234 5678", None, "+390212345678", "IT"),
            ("02 1234 5678", Some("IT"), "+390212345678", "IT"),
            ("+39 06 698 12345", None, "+390669812345", "VA"),
            ("+44 20 7946 0958", None, "+442079460958", "GB"),
            ("(020) 7946 0958", Some("GB"), "+442079460958", "GB"),
            ("+234 803 123 4567", None, "+2348031234567", "NG"),
            ("+1 (650) 253-0000", None, "+16502530000", "US"),
        ] {
            let n = canon(raw, country).unwrap_or_else(|e| panic!("{raw}: {e}"));
            assert_eq!(n.expose_e164(), e164, "{raw}");
            assert_eq!(n.country_iso2(), Some(iso));
        }
    }

    #[test]
    fn malformed_ambiguous_and_unsupported_inputs_are_refused() {
        for (raw, country, err) in [
            ("", None, NormalizationError::Length),
            ("+12345678901234567890123", None, NormalizationError::Length),
            ("tel:+390669821234", None, NormalizationError::Characters),
            (
                "sip:alice@example.com",
                None,
                NormalizationError::Characters,
            ),
            (
                "+39 06 6982 1234 ext 5",
                None,
                NormalizationError::Characters,
            ),
            (
                "+39 06 6982 1234;ext=5",
                None,
                NormalizationError::Characters,
            ),
            ("+39 06 6982 1234#5", None, NormalizationError::Characters),
            ("++390669821234", None, NormalizationError::Characters),
            ("02 1234 5678", None, NormalizationError::Unparseable),
            (
                "02 1234 5678",
                Some("it"),
                NormalizationError::DefaultCountry,
            ),
            (
                "02 1234 5678",
                Some("ZZ"),
                NormalizationError::DefaultCountry,
            ),
            ("+3 9", None, NormalizationError::Length),
            ("+1 123 456 7890", None, NormalizationError::Invalid),
            ("+1 650 253 000", None, NormalizationError::Invalid),
        ] {
            assert_eq!(canon(raw, country).map(|_| ()).unwrap_err(), err, "{raw}");
        }
    }

    #[test]
    fn masked_display_hides_at_least_six_national_digits() {
        let n = canon("+39 02 1234 5678", None).unwrap();
        assert_eq!(
            n.masked_display(),
            "+39\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}5678"
        );
        let n = canon("+44 20 7946 0958", None).unwrap();
        assert!(n.masked_display().ends_with("0958"));
        // A short national number reveals fewer digits.
        let n = canon("+354 412 3456", None).unwrap(); // Iceland, 7 national digits
        assert_eq!(
            n.masked_display(),
            "+354\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}6"
        );
    }

    #[test]
    fn secrets_never_print() {
        let raw = RawContactValue::new("+390212345678");
        assert!(!format!("{raw:?}").contains("0212345678"));
        let n = canon("+390212345678", None).unwrap();
        assert!(!format!("{n:?}").contains("0212345678"));
        let err = canon("+39 06 6982 1234 ext 5", None).unwrap_err();
        assert!(!err.to_string().contains("6982"));
        assert!(!err.message().contains("6982"));
    }
}
