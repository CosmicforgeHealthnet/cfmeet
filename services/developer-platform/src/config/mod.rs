//! Developer Platform configuration, read from the environment.
//!
//! | Variable | Required | Meaning |
//! |---|---|---|
//! | `DATABASE_URL` | yes | the Developer Platform's OWN PostgreSQL database |
//! | `DEVELOPER_PLATFORM_DATABASE_MAX_CONNECTIONS` | no | pool size (1–200, default 10) |
//! | `DEVELOPER_PLATFORM_BIND_ADDRESS` | no | default `127.0.0.1:8098` |
//! | `DEVELOPER_PLATFORM_GATEWAY_API_ENABLED` | no | feature flag (default `false`): serve requests the API Gateway forwards; off, they are 404 |
//! | `DEVELOPER_PLATFORM_PRODUCTION_ACCESS_WORKFLOW_ENABLED` | no | feature flag (default `false`): customer production-access submit / cancel / activate; off, they are 503 `CAPABILITY_UNAVAILABLE` (reads and operator decisions stay) |
//! | `DEVELOPER_PLATFORM_WORKER_INTERVAL_MS` | no | worker loop interval (100–600000, default 5000) |
//! | `DEVELOPER_PLATFORM_WORKER_INSTANCE_ID` | no | worker instance id (default `worker-<pid>`) |
//! | `LOG_FORMAT` | no | `pretty` / `json` |
//! | `DEVELOPER_PLATFORM_WEBHOOKS_ENABLED` | no | feature flag (default `false`): central customer webhooks (Section 53); off, every webhook route is 404 |
//! | `DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE` | no | `shadow` (default: ingest and record, never send) / `active` |
//! | `DEVELOPER_PLATFORM_WEBHOOK_SECRET_KEY` (`_ID`, `_PREVIOUS`, `_PREVIOUS_ID`, `_FILE` variants) | no | signing-secret sealing key ring; unset = webhook subscription routes 503 (fail closed) |
//! | `DEVELOPER_PLATFORM_WEBHOOK_ALLOW_PRIVATE_ENDPOINTS` | no | http / private endpoints, `local` runtime only |
//! | `DEVELOPER_PLATFORM_WEBHOOK_REQUEST_TIMEOUT_MS`, `_MAX_ATTEMPTS`, `_AUTO_DISABLE_AFTER` | no | delivery timeout, attempts before dead letter, consecutive failures before automatic disablement |
//! | `DEVELOPER_PLATFORM_WEBHOOK_MUTATIONS_FROZEN` | no | Section 53 migration freeze of customer / console webhook mutations (default `false`) |
//! | `DEVELOPER_PLATFORM_WEBHOOK_MIGRATION_TRANSFER_KEY` (`_FILE`, `_ID`) | no | the webhook migration transfer key (Section 53 §9.1); unset = secret import / rollback export refused |
//! | `DEVELOPER_PLATFORM_CREDENTIAL_LIFECYCLE_ENABLED` | no | PLT-008 credential lifecycle webhooks (`credential.expiring_soon` / `credential.expired`; default `true`; acts only with webhooks enabled and Platform Access configured) |
//! | `DEVELOPER_PLATFORM_CREDENTIAL_EXPIRY_WARNING_DAYS` | no | lead times in days, comma-separated, 1–8 distinct values of 1–365 (default `14,7,1`) |
//! | `DEVELOPER_PLATFORM_CREDENTIAL_EXPIRED_LOOKBACK_DAYS` | no | how long after expiry `credential.expired` is still sent (0–90, default 7) |
//! | `DEVELOPER_PLATFORM_CREDENTIAL_LIFECYCLE_INTERVAL_SECS` | no | minimum time between two sweeps across all worker instances (60–86400, default 3600) |
//! | `DEVELOPER_PLATFORM_CREDENTIAL_LIFECYCLE_MAX_ENVIRONMENTS` | no | subscribed environments examined per sweep (1–10000, default 500) |
//! | `DEVELOPER_PLATFORM_REFERENCE_RECONCILIATION_INTERVAL_SECS` | no | DATA-017 report-only reference reconciliation logged by the worker (0 = off, else 300–604800; default 86400) |
//! | `DEVELOPER_PLATFORM_REFERENCE_RECONCILIATION_REMOTE_LIMIT` | no | ids of each kind verified at Platform Access per report (0 = local checks only, max 100000; default 1000) |
//! | `DEVELOPER_PLATFORM_NOTIFICATIONS_PRODUCTION_ACCESS_ROUTE` | no | who notifies the applicant of a production-access decision: `legacy` (default: nothing is published; the operator-console email through the Meeting Service stays the only notice) or `notification-service` (`developer-platform.production-access.decided` is published to the Notification Service — switch the operator-console email off at the same time, never both). Effective only with `CF_NOTIFICATIONS_BASE_URL` |
//! | `DEVELOPER_PLATFORM_NOTIFICATIONS_CREDENTIAL_ROUTE` | no | `notification-service` (default) or `off`: the worker publishes `developer-platform.credential.expiring-soon` / `.expired` to the Notification Service (no other user notice exists for them; customer webhooks are unaffected). Effective only with `CF_NOTIFICATIONS_BASE_URL`, Platform Access and `DEVELOPER_PLATFORM_CREDENTIAL_LIFECYCLE_ENABLED` (same lead times, lookback, interval and environment bound) |
//! | `DEVELOPER_PLATFORM_USERS_API_ENABLED` | no | feature flag (default `false`, ADR 0062): the partner Users API (`/v1/users*`, machine credentials, delegated to Platform Access) and the console read `GET /v1/environments/{id}/users`; off, they are 404 |
//! | `DEVELOPER_PLATFORM_ALLOWED_ORIGINS_ENABLED` | no | feature flag (default `false`, ADR 0062): `GET` / `PUT /v1/environments/{id}/allowed-origins` (delegated to Platform Access); off, 404 |
//! | `DEVELOPER_PLATFORM_TEST_TOKENS_ENABLED` | no | feature flag (default `false`, ADR 0062): the console development token generator `POST /v1/environments/{id}/test-user-tokens` (SANDBOX only); off, 404 |
//! | `DEVELOPER_PLATFORM_PUSH_CREDENTIALS_ENABLED` | no | feature flag (default `false`, ADR 0063): partner push credentials `/v1/environments/{id}/push-credentials*` (sealed to the Notification Service, metadata only here); off, 404. Needs `CF_NOTIFICATIONS_BASE_URL` and `CF_NOTIFICATIONS_SEALING_KEY`, else 503 `CAPABILITY_UNAVAILABLE` |
//! | `DEVELOPER_PLATFORM_APPS_ENABLED` | no | feature flag (default `false`, ADR 0064 / 0065): app registration on projects (`/v1/projects/{id}/app*`) and the operator app review routes; off, 404 |
//! | `DEVELOPER_PLATFORM_OAUTH_ENABLED` | no | feature flag (default `false`): the anonymous OAuth endpoints `/v1/oauth/token`, `/revoke`, `/userinfo`, `/metadata`; off, 404 |
//! | `DEVELOPER_PLATFORM_APP_DIRECTORY_ENABLED` | no | feature flag (default `false`): the anonymous app directory `/v1/app-directory*`; off, 404 |
//! | `DEVELOPER_PLATFORM_INSTALL_EVENTS_ENABLED` | no | feature flag (default `false`): the worker projects Platform Access `APP_GRANT` changes and event ingestion routes install-target events to installed apps; off, nothing changes |
//! | `DEVELOPER_PLATFORM_APP_REVIEWERS` | no | comma-separated operator names allowed to review / approve / reject / publish apps (empty: no decision possible) |
//! | `DEVELOPER_PLATFORM_OAUTH_ISSUER`, `DEVELOPER_PLATFORM_OAUTH_AUTHORIZATION_ENDPOINT` | no | https URLs advertised by `/v1/oauth/metadata` (defaults `https://api.cfmeet.com`, `https://cfmeet.com/oauth/authorize`) |
//! | `CF_NOTIFICATIONS_SEALING_KEY` | no | the Notification Service's public HPKE key `<kid>:<base64url>` that push secrets are sealed to on receipt |
//! | `DEVELOPER_PLATFORM_CREATION_CAPS_ENABLED` | no | feature flag (default `false`): enforce the workspace / project creation caps below (409 `LIMIT_EXCEEDED`) |
//! | `DEVELOPER_PLATFORM_MAX_WORKSPACES_PER_OWNER` | no | workspaces (not CLOSED) a CF Meet account may OWN (0 = unlimited, max 1000, default 3) |
//! | `DEVELOPER_PLATFORM_MAX_PROJECTS_PER_WORKSPACE` | no | projects (not ARCHIVED) per workspace (0 = unlimited, max 10000, default 10) |
//! | `DEVELOPER_PLATFORM_RETENTION_ENABLED` | no | worker: bounded retention purge of webhook, API usage, rejection and activity history (default `true`; per-class bounds `DEVELOPER_PLATFORM_RETENTION_*_DAYS`, see [`crate::retention`]) |
//!
//! Without `CF_NOTIFICATIONS_BASE_URL` nothing is published to the Notification Service,
//! whatever the two routes say (today's behaviour). The API process enqueues production-
//! access decisions; the worker enqueues credential notices and relays every publication,
//! so both processes need the variable.
//!
//! The webhook variables are detailed in [`webhooks`]. Service authentication (`CF_*`) is
//! read by [`service_auth`]. Error messages never echo a value.

