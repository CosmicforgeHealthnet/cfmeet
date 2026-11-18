//! Notification Service configuration, read from the environment. Secrets are FILES only
//! (inline secrets are refused); error messages never echo a value.
//!
//! | Variable | Required | Meaning |
//! |---|---|---|
//! | `DATABASE_URL` | yes | the service's OWN database `cfmeet_notifications` |
//! | `NOTIFICATIONS_DATABASE_MAX_CONNECTIONS` | no | 1–100, default 10 |
//! | `NOTIFICATIONS_BIND_ADDRESS` | no | default `127.0.0.1:8102` |
//! | `NOTIFICATIONS_REPLICA_ID` | no | lease-holder name (default: random) |
//! | `NOTIFICATIONS_WORKERS_ENABLED` | no | `true` (default) / `false` |
//! | `LOG_FORMAT` | no | `pretty` / `json` |
//! | `CF_SERVICE_ID` | no | if set, must be `cfmeet.notifications` |
//! | `CF_RUNTIME_ENVIRONMENT` | yes | `local`, `test`, `staging`, `production` |
//! | `CF_SERVICE_TRUST_BUNDLE_FILE` | yes | receiver trust bundle of `cfmeet.notifications` |
//! | `NOTIFICATIONS_DATA_KEY_FILE` / `_ID` | yes | 32-byte base64 master key; seals data at rest and keys recipient digests |
//! | `NOTIFICATIONS_DATA_KEY_PREVIOUS_FILE` / `_ID` | no | previous master key (rotation; opens only) |
//! | `NOTIFICATIONS_HPKE_PRIVATE_KEY_FILE` / `NOTIFICATIONS_HPKE_KEY_ID` | no | opens sealed codes / tokens (renderer only) |
//! | `NOTIFICATIONS_EMAIL_PROVIDER` | no | `resend` / `smtp` / `none`; unset: `smtp` if `NOTIFICATIONS_SMTP_HOST` is set, else `none` |
//! | `NOTIFICATIONS_RESEND_API_KEY_FILE`, `NOTIFICATIONS_RESEND_API_URL` | with resend | sending-only key file; default URL `https://api.resend.com` |
//! | `NOTIFICATIONS_RESEND_WEBHOOK_SECRET_FILE` | no | `whsec_…`; the provider callback answers 404 without it |
//! | `NOTIFICATIONS_SMTP_HOST`, `_PORT`, `_USERNAME`, `_PASSWORD_FILE` | with smtp | |
//! | `NOTIFICATIONS_EMAIL_FROM` | no | default `CF Meet <no-reply@cfmeet.com>` |
//! | `NOTIFICATIONS_EMAIL_REPLY_TO` | no | |
//! | `NOTIFICATIONS_EMAIL_SENDER_PROFILES` | no | `profile=Name <addr>;…` (e.g. `organization-invitation=…`); unknown profiles use the default sender |
//! | `NOTIFICATIONS_EMAIL_MAX_PER_SECOND` | no | 1–50, default 2 (Resend free plan) |
//! | `NOTIFICATIONS_EMAIL_MAX_ATTEMPTS` | no | 1–30, default 8 |
//! | `NOTIFICATIONS_EMAIL_RETENTION_DAYS` | no | 7–365, default 30 |
//! | `NOTIFICATIONS_APP_URL` | no | link base in emails, default `https://cfmeet.com` |
//! | `NOTIFICATIONS_PUSH_PROVIDER` | no | `none` (default), `live`, `local-test` (local/test only) |
//! | `NOTIFICATIONS_PUSH_WEB_VAPID_KEY_FILE`, `_WEB_SUBJECT`, `_WEB_ALLOWED_HOSTS` | with live Web Push | VAPID key: P-256 PKCS#8 (or SEC1) PEM file — the SAME file Messaging's `activate-web-push` generated (`MESSAGING_PUSH_WEB_VAPID_KEY_FILE`), so existing browser subscriptions stay valid; subject `https://cfmeet.com`; hosts default to Messaging's allowlist |
//! | `NOTIFICATIONS_PUSH_APNS_KEY_FILE`, `_APNS_KEY_ID`, `_APNS_TEAM_ID`, `_APNS_TOPIC`, `_APNS_BASE_URL` | with live APNs | |
//! | `NOTIFICATIONS_PUSH_FCM_SERVICE_ACCOUNT_FILE`, `_FCM_BASE_URL` | with live FCM | |
//! | `NOTIFICATIONS_PARTNER_PUSH_ENABLED` | no | `false` (default) / `true`: ADR 0063 partner push (provider vault, `platform-principal` registrations, call fast lane and recipients). Off: every partner route answers 404 and every partner shape is refused exactly as before |
//! | `NOTIFICATIONS_PARTNER_PUSH_TRANSPORT` | no | `live` (default: the partner's own APNs / FCM keys through `cf-push-transport`) or `local-test` (local/test only: the deterministic recording transport) |
//! | `NOTIFICATIONS_AUTHORITIES` | no | comma list of owner APIs to call: `account`, `meeting`, `messaging`, `communications`, `organizations` (default none) |
//! | `CF_MEETING_SERVICE_BASE_URL`, `CF_MESSAGING_BASE_URL`, `CF_COMMUNICATIONS_BASE_URL`, `CF_ORGANIZATIONS_BASE_URL` | per authority | internal base URLs |
//! | `CF_SERVICE_AUTH_KEY_ID`, `CF_SERVICE_AUTH_PRIVATE_KEY_FILE` | with authorities | outbound signing |
//! | `CFMEET_COMMUNICATION_TENANT_ID`, `_APPLICATION_ID`, `_ENVIRONMENT_ID`, `_ENVIRONMENT_KIND`, `_SERVICE_ACCOUNT_ID` | with `communications` | the first-party scope X4 requires as delegated context |
//! | `NOTIFICATIONS_OPERATOR_EMAILS_FILE` | no | operator group addresses (one per line) for operator alerts |
//! | `NOTIFICATIONS_OPERATOR_ALERT_TOKEN_FILE` | no | static bearer of the I12 Alertmanager receiver; I12 answers 404 without it |

