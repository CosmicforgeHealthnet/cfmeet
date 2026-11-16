//! Passwordless sign-in support: app-review / test accounts, the request details shown in
//! code emails, and new-device detection for sign-in alerts.

use std::collections::HashMap;

use axum::http::HeaderMap;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use uuid::Uuid;

use crate::error::ApiResult;
use crate::services::account_security::{client_label, normalize_email, AuthPolicy};
use crate::services::otp::constant_time_eq;
use crate::templates::email::RequestDetails;

/// How far back a device counts as "seen before" for the new sign-in alert.
pub const KNOWN_DEVICE_WINDOW_DAYS: i32 = 90;

/// Fixed-code accounts for app-store review and automated acceptance, which cannot read
/// an inbox (`AUTH_TEST_ACCOUNTS="review@example.com:246810,qa@example.com:135790"`).
///
/// Refused outside a development `APP_ENV` unless `AUTH_TEST_ACCOUNTS_ALLOW_IN_PRODUCTION`
/// is `true` (the App Store / Play review case), so a stray variable can never open a
/// production backdoor silently. No email is sent to these addresses; sign-ins are
/// audited with `test_account: true`; they can never be made administrators.
#[derive(Clone, Default)]
pub struct TestAccounts {
    codes: HashMap<String, String>,
}

impl std::fmt::Debug for TestAccounts {
    // The fixed codes are credentials: never printed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestAccounts")
            .field("count", &self.codes.len())
            .finish()
    }
}

impl TestAccounts {
    /// Parse and gate the configuration. `Err` is a startup refusal with the reason.
    pub fn from_lookup(get: &impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let Some(raw) = get("AUTH_TEST_ACCOUNTS").filter(|v| !v.trim().is_empty()) else {
            return Ok(Self::default());
        };
        let mut codes = HashMap::new();
        for entry in raw.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            let (email, code) = entry
                .rsplit_once(':')
                .ok_or("AUTH_TEST_ACCOUNTS entries must be email:code")?;
            let email = normalize_email(email);
            let code = code.trim();
            if !email.contains('@') || code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit())
            {
                return Err("AUTH_TEST_ACCOUNTS entries must be email:<6 digits>".into());
            }
            codes.insert(email, code.to_string());
        }
        let development = crate::config::is_development_env(get("APP_ENV").as_deref());
        let allowed = matches!(
            get("AUTH_TEST_ACCOUNTS_ALLOW_IN_PRODUCTION")
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase()
                .as_str(),
            "true" | "1" | "yes"
        );
        if !development && !allowed {
            return Err("AUTH_TEST_ACCOUNTS is set outside a development APP_ENV; set AUTH_TEST_ACCOUNTS_ALLOW_IN_PRODUCTION=true only for store-review accounts".into());
        }
        if !development {
            tracing::warn!(
                count = codes.len(),
                "fixed-code test accounts are enabled in a production-like environment"
            );
        }
        Ok(Self { codes })
    }

    pub fn is_test_account(&self, email: &str) -> bool {
        self.codes.contains_key(&normalize_email(email))
    }

    /// `Some(matches)` for a test account, `None` for every other address.
    pub fn check(&self, email: &str, code: &str) -> Option<bool> {
        self.codes
            .get(&normalize_email(email))
            .map(|fixed| constant_time_eq(fixed.as_bytes(), code.as_bytes()))
    }
}

/// `102.89.x.x` / `2001:db8:x:x`: enough to recognise a network, not to locate a person.
pub fn mask_address(address: &str) -> String {
    if address.contains(':') {
        let parts: Vec<&str> = address.split(':').filter(|p| !p.is_empty()).take(2).collect();
        format!("{}:x:x", parts.join(":"))
    } else {
        let parts: Vec<&str> = address.split('.').take(2).collect();
        format!("{}.x.x", parts.join("."))
    }
}

/// Where and when the current request comes from, for code emails and sign-in alerts.
pub fn request_details(policy: &AuthPolicy, headers: &HeaderMap) -> RequestDetails {
    RequestDetails {
        client: client_label(headers).map(|l| l.chars().take(80).collect()),
        address: policy.client_ip(headers).map(|ip| mask_address(&ip)),
        when_utc: chrono::Utc::now().format("%Y-%m-%d %H:%M").to_string(),
    }
}

/// Whether another session of this account, opened in the last
/// [`KNOWN_DEVICE_WINDOW_DAYS`], came from the same client label.
pub async fn is_known_device(
    db: &DatabaseConnection,
    user_id: Uuid,
    new_session_id: Uuid,
    label: Option<&str>,
) -> ApiResult<bool> {
    let row = db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT 1 AS known FROM account_sessions \
             WHERE user_id = $1 AND id <> $2 AND client_label IS NOT DISTINCT FROM $3 \
               AND created_at > now() - make_interval(days => $4) LIMIT 1",
            [
                user_id.into(),
                new_session_id.into(),
                label.map(str::to_string).into(),
                KNOWN_DEVICE_WINDOW_DAYS.into(),
            ],
        ))
        .await?;
    Ok(row.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |k| {
            pairs
                .iter()
                .find(|(key, _)| *key == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn test_accounts_are_refused_in_production_unless_explicitly_allowed() {
        assert!(TestAccounts::from_lookup(&lookup(&[])).unwrap().codes.is_empty());
        let prod = TestAccounts::from_lookup(&lookup(&[("AUTH_TEST_ACCOUNTS", "r@x.com:246810")]));
        assert!(prod.is_err(), "unset APP_ENV is production-like");
        let prod = TestAccounts::from_lookup(&lookup(&[
            ("APP_ENV", "production"),
            ("AUTH_TEST_ACCOUNTS", "r@x.com:246810"),
        ]));
        assert!(prod.is_err());
        let allowed = TestAccounts::from_lookup(&lookup(&[
            ("APP_ENV", "production"),
            ("AUTH_TEST_ACCOUNTS", "r@x.com:246810"),
            ("AUTH_TEST_ACCOUNTS_ALLOW_IN_PRODUCTION", "true"),
        ]))
        .unwrap();
        assert_eq!(allowed.check("R@X.com ", "246810"), Some(true));
        assert_eq!(allowed.check("r@x.com", "246811"), Some(false));
        assert_eq!(allowed.check("other@x.com", "246810"), None);
        assert!(allowed.is_test_account("r@x.com"));
        assert!(!format!("{allowed:?}").contains("246810"), "codes never printed");
    }

    #[test]
    fn malformed_test_account_entries_are_refused() {
        for raw in ["r@x.com", "r@x.com:12345", "r@x.com:abcdef", "nobody:123456"] {
            let pairs: &'static [(&'static str, &'static str)] = Box::leak(Box::new([
                ("APP_ENV", "test"),
                ("AUTH_TEST_ACCOUNTS", raw),
            ]));
            assert!(TestAccounts::from_lookup(&lookup(pairs)).is_err(), "{raw}");
        }
        let ok = TestAccounts::from_lookup(&lookup(&[
            ("APP_ENV", "test"),
            ("AUTH_TEST_ACCOUNTS", " a@x.com:000001 , b@x.com:999999 "),
        ]))
        .unwrap();
        assert_eq!(ok.check("a@x.com", "000001"), Some(true));
        assert_eq!(ok.check("b@x.com", "999999"), Some(true));
    }

    #[test]
    fn addresses_are_masked() {
        assert_eq!(mask_address("102.89.34.7"), "102.89.x.x");
        assert_eq!(mask_address("2001:db8:85a3::8a2e:370:7334"), "2001:db8:x:x");
    }
}