pub mod service_auth;
pub mod webhooks;

use std::{fmt, net::SocketAddr, time::Duration};

use secrecy::{ExposeSecret, SecretString};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{var} is not set")]
    Missing { var: &'static str },
    /// The message never echoes the offending value (it may be a secret).
    #[error("{var} is invalid: {reason}")]
    Invalid { var: &'static str, reason: String },
}

impl ConfigError {
    pub fn invalid(var: &'static str, reason: impl Into<String>) -> Self {
        ConfigError::Invalid {
            var,
            reason: reason.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Pretty,
    Json,
}

/// Disabled-by-default feature flags (Part VI rollout).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FeatureFlags {
    /// Serve customer requests forwarded by the API Gateway (`/internal/v1/gateway/**`).
    pub gateway_api: bool,
    /// Section 55: software-only production-access requests may be approved by the
    /// AUTOMATIC policy (`DEVELOPER_PLATFORM_PRODUCTION_AUTO_APPROVAL`, default off).
    /// Regulated telecom / numbering / SMS capabilities always need an operator.
    pub production_auto_approval: bool,
    /// Part VI §21.2 `PRODUCTION_ACCESS_WORKFLOW_ENABLED`: the customer self-service
    /// production-access flow (submit / cancel / activate). Off: those answer 503
    /// `CAPABILITY_UNAVAILABLE`; reads and operator decisions (review, suspension) stay
    /// available so an operator can always act on existing requests.
    pub production_access_workflow: bool,
}

/// ADR 0062 / 0063 "Build" features of the Developer Console, each default-off (kept out
/// of [`FeatureFlags`] so its existing shape is unchanged).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildFeatures {
    /// `DEVELOPER_PLATFORM_USERS_API_ENABLED`: `/v1/users*` and the console user list.
    pub users_api: bool,
    /// `DEVELOPER_PLATFORM_ALLOWED_ORIGINS_ENABLED`.
    pub allowed_origins: bool,
    /// `DEVELOPER_PLATFORM_TEST_TOKENS_ENABLED`: the SANDBOX token generator.
    pub test_tokens: bool,
    /// `DEVELOPER_PLATFORM_PUSH_CREDENTIALS_ENABLED`.
    pub push_credentials: bool,
    /// `DEVELOPER_PLATFORM_CREATION_CAPS_ENABLED`: enforce the two caps below.
    pub creation_caps: bool,
    /// Workspaces one CF Meet account may own (0 = unlimited).
    pub max_workspaces_per_owner: u32,
    /// Projects per workspace (0 = unlimited).
    pub max_projects_per_workspace: u32,
    /// `DEVELOPER_PLATFORM_APPS_ENABLED` (ADR 0064 / 0065): app registration, versions,
    /// client secrets, review submission, grants and test accounts on projects, and the
    /// operator review routes.
    pub apps: bool,
    /// `DEVELOPER_PLATFORM_OAUTH_ENABLED`: the anonymous OAuth endpoints `/v1/oauth/*`.
    pub oauth: bool,
    /// `DEVELOPER_PLATFORM_APP_DIRECTORY_ENABLED`: the anonymous `/v1/app-directory*`.
    pub app_directory: bool,
    /// `DEVELOPER_PLATFORM_INSTALL_EVENTS_ENABLED`: the Platform Access `APP_GRANT`
    /// projection (worker) and the install routing pass of event ingestion.
    pub install_events: bool,
}

impl Default for BuildFeatures {
    fn default() -> Self {
        Self {
            users_api: false,
            allowed_origins: false,
            test_tokens: false,
            push_credentials: false,
            creation_caps: false,
            max_workspaces_per_owner: 3,
            max_projects_per_workspace: 10,
            apps: false,
            oauth: false,
            app_directory: false,
            install_events: false,
        }
    }
}

fn build_features(get: &impl Fn(&str) -> Option<String>) -> Result<BuildFeatures, ConfigError> {
    Ok(BuildFeatures {
        users_api: flag(get, "DEVELOPER_PLATFORM_USERS_API_ENABLED")?,
        allowed_origins: flag(get, "DEVELOPER_PLATFORM_ALLOWED_ORIGINS_ENABLED")?,
        test_tokens: flag(get, "DEVELOPER_PLATFORM_TEST_TOKENS_ENABLED")?,
        push_credentials: flag(get, "DEVELOPER_PLATFORM_PUSH_CREDENTIALS_ENABLED")?,
        creation_caps: flag(get, "DEVELOPER_PLATFORM_CREATION_CAPS_ENABLED")?,
        max_workspaces_per_owner: bounded(
            get,
            "DEVELOPER_PLATFORM_MAX_WORKSPACES_PER_OWNER",
            3,
            0,
            1_000,
        )? as u32,
        max_projects_per_workspace: bounded(
            get,
            "DEVELOPER_PLATFORM_MAX_PROJECTS_PER_WORKSPACE",
            10,
            0,
            10_000,
        )? as u32,
        apps: flag(get, "DEVELOPER_PLATFORM_APPS_ENABLED")?,
        oauth: flag(get, "DEVELOPER_PLATFORM_OAUTH_ENABLED")?,
        app_directory: flag(get, "DEVELOPER_PLATFORM_APP_DIRECTORY_ENABLED")?,
        install_events: flag(get, "DEVELOPER_PLATFORM_INSTALL_EVENTS_ENABLED")?,
    })
}

/// The Notification Service's public sealing key (`CF_NOTIFICATIONS_SEALING_KEY`,
/// `<kid>:<base64url public key>`); push secrets are sealed to it on receipt.
fn notifications_sealing_key(
    get: &impl Fn(&str) -> Option<String>,
) -> Result<Option<cf_notification_client::SealingKey>, ConfigError> {
    match get("CF_NOTIFICATIONS_SEALING_KEY") {
        None => Ok(None),
        Some(v) => cf_notification_client::SealingKey::parse(&v)
            .map(Some)
            .map_err(|_| {
                ConfigError::invalid(
                    "CF_NOTIFICATIONS_SEALING_KEY",
                    "must be <kid>:<base64url public key>",
                )
            }),
    }
}

/// ADR 0064: the public URLs `/v1/oauth/metadata` advertises.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthEndpoints {
    /// `DEVELOPER_PLATFORM_OAUTH_ISSUER` (default `https://api.cfmeet.com`): the issuer
    /// and the base of the token, revocation and userinfo endpoints (`/v1/oauth/*`).
    pub issuer: String,
    /// `DEVELOPER_PLATFORM_OAUTH_AUTHORIZATION_ENDPOINT` (default
    /// `https://cfmeet.com/oauth/authorize`, the frontend consent page).
    pub authorization_endpoint: String,
}

