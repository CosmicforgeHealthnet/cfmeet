//! Lifecycle stewardship policy (Workstreams P–X), read from the environment.
//!
//! | Variable | Default | Meaning |
//! |---|---|---|
//! | `NUMBERING_QUARANTINE_DAYS` | 90 | default cooldown of a released number (1–3650) |
//! | `NUMBERING_QUARANTINE_SYNTHETIC_DAYS` | 7 | cooldown of synthetic test inventory (1–3650) |
//! | `NUMBERING_QUARANTINE_REVIEW_AFTER_LATE_EVENTS` | 25 | late voice / SMS events after which completion needs a review (1–100000) |
//! | `NUMBERING_RISK_MAX_ACTIVE_NUMBERS` | 100 | open assignments + current reservations per application environment (1–1000000) |
//! | `NUMBERING_RISK_DAILY_ALLOCATIONS` / `…_WEEKLY_ALLOCATIONS` | 50 / 200 | allocation velocity (1–1000000) |
//! | `NUMBERING_RISK_RESTRICTED_COUNTRIES` | none | ISO countries where allocation is refused (comma list) |
//! | `NUMBERING_NUMBER_SPEND_THRESHOLD_MINOR` | 50000 | provider cost per number per 24 h before outbound grants stop |
//! | `NUMBERING_PROVIDER_COST_SPIKE_MINOR` | 1000000 | provider cost per hour that opens the provisioning breaker |
//! | `NUMBERING_PORTING_ENABLED` | false | porting APIs (no live portability provider is integrated) |
//! | `NUMBERING_RELAY_LEASE_MAX_HOURS` | 9600 | longest relay-number lease (1–10000; the default covers Privacy Relay's longest session) |

use chrono::Duration;

use super::ConfigError;
use crate::stewardship::StewardshipSettings;

fn bounded(
    get: &impl Fn(&str) -> Option<String>,
    var: &'static str,
    default: i64,
    min: i64,
    max: i64,
) -> Result<i64, ConfigError> {
    match get(var)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
    {
        None => Ok(default),
        Some(v) => v
            .parse::<i64>()
            .ok()
            .filter(|n| (min..=max).contains(n))
            .ok_or_else(|| {
                ConfigError::invalid(var, format!("must be an integer between {min} and {max}"))
            }),
    }
}

/// Reads the stewardship policy (fail closed on anything malformed).
pub fn from_lookup(
    get: impl Fn(&str) -> Option<String>,
) -> Result<StewardshipSettings, ConfigError> {
    let mut s = StewardshipSettings::default();
    s.quarantine.default_days = bounded(&get, "NUMBERING_QUARANTINE_DAYS", 90, 1, 3650)?;
    s.quarantine.synthetic_days = bounded(&get, "NUMBERING_QUARANTINE_SYNTHETIC_DAYS", 7, 1, 3650)?;
    s.quarantine.review_after_late_events = bounded(
        &get,
        "NUMBERING_QUARANTINE_REVIEW_AFTER_LATE_EVENTS",
        25,
        1,
        100_000,
    )?;
    s.risk.max_active_numbers =
        bounded(&get, "NUMBERING_RISK_MAX_ACTIVE_NUMBERS", 100, 1, 1_000_000)?;
    s.risk.daily_allocations = bounded(&get, "NUMBERING_RISK_DAILY_ALLOCATIONS", 50, 1, 1_000_000)?;
    s.risk.weekly_allocations =
        bounded(&get, "NUMBERING_RISK_WEEKLY_ALLOCATIONS", 200, 1, 1_000_000)?;
    s.risk.number_spend_threshold_minor = bounded(
        &get,
        "NUMBERING_NUMBER_SPEND_THRESHOLD_MINOR",
        50_000,
        1,
        i64::MAX / 2,
    )?;
    s.risk.provider_cost_spike_minor = bounded(
        &get,
        "NUMBERING_PROVIDER_COST_SPIKE_MINOR",
        1_000_000,
        1,
        i64::MAX / 2,
    )?;
    s.relay_lease_max = Duration::hours(bounded(
        &get,
        "NUMBERING_RELAY_LEASE_MAX_HOURS",
        9600,
        1,
        10_000,
    )?);
    if let Some(v) = get("NUMBERING_RISK_RESTRICTED_COUNTRIES").filter(|v| !v.trim().is_empty()) {
        for c in v.split(',').map(str::trim) {
            if c.len() != 2 || !c.bytes().all(|b| b.is_ascii_uppercase()) {
                return Err(ConfigError::invalid(
                    "NUMBERING_RISK_RESTRICTED_COUNTRIES",
                    "a comma list of ISO 3166-1 alpha-2 codes",
                ));
            }
            s.risk.restricted_countries.insert(c.to_string());
        }
    }
    s.porting_enabled = match get("NUMBERING_PORTING_ENABLED").as_deref().map(str::trim) {
        None | Some("") | Some("false") => false,
        Some("true") => true,
        Some(_) => {
            return Err(ConfigError::invalid(
                "NUMBERING_PORTING_ENABLED",
                "must be true or false",
            ))
        }
    };
    Ok(s)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn defaults_bounds_and_refusals() {
        let s = from_lookup(lookup(&[])).unwrap();
        assert!(!s.porting_enabled);
        assert_eq!(s.quarantine.default_days, 90);
        let s = from_lookup(lookup(&[
            ("NUMBERING_PORTING_ENABLED", "true"),
            ("NUMBERING_RISK_RESTRICTED_COUNTRIES", "KP, IR"),
        ]))
        .unwrap();
        assert!(s.porting_enabled);
        assert!(s.risk.restricted_countries.contains("KP"));
        for (var, value) in [
            ("NUMBERING_PORTING_ENABLED", "yes"),
            ("NUMBERING_QUARANTINE_DAYS", "0"),
            ("NUMBERING_RISK_RESTRICTED_COUNTRIES", "usa"),
            ("NUMBERING_RELAY_LEASE_MAX_HOURS", "10001"),
        ] {
            assert!(
                from_lookup(lookup(&[(var, value)])).is_err(),
                "{var}={value}"
            );
        }
    }
}
