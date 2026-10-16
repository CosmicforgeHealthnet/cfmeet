//! Canonical E.164 numbers (strict: the gateway never guesses a country).

/// Accepts `+` followed by 7–15 digits without a leading zero (separators ` .-()`
/// removed). Anything else is not a number.
pub fn normalize(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let rest = trimmed.strip_prefix('+')?;
    let digits: String = rest
        .chars()
        .filter(|c| !matches!(c, ' ' | '-' | '.' | '(' | ')'))
        .collect();
    let ok = (7..=15).contains(&digits.len())
        && digits.bytes().all(|b| b.is_ascii_digit())
        && !digits.starts_with('0')
        && rest
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, ' ' | '-' | '.' | '(' | ')'));
    ok.then(|| format!("+{digits}"))
}

/// Whether a canonical number starts with one of the allowed prefixes.
pub fn allowed(e164: &str, prefixes: &[String]) -> bool {
    prefixes.iter().any(|p| e164.starts_with(p.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_international_numbers_are_numbers() {
        assert_eq!(
            normalize("+1 (555) 010-0123").as_deref(),
            Some("+15550100123")
        );
        assert_eq!(
            normalize(" +2348031234567 ").as_deref(),
            Some("+2348031234567")
        );
        for bad in [
            "5550100123",
            "+0123456789",
            "+12",
            "+1234567890123456",
            "+1555abc0123",
            "tel:+15550100123",
        ] {
            assert_eq!(normalize(bad), None, "{bad}");
        }
        assert!(allowed("+15550100123", &["+44".into(), "+1".into()]));
        assert!(!allowed("+15550100123", &[]));
    }
}
