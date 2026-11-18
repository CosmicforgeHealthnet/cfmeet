//! CF Meet Platform "Build" configuration (ADR 0062): partner users, user tokens, allowed
//! websites and partner meeting principals. Every feature is OFF by default; with a flag
//! off its routes answer exactly like an unknown route (`404 NOT_FOUND`).
//!
//! | Variable | Default | Meaning |
//! |---|---|---|
//! | `PLATFORM_ACCESS_END_USERS_ENABLED` | `false` | Partner Users API (`/internal/v1/end-users*`) and the console read |
//! | `PLATFORM_ACCESS_USER_TOKENS_ENABLED` | `false` | user-token minting (also needs the Users API flag), `/internal/v1/user-tokens/*` |
//! | `PLATFORM_ACCESS_ENVIRONMENT_ORIGINS_ENABLED` | `false` | allowed websites (origins) reads, feed and console management |
//! | `PLATFORM_ACCESS_MEETING_PLATFORM_ENABLED` | `false` | `POST /internal/v1/principals/ensure-meeting-platform` |
//! | `PLATFORM_ACCESS_MAX_END_USERS_SANDBOX` / `_TEST` / `_PRODUCTION` | 100 / 1000 / 100000 | ACTIVE partner users per environment |
//! | `PLATFORM_ACCESS_USER_TOKEN_SIGNING_KEY_FILE` | — | PKCS#8 PEM Ed25519 private key FILE signing user tokens (required when user tokens are enabled; inline keys are refused) |
//! | `PLATFORM_ACCESS_USER_TOKEN_PREVIOUS_PUBLIC_KEYS` | — | comma-separated base64url raw Ed25519 public keys still accepted during a rotation (`ACTIVE`) |
//! | `PLATFORM_ACCESS_USER_TOKEN_RETIRED_PUBLIC_KEYS` | — | comma-separated base64url raw Ed25519 public keys that are retired: tokens they signed are `USER_TOKEN_REVOKED` |

use std::{fmt, sync::Arc};

use secrecy::ExposeSecret;

use super::{secret, ConfigError};
use crate::{domain::EnvironmentKind, identity::user_tokens::UserTokenKeys};

/// Per-environment-kind caps of ACTIVE partner users.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndUserCaps {
    pub sandbox: i64,
    pub test: i64,
    pub production: i64,
}

impl Default for EndUserCaps {
    fn default() -> Self {
        Self {
            sandbox: 100,
            test: 1_000,
            production: 100_000,
        }
    }
}

impl EndUserCaps {
    pub fn for_kind(&self, kind: EnvironmentKind) -> i64 {
        match kind {
            EnvironmentKind::Sandbox => self.sandbox,
            EnvironmentKind::Test => self.test,
            EnvironmentKind::Production => self.production,
        }
    }
}

/// The feature flags, caps and user-token keys.
#[derive(Clone, Default)]
pub struct PlatformBuildConfig {
    pub end_users_enabled: bool,
    pub user_tokens_enabled: bool,
    pub environment_origins_enabled: bool,
    pub meeting_platform_enabled: bool,
    pub end_user_caps: EndUserCaps,
    /// Present whenever a signing key is configured (always when user tokens are enabled).
    pub user_token_keys: Option<Arc<UserTokenKeys>>,
}

impl fmt::Debug for PlatformBuildConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlatformBuildConfig")
            .field("end_users_enabled", &self.end_users_enabled)
            .field("user_tokens_enabled", &self.user_tokens_enabled)
            .field(
                "environment_origins_enabled",
                &self.environment_origins_enabled,
            )
            .field("meeting_platform_enabled", &self.meeting_platform_enabled)
            .field("end_user_caps", &self.end_user_caps)
            .field("user_token_keys", &self.user_token_keys)
            .finish()
    }
}

pub const SIGNING_KEY_VAR: &str = "PLATFORM_ACCESS_USER_TOKEN_SIGNING_KEY";
pub const SIGNING_KEY_FILE_VAR: &str = "PLATFORM_ACCESS_USER_TOKEN_SIGNING_KEY_FILE";
pub const PREVIOUS_KEYS_VAR: &str = "PLATFORM_ACCESS_USER_TOKEN_PREVIOUS_PUBLIC_KEYS";
pub const RETIRED_KEYS_VAR: &str = "PLATFORM_ACCESS_USER_TOKEN_RETIRED_PUBLIC_KEYS";