impl Default for OAuthEndpoints {
    fn default() -> Self {
        Self {
            issuer: "https://api.cfmeet.com".into(),
            authorization_endpoint: "https://cfmeet.com/oauth/authorize".into(),
        }
    }
}

impl OAuthEndpoints {
    fn from_lookup(get: &impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let defaults = Self::default();
        let https = |var: &'static str, default: String| -> Result<String, ConfigError> {
            match get(var) {
                None => Ok(default),
                Some(v)
                    if v.starts_with("https://")
                        && v.len() <= 200
                        && !v.contains(['?', '#', ' ', '@']) =>
                {
                    Ok(v.trim_end_matches('/').to_string())
                }
                Some(_) => Err(ConfigError::invalid(var, "must be an https URL")),
            }
        };
        Ok(Self {
            issuer: https("DEVELOPER_PLATFORM_OAUTH_ISSUER", defaults.issuer)?,
            authorization_endpoint: https(
                "DEVELOPER_PLATFORM_OAUTH_AUTHORIZATION_ENDPOINT",
                defaults.authorization_endpoint,
            )?,
        })
    }
}

pub struct DeveloperPlatformConfig {
    /// Final completion (production-access workflow): the named operators allowed to
    /// review, approve or reject production-access requests
    /// (`DEVELOPER_PLATFORM_PRODUCTION_REVIEWERS`, comma separated operator names). Empty:
    /// any operator holding `developer-platform.administer` (previous behaviour). Customer
    /// submissions (`…_PRODUCTION_ACCESS_WORKFLOW_ENABLED`) require at least one reviewer.
    pub production_reviewers: Vec<String>,
    /// ADR 0065: the named operators allowed to review, approve, reject and publish apps
    /// (`DEVELOPER_PLATFORM_APP_REVIEWERS`, comma separated operator names). Empty: no app
    /// review decision is possible (fail closed).
    pub app_reviewers: Vec<String>,
    /// ADR 0064: the public OAuth endpoints advertised by `/v1/oauth/metadata`.
    pub oauth_endpoints: OAuthEndpoints,
    /// `…_PRODUCTION_ACCESS_WORKFLOW_ENABLED=true` was requested but no reviewer is named:
    /// the workflow was kept off (reported at startup).
    pub production_access_withheld: bool,
    /// `…_PRODUCTION_AUTO_APPROVAL=true` was requested and ignored (reported at startup).
    pub auto_approval_refused: bool,
    pub database_url: SecretString,
    pub database_max_connections: u32,
    pub bind_address: SocketAddr,
    pub flags: FeatureFlags,
    pub worker_interval: Duration,
    pub worker_instance_id: String,
    pub log_format: LogFormat,
    /// Central customer webhooks (Section 53).
    pub webhooks: crate::webhooks::WebhookSettings,
    /// Section 15 (PLT-008): credential lifecycle webhooks.
    pub credential_lifecycle: crate::control::credential_lifecycle::CredentialLifecycleSettings,
    /// DATA-017: period of the worker's report-only reference reconciliation (None: off).
    pub reference_reconciliation_interval: Option<Duration>,
    /// DATA-017: ids of each kind verified at Platform Access per report.
    pub reference_reconciliation_remote_limit: u64,
    /// Notification Service N7 / N11: which events are routed to the Notification Service
    /// (requested; effective only with `CF_NOTIFICATIONS_BASE_URL`).
    pub notification_routes: NotificationRoutes,
    /// ADR 0062 / 0063 Developer Console features (all default off).
    pub build: BuildFeatures,
    /// `CF_NOTIFICATIONS_SEALING_KEY` (push credentials are sealed to it).
    pub notifications_sealing_key: Option<cf_notification_client::SealingKey>,
}

