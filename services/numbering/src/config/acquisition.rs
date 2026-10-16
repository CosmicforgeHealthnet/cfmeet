//! Acquisition limits (Workstreams H–K), read from the environment.
//!
//! | Variable | Default | Meaning |
//! |---|---|---|
//! | `NUMBERING_SEARCH_REQUESTS_PER_MINUTE` | 120 | searches per application environment per minute (1–10000) |
//! | `NUMBERING_SEARCH_PAGE_MAX` | 50 | largest search page (1–100) |
//! | `NUMBERING_INVENTORY_REFRESH_SECONDS` | 60 | minimum interval between provider refreshes of one query (5–3600) |
//! | `NUMBERING_RESERVATION_TTL_SECONDS` | 900 | default reservation lifetime (60–86400) |
//! | `NUMBERING_PRE_ELIGIBILITY_HOLDS` | `SANDBOX,TEST` | environments permitting a hold before eligibility (`none`, or a list of SANDBOX / TEST; PRODUCTION is refused) |

use chrono::Duration;

use super::ConfigError;
use crate::{acquisition::AcquisitionSettings, numbers::EnvironmentKind};

const HOLDS: &str = "NUMBERING_PRE_ELIGIBILITY_HOLDS";

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

/// Reads the acquisition limits (fail closed on anything malformed).
pub fn from_lookup(
    get: impl Fn(&str) -> Option<String>,
) -> Result<AcquisitionSettings, ConfigError> {
    let mut s = AcquisitionSettings {
        search_requests_per_minute: bounded(
            &get,
            "NUMBERING_SEARCH_REQUESTS_PER_MINUTE",
            120,
            1,
            10_000,
        )? as i32,
        search_page_max: bounded(&get, "NUMBERING_SEARCH_PAGE_MAX", 50, 1, 100)? as u64,
        refresh_interval: Duration::seconds(bounded(
            &get,
            "NUMBERING_INVENTORY_REFRESH_SECONDS",
            60,
            5,
            3_600,
        )?),
        reservation_default_ttl: Duration::seconds(bounded(
            &get,
            "NUMBERING_RESERVATION_TTL_SECONDS",
            900,
            60,
            86_400,
        )?),
        ..AcquisitionSettings::default()
    };
    if let Some(v) = get(HOLDS)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
    {
        s.pre_eligibility_hold_environments.clear();
        if !v.eq_ignore_ascii_case("none") {
            for kind in v.split(',').map(str::trim) {
                match EnvironmentKind::parse(kind) {
                    Some(k @ (EnvironmentKind::Sandbox | EnvironmentKind::Test)) => {
                        s.pre_eligibility_hold_environments.insert(k);
                    }
                    Some(EnvironmentKind::Production) => {
                        return Err(ConfigError::invalid(
                            HOLDS,
                            "PRODUCTION never permits a hold before eligibility",
                        ))
                    }
                    None => return Err(ConfigError::invalid(HOLDS, "unknown environment kind")),
                }
            }
        }
    }
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
    fn defaults_bounds_and_the_production_hold_refusal() {
        let s = from_lookup(lookup(&[])).unwrap();
        assert!(s.permits_pre_eligibility_hold(EnvironmentKind::Sandbox));
        assert!(!s.permits_pre_eligibility_hold(EnvironmentKind::Production));
        let none = from_lookup(lookup(&[(HOLDS, "none")])).unwrap();
        assert!(!none.permits_pre_eligibility_hold(EnvironmentKind::Sandbox));
        for (var, value) in [
            (HOLDS, "PRODUCTION"),
            (HOLDS, "SANDBOX,LIVE"),
            ("NUMBERING_SEARCH_PAGE_MAX", "0"),
            ("NUMBERING_SEARCH_REQUESTS_PER_MINUTE", "x"),
            ("NUMBERING_RESERVATION_TTL_SECONDS", "30"),
        ] {
            assert!(
                from_lookup(lookup(&[(var, value)])).is_err(),
                "{var}={value}"
            );
        }
    }
}