fn flag(get: &impl Fn(&str) -> Option<String>, var: &'static str) -> Result<bool, ConfigError> {
    match get(var).as_deref().map(str::trim) {
        None | Some("") => Ok(false),
        Some(v) if v.eq_ignore_ascii_case("true") || v == "1" => Ok(true),
        Some(v) if v.eq_ignore_ascii_case("false") || v == "0" => Ok(false),
        Some(_) => Err(ConfigError::invalid(
            var,
            "must be `true` or `false`".to_string(),
        )),
    }
}

fn cap(
    get: &impl Fn(&str) -> Option<String>,
    var: &'static str,
    default: i64,
) -> Result<i64, ConfigError> {
    match get(var).map(|v| v.trim().to_string()).filter(|v| !v.is_empty()) {
        None => Ok(default),
        Some(v) => v
            .parse::<i64>()
            .ok()
            .filter(|n| (0..=10_000_000).contains(n))
            .ok_or_else(|| {
                ConfigError::invalid(var, "must be an integer between 0 and 10000000".to_string())
            }),
    }
}

impl PlatformBuildConfig {
    pub fn from_lookup(get: &impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let defaults = EndUserCaps::default();
        let end_user_caps = EndUserCaps {
            sandbox: cap(get, "PLATFORM_ACCESS_MAX_END_USERS_SANDBOX", defaults.sandbox)?,
            test: cap(get, "PLATFORM_ACCESS_MAX_END_USERS_TEST", defaults.test)?,
            production: cap(
                get,
                "PLATFORM_ACCESS_MAX_END_USERS_PRODUCTION",
                defaults.production,
            )?,
        };
        let user_tokens_enabled = flag(get, "PLATFORM_ACCESS_USER_TOKENS_ENABLED")?;
        if get(SIGNING_KEY_VAR).is_some_and(|v| !v.trim().is_empty()) {
            return Err(ConfigError::invalid(
                SIGNING_KEY_VAR,
                format!("inline private keys are not accepted; set {SIGNING_KEY_FILE_VAR}"),
            ));
        }
        let user_token_keys =
            match secret::read_secret(get, SIGNING_KEY_VAR, SIGNING_KEY_FILE_VAR)? {
                None => None,
                Some(pem) => {
                    let previous = public_keys(get, PREVIOUS_KEYS_VAR)?;
                    let retired = public_keys(get, RETIRED_KEYS_VAR)?;
                    let keys = UserTokenKeys::from_pkcs8_pem(pem.expose_secret(), previous, retired)
                        .map_err(|reason| ConfigError::invalid(SIGNING_KEY_FILE_VAR, reason))?;
                    Some(Arc::new(keys))
                }
            };
        if user_tokens_enabled && user_token_keys.is_none() {
            return Err(ConfigError::Missing {
                var: SIGNING_KEY_FILE_VAR,
            });
        }
        Ok(Self {
            end_users_enabled: flag(get, "PLATFORM_ACCESS_END_USERS_ENABLED")?,
            user_tokens_enabled,
            environment_origins_enabled: flag(get, "PLATFORM_ACCESS_ENVIRONMENT_ORIGINS_ENABLED")?,
            meeting_platform_enabled: flag(get, "PLATFORM_ACCESS_MEETING_PLATFORM_ENABLED")?,
            end_user_caps,
            user_token_keys,
        })
    }

    /// Whether minting is available (the Users API AND user tokens are on).
    pub fn minting_enabled(&self) -> bool {
        self.end_users_enabled && self.user_tokens_enabled && self.user_token_keys.is_some()
    }

    /// Whether verification / key reads are available.
    pub fn verification_enabled(&self) -> bool {
        self.user_tokens_enabled && self.user_token_keys.is_some()
    }
}