/// The per-event routes to the Notification Service (requested in configuration).
///
/// An event with an existing user notice outside the Notification Service (the
/// production-access decision email the operator console sends through the Meeting
/// Service) defaults to `legacy`: publishing it is an explicit, mutually exclusive switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotificationRoutes {
    /// `DEVELOPER_PLATFORM_NOTIFICATIONS_PRODUCTION_ACCESS_ROUTE=notification-service`.
    pub production_access: bool,
    /// `DEVELOPER_PLATFORM_NOTIFICATIONS_CREDENTIAL_ROUTE` (default `notification-service`).
    pub credential_lifecycle: bool,
}

impl Default for NotificationRoutes {
    fn default() -> Self {
        Self {
            production_access: false,
            credential_lifecycle: true,
        }
    }
}

fn notification_routes(
    get: &impl Fn(&str) -> Option<String>,
) -> Result<NotificationRoutes, ConfigError> {
    const PRODUCTION: &str = "DEVELOPER_PLATFORM_NOTIFICATIONS_PRODUCTION_ACCESS_ROUTE";
    const CREDENTIAL: &str = "DEVELOPER_PLATFORM_NOTIFICATIONS_CREDENTIAL_ROUTE";
    let production_access = match get(PRODUCTION).as_deref() {
        None | Some("legacy") => false,
        Some("notification-service") => true,
        Some(_) => {
            return Err(ConfigError::invalid(
                PRODUCTION,
                "must be `legacy` or `notification-service`",
            ))
        }
    };
    let credential_lifecycle = match get(CREDENTIAL).as_deref() {
        None | Some("notification-service") => true,
        Some("off") => false,
        Some(_) => {
            return Err(ConfigError::invalid(
                CREDENTIAL,
                "must be `notification-service` or `off`",
            ))
        }
    };
    Ok(NotificationRoutes {
        production_access,
        credential_lifecycle,
    })
}

