//! KYC and eligibility configuration (Part IV Workstream F).
//!
//! | Variable | Required | Meaning |
//! |---|---|---|
//! | `NUMBERING_KYC_VERIFIER` | no | `manual` (default) or `deterministic` (refused in staging / production) |
//! | `NUMBERING_KYC_EVIDENCE_KEY` | no* | base64 32-byte AES-256-GCM key sealing evidence |
//! | `NUMBERING_KYC_EVIDENCE_KEY_ID` | no | its key id (default `kyc-1`) |
//! | `NUMBERING_KYC_EVIDENCE_PREVIOUS_KEY` / `_PREVIOUS_KEY_ID` | no | the previous key during rotation (open only) |
//! | `NUMBERING_KYC_EVIDENCE_STORAGE_*` | no* | private S3-compatible store (`cf_object_storage::S3Config`); staging / production require `..._SERVER_SIDE_ENCRYPTION` |
//! | `NUMBERING_KYC_EVIDENCE_RETENTION_DAYS` | no | evidence retention (30–3650, default 730) |
//! | `NUMBERING_KYC_ABANDONMENT_DAYS` | no | unfinished cases expire after (7–365, default 90) |
//! | `NUMBERING_ELIGIBILITY_ENABLED` | no | master flag (`true` default / `false`) |
//! | `NUMBERING_ELIGIBILITY_DISABLED_COUNTRIES` | no | comma-separated ISO codes refused by policy |
//! | `NUMBERING_ELIGIBILITY_RISK_RESTRICTED_COUNTRIES` | no | comma-separated ISO codes under an operator risk restriction |
//! | `NUMBERING_ELIGIBILITY_MAX_NUMBERS_{SANDBOX,TEST,PRODUCTION}` | no | open assignments per environment (defaults 5 / 25 / 100) |
//! | `NUMBERING_ELIGIBILITY_TTL_MINUTES` | no | validity of an ELIGIBLE decision (1–1440, default 30) |
//!
//! (*) The key and the store are configured together or not at all; without them evidence
//! is refused (`EVIDENCE_STORAGE_UNAVAILABLE`) — never stored unprotected. Key material is
//! held only inside the `SecretBox` and never printed.

use std::{collections::BTreeSet, fmt, sync::Arc};

use cf_object_storage::{ObjectStore, S3Config, S3ObjectStore};
use cf_service_auth::RuntimeEnvironment;
use cf_webhooks::secret_box::{SealingKey, SecretBox};

use super::ConfigError;
use crate::numbers::{
    eligibility::EligibilityPolicy,
    kyc::{evidence::EvidenceVault, KycSettings},
};

pub const EVIDENCE_STORAGE_PREFIX: &str = "NUMBERING_KYC_EVIDENCE_STORAGE_";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifierChoice {
    Manual,
    Deterministic,
}

pub struct KycConfig {
    pub verifier: VerifierChoice,
    pub vault: Arc<EvidenceVault>,
    pub settings: KycSettings,
    pub eligibility: EligibilityPolicy,
}

impl fmt::Debug for KycConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KycConfig")
            .field("verifier", &self.verifier)
            .field("vault", &self.vault)
            .field("settings", &self.settings)
            .field("eligibility", &self.eligibility)
            .finish()
    }
}

fn bounded(
    get: &impl Fn(&str) -> Option<String>,
    var: &'static str,
    default: i64,
    min: i64,
    max: i64,
) -> Result<i64, ConfigError> {
    match get(var) {
        None => Ok(default),
        Some(v) => v
            .trim()
            .parse::<i64>()
            .ok()
            .filter(|n| (min..=max).contains(n))
            .ok_or_else(|| {
                ConfigError::invalid(var, format!("must be an integer between {min} and {max}"))
            }),
    }
}

fn countries(
    get: &impl Fn(&str) -> Option<String>,
    var: &'static str,
) -> Result<BTreeSet<String>, ConfigError> {
    let mut out = BTreeSet::new();
    for c in get(var)
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        if c.len() != 2 || !c.bytes().all(|b| b.is_ascii_uppercase()) {
            return Err(ConfigError::invalid(
                var,
                "must be comma-separated ISO 3166-1 alpha-2 codes",
            ));
        }
        out.insert(c.to_string());
    }
    Ok(out)
}

fn key(
    get: &impl Fn(&str) -> Option<String>,
    var: &'static str,
    kid_var: &str,
    default_kid: &str,
) -> Result<Option<SealingKey>, ConfigError> {
    let Some(material) = get(var) else {
        return Ok(None);
    };
    let kid = get(kid_var).unwrap_or_else(|| default_kid.to_string());
    SealingKey::from_base64(&kid, &material)
        .map(Some)
        .map_err(|_| ConfigError::invalid(var, "must be a base64 32-byte key with a valid key id"))
}

