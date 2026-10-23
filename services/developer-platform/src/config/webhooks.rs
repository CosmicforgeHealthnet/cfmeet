//! Central customer webhooks (Section 53) configuration.
//!
//! | Variable | Required | Meaning |
//! |---|---|---|
//! | `DEVELOPER_PLATFORM_WEBHOOKS_ENABLED` | no | feature flag (default `false`): off, every webhook route (customer, console, event ingestion) is 404 and the worker delivers nothing |
//! | `DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE` | no | `shadow` (default): events are ingested, projected and their deliveries recorded as SHADOW — nothing is sent, so enabling the Developer Platform before the cut-over never double-delivers; `active`: deliveries are sent |
//! | `DEVELOPER_PLATFORM_WEBHOOK_SECRET_KEY` | no | the deployment key sealing signing secrets at rest (base64, 32 bytes, not weak); unset = subscription routes answer 503 CAPABILITY_UNAVAILABLE (fail closed) and nothing can be signed |
//! | `DEVELOPER_PLATFORM_WEBHOOK_SECRET_KEY_ID` | no | its key id (default `k0`) |
//! | `DEVELOPER_PLATFORM_WEBHOOK_SECRET_KEY_PREVIOUS` | no | the previous key: opens only (rotation overlap); requires the current key |
//! | `DEVELOPER_PLATFORM_WEBHOOK_SECRET_KEY_PREVIOUS_ID` | with the previous key | its key id (must differ from the current one) |
//! | `DEVELOPER_PLATFORM_WEBHOOK_ALLOW_PRIVATE_ENDPOINTS` | no | `true` permits http and private / loopback endpoints — ONLY in the `local` runtime (`CF_RUNTIME_ENVIRONMENT=local`); refused anywhere else |
//! | `DEVELOPER_PLATFORM_WEBHOOK_REQUEST_TIMEOUT_MS` | no | delivery request timeout (500–30000, default 10000) |
//! | `DEVELOPER_PLATFORM_WEBHOOK_MAX_ATTEMPTS` | no | attempts before a delivery is dead-lettered (1–20, default 8) |
//! | `DEVELOPER_PLATFORM_WEBHOOK_AUTO_DISABLE_AFTER` | no | consecutive failed attempts after which an endpoint is disabled automatically (5–10000, default 50) |
//!
//! Every key may instead be a mounted secret file (`<NAME>_FILE`): both forms set is
//! refused, an unreadable / empty file is refused (never a fallback to "no key"), and
//! outside the `local` / `test` runtimes a file readable by group or others is refused.
//! Error messages name the variable, never its value.

use std::{sync::Arc, time::Duration};

use cf_webhooks::{
    endpoint::EndpointPolicy,
    secret_box::{SealingKey, SecretBox, SecretBoxError, DEFAULT_KEY_ID},
};
use secrecy::{ExposeSecret, SecretString};

use super::{bounded, flag, ConfigError};
use crate::webhooks::{DeliveryMode, WebhookSettings};

const KEY: &str = "DEVELOPER_PLATFORM_WEBHOOK_SECRET_KEY";
const KEY_FILE: &str = "DEVELOPER_PLATFORM_WEBHOOK_SECRET_KEY_FILE";
const KEY_ID: &str = "DEVELOPER_PLATFORM_WEBHOOK_SECRET_KEY_ID";
const PREVIOUS: &str = "DEVELOPER_PLATFORM_WEBHOOK_SECRET_KEY_PREVIOUS";
const PREVIOUS_FILE: &str = "DEVELOPER_PLATFORM_WEBHOOK_SECRET_KEY_PREVIOUS_FILE";
const PREVIOUS_ID: &str = "DEVELOPER_PLATFORM_WEBHOOK_SECRET_KEY_PREVIOUS_ID";
const ALLOW_PRIVATE: &str = "DEVELOPER_PLATFORM_WEBHOOK_ALLOW_PRIVATE_ENDPOINTS";

/// Largest key file accepted.
const MAX_KEY_FILE_BYTES: u64 = 16 * 1024;

fn runtime(get: &impl Fn(&str) -> Option<String>) -> Option<String> {
    get("CF_RUNTIME_ENVIRONMENT").map(|v| v.trim().to_string())
}