impl fmt::Debug for DeveloperPlatformConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeveloperPlatformConfig")
            .field("database_url", &"[REDACTED]")
            .field("database_max_connections", &self.database_max_connections)
            .field("bind_address", &self.bind_address)
            .field("flags", &self.flags)
            .field("worker_interval", &self.worker_interval)
            .field("worker_instance_id", &self.worker_instance_id)
            .field("webhooks", &self.webhooks)
            .field("credential_lifecycle", &self.credential_lifecycle)
            .field("notification_routes", &self.notification_routes)
            .field("build", &self.build)
            .field(
                "notifications_sealing_key",
                &self.notifications_sealing_key.as_ref().map(|k| k.kid()),
            )
            .field(
                "reference_reconciliation_interval",
                &self.reference_reconciliation_interval,
            )
            .finish_non_exhaustive()
    }
}

pub const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:8098";

pub(crate) fn bounded(
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

/// A boolean flag: `true` / `false` (unset: `false`, fail closed).
pub(crate) fn flag(
    get: &impl Fn(&str) -> Option<String>,
    var: &'static str,
) -> Result<bool, ConfigError> {
    match get(var).as_deref() {
        None | Some("false") | Some("0") => Ok(false),
        Some("true") | Some("1") => Ok(true),
        Some(_) => Err(ConfigError::invalid(var, "must be `true` or `false`")),
    }
}

/// A worker instance id: `[a-z0-9][a-z0-9._-]{0,63}` (the persisted format).
pub fn valid_instance_id(value: &str) -> bool {
    let b = value.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'_' | b'-')
        })
}

/// The PLT-008 credential lifecycle settings (defaults: on, 14/7/1 days, 7-day lookback,
/// hourly, 500 environments per sweep).
fn credential_lifecycle(
    get: &impl Fn(&str) -> Option<String>,
) -> Result<crate::control::credential_lifecycle::CredentialLifecycleSettings, ConfigError> {
    use crate::control::credential_lifecycle::{parse_warning_days, CredentialLifecycleSettings};
    let defaults = CredentialLifecycleSettings::default();
    let enabled = match get("DEVELOPER_PLATFORM_CREDENTIAL_LIFECYCLE_ENABLED").as_deref() {
        None | Some("true") | Some("1") => true,
        Some("false") | Some("0") => false,
        Some(_) => {
            return Err(ConfigError::invalid(
                "DEVELOPER_PLATFORM_CREDENTIAL_LIFECYCLE_ENABLED",
                "must be `true` or `false`",
            ))
        }
    };
    let warning_days = match get("DEVELOPER_PLATFORM_CREDENTIAL_EXPIRY_WARNING_DAYS") {
        None => defaults.warning_days,
        Some(v) => parse_warning_days(&v).ok_or_else(|| {
            ConfigError::invalid(
                "DEVELOPER_PLATFORM_CREDENTIAL_EXPIRY_WARNING_DAYS",
                "must be 1-8 distinct comma-separated days between 1 and 365",
            )
        })?,
    };
    Ok(CredentialLifecycleSettings {
        enabled,
        warning_days,
        expired_lookback: chrono::Duration::days(bounded(
            get,
            "DEVELOPER_PLATFORM_CREDENTIAL_EXPIRED_LOOKBACK_DAYS",
            7,
            0,
            90,
        )?),
        interval: Duration::from_secs(bounded(
            get,
            "DEVELOPER_PLATFORM_CREDENTIAL_LIFECYCLE_INTERVAL_SECS",
            3_600,
            60,
            86_400,
        )? as u64),
        max_environments: bounded(
            get,
            "DEVELOPER_PLATFORM_CREDENTIAL_LIFECYCLE_MAX_ENVIRONMENTS",
            500,
            1,
            10_000,
        )? as u64,
    })
}