use std::{fmt, net::SocketAddr, path::PathBuf, time::Duration};

use base64::Engine;
use cf_service_auth::{
    services, KeyId, RuntimeEnvironment, ServiceId, ServiceTokenVerifier, SharedClock, TokenTtl,
    TrustPolicy,
};
use secrecy::{ExposeSecret, SecretString};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{var} is not set")]
    Missing { var: &'static str },
    /// The message never echoes the offending value (it may be a secret).
    #[error("{var} is invalid: {reason}")]
    Invalid { var: &'static str, reason: String },
}

fn invalid(var: &'static str, reason: impl Into<String>) -> ConfigError {
    ConfigError::Invalid {
        var,
        reason: reason.into(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Pretty,
    Json,
}

pub const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:8102";
pub const DEFAULT_FROM: &str = "CF Meet <no-reply@cfmeet.com>";
pub const DEFAULT_RESEND_URL: &str = "https://api.resend.com";

pub fn notifications_service_id() -> ServiceId {
    ServiceId::parse(services::NOTIFICATIONS).expect("canonical identity")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmailProviderKind {
    Resend,
    Smtp,
    None,
}

impl EmailProviderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Resend => "resend",
            Self::Smtp => "smtp",
            Self::None => "none",
        }
    }
}

#[derive(Clone)]
pub struct EmailConfig {
    pub provider: EmailProviderKind,
    pub resend_api_url: String,
    pub resend_api_key: SecretString,
    pub resend_webhook_secret: Option<SecretString>,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_username: String,
    pub smtp_password: SecretString,
    pub from: String,
    pub reply_to: Option<String>,
    /// Named sender profiles (`organization-invitation` → From header). Configuration,
    /// not code: the owner decides the invitation sender later.
    pub sender_profiles: Vec<(String, String)>,
    pub max_per_second: u32,
    pub max_attempts: i32,
    pub retention_days: i64,
    pub app_url: String,
}

impl fmt::Debug for EmailConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EmailConfig")
            .field("provider", &self.provider)
            .field("from", &self.from)
            .field("webhook", &self.resend_webhook_secret.is_some())
            .field(
                "sender_profiles",
                &self
                    .sender_profiles
                    .iter()
                    .map(|(p, _)| p)
                    .collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

impl EmailConfig {
    /// The From header for a sender profile (unknown profiles use the default sender).
    pub fn from_for(&self, profile: &str) -> &str {
        self.sender_profiles
            .iter()
            .find(|(p, _)| p == profile)
            .map(|(_, f)| f.as_str())
            .unwrap_or(&self.from)
    }

    /// A test / local configuration (no provider).
    pub fn disabled() -> Self {
        Self {
            provider: EmailProviderKind::None,
            resend_api_url: DEFAULT_RESEND_URL.into(),
            resend_api_key: SecretString::from(String::new()),
            resend_webhook_secret: None,
            smtp_host: String::new(),
            smtp_port: 587,
            smtp_username: String::new(),
            smtp_password: SecretString::from(String::new()),
            from: DEFAULT_FROM.into(),
            reply_to: None,
            sender_profiles: Vec::new(),
            max_per_second: 50,
            max_attempts: 8,
            retention_days: 30,
            app_url: "https://cfmeet.com".into(),
        }
    }
}

/// A plain `Name <local@domain>` or `local@domain` check (no header injection).
pub fn valid_mailbox(value: &str) -> bool {
    let v = value.trim();
    if v.is_empty() || v.len() > 254 || v.chars().any(|c| c.is_control()) {
        return false;
    }
    let addr = match (v.find('<'), v.rfind('>')) {
        (Some(a), Some(b)) if a < b && b == v.len() - 1 => &v[a + 1..b],
        (None, None) => v,
        _ => return false,
    };
    valid_address(addr)
}

pub fn valid_address(addr: &str) -> bool {
    let Some((local, domain)) = addr.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && local.len() <= 64
        && domain.contains('.')
        && domain.len() <= 253
        && !addr
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '<' | '>' | '"' | ',' | ';'))
        && domain
            .split('.')
            .all(|l| !l.is_empty() && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushProviderKind {
    None,
    Live,
    LocalTest,
}

/// The transport partner pushes use (ADR 0063).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartnerTransportKind {
    /// The partner's own APNs / FCM credentials through the `cf-push-transport` adapters.
    Live,
    /// The deterministic recording transport (local / test only).
    LocalTest,
}

#[derive(Debug, Clone)]
pub struct OutboundSettings {
    pub key_id: KeyId,
    pub private_key_file: PathBuf,
    pub token_ttl: TokenTtl,
    pub http_timeout: Duration,
}

/// The CF Meet first-party scope the Hub's X4 requires as signed delegated context (the
/// same `CFMEET_COMMUNICATION_*` values the Meeting Service bridge uses).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirstPartyScope {
    pub tenant_id: uuid::Uuid,
    pub application_id: uuid::Uuid,
    pub environment_id: uuid::Uuid,
    pub kind: cf_service_auth::ApplicationEnvironmentKind,
    pub service_account_id: uuid::Uuid,
}

