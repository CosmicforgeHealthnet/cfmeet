//! Application Configuration
//!
//! Core application settings loaded from environment variables.

use crate::error::{ApiError, ApiResult};

/// Application configuration
#[derive(Clone)]
pub struct AppConfig {
    /// Database connection URL
    pub database_url: String,

    /// JWT signing secret
    pub jwt_secret: String,

    /// Server host address
    pub host: String,

    /// Server port
    pub port: u16,

    /// Frontend URLs for CORS (comma-separated in env)
    pub frontend_urls: Vec<String>,

    /// Frontend/Hosted UI URL (e.g., https://meet.cosmicforge.com)
    pub app_url: String,

    /// Google OAuth Client ID
    pub google_client_id: String,

    /// Google OAuth Client Secret
    pub google_client_secret: String,

    /// Google OAuth Redirect URL
    pub google_redirect_url: String,

    /// Require email verification before login (default: true)
    pub require_email_verification: bool,
}

impl AppConfig {
    /// Load configuration from environment variables
    pub fn from_env() -> ApiResult<Self> {
        Ok(Self {
            database_url: std::env::var("DATABASE_URL")
                .map_err(|_| ApiError::InternalError("DATABASE_URL must be set".to_string()))?,

            jwt_secret: validated_jwt_secret(
                std::env::var("JWT_SECRET").ok(),
                std::env::var("APP_ENV").ok(),
            )?,

            host: std::env::var("HOST").unwrap_or_else(|_| "127.0.0.1".to_string()),

            port: std::env::var("PORT")
                .unwrap_or_else(|_| "8080".to_string())
                .parse()
                .map_err(|_| ApiError::InternalError("Invalid PORT".to_string()))?,

            frontend_urls: browser_origins(
                &std::env::var("FRONTEND_URLS")
                    .unwrap_or_else(|_| "http://localhost:3000".to_string()),
                is_production_env(
                    std::env::var("APP_ENV").ok().as_deref(),
                    std::env::var("CF_RUNTIME_ENVIRONMENT").ok().as_deref(),
                ),
            )?,

            app_url: std::env::var("APP_URL")
                .unwrap_or_else(|_| "http://localhost:3000".to_string())
                .trim_end_matches('/')
                .to_string(),

            google_client_id: std::env::var("GOOGLE_CLIENT_ID")
                .map_err(|_| ApiError::InternalError("GOOGLE_CLIENT_ID must be set".to_string()))?,

            google_client_secret: std::env::var("GOOGLE_CLIENT_SECRET").map_err(|_| {
                ApiError::InternalError("GOOGLE_CLIENT_SECRET must be set".to_string())
            })?,

            google_redirect_url: std::env::var("GOOGLE_REDIRECT_URL").map_err(|_| {
                ApiError::InternalError("GOOGLE_REDIRECT_URL must be set".to_string())
            })?,

            require_email_verification: std::env::var("EMAIL_VERIFICATION_REQUIRED")
                .unwrap_or_else(|_| "true".to_string())
                .parse()
                .unwrap_or(true),
        })
    }

    /// Get the server address (host:port)
    pub fn server_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// Generate join URL for a meeting
    pub fn join_url(&self, meeting_identifier: &str) -> String {
        format!("{}/join/{}", self.app_url, meeting_identifier)
    }
}

/// The value shipped in `.env.example`; never acceptable outside development.
const INSECURE_EXAMPLE_JWT_SECRET: &str = "your-secret-key-change-this-in-production";
/// HS256 signing material shorter than this is refused outside development.
pub const MIN_JWT_SECRET_BYTES: usize = 32;

/// `APP_ENV` explicitly names a development environment (unset = production-like).
pub fn is_development_env(app_env: Option<&str>) -> bool {
    matches!(
        app_env
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "dev" | "development" | "local" | "test"
    )
}

/// The deployment explicitly names itself production: `CF_RUNTIME_ENVIRONMENT=production`
/// (set with the production signing identity) or `APP_ENV=prod|production`.
pub fn is_production_env(app_env: Option<&str>, runtime: Option<&str>) -> bool {
    let named = |v: Option<&str>, names: &[&str]| {
        let v = v.unwrap_or_default().trim().to_ascii_lowercase();
        names.contains(&v.as_str())
    };
    named(runtime, &["production"]) || named(app_env, &["prod", "production"])
}

/// A browser origin production may answer: `https://` with a public host name, no port, no
/// path, and never a loopback, private-network, tunnel or staging/dev host.
pub fn is_production_safe_origin(origin: &str) -> bool {
    let Some(host) = origin.strip_prefix("https://") else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    if host.is_empty() || host.contains(['/', ':', '@', '*', '?', '#']) || !host.contains('.') {
        return false;
    }
    if host.parse::<std::net::Ipv4Addr>().is_ok()
        || host == "localhost"
        || host.ends_with(".localhost")
    {
        return false;
    }
    const REFUSED_SUFFIXES: &[&str] = &[
        ".devtunnels.ms",
        ".ngrok.io",
        ".ngrok-free.app",
        ".ngrok.app",
        ".trycloudflare.com",
        ".loca.lt",
        ".local",
        ".internal",
        ".vercel.app",
    ];
    if REFUSED_SUFFIXES.iter().any(|s| host.ends_with(s)) {
        return false;
    }
    !host.split(['.', '-']).any(|label| {
        matches!(
            label,
            "staging" | "stage" | "dev" | "development" | "test" | "preview" | "local" | "sandbox"
        )
    })
}