/// Parses the webhook settings from `get` (values already trimmed, empty = unset).
pub fn from_lookup(get: &impl Fn(&str) -> Option<String>) -> Result<WebhookSettings, ConfigError> {
    let enabled = flag(get, "DEVELOPER_PLATFORM_WEBHOOKS_ENABLED")?;
    let mode = match get("DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE").as_deref() {
        None | Some("shadow") => DeliveryMode::Shadow,
        Some("active") => DeliveryMode::Active,
        Some(_) => {
            return Err(ConfigError::invalid(
                "DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE",
                "must be `active` or `shadow`",
            ))
        }
    };
    let local = runtime(get).as_deref() == Some("local");
    let allow_private = flag(get, ALLOW_PRIVATE)?;
    if allow_private && !local {
        return Err(ConfigError::invalid(
            ALLOW_PRIVATE,
            "is permitted only in the local runtime environment",
        ));
    }
    let request_timeout = Duration::from_millis(bounded(
        get,
        "DEVELOPER_PLATFORM_WEBHOOK_REQUEST_TIMEOUT_MS",
        10_000,
        500,
        30_000,
    )? as u64);
    let max_attempts = bounded(
        get,
        "DEVELOPER_PLATFORM_WEBHOOK_MAX_ATTEMPTS",
        cf_webhooks::retry::MAX_ATTEMPTS as i64,
        1,
        20,
    )? as u32;
    let auto_disable_after = bounded(
        get,
        "DEVELOPER_PLATFORM_WEBHOOK_AUTO_DISABLE_AFTER",
        50,
        5,
        10_000,
    )? as i32;
    Ok(WebhookSettings {
        enabled,
        key_ring: key_ring(get)?.map(Arc::new),
        policy: EndpointPolicy {
            allow_insecure_http: allow_private,
            allow_private_networks: allow_private,
        },
        mode,
        request_timeout,
        max_attempts,
        auto_disable_after,
    })
}

/// The sealing key ring (current + optional previous), or `None` when no key is set.
pub fn key_ring(get: &impl Fn(&str) -> Option<String>) -> Result<Option<SecretBox>, ConfigError> {
    let permissive = matches!(runtime(get).as_deref(), Some("local") | Some("test"));
    let current = read_key(get, KEY, KEY_FILE, permissive)?;
    let previous = read_key(get, PREVIOUS, PREVIOUS_FILE, permissive)?;
    let key_error = |var: &'static str, e: SecretBoxError| {
        ConfigError::invalid(
            var,
            match e {
                SecretBoxError::WeakKey => {
                    "is a weak key (repeated or low-entropy bytes); generate 32 random bytes"
                }
                SecretBoxError::KeyId => "has an invalid key id ([A-Za-z0-9_-], 1-32)",
                _ => "must be 32 bytes, base64",
            },
        )
    };
    let Some(current) = current else {
        if previous.is_some() {
            return Err(ConfigError::Missing { var: KEY });
        }
        return Ok(None);
    };
    let current_id = get(KEY_ID).unwrap_or_else(|| DEFAULT_KEY_ID.to_string());
    let current = SealingKey::from_base64(&current_id, current.expose_secret())
        .map_err(|e| key_error(KEY, e))?;
    let previous = match previous {
        None => None,
        Some(raw) => {
            let id = get(PREVIOUS_ID).ok_or(ConfigError::Missing { var: PREVIOUS_ID })?;
            Some(
                SealingKey::from_base64(&id, raw.expose_secret())
                    .map_err(|e| key_error(PREVIOUS, e))?,
            )
        }
    };
    SecretBox::new(current, previous).map(Some).map_err(|_| {
        ConfigError::invalid(
            PREVIOUS,
            "must differ from the current key in both id and value",
        )
    })
}

/// `name` inline or `file_var` (a mounted file), exactly one.
fn read_key(
    get: &impl Fn(&str) -> Option<String>,
    name: &'static str,
    file_var: &'static str,
    permissive: bool,
) -> Result<Option<SecretString>, ConfigError> {
    match (get(name), get(file_var)) {
        (Some(_), Some(_)) => Err(ConfigError::invalid(
            name,
            format!("both {name} and {file_var} are set; configure exactly one"),
        )),
        (Some(v), None) => Ok(Some(SecretString::from(v))),
        (None, Some(path)) => read_file(file_var, &path, permissive).map(Some),
        (None, None) => Ok(None),
    }
}

