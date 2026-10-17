//! E.164 normalization and validation.
//!
//! Input may carry formatting (`+1 (415) 555-0100`, `00 44 20 7946 0958`); the canonical
//! form is `+` followed by 7–15 digits, the first non-zero (ITU-T E.164: at most 15 digits
//! including the country calling code). The country is never guessed from the digits: the
//! caller states it (ISO 3166-1 alpha-2) and the number must start with that country's
//! calling code. Numbering-plan details (number types, valid national ranges) are evidence
//! from inventory, not inferred here; the country table is the minimal set Part IV
//! supports and grows with the country catalog (Workstream E).

use super::DomainError;

/// ISO 3166-1 alpha-2 → E.164 country calling code.
const COUNTRIES: &[(&str, &str)] = &[
    ("NG", "234"),
    ("GH", "233"),
    ("KE", "254"),
    ("ZA", "27"),
    ("EG", "20"),
    ("GB", "44"),
    ("IE", "353"),
    ("FR", "33"),
    ("DE", "49"),
    ("NL", "31"),
    ("ES", "34"),
    ("IT", "39"),
    ("US", "1"),
    ("CA", "1"),
    ("IN", "91"),
    ("AE", "971"),
    ("AU", "61"),
];

/// A validated canonical number with its stated country.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct E164Number {
    pub e164: String,
    pub country_code: String,
    pub calling_code: String,
}

/// The calling code of a supported country.
pub fn calling_code(country: &str) -> Option<&'static str> {
    COUNTRIES
        .iter()
        .find(|(c, _)| *c == country)
        .map(|(_, code)| *code)
}

/// Normalizes `raw` to canonical E.164 (formatting removed). Refuses letters, extensions,
/// national (trunk-prefixed) forms and lengths outside E.164.
pub fn normalize(raw: &str) -> Result<String, DomainError> {
    let invalid = || DomainError::Validation("the number is not a valid E.164 number".into());
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > 32 {
        return Err(invalid());
    }
    let digits_part = if let Some(rest) = trimmed.strip_prefix('+') {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("00") {
        rest
    } else {
        // A national form ("0801…") is ambiguous without a plan: refuse, never guess.
        return Err(invalid());
    };
    let mut digits = String::with_capacity(15);
    for c in digits_part.chars() {
        match c {
            '0'..='9' => digits.push(c),
            ' ' | '-' | '.' | '(' | ')' => {}
            _ => return Err(invalid()),
        }
    }
    if !(7..=15).contains(&digits.len()) || digits.starts_with('0') {
        return Err(invalid());
    }
    Ok(format!("+{digits}"))
}

/// Normalizes `raw` and checks it belongs to `country` (stated, never inferred).
pub fn validate(raw: &str, country: &str) -> Result<E164Number, DomainError> {
    let e164 = normalize(raw)?;
    let code = calling_code(country).ok_or_else(|| {
        DomainError::Validation("the country is not supported by the number catalog yet".into())
    })?;
    if !e164[1..].starts_with(code) || e164.len() - 1 - code.len() < 4 {
        return Err(DomainError::Validation(
            "the number does not belong to the stated country".into(),
        ));
    }
    Ok(E164Number {
        e164,
        country_code: country.to_string(),
        calling_code: code.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatting_is_removed_and_both_international_prefixes_are_accepted() {
        assert_eq!(normalize("+1 (415) 555-0100").unwrap(), "+14155550100");
        assert_eq!(normalize("00 44 20 7946 0958").unwrap(), "+442079460958");
        assert_eq!(normalize("+234.801.234.5678").unwrap(), "+2348012345678");
    }

    #[test]
    fn invalid_forms_are_refused() {
        for bad in [
            "",
            "08012345678",         // national form: ambiguous
            "+0123456789",         // leading zero country code
            "+12345",              // too short
            "+1234567890123456",   // 16 digits
            "+1 415 555 0100 x12", // extension
            "+1-800-FLOWERS",      // letters
            "tel:+14155550100",    // URI
            "++14155550100",
        ] {
            assert!(normalize(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_stated_country_must_match_the_calling_code() {
        let n = validate("+234 801 234 5678", "NG").unwrap();
        assert_eq!(
            (n.e164.as_str(), n.calling_code.as_str()),
            ("+2348012345678", "234")
        );
        assert!(validate("+2348012345678", "GH").is_err());
        assert!(validate("+14155550100", "US").is_ok());
        assert!(
            validate("+14165550100", "CA").is_ok(),
            "+1 is shared; the stated country decides"
        );
        assert!(validate("+2348012345678", "ZZ").is_err());
        assert!(validate("+2341", "NG").is_err());
    }
}