impl KycConfig {
    /// Reads the configuration; `store_override` replaces the S3 store (tests).
    pub fn from_lookup(
        get: impl Fn(&str) -> Option<String>,
        runtime: RuntimeEnvironment,
        store_override: Option<Arc<dyn ObjectStore>>,
    ) -> Result<Self, ConfigError> {
        let get = |key: &str| get(key).filter(|v| !v.trim().is_empty());
        let protected_runtime = matches!(
            runtime,
            RuntimeEnvironment::Staging | RuntimeEnvironment::Production
        );
        let verifier =
            match get("NUMBERING_KYC_VERIFIER").as_deref().map(str::trim) {
                None | Some("manual") => VerifierChoice::Manual,
                Some("deterministic") if protected_runtime => return Err(ConfigError::invalid(
                    "NUMBERING_KYC_VERIFIER",
                    "the deterministic verifier is test-only and refused in staging / production",
                )),
                Some("deterministic") => VerifierChoice::Deterministic,
                Some(_) => {
                    return Err(ConfigError::invalid(
                        "NUMBERING_KYC_VERIFIER",
                        "must be `manual` or `deterministic`",
                    ))
                }
            };
        let current = key(
            &get,
            "NUMBERING_KYC_EVIDENCE_KEY",
            "NUMBERING_KYC_EVIDENCE_KEY_ID",
            "kyc-1",
        )?;
        let previous = key(
            &get,
            "NUMBERING_KYC_EVIDENCE_PREVIOUS_KEY",
            "NUMBERING_KYC_EVIDENCE_PREVIOUS_KEY_ID",
            "kyc-0",
        )?;
        let store: Option<Arc<dyn ObjectStore>> = match store_override {
            Some(s) => Some(s),
            None => match S3Config::from_lookup(EVIDENCE_STORAGE_PREFIX, &get)
                .map_err(|e| ConfigError::invalid("NUMBERING_KYC_EVIDENCE_STORAGE_ENDPOINT", e))?
            {
                None => None,
                Some(config) => {
                    if protected_runtime && config.server_side_encryption.is_none() {
                        return Err(ConfigError::invalid(
                            "NUMBERING_KYC_EVIDENCE_STORAGE_SERVER_SIDE_ENCRYPTION",
                            "evidence storage requires server-side encryption in staging / production",
                        ));
                    }
                    Some(Arc::new(S3ObjectStore::new(config).map_err(|e| {
                        ConfigError::invalid("NUMBERING_KYC_EVIDENCE_STORAGE_ENDPOINT", e)
                    })?))
                }
            },
        };
        let vault = match (current, store) {
            (None, None) => {
                if previous.is_some() {
                    return Err(ConfigError::invalid(
                        "NUMBERING_KYC_EVIDENCE_PREVIOUS_KEY",
                        "a previous key needs a current key",
                    ));
                }
                EvidenceVault::unconfigured()
            }
            (Some(current), Some(store)) => {
                let sealing = SecretBox::new(current, previous).map_err(|_| {
                    ConfigError::invalid(
                        "NUMBERING_KYC_EVIDENCE_PREVIOUS_KEY",
                        "must differ from the current key",
                    )
                })?;
                EvidenceVault::new(store, Some(Arc::new(sealing)))
            }
            (Some(_), None) => {
                return Err(ConfigError::invalid(
                    "NUMBERING_KYC_EVIDENCE_STORAGE_ENDPOINT",
                    "an evidence key needs a private evidence store",
                ))
            }
            (None, Some(_)) => {
                return Err(ConfigError::invalid(
                    "NUMBERING_KYC_EVIDENCE_KEY",
                    "evidence storage needs a sealing key",
                ))
            }
        };
        let defaults = KycSettings::default();
        let settings = KycSettings {
            evidence_retention: chrono::Duration::days(bounded(
                &get,
                "NUMBERING_KYC_EVIDENCE_RETENTION_DAYS",
                defaults.evidence_retention.num_days(),
                30,
                3_650,
            )?),
            abandonment: chrono::Duration::days(bounded(
                &get,
                "NUMBERING_KYC_ABANDONMENT_DAYS",
                defaults.abandonment.num_days(),
                7,
                365,
            )?),
            ..defaults
        };
        let policy_defaults = EligibilityPolicy::default();
        let enabled = match get("NUMBERING_ELIGIBILITY_ENABLED")
            .as_deref()
            .map(str::trim)
        {
            None | Some("true") => true,
            Some("false") => false,
            Some(_) => {
                return Err(ConfigError::invalid(
                    "NUMBERING_ELIGIBILITY_ENABLED",
                    "must be `true` or `false`",
                ))
            }
        };
        let eligibility = EligibilityPolicy {
            enabled,
            ttl: chrono::Duration::minutes(bounded(
                &get,
                "NUMBERING_ELIGIBILITY_TTL_MINUTES",
                30,
                1,
                1_440,
            )?),
            disabled_countries: countries(&get, "NUMBERING_ELIGIBILITY_DISABLED_COUNTRIES")?,
            risk_restricted_countries: countries(
                &get,
                "NUMBERING_ELIGIBILITY_RISK_RESTRICTED_COUNTRIES",
            )?,
            max_numbers_sandbox: bounded(
                &get,
                "NUMBERING_ELIGIBILITY_MAX_NUMBERS_SANDBOX",
                policy_defaults.max_numbers_sandbox,
                0,
                10_000,
            )?,
            max_numbers_test: bounded(
                &get,
                "NUMBERING_ELIGIBILITY_MAX_NUMBERS_TEST",
                policy_defaults.max_numbers_test,
                0,
                10_000,
            )?,
            max_numbers_production: bounded(
                &get,
                "NUMBERING_ELIGIBILITY_MAX_NUMBERS_PRODUCTION",
                policy_defaults.max_numbers_production,
                0,
                1_000_000,
            )?,
            ..policy_defaults
        };
        Ok(Self {
            verifier,
            vault: Arc::new(vault),
            settings,
            eligibility,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    const KEY_A: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";
    const KEY_B: &str = "HyAhIiMkJSYnKCkqKywtLi8wMTIzNDU2Nzg5Ojs8PT4=";

    fn read(
        vars: &[(&str, &str)],
        runtime: RuntimeEnvironment,
        store: bool,
    ) -> Result<KycConfig, ConfigError> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let store: Option<Arc<dyn ObjectStore>> =
            store.then(|| Arc::new(cf_object_storage::MemoryObjectStore::new()) as _);
        KycConfig::from_lookup(|k| map.get(k).cloned(), runtime, store)
    }

    #[test]
    fn defaults_fail_closed_and_test_adapters_stay_out_of_production() {
        let c = read(&[], RuntimeEnvironment::Production, false).unwrap();
        assert_eq!(c.verifier, VerifierChoice::Manual);
        assert!(
            !c.vault.is_configured(),
            "no key, no store: evidence refused"
        );
        assert!(read(
            &[("NUMBERING_KYC_VERIFIER", "deterministic")],
            RuntimeEnvironment::Production,
            false
        )
        .is_err());
        assert!(read(
            &[("NUMBERING_KYC_VERIFIER", "deterministic")],
            RuntimeEnvironment::Staging,
            false
        )
        .is_err());
        assert_eq!(
            read(
                &[("NUMBERING_KYC_VERIFIER", "deterministic")],
                RuntimeEnvironment::Local,
                false
            )
            .unwrap()
            .verifier,
            VerifierChoice::Deterministic
        );
        // Key and store come together.
        assert!(read(
            &[("NUMBERING_KYC_EVIDENCE_KEY", KEY_A)],
            RuntimeEnvironment::Local,
            false
        )
        .is_err());
        assert!(read(&[], RuntimeEnvironment::Local, true).is_err());
        assert!(read(
            &[("NUMBERING_KYC_EVIDENCE_KEY", "short")],
            RuntimeEnvironment::Local,
            true
        )
        .is_err());
        let c = read(
            &[
                ("NUMBERING_KYC_EVIDENCE_KEY", KEY_A),
                ("NUMBERING_KYC_EVIDENCE_PREVIOUS_KEY", KEY_B),
            ],
            RuntimeEnvironment::Local,
            true,
        )
        .unwrap();
        assert!(c.vault.is_configured());
        assert!(
            !format!("{c:?}").contains(KEY_A),
            "key material is never printed"
        );
        assert!(read(
            &[("NUMBERING_ELIGIBILITY_DISABLED_COUNTRIES", "US,nigeria")],
            RuntimeEnvironment::Local,
            false
        )
        .is_err());
        let c = read(
            &[
                ("NUMBERING_ELIGIBILITY_DISABLED_COUNTRIES", "US, GB"),
                ("NUMBERING_ELIGIBILITY_ENABLED", "false"),
            ],
            RuntimeEnvironment::Local,
            false,
        )
        .unwrap();
        assert!(!c.eligibility.enabled && c.eligibility.disabled_countries.contains("GB"));
    }
}