#[derive(Debug, Clone, Default)]
pub struct AuthorityTargets {
    pub account: Option<String>,
    pub meeting: Option<String>,
    pub messaging: Option<String>,
    pub communications: Option<String>,
    pub organizations: Option<String>,
    /// Required with `communications`.
    pub first_party_scope: Option<FirstPartyScope>,
}

impl AuthorityTargets {
    pub fn enabled(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        for (name, on) in [
            ("account", self.account.is_some()),
            ("meeting", self.meeting.is_some()),
            ("messaging", self.messaging.is_some()),
            ("communications", self.communications.is_some()),
            ("organizations", self.organizations.is_some()),
        ] {
            if on {
                v.push(name);
            }
        }
        v
    }
}

pub struct NotificationsConfig {
    pub database_url: SecretString,
    pub database_max_connections: u32,
    pub bind_address: SocketAddr,
    pub replica_id: String,
    pub workers_enabled: bool,
    pub log_format: LogFormat,
    pub runtime_environment: RuntimeEnvironment,
    pub trust_bundle_file: PathBuf,
    pub data_key: (String, Vec<u8>),
    pub data_key_previous: Option<(String, Vec<u8>)>,
    pub hpke: Option<(String, Vec<u8>)>,
    pub email: EmailConfig,
    pub push_provider: PushProviderKind,
    pub push: cf_push_transport::config::PushAdapterConfig,
    /// `NOTIFICATIONS_PARTNER_PUSH_ENABLED` (default off).
    pub partner_push_enabled: bool,
    pub partner_push_transport: PartnerTransportKind,
    pub authorities: AuthorityTargets,
    pub outbound: Option<OutboundSettings>,
    pub operator_emails: Vec<String>,
    pub operator_alert_token: Option<SecretString>,
}

impl fmt::Debug for NotificationsConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NotificationsConfig")
            .field("database_url", &"[REDACTED]")
            .field("bind_address", &self.bind_address)
            .field("replica_id", &self.replica_id)
            .field("workers_enabled", &self.workers_enabled)
            .field("runtime_environment", &self.runtime_environment)
            .field("data_key_id", &self.data_key.0)
            .field("hpke", &self.hpke.as_ref().map(|(k, _)| k))
            .field("email", &self.email)
XX, &self.authorities.enabled())
            .finish_non_exhaustive()
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
            .parse::<i64>()
            .ok()
            .filter(|n| (min..=max).contains(n))
            .ok_or_else(|| invalid(var, format!("must be an integer between {min} and {max}"))),
    }
}

fn read_secret_file(var: &'static str, path: &str) -> Result<String, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|_| invalid(var, "cannot read the file"))?;
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err(invalid(var, "the file is empty"));
    }
    Ok(text)
}

fn key_id(var: &'static str, value: &str) -> Result<String, ConfigError> {
    if cf_webhooks::secret_box::valid_key_id(value) {
        Ok(value.to_string())
    } else {
        Err(invalid(var, "is not a valid key id"))
    }
}

fn decode_key(var: &'static str, text: &str) -> Result<Vec<u8>, ConfigError> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(text.trim())
        .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(text.trim()))
        .map_err(|_| invalid(var, "must be base64"))?;
    if raw.len() != 32 {
        return Err(invalid(var, "must decode to 32 bytes"));
    }
    Ok(raw)
}