/// M1: the CORS allow-list from `FRONTEND_URLS`. Outside production every listed origin is
/// kept (staging, local development). In production an origin that is not
/// [`is_production_safe_origin`] is dropped with a warning, so a copied staging or developer
/// list can never open the production API to those sites; a production list with no safe
/// origin left refuses to start.
pub fn browser_origins(raw: &str, production: bool) -> ApiResult<Vec<String>> {
    let listed: Vec<String> = raw
        .split(',')
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if !production {
        return Ok(listed);
    }
    let (safe, refused): (Vec<String>, Vec<String>) = listed
        .into_iter()
        .partition(|origin| is_production_safe_origin(origin));
    if !refused.is_empty() {
        tracing::warn!(
            refused = ?refused,
            "FRONTEND_URLS lists origins production never answers; they are ignored"
        );
    }
    if safe.is_empty() {
        return Err(ApiError::InternalError(
            "FRONTEND_URLS has no production origin (https, public host, no port) in production"
                .to_string(),
        ));
    }
    Ok(safe)
}

/// SEC-001 / ACC-018: refuse to start with missing, example or short signing material unless
/// `APP_ENV` explicitly names a development environment. An unset `APP_ENV` is treated as
/// production-like. The value itself is never logged.
pub fn validated_jwt_secret(secret: Option<String>, app_env: Option<String>) -> ApiResult<String> {
    let development = is_development_env(app_env.as_deref());
    let secret = secret.filter(|s| !s.is_empty());
    match secret {
        Some(s) if s != INSECURE_EXAMPLE_JWT_SECRET && s.len() >= MIN_JWT_SECRET_BYTES => Ok(s),
        Some(s) if development => {
            tracing::warn!("JWT_SECRET is weak; accepted only because APP_ENV is a development environment");
            Ok(s)
        }
        None if development => {
            tracing::warn!("JWT_SECRET is unset; using the development example value");
            Ok(INSECURE_EXAMPLE_JWT_SECRET.to_string())
        }
        _ => Err(ApiError::InternalError(format!(
            "JWT_SECRET must be set to at least {MIN_JWT_SECRET_BYTES} bytes of non-example secret material (APP_ENV is not a development environment)"
        ))),
    }
}

#[cfg(test)]
mod origin_tests {
    use super::*;

    const AUDITED: &str = "http://localhost:3000,http://localhost:3001,https://17730sdl-3000.uks1.devtunnels.ms,https://staging.cfmeet.com,https://cfmeet-staging.vercel.app,https://www.cfmeet.com,https://cfmeet.com/";

    #[test]
    fn production_keeps_only_the_public_https_origins() {
        assert_eq!(
            browser_origins(AUDITED, true).unwrap(),
            vec!["https://www.cfmeet.com", "https://cfmeet.com"]
        );
        for bad in [
            "http://cfmeet.com",
            "https://cfmeet.com:8443",
            "https://127.0.0.1",
            "https://dev.cfmeet.com",
            "https://app-preview.cfmeet.com",
            "https://*.cfmeet.com",
            "null",
        ] {
            assert!(!is_production_safe_origin(bad), "{bad}");
        }
    }

    #[test]
    fn production_without_a_safe_origin_refuses_to_start() {
        assert!(browser_origins("http://localhost:3000", true).is_err());
        assert!(browser_origins("", true).is_err());
    }

    #[test]
    fn staging_and_local_keep_their_lists() {
        assert_eq!(browser_origins(AUDITED, false).unwrap().len(), 7);
    }

    #[test]
    fn production_is_named_explicitly() {
        assert!(is_production_env(None, Some("production")));
        assert!(is_production_env(Some("PROD"), None));
        assert!(!is_production_env(None, Some("staging")));
        assert!(!is_production_env(Some("dev"), None));
        assert!(!is_production_env(None, None));
    }
}

#[cfg(test)]
mod jwt_secret_tests {
    use super::*;

    #[test]
    fn production_like_environments_refuse_missing_example_or_short_secrets() {
        for env in [None, Some("prod".to_string()), Some("staging".to_string())] {
            assert!(validated_jwt_secret(None, env.clone()).is_err());
            assert!(validated_jwt_secret(Some(String::new()), env.clone()).is_err());
            assert!(
                validated_jwt_secret(Some(INSECURE_EXAMPLE_JWT_SECRET.into()), env.clone())
                    .is_err()
            );
            assert!(validated_jwt_secret(Some("short-secret".into()), env.clone()).is_err());
            let strong = "0123456789abcdef0123456789abcdef".to_string();
            assert_eq!(
                validated_jwt_secret(Some(strong.clone()), env).unwrap(),
                strong
            );
        }
    }

    #[test]
    fn development_keeps_working_with_weak_material() {
        assert!(validated_jwt_secret(None, Some("dev".into())).is_ok());
        assert!(validated_jwt_secret(Some("test-secret".into()), Some("TEST".into())).is_ok());
    }
}