fn read_file(var: &'static str, path: &str, permissive: bool) -> Result<SecretString, ConfigError> {
    let unreadable = || ConfigError::invalid(var, "the file cannot be read");
    let meta = std::fs::metadata(path).map_err(|_| unreadable())?;
    if !meta.is_file() || meta.len() > MAX_KEY_FILE_BYTES {
        return Err(ConfigError::invalid(
            var,
            "must name a regular file of at most 16 KiB",
        ));
    }
    #[cfg(unix)]
    if !permissive {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(ConfigError::invalid(
                var,
                "the file must not be readable by group or others (chmod 0400 / 0600)",
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = permissive;
    let text = String::from_utf8(std::fs::read(path).map_err(|_| unreadable())?)
        .map_err(|_| ConfigError::invalid(var, "the file is not UTF-8"))?;
    let value = text.trim();
    if value.is_empty() {
        return Err(ConfigError::invalid(var, "the file is empty"));
    }
    Ok(SecretString::from(value.to_string()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use base64::Engine;

    use super::*;

    fn b64(seed: u8) -> String {
        let mut k = [0u8; 32];
        for (i, b) in k.iter_mut().enumerate() {
            *b = seed.wrapping_add((i as u8).wrapping_mul(7));
        }
        base64::engine::general_purpose::STANDARD.encode(k)
    }

    fn lookup(pairs: &[(&str, String)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn defaults_are_disabled_shadow_and_fail_closed() {
        let s = from_lookup(&lookup(&[])).unwrap();
        assert!(!s.enabled);
        assert_eq!(
            s.mode,
            DeliveryMode::Shadow,
            "never double-delivers by default"
        );
        assert!(
            s.key_ring.is_none(),
            "no key: subscription routes fail closed"
        );
        assert_eq!(s.policy, EndpointPolicy::STRICT);
        assert_eq!(s.max_attempts, cf_webhooks::retry::MAX_ATTEMPTS);
        assert_eq!(s.auto_disable_after, 50);
    }

    #[test]
    fn the_key_ring_rotates_and_refuses_ambiguity_without_echo() {
        let one = key_ring(&lookup(&[(KEY, b64(1))])).unwrap().unwrap();
        assert_eq!((one.current_kid(), one.previous_kid()), ("k0", None));
        let rotated = key_ring(&lookup(&[
            (KEY, b64(2)),
            (KEY_ID, "k1".into()),
            (PREVIOUS, b64(1)),
            (PREVIOUS_ID, "k0".into()),
        ]))
        .unwrap()
        .unwrap();
        let sealed = one.seal("whsec_x", "sub");
        assert_eq!(rotated.open(&sealed, "sub").unwrap(), "whsec_x");
        let weak = base64::engine::general_purpose::STANDARD.encode([42u8; 32]);
        for pairs in [
            vec![(KEY, weak.clone())],
            vec![(KEY, "c2hvcnQ=".to_string())],
            vec![(KEY, b64(1)), (PREVIOUS, b64(2))],
            vec![(PREVIOUS, b64(2)), (PREVIOUS_ID, "old".to_string())],
            vec![
                (KEY, b64(1)),
                (PREVIOUS, b64(1)),
                (PREVIOUS_ID, "old".to_string()),
            ],
            vec![(KEY, b64(1)), (KEY_FILE, "/nonexistent".to_string())],
            vec![(KEY_FILE, "/nonexistent/key".to_string())],
        ] {
            let err = key_ring(&lookup(&pairs)).expect_err("refused");
            for (_, v) in &pairs {
                assert!(!err.to_string().contains(v.as_str()), "{err}");
            }
        }
    }

    #[test]
    fn private_endpoints_only_in_the_local_runtime_and_modes_are_validated() {
        let local = from_lookup(&lookup(&[
            (ALLOW_PRIVATE, "true".into()),
            ("CF_RUNTIME_ENVIRONMENT", "local".into()),
        ]))
        .unwrap();
        assert!(local.policy.allow_private_networks && local.policy.allow_insecure_http);
        for env in ["test", "staging", "production"] {
            assert!(from_lookup(&lookup(&[
                (ALLOW_PRIVATE, "true".into()),
                ("CF_RUNTIME_ENVIRONMENT", env.into()),
            ]))
            .is_err());
        }
        assert!(from_lookup(&lookup(&[(ALLOW_PRIVATE, "true".into())])).is_err());
        let active = from_lookup(&lookup(&[
            ("DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE", "active".into()),
            ("DEVELOPER_PLATFORM_WEBHOOKS_ENABLED", "true".into()),
        ]))
        .unwrap();
        assert!(active.enabled && active.mode == DeliveryMode::Active);
        assert!(from_lookup(&lookup(&[(
            "DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE",
            "loud".into()
        )]))
        .is_err());
    }
}