fn flag(
    get: &impl Fn(&str) -> Option<String>,
    var: &'static str,
    default: bool,
) -> Result<bool, ConfigError> {
    match get(var).as_deref() {
        None => Ok(default),
        Some("true") => Ok(true),
        Some("false") => Ok(false),
        Some(_) => Err(invalid(var, "must be `true` or `false`")),
    }
}

impl NotificationsConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// Builds the configuration from an arbitrary key lookup (environment, tests).
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let get = |key: &str| {
            get(key)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let required = |var: &'static str| get(var).ok_or(ConfigError::Missing { var });
        for inline in [
            "CF_SERVICE_AUTH_PRIVATE_KEY",
            "NOTIFICATIONS_RESEND_API_KEY",
            "NOTIFICATIONS_SMTP_PASSWORD",
            "NOTIFICATIONS_RESEND_WEBHOOK_SECRET",
            "NOTIFICATIONS_DATA_KEY",
        ] {
            if get(inline).is_some() {
                return Err(ConfigError::Invalid {
                    var: "inline secret",
                    reason: format!("{inline} is not accepted; use the _FILE variable"),
                });
            }
        }
        let database_url = SecretString::from(required("DATABASE_URL")?);
        let bind_address = match get("NOTIFICATIONS_BIND_ADDRESS") {
            None => DEFAULT_BIND_ADDRESS.parse().expect("valid default"),
            Some(v) => v.parse::<SocketAddr>().map_err(|_| {
                invalid(
                    "NOTIFICATIONS_BIND_ADDRESS",
                    "must be a socket address such as 127.0.0.1:8102",
                )
            })?,
        };
        let database_max_connections =
            bounded(&get, "NOTIFICATIONS_DATABASE_MAX_CONNECTIONS", 10, 1, 100)? as u32;
        let replica_id = match get("NOTIFICATIONS_REPLICA_ID") {
            None => format!(
                "notifications-{}",
                &uuid::Uuid::new_v4().simple().to_string()[..12]
            ),
            Some(v)
                if (1..=64).contains(&v.len())
                    && v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') =>
            {
                v
            }
            Some(_) => {
                return Err(invalid(
                    "NOTIFICATIONS_REPLICA_ID",
                    "must be 1-64 of [A-Za-z0-9-]",
                ))
            }
        };
        let workers_enabled = flag(&get, "NOTIFICATIONS_WORKERS_ENABLED", true)?;
        let log_format = match get("LOG_FORMAT").as_deref() {
            None | Some("pretty") => LogFormat::Pretty,
            Some("json") => LogFormat::Json,
            Some(_) => return Err(invalid("LOG_FORMAT", "must be `pretty` or `json`")),
        };
        if let Some(id) = get("CF_SERVICE_ID") {
            if id != services::NOTIFICATIONS {
                return Err(invalid("CF_SERVICE_ID", "must be cfmeet.notifications"));
            }
        }
        let runtime_environment = RuntimeEnvironment::parse(&required("CF_RUNTIME_ENVIRONMENT")?)
            .map_err(|_| {
            invalid(
                "CF_RUNTIME_ENVIRONMENT",
                "must be local, test, staging or production",
            )
        })?;
        let production_like = matches!(
            runtime_environment,
            RuntimeEnvironment::Staging | RuntimeEnvironment::Production
        );
        let trust_bundle_file = PathBuf::from(required("CF_SERVICE_TRUST_BUNDLE_FILE")?);

        let data_key = (
            key_id(
                "NOTIFICATIONS_DATA_KEY_ID",
                &required("NOTIFICATIONS_DATA_KEY_ID")?,
            )?,
            decode_key(
                "NOTIFICATIONS_DATA_KEY_FILE",
                &read_secret_file(
                    "NOTIFICATIONS_DATA_KEY_FILE",
                    &required("NOTIFICATIONS_DATA_KEY_FILE")?,
                )?,
            )?,
        );
        let data_key_previous = match get("NOTIFICATIONS_DATA_KEY_PREVIOUS_FILE") {
            None => None,
            Some(path) => Some((
                key_id(
                    "NOTIFICATIONS_DATA_KEY_PREVIOUS_ID",
                    &required("NOTIFICATIONS_DATA_KEY_PREVIOUS_ID")?,
                )?,
                decode_key(
                    "NOTIFICATIONS_DATA_KEY_PREVIOUS_FILE",
                    &read_secret_file("NOTIFICATIONS_DATA_KEY_PREVIOUS_FILE", &path)?,
                )?,
            )),
        };
        let hpke = match get("NOTIFICATIONS_HPKE_PRIVATE_KEY_FILE") {
            None => None,
            Some(path) => {
                let kid = required("NOTIFICATIONS_HPKE_KEY_ID")?;
                if !cf_notification_client::seal::valid_kid(&kid) {
                    return Err(invalid(
                        "NOTIFICATIONS_HPKE_KEY_ID",
                        "is not a valid key id",
                    ));
                }
                Some((
                    kid,
                    decode_key(
                        "NOTIFICATIONS_HPKE_PRIVATE_KEY_FILE",
                        &read_secret_file("NOTIFICATIONS_HPKE_PRIVATE_KEY_FILE", &path)?,
                    )?,
                ))
            }
        };

        // --- email (Document 4 §14.1: same selection rules as the Meeting Service) ---
        let provider = match get("NOTIFICATIONS_EMAIL_PROVIDER").as_deref() {
            Some("resend") => EmailProviderKind::Resend,
            Some("smtp") => EmailProviderKind::Smtp,
            Some("none") => EmailProviderKind::None,
            None if get("NOTIFICATIONS_SMTP_HOST").is_some() => EmailProviderKind::Smtp,
            None => EmailProviderKind::None,
            Some(_) => {
                return Err(invalid(
                    "NOTIFICATIONS_EMAIL_PROVIDER",
                    "must be resend, smtp or none",
                ))
            }
        };
        let resend_api_key = if provider == EmailProviderKind::Resend {
            let key = read_secret_file(
                "NOTIFICATIONS_RESEND_API_KEY_FILE",
                &required("NOTIFICATIONS_RESEND_API_KEY_FILE")?,
            )?;
            if !key.starts_with("re_") {
                return Err(invalid(
                    "NOTIFICATIONS_RESEND_API_KEY_FILE",
                    "is not a Resend API key",
                ));
            }
            SecretString::from(key)
        } else {
            SecretString::from(String::new())
        };
        let resend_webhook_secret = match get("NOTIFICATIONS_RESEND_WEBHOOK_SECRET_FILE") {
            None => None,
            Some(path) => {
                let s = read_secret_file("NOTIFICATIONS_RESEND_WEBHOOK_SECRET_FILE", &path)?;
                if !s.starts_with("whsec_") {
                    return Err(invalid(
                        "NOTIFICATIONS_RESEND_WEBHOOK_SECRET_FILE",
                        "is not a whsec_ signing secret",
                    ));
                }
                Some(SecretString::from(s))
            }
        };
        let resend_api_url =
            get("NOTIFICATIONS_RESEND_API_URL").unwrap_or_else(|| DEFAULT_RESEND_URL.into());
        if production_like && !resend_api_url.starts_with("https://") {
            return Err(invalid(
                "NOTIFICATIONS_RESEND_API_URL",
                "must be https in staging and production",
            ));
        }
        let (smtp_host, smtp_port, smtp_username, smtp_password) =
            if provider == EmailProviderKind::Smtp {
                (
                    required("NOTIFICATIONS_SMTP_HOST")?,
                    bounded(&get, "NOTIFICATIONS_SMTP_PORT", 587, 1, 65535)? as u16,
                    required("NOTIFICATIONS_SMTP_USERNAME")?,
                    SecretString::from(read_secret_file(
                        "NOTIFICATIONS_SMTP_PASSWORD_FILE",
                        &required("NOTIFICATIONS_SMTP_PASSWORD_FILE")?,
                    )?),
                )
            } else {
                (
                    String::new(),
                    587,
                    String::new(),
                    SecretString::from(String::new()),
                )
            };
        let from = get("NOTIFICATIONS_EMAIL_FROM").unwrap_or_else(|| DEFAULT_FROM.into());
        if !valid_mailbox(&from) {
            return Err(invalid(
                "NOTIFICATIONS_EMAIL_FROM",
                "must be `Name <address>` or an address",
            ));
        }
        let reply_to = get("NOTIFICATIONS_EMAIL_REPLY_TO");
        if reply_to.as_deref().is_some_and(|r| !valid_mailbox(r)) {
            return Err(invalid(
                "NOTIFICATIONS_EMAIL_REPLY_TO",
                "must be an address",
            ));
        }
        let mut sender_profiles = Vec::new();
        for entry in get("NOTIFICATIONS_EMAIL_SENDER_PROFILES")
            .unwrap_or_default()
            .split(';')
            .map(str::trim)
            .filter(|e| !e.is_empty())
        {
            let Some((profile, mailbox)) = entry.split_once('=') else {
                return Err(invalid(
                    "NOTIFICATIONS_EMAIL_SENDER_PROFILES",
                    "entries are profile=Name <address>",
                ));
            };
            let profile = profile.trim();
            if !(1..=32).contains(&profile.len())
                || !profile
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                || !valid_mailbox(mailbox)
            {
                return Err(invalid(
                    "NOTIFICATIONS_EMAIL_SENDER_PROFILES",
                    "invalid profile entry",
                ));
            }
            sender_profiles.push((profile.to_string(), mailbox.trim().to_string()));
        }
        let app_url = get("NOTIFICATIONS_APP_URL").unwrap_or_else(|| "https://cfmeet.com".into());
        if !(app_url.starts_with("https://")
            || (!production_like && app_url.starts_with("http://")))
        {
            return Err(invalid("NOTIFICATIONS_APP_URL", "must be an https URL"));
        }
        let email = EmailConfig {
            provider,
            resend_api_url,
            resend_api_key,
            resend_webhook_secret,
            smtp_host,
            smtp_port,
            smtp_username,
            smtp_password,
            from,
            reply_to,
            sender_profiles,
            max_per_second: bounded(&get, "NOTIFICATIONS_EMAIL_MAX_PER_SECOND", 2, 1, 50)? as u32,
            max_attempts: bounded(&get, "NOTIFICATIONS_EMAIL_MAX_ATTEMPTS", 8, 1, 30)? as i32,
            retention_days: bounded(&get, "NOTIFICATIONS_EMAIL_RETENTION_DAYS", 30, 7, 365)?,
            app_url: app_url.trim_end_matches('/').to_string(),
        };

        // --- push ---
        let push_provider = match get("NOTIFICATIONS_PUSH_PROVIDER").as_deref() {
            None | Some("none") => PushProviderKind::None,
            Some("live") => PushProviderKind::Live,
            Some("local-test") if !production_like => PushProviderKind::LocalTest,
            Some("local-test") => {
                return Err(invalid(
                    "NOTIFICATIONS_PUSH_PROVIDER",
                    "local-test is refused in staging and production",
                ))
            }
            Some(_) => {
                return Err(invalid(
                    "NOTIFICATIONS_PUSH_PROVIDER",
                    "must be none, live or local-test",
                ))
            }
        };
        let mut push = cf_push_transport::config::PushAdapterConfig::default();
        if push_provider == PushProviderKind::Live {
            if let Some(file) = get("NOTIFICATIONS_PUSH_WEB_VAPID_KEY_FILE") {
                push.web = Some(cf_push_transport::config::WebPushConfig {
                    vapid_key_file: PathBuf::from(file),
                    subject: required("NOTIFICATIONS_PUSH_WEB_SUBJECT")?,
                    allowed_hosts: get("NOTIFICATIONS_PUSH_WEB_ALLOWED_HOSTS")
                        .map(|h| {
                            h.split(',')
                                .map(|s| s.trim().to_string())
                                .filter(|s| !s.is_empty())
                                .collect()
                        })
                        // Messaging's default allowlist (N9 parity): every browser
                        // subscription Messaging accepted is accepted here (Edge's WNS
                        // endpoints are `<node>.notify.windows.com`, Firefox's
                        // `<x>.push.services.mozilla.com`).
                        .unwrap_or_else(|| {
                            cf_push_transport::adapters::DEFAULT_WEB_PUSH_HOSTS
                                .iter()
                                .map(|s| s.to_string())
                                .collect()
                        }),
                });
            }
            if let Some(file) = get("NOTIFICATIONS_PUSH_APNS_KEY_FILE") {
                push.apns = Some(cf_push_transport::config::ApnsConfig {
                    key_file: PathBuf::from(file),
                    key_id: required("NOTIFICATIONS_PUSH_APNS_KEY_ID")?,
                    team_id: required("NOTIFICATIONS_PUSH_APNS_TEAM_ID")?,
                    topic: required("NOTIFICATIONS_PUSH_APNS_TOPIC")?,
                    base_url: get("NOTIFICATIONS_PUSH_APNS_BASE_URL")
                        .unwrap_or_else(|| "https://api.push.apple.com".into()),
                });
            }
            if let Some(file) = get("NOTIFICATIONS_PUSH_FCM_SERVICE_ACCOUNT_FILE") {
                push.fcm = Some(cf_push_transport::config::FcmConfig {
                    service_account_file: PathBuf::from(file),
                    base_url: get("NOTIFICATIONS_PUSH_FCM_BASE_URL")
                        .unwrap_or_else(|| "https://fcm.googleapis.com".into()),
                });
            }
        }

        // --- partner push (ADR 0063) ---
        let partner_push_enabled = flag(&get, "NOTIFICATIONS_PARTNER_PUSH_ENABLED", false)?;
        let partner_push_transport = match get("NOTIFICATIONS_PARTNER_PUSH_TRANSPORT").as_deref() {
            None | Some("live") => PartnerTransportKind::Live,
            Some("local-test") if !production_like => PartnerTransportKind::LocalTest,
            Some("local-test") => {
                return Err(invalid(
                    "NOTIFICATIONS_PARTNER_PUSH_TRANSPORT",
                    "local-test is refused in staging and production",
                ))
            }
            Some(_) => {
                return Err(invalid(
                    "NOTIFICATIONS_PARTNER_PUSH_TRANSPORT",
                    "must be live or local-test",
                ))
            }
        };

        // --- owner APIs X1–X5 ---
        let mut authorities = AuthorityTargets::default();
        for name in get("NOTIFICATIONS_AUTHORITIES")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|n| !n.is_empty())
        {
            match name {
                "account" => authorities.account = Some(required("CF_MEETING_SERVICE_BASE_URL")?),
                "meeting" => authorities.meeting = Some(required("CF_MEETING_SERVICE_BASE_URL")?),
                "messaging" => authorities.messaging = Some(required("CF_MESSAGING_BASE_URL")?),
                "communications" => {
                    authorities.communications = Some(required("CF_COMMUNICATIONS_BASE_URL")?);
                    let id = |var: &'static str| -> Result<uuid::Uuid, ConfigError> {
                        uuid::Uuid::parse_str(&required(var)?)
                            .map_err(|_| invalid(var, "must be a UUID"))
                    };
                    let kind = match required("CFMEET_COMMUNICATION_ENVIRONMENT_KIND")?.as_str() {
                        "SANDBOX" => cf_service_auth::ApplicationEnvironmentKind::Sandbox,
                        "TEST" => cf_service_auth::ApplicationEnvironmentKind::Test,
                        "PRODUCTION" => cf_service_auth::ApplicationEnvironmentKind::Production,
                        _ => {
                            return Err(invalid(
                                "CFMEET_COMMUNICATION_ENVIRONMENT_KIND",
                                "must be SANDBOX, TEST or PRODUCTION",
                            ))
                        }
                    };
                    authorities.first_party_scope = Some(FirstPartyScope {
                        tenant_id: id("CFMEET_COMMUNICATION_TENANT_ID")?,
                        application_id: id("CFMEET_COMMUNICATION_APPLICATION_ID")?,
                        environment_id: id("CFMEET_COMMUNICATION_ENVIRONMENT_ID")?,
                        kind,
                        service_account_id: id("CFMEET_COMMUNICATION_SERVICE_ACCOUNT_ID")?,
                    });
                }
                "organizations" => {
                    authorities.organizations = Some(required("CF_ORGANIZATIONS_BASE_URL")?)
                }
                _ => {
                    return Err(invalid(
                        "NOTIFICATIONS_AUTHORITIES",
                        "names an unknown owner API",
                    ))
                }
            }
        }
        let outbound = if authorities.enabled().is_empty() {
            None
        } else {
            Some(OutboundSettings {
                key_id: KeyId::parse(&required("CF_SERVICE_AUTH_KEY_ID")?)
                    .map_err(|_| invalid("CF_SERVICE_AUTH_KEY_ID", "is not a valid key id"))?,
                private_key_file: PathBuf::from(required("CF_SERVICE_AUTH_PRIVATE_KEY_FILE")?),
                token_ttl: TokenTtl::default(),
                http_timeout: Duration::from_millis(bounded(
                    &get,
                    "CF_INTERNAL_HTTP_TIMEOUT_MS",
                    3_000,
                    100,
                    30_000,
                )? as u64),
            })
        };
        let operator_emails = match get("NOTIFICATIONS_OPERATOR_EMAILS_FILE") {
            None => Vec::new(),
            Some(path) => {
                let text = read_secret_file("NOTIFICATIONS_OPERATOR_EMAILS_FILE", &path)?;
                let list: Vec<String> = text
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty() && !l.starts_with('#'))
                    .map(str::to_string)
                    .collect();
                if list.iter().any(|a| !valid_address(a)) || list.len() > 20 {
                    return Err(invalid(
                        "NOTIFICATIONS_OPERATOR_EMAILS_FILE",
                        "must hold up to 20 addresses",
                    ));
                }
                list
            }
        };
        let operator_alert_token = match get("NOTIFICATIONS_OPERATOR_ALERT_TOKEN_FILE") {
            None => None,
            Some(path) => {
                let t = read_secret_file("NOTIFICATIONS_OPERATOR_ALERT_TOKEN_FILE", &path)?;
                if t.len() < 32 {
                    return Err(invalid(
                        "NOTIFICATIONS_OPERATOR_ALERT_TOKEN_FILE",
                        "must be at least 32 characters",
                    ));
                }
                Some(SecretString::from(t))
            }
        };
        Ok(Self {
            database_url,
            database_max_connections,
            bind_address,
            replica_id,
            workers_enabled,
            log_format,
            runtime_environment,
            trust_bundle_file,
            data_key,
            data_key_previous,
            hpke,
            email,
            push_provider,
            push,
            partner_push_enabled,
            partner_push_transport,
            authorities,
            outbound,
            operator_emails,
            operator_alert_token,
        })
    }

    pub fn database_url(&self) -> &str {
        self.database_url.expose_secret()
    }

    pub fn verifier(
        &self,
        clock: SharedClock,
    ) -> Result<std::sync::Arc<ServiceTokenVerifier>, ConfigError> {
        let policy = TrustPolicy::from_file(
            &self.trust_bundle_file,
            &notifications_service_id(),
            self.runtime_environment,
        )
        .map_err(|e| invalid("CF_SERVICE_TRUST_BUNDLE_FILE", e.to_string()))?;
        Ok(std::sync::Arc::new(ServiceTokenVerifier::new(
            policy, clock,
        )))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn write(dir: &std::path::Path, name: &str, content: &str) -> String {
        let p = dir.join(name);
        std::fs::write(&p, content).unwrap();
        p.to_string_lossy().into_owned()
    }

    fn base(dir: &std::path::Path) -> HashMap<String, String> {
        let raw: Vec<u8> = (0u8..32)
            .map(|i| i.wrapping_mul(37).wrapping_add(11))
            .collect();
        let key = base64::engine::general_purpose::STANDARD.encode(raw);
        HashMap::from([
            (
                "DATABASE_URL".to_string(),
                "postgres://n:pw-secret@db/n".to_string(),
            ),
            ("CF_RUNTIME_ENVIRONMENT".into(), "production".into()),
            (
                "CF_SERVICE_TRUST_BUNDLE_FILE".into(),
                "/run/secrets/trust.json".into(),
            ),
            ("NOTIFICATIONS_DATA_KEY_ID".into(), "data-1".into()),
            (
                "NOTIFICATIONS_DATA_KEY_FILE".into(),
                write(dir, "data.key", &key),
            ),
        ])
    }

    fn parse(map: HashMap<String, String>) -> Result<NotificationsConfig, ConfigError> {
        NotificationsConfig::from_lookup(move |k| map.get(k).cloned())
    }

    fn tmp() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("notif-cfg-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn dark_defaults_deliver_nothing() {
        let d = tmp();
        let c = parse(base(&d)).unwrap();
        assert_eq!(c.email.provider, EmailProviderKind::None);
        assert_eq!(c.push_provider, PushProviderKind::None);
        assert_eq!(c.email.from, "CF Meet <no-reply@cfmeet.com>");
        assert!(c.authorities.enabled().is_empty());
        assert!(c.outbound.is_none());
        let dbg = format!("{c:?}");
        assert!(!dbg.contains("pw-secret"));
    }

    #[test]
    fn inline_secrets_are_refused_without_echo() {
        let d = tmp();
        let mut m = base(&d);
        m.insert(
            "NOTIFICATIONS_RESEND_API_KEY".into(),
            "re_inline_secret_value".into(),
        );
        let e = parse(m).unwrap_err().to_string();
        assert!(e.contains("_FILE"));
        assert!(!e.contains("re_inline_secret_value"));
    }

    #[test]
    fn resend_requires_a_resend_key_file_and_profiles_are_configurable() {
        let d = tmp();
        let mut m = base(&d);
        m.insert("NOTIFICATIONS_EMAIL_PROVIDER".into(), "resend".into());
        assert!(parse(m.clone()).is_err());
        m.insert(
            "NOTIFICATIONS_RESEND_API_KEY_FILE".into(),
            write(&d, "k", "re_abcdefghijkl"),
        );
        m.insert(
            "NOTIFICATIONS_EMAIL_SENDER_PROFILES".into(),
            "organization-invitation=CF Meet Teams <no-reply@cfmeet.com>".into(),
        );
        let c = parse(m).unwrap();
        assert_eq!(c.email.provider, EmailProviderKind::Resend);
        assert_eq!(
            c.email.from_for("organization-invitation"),
            "CF Meet Teams <no-reply@cfmeet.com>"
        );
        assert_eq!(c.email.from_for("default"), DEFAULT_FROM);
    }

    #[test]
    fn local_test_push_is_refused_in_production() {
        let d = tmp();
        let mut m = base(&d);
        m.insert("NOTIFICATIONS_PUSH_PROVIDER".into(), "local-test".into());
        assert!(parse(m).is_err());
    }

    #[test]
    fn partner_push_is_off_by_default_and_its_test_transport_is_refused_in_production() {
        let d = tmp();
        let c = parse(base(&d)).unwrap();
        assert!(!c.partner_push_enabled);
        assert_eq!(c.partner_push_transport, PartnerTransportKind::Live);
        let mut m = base(&d);
        m.insert("NOTIFICATIONS_PARTNER_PUSH_ENABLED".into(), "true".into());
        assert!(parse(m.clone()).unwrap().partner_push_enabled);
        m.insert("NOTIFICATIONS_PARTNER_PUSH_ENABLED".into(), "yes".into());
        assert!(parse(m.clone()).is_err());
        m.insert("NOTIFICATIONS_PARTNER_PUSH_ENABLED".into(), "true".into());
        m.insert(
            "NOTIFICATIONS_PARTNER_PUSH_TRANSPORT".into(),
            "local-test".into(),
        );
        assert!(parse(m).is_err());
    }

    #[test]
    fn mailbox_validation_refuses_header_injection() {
        assert!(valid_mailbox("CF Meet <no-reply@cfmeet.com>"));
        assert!(valid_mailbox("no-reply@cfmeet.com"));
        assert!(!valid_mailbox("x@y.com\r\nBcc: z@w.com"));
        assert!(!valid_mailbox("Name <a@b.com"));
        assert!(!valid_mailbox("a@b"));
    }
}