impl DeveloperPlatformConfig {
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
        let database_url =
            get("DATABASE_URL")
                .map(SecretString::from)
                .ok_or(ConfigError::Missing {
                    var: "DATABASE_URL",
                })?;
        let bind_address = match get("DEVELOPER_PLATFORM_BIND_ADDRESS") {
            None => DEFAULT_BIND_ADDRESS.parse().expect("valid default"),
            Some(v) => v.parse::<SocketAddr>().map_err(|_| {
                ConfigError::invalid(
                    "DEVELOPER_PLATFORM_BIND_ADDRESS",
                    "must be a socket address such as 127.0.0.1:8098",
                )
            })?,
        };
        let database_max_connections = bounded(
            &get,
            "DEVELOPER_PLATFORM_DATABASE_MAX_CONNECTIONS",
            10,
            1,
            200,
        )? as u32;
        let mut flags = FeatureFlags {
            gateway_api: flag(&get, "DEVELOPER_PLATFORM_GATEWAY_API_ENABLED")?,
            production_auto_approval: flag(&get, "DEVELOPER_PLATFORM_PRODUCTION_AUTO_APPROVAL")?,
            production_access_workflow: flag(
                &get,
                "DEVELOPER_PLATFORM_PRODUCTION_ACCESS_WORKFLOW_ENABLED",
            )?,
        };
        let production_reviewers: Vec<String> = get("DEVELOPER_PLATFORM_PRODUCTION_REVIEWERS")
            .unwrap_or_default()
            .split(',')
            .map(|r| r.trim().to_string())
            .filter(|r| !r.is_empty())
            .collect();
        if production_reviewers
            .iter()
            .any(|r| !crate::control::operator::valid_operator(r))
        {
            return Err(ConfigError::invalid(
                "DEVELOPER_PLATFORM_PRODUCTION_REVIEWERS",
                "must be comma-separated operator names",
            ));
        }
        let app_reviewers: Vec<String> = get("DEVELOPER_PLATFORM_APP_REVIEWERS")
            .unwrap_or_default()
            .split(',')
            .map(|r| r.trim().to_string())
            .filter(|r| !r.is_empty())
            .collect();
        if app_reviewers
            .iter()
            .any(|r| !crate::control::operator::valid_operator(r))
        {
            return Err(ConfigError::invalid(
                "DEVELOPER_PLATFORM_APP_REVIEWERS",
                "must be comma-separated operator names",
            ));
        }
        let oauth_endpoints = OAuthEndpoints::from_lookup(&get)?;
        // Customer submissions are only accepted while someone can decide them: without a
        // named reviewer the workflow stays off (submit / cancel / activate answer 503
        // `CAPABILITY_UNAVAILABLE`) and everything else starts normally.
        let production_access_withheld =
            flags.production_access_workflow && production_reviewers.is_empty();
        if production_access_withheld {
            flags.production_access_workflow = false;
        }
        // Owner decision (final completion): manual approval only. A request for automatic
        // approval is ignored (never a startup failure of the whole service).
        let auto_approval_refused = flags.production_auto_approval;
        flags.production_auto_approval = false;
        let worker_interval = Duration::from_millis(bounded(
            &get,
            "DEVELOPER_PLATFORM_WORKER_INTERVAL_MS",
            5_000,
            100,
            600_000,
        )? as u64);
        let worker_instance_id = match get("DEVELOPER_PLATFORM_WORKER_INSTANCE_ID") {
            None => format!("worker-{}", std::process::id()),
            Some(v) if valid_instance_id(&v) => v,
            Some(_) => {
                return Err(ConfigError::invalid(
                    "DEVELOPER_PLATFORM_WORKER_INSTANCE_ID",
                    "must be 1-64 characters of [a-z0-9._-], starting alphanumeric",
                ))
            }
        };
        let log_format = match get("LOG_FORMAT").as_deref() {
            None | Some("pretty") => LogFormat::Pretty,
            Some("json") => LogFormat::Json,
            Some(_) => {
                return Err(ConfigError::invalid(
                    "LOG_FORMAT",
                    "must be `pretty` or `json`",
                ))
            }
        };
        let webhooks = webhooks::from_lookup(&get)?;
        let credential_lifecycle = credential_lifecycle(&get)?;
        let notification_routes = notification_routes(&get)?;
        let build = build_features(&get)?;
        let notifications_sealing_key = notifications_sealing_key(&get)?;
        let reference_reconciliation_interval = match bounded(
            &get,
            "DEVELOPER_PLATFORM_REFERENCE_RECONCILIATION_INTERVAL_SECS",
            86_400,
            0,
            604_800,
        )? {
            0 => None,
            n if n < 300 => {
                return Err(ConfigError::invalid(
                    "DEVELOPER_PLATFORM_REFERENCE_RECONCILIATION_INTERVAL_SECS",
                    "must be 0 (off) or between 300 and 604800",
                ))
            }
            n => Some(Duration::from_secs(n as u64)),
        };
        let reference_reconciliation_remote_limit = bounded(
            &get,
            "DEVELOPER_PLATFORM_REFERENCE_RECONCILIATION_REMOTE_LIMIT",
            1_000,
            0,
            100_000,
        )? as u64;
        Ok(Self {
            production_reviewers,
            app_reviewers,
            oauth_endpoints,
            production_access_withheld,
            auto_approval_refused,
            database_url,
            database_max_connections,
            bind_address,
            flags,
            worker_interval,
            worker_instance_id,
            log_format,
            webhooks,
            credential_lifecycle,
            reference_reconciliation_interval,
            reference_reconciliation_remote_limit,
            notification_routes,
            build,
            notifications_sealing_key,
        })
    }

    pub fn database_url(&self) -> &str {
        self.database_url.expose_secret()
    }
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

    const BASE: &[(&str, &str)] = &[("DATABASE_URL", "postgres://devplat:pw-secret-9@db/dp")];

    #[test]
    fn requires_its_own_database_and_flags_default_off() {
        let cfg = DeveloperPlatformConfig::from_lookup(lookup(BASE)).unwrap();
        assert_eq!(cfg.bind_address.to_string(), DEFAULT_BIND_ADDRESS);
        assert_eq!(cfg.flags, FeatureFlags::default());
        assert!(!cfg.flags.gateway_api, "disabled by default");
        assert!(
            !cfg.flags.production_access_workflow,
            "the production-access workflow is disabled by default"
        );
        assert!(!format!("{cfg:?}").contains("pw-secret-9"));
        assert!(DeveloperPlatformConfig::from_lookup(lookup(&[])).is_err());
    }

    #[test]
    fn production_access_needs_named_reviewers_and_never_auto_approves() {
        let with = |extra: &[(&'static str, &'static str)]| {
            let mut v: Vec<(&str, &str)> = BASE.to_vec();
            v.extend_from_slice(extra);
            DeveloperPlatformConfig::from_lookup(lookup(&v))
        };
        // Without a named reviewer the service starts and submissions stay off.
        let withheld = with(&[(
            "DEVELOPER_PLATFORM_PRODUCTION_ACCESS_WORKFLOW_ENABLED",
            "true",
        )])
        .expect("starts without reviewers");
        assert!(!withheld.flags.production_access_workflow);
        assert!(withheld.production_access_withheld);
        let cfg = with(&[
            (
                "DEVELOPER_PLATFORM_PRODUCTION_ACCESS_WORKFLOW_ENABLED",
                "true",
            ),
            (
                "DEVELOPER_PLATFORM_PRODUCTION_REVIEWERS",
                "github-alice, github-bob",
            ),
        ])
        .unwrap();
        assert_eq!(cfg.production_reviewers, ["github-alice", "github-bob"]);
        assert!(cfg.flags.production_access_workflow);
        assert!(!cfg.production_access_withheld);
        assert!(with(&[("DEVELOPER_PLATFORM_PRODUCTION_REVIEWERS", "bad name")]).is_err());
        // Automatic approval is never effective (manual review only), and never a crash.
        let auto = with(&[("DEVELOPER_PLATFORM_PRODUCTION_AUTO_APPROVAL", "true")]).unwrap();
        assert!(!auto.flags.production_auto_approval);
        assert!(auto.auto_approval_refused);
        assert!(with(&[]).unwrap().production_reviewers.is_empty());
    }

    #[test]
    fn credential_lifecycle_and_reconciliation_have_sane_defaults() {
        let cfg = DeveloperPlatformConfig::from_lookup(lookup(BASE)).unwrap();
        let c = &cfg.credential_lifecycle;
        assert!(c.enabled);
        assert_eq!(c.warning_days, vec![14, 7, 1]);
        assert_eq!(c.expired_lookback, chrono::Duration::days(7));
        assert_eq!(c.interval, Duration::from_secs(3600));
        assert_eq!(c.max_environments, 500);
        assert_eq!(
            cfg.reference_reconciliation_interval,
            Some(Duration::from_secs(86_400))
        );
        assert_eq!(cfg.reference_reconciliation_remote_limit, 1_000);
        let mut pairs = BASE.to_vec();
        pairs.extend([
            ("DEVELOPER_PLATFORM_CREDENTIAL_LIFECYCLE_ENABLED", "false"),
            ("DEVELOPER_PLATFORM_CREDENTIAL_EXPIRY_WARNING_DAYS", "3, 30"),
            ("DEVELOPER_PLATFORM_CREDENTIAL_EXPIRED_LOOKBACK_DAYS", "0"),
            (
                "DEVELOPER_PLATFORM_REFERENCE_RECONCILIATION_INTERVAL_SECS",
                "0",
            ),
        ]);
        let cfg = DeveloperPlatformConfig::from_lookup(lookup(&pairs)).unwrap();
        assert!(!cfg.credential_lifecycle.enabled);
        assert_eq!(cfg.credential_lifecycle.warning_days, vec![30, 3]);
        assert_eq!(cfg.reference_reconciliation_interval, None);
    }

    #[test]
    fn the_production_access_notice_stays_legacy_unless_explicitly_routed() {
        let with = |extra: &[(&'static str, &'static str)]| {
            let mut v: Vec<(&str, &str)> = BASE.to_vec();
            v.extend_from_slice(extra);
            DeveloperPlatformConfig::from_lookup(lookup(&v)).map(|c| c.notification_routes)
        };
        assert_eq!(
            with(&[]).unwrap(),
            NotificationRoutes {
                production_access: false,
                credential_lifecycle: true,
            }
        );
        let routed = with(&[
            (
                "DEVELOPER_PLATFORM_NOTIFICATIONS_PRODUCTION_ACCESS_ROUTE",
                "notification-service",
            ),
            ("DEVELOPER_PLATFORM_NOTIFICATIONS_CREDENTIAL_ROUTE", "off"),
        ])
        .unwrap();
        assert!(routed.production_access && !routed.credential_lifecycle);
        assert!(
            !with(&[(
                "DEVELOPER_PLATFORM_NOTIFICATIONS_PRODUCTION_ACCESS_ROUTE",
                "legacy"
            )])
            .unwrap()
            .production_access
        );
        for (var, value) in [
            (
                "DEVELOPER_PLATFORM_NOTIFICATIONS_PRODUCTION_ACCESS_ROUTE",
                "both-please",
            ),
            (
                "DEVELOPER_PLATFORM_NOTIFICATIONS_CREDENTIAL_ROUTE",
                "legacy",
            ),
        ] {
            let err = with(&[(var, value)]).unwrap_err();
            assert!(!err.to_string().contains(value), "{var}");
        }
    }

    #[test]
    fn app_features_are_off_by_default_and_reviewers_are_named() {
        let cfg = DeveloperPlatformConfig::from_lookup(lookup(BASE)).unwrap();
        let b = cfg.build;
        assert!(!b.apps && !b.oauth && !b.app_directory && !b.install_events);
        assert!(cfg.app_reviewers.is_empty());
        assert_eq!(cfg.oauth_endpoints, OAuthEndpoints::default());
        let mut pairs = BASE.to_vec();
        pairs.extend([
            ("DEVELOPER_PLATFORM_APPS_ENABLED", "true"),
            ("DEVELOPER_PLATFORM_OAUTH_ENABLED", "true"),
            ("DEVELOPER_PLATFORM_APP_DIRECTORY_ENABLED", "true"),
            ("DEVELOPER_PLATFORM_INSTALL_EVENTS_ENABLED", "true"),
            ("DEVELOPER_PLATFORM_APP_REVIEWERS", "github-alice, github-bob"),
            ("DEVELOPER_PLATFORM_OAUTH_ISSUER", "https://api.example.test/"),
        ]);
        let cfg = DeveloperPlatformConfig::from_lookup(lookup(&pairs)).unwrap();
        let b = cfg.build;
        assert!(b.apps && b.oauth && b.app_directory && b.install_events);
        assert_eq!(cfg.app_reviewers, ["github-alice", "github-bob"]);
        assert_eq!(cfg.oauth_endpoints.issuer, "https://api.example.test");
        for (var, value) in [
            ("DEVELOPER_PLATFORM_APP_REVIEWERS", "not a name!"),
            ("DEVELOPER_PLATFORM_OAUTH_ISSUER", "http://api.example.test"),
            ("DEVELOPER_PLATFORM_APPS_ENABLED", "maybe"),
        ] {
            let mut pairs = BASE.to_vec();
            pairs.push((var, value));
            assert!(DeveloperPlatformConfig::from_lookup(lookup(&pairs)).is_err(), "{var}");
        }
    }

    #[test]
    fn build_features_are_off_by_default_and_caps_are_bounded() {
        let cfg = DeveloperPlatformConfig::from_lookup(lookup(BASE)).unwrap();
        assert_eq!(cfg.build, BuildFeatures::default());
        assert!(!cfg.build.users_api && !cfg.build.allowed_origins);
        assert!(!cfg.build.test_tokens && !cfg.build.push_credentials);
        assert!(!cfg.build.creation_caps);
        assert_eq!(cfg.build.max_workspaces_per_owner, 3);
        assert_eq!(cfg.build.max_projects_per_workspace, 10);
        assert!(cfg.notifications_sealing_key.is_none());
        let (public, _) =
            cf_notification_client::seal::generate_keypair("notif-hpke-2026-11").unwrap();
        let mut pairs = BASE.to_vec();
        pairs.extend([
            ("DEVELOPER_PLATFORM_USERS_API_ENABLED", "true"),
            ("DEVELOPER_PLATFORM_ALLOWED_ORIGINS_ENABLED", "true"),
            ("DEVELOPER_PLATFORM_TEST_TOKENS_ENABLED", "true"),
            ("DEVELOPER_PLATFORM_PUSH_CREDENTIALS_ENABLED", "true"),
            ("DEVELOPER_PLATFORM_CREATION_CAPS_ENABLED", "true"),
            ("DEVELOPER_PLATFORM_MAX_WORKSPACES_PER_OWNER", "0"),
            ("DEVELOPER_PLATFORM_MAX_PROJECTS_PER_WORKSPACE", "25"),
            ("CF_NOTIFICATIONS_SEALING_KEY", public.as_str()),
        ]);
        let cfg = DeveloperPlatformConfig::from_lookup(lookup(&pairs)).unwrap();
        let b = cfg.build;
        assert!(b.users_api && b.allowed_origins && b.test_tokens && b.push_credentials);
        assert!(b.creation_caps);
        assert_eq!(b.max_workspaces_per_owner, 0);
        assert_eq!(b.max_projects_per_workspace, 25);
        assert_eq!(
            cfg.notifications_sealing_key.as_ref().map(|k| k.kid()),
            Some("notif-hpke-2026-11")
        );
        for (var, value) in [
            ("DEVELOPER_PLATFORM_USERS_API_ENABLED", "kind-of"),
            ("DEVELOPER_PLATFORM_MAX_WORKSPACES_PER_OWNER", "-1"),
            ("DEVELOPER_PLATFORM_MAX_PROJECTS_PER_WORKSPACE", "99999999"),
            ("CF_NOTIFICATIONS_SEALING_KEY", "not-a-key-zz"),
        ] {
            let mut pairs = BASE.to_vec();
            pairs.push((var, value));
            let err = DeveloperPlatformConfig::from_lookup(lookup(&pairs)).unwrap_err();
            assert!(!err.to_string().contains(value), "{var}");
        }
    }

    #[test]
    fn invalid_values_are_refused_without_echo() {
        for (var, value) in [
            ("DEVELOPER_PLATFORM_BIND_ADDRESS", "not-an-address-xyz"),
            ("DEVELOPER_PLATFORM_DATABASE_MAX_CONNECTIONS", "999"),
            ("DEVELOPER_PLATFORM_GATEWAY_API_ENABLED", "maybe-later"),
            ("DEVELOPER_PLATFORM_WORKER_INTERVAL_MS", "5"),
            ("DEVELOPER_PLATFORM_WORKER_INSTANCE_ID", "Has Space"),
            ("LOG_FORMAT", "xml-format"),
            (
                "DEVELOPER_PLATFORM_CREDENTIAL_LIFECYCLE_ENABLED",
                "perhaps-yes",
            ),
            (
                "DEVELOPER_PLATFORM_CREDENTIAL_EXPIRY_WARNING_DAYS",
                "14,14,9999",
            ),
            (
                "DEVELOPER_PLATFORM_CREDENTIAL_EXPIRED_LOOKBACK_DAYS",
                "9191",
            ),
            ("DEVELOPER_PLATFORM_CREDENTIAL_LIFECYCLE_INTERVAL_SECS", "5"),
            (
                "DEVELOPER_PLATFORM_CREDENTIAL_LIFECYCLE_MAX_ENVIRONMENTS",
                "-3",
            ),
            (
                "DEVELOPER_PLATFORM_REFERENCE_RECONCILIATION_INTERVAL_SECS",
                "59",
            ),
            (
                "DEVELOPER_PLATFORM_REFERENCE_RECONCILIATION_REMOTE_LIMIT",
                "-5",
            ),
        ] {
            let mut pairs = BASE.to_vec();
            pairs.push((var, value));
            let err = DeveloperPlatformConfig::from_lookup(lookup(&pairs)).unwrap_err();
            assert!(!err.to_string().contains(value), "{var}");
        }
        let mut on = BASE.to_vec();
        on.push(("DEVELOPER_PLATFORM_GATEWAY_API_ENABLED", "true"));
        assert!(
            DeveloperPlatformConfig::from_lookup(lookup(&on))
                .unwrap()
                .flags
                .gateway_api
        );
    }
}