fn public_keys(
    get: &impl Fn(&str) -> Option<String>,
    var: &'static str,
) -> Result<Vec<ed25519_dalek::VerifyingKey>, ConfigError> {
    let Some(raw) = get(var) else {
        return Ok(Vec::new());
    };
    raw.split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| {
            crate::identity::user_tokens::parse_public_key(v).ok_or_else(|| {
                ConfigError::invalid(
                    var,
                    "every entry must be a base64url (unpadded) raw 32-byte Ed25519 public key"
                        .to_string(),
                )
            })
        })
        .collect()
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
    fn everything_is_off_by_default() {
        let cfg = PlatformBuildConfig::from_lookup(&lookup(&[])).unwrap();
        assert!(!cfg.end_users_enabled);
        assert!(!cfg.user_tokens_enabled);
        assert!(!cfg.environment_origins_enabled);
        assert!(!cfg.meeting_platform_enabled);
        assert!(!cfg.minting_enabled());
        assert!(!cfg.verification_enabled());
        assert_eq!(cfg.end_user_caps, EndUserCaps::default());
        assert_eq!(cfg.end_user_caps.for_kind(EnvironmentKind::Sandbox), 100);
        assert_eq!(cfg.end_user_caps.for_kind(EnvironmentKind::Test), 1000);
        assert_eq!(
            cfg.end_user_caps.for_kind(EnvironmentKind::Production),
            100_000
        );
    }

    #[test]
    fn flags_and_caps_are_parsed_strictly() {
        let cfg = PlatformBuildConfig::from_lookup(&lookup(&[
            ("PLATFORM_ACCESS_END_USERS_ENABLED", "true"),
            ("PLATFORM_ACCESS_ENVIRONMENT_ORIGINS_ENABLED", "1"),
            ("PLATFORM_ACCESS_MEETING_PLATFORM_ENABLED", "false"),
            ("PLATFORM_ACCESS_MAX_END_USERS_SANDBOX", "3"),
        ]))
        .unwrap();
        assert!(cfg.end_users_enabled && cfg.environment_origins_enabled);
        assert!(!cfg.meeting_platform_enabled);
        assert_eq!(cfg.end_user_caps.sandbox, 3);
        for (var, value) in [
            ("PLATFORM_ACCESS_END_USERS_ENABLED", "yes"),
            ("PLATFORM_ACCESS_MAX_END_USERS_TEST", "-1"),
            ("PLATFORM_ACCESS_MAX_END_USERS_PRODUCTION", "many"),
        ] {
            assert!(
                PlatformBuildConfig::from_lookup(&lookup(&[(var, value)])).is_err(),
                "{var}={value}"
            );
        }
    }

    #[test]
    fn user_tokens_need_a_signing_key_file_never_inline() {
        let err = PlatformBuildConfig::from_lookup(&lookup(&[(
            "PLATFORM_ACCESS_USER_TOKENS_ENABLED",
            "true",
        )]))
        .unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Missing {
                var: SIGNING_KEY_FILE_VAR
            }
        ));
        let err = PlatformBuildConfig::from_lookup(&lookup(&[(SIGNING_KEY_VAR, "abc")]))
            .unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }));
        assert!(!err.to_string().contains("abc"));
        assert!(PlatformBuildConfig::from_lookup(&lookup(&[(
            PREVIOUS_KEYS_VAR,
            "not-a-key"
        )]))
        .is_ok());
    }

    #[test]
    fn a_pem_key_file_enables_user_tokens() {
        use super::super::secret::tests::TempSecret;
        let keys = UserTokenKeys::generate();
        let previous = UserTokenKeys::generate();
        let file = TempSecret::new(&keys.to_pkcs8_pem_for_tests(), 0o600);
        let path = file.path();
        let previous_x = previous.public_keys()[0].x.clone();
        let cfg = PlatformBuildConfig::from_lookup(&lookup(&[
            ("CF_RUNTIME_ENVIRONMENT", "production"),
            ("PLATFORM_ACCESS_END_USERS_ENABLED", "true"),
            ("PLATFORM_ACCESS_USER_TOKENS_ENABLED", "true"),
            (SIGNING_KEY_FILE_VAR, path.as_str()),
            (RETIRED_KEYS_VAR, previous_x.as_str()),
        ]))
        .unwrap();
        assert!(cfg.minting_enabled() && cfg.verification_enabled());
        let loaded = cfg.user_token_keys.unwrap();
        assert_eq!(loaded.kid(), keys.kid());
        assert_eq!(loaded.public_keys().len(), 2);
        assert!(!format!("{loaded:?}").contains("BEGIN"));
        // A malformed previous / retired key list is refused once a key is configured.
        let err = PlatformBuildConfig::from_lookup(&lookup(&[
            ("CF_RUNTIME_ENVIRONMENT", "production"),
            (SIGNING_KEY_FILE_VAR, path.as_str()),
            (PREVIOUS_KEYS_VAR, "not-a-key"),
        ]))
        .unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }));
    }
}
