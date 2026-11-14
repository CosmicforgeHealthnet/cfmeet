//! Transactional email configuration (unified email delivery).
//!
//! `EMAIL_PROVIDER` selects the delivery provider of the email worker:
//!
//! | value | transport |
//! |---|---|
//! | `resend` | Resend HTTPS API (`RESEND_API_KEY_FILE`, preferred, or `RESEND_API_KEY`) |
//! | `smtp` | SMTP (`SMTP_HOST`, `SMTP_PORT`, `SMTP_USERNAME`, `SMTP_PASSWORD`) — legacy |
//! | `none` | no transport: messages stay durably QUEUED until a provider is configured |
//!
//! Unset: `smtp` when `SMTP_HOST` is set (behaviour before Resend), otherwise `none`.
//! Messages are always recorded in the durable ledger (`email_deliveries`) first, so the
//! provider can be switched without losing queued mail.
//!
//! Sender: `EMAIL_FROM_ADDRESS` / `EMAIL_FROM_NAME` (default `CF Meet <no-reply@cfmeet.com>`).
//! With SMTP, `SMTP_FROM_EMAIL` / `SMTP_FROM_NAME` still apply when the new variables are
//! unset (a Gmail relay only sends as its own account).

use crate::error::{ApiError, ApiResult};

pub const DEFAULT_FROM_ADDRESS: &str = "no-reply@cfmeet.com";
pub const DEFAULT_FROM_NAME: &str = "CF Meet";
pub const DEFAULT_RESEND_API_URL: &str = "https://api.resend.com";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmailProviderKind {
    Resend,
    Smtp,
    None,
}

impl EmailProviderKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Resend => "resend",
            Self::Smtp => "smtp",
            Self::None => "none",
        }
    }
}

/// Email configuration loaded from environment variables. Secrets are held only in
/// memory and never printed (`Debug` is not derived).
#[derive(Clone)]
pub struct EmailConfig {
    pub provider: EmailProviderKind,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_username: String,
    pub smtp_password: String,
    pub from_email: String,
    pub from_name: String,
    pub reply_to: Option<String>,
    pub app_url: String,
    pub resend_api_key: String,
    pub resend_api_url: String,
    /// Svix signing secret of the Resend webhook (`whsec_…`); enables
    /// `POST /api/v1/email/provider-events/resend`.
    pub resend_webhook_secret: Option<String>,
    /// Worker settings.
    pub max_attempts: i32,
    pub max_per_second: u32,
    pub batch: u64,
    pub poll_millis: u64,
    /// Days a final ledger row (metadata only — content is erased) is kept.
    pub retention_days: i64,
}

impl Default for EmailConfig {
    fn default() -> Self {
        Self {
            provider: EmailProviderKind::None,
            smtp_host: String::new(),
            smtp_port: 587,
            smtp_username: String::new(),
            smtp_password: String::new(),
            from_email: DEFAULT_FROM_ADDRESS.to_string(),
            from_name: DEFAULT_FROM_NAME.to_string(),
            reply_to: None,
            app_url: "http://localhost:3000".to_string(),
            resend_api_key: String::new(),
            resend_api_url: DEFAULT_RESEND_API_URL.to_string(),
            resend_webhook_secret: None,
            max_attempts: 8,
            max_per_second: 2,
            batch: 10,
            poll_millis: 1000,
            retention_days: 30,
        }
    }
}

/// A syntactically valid single address (`local@domain.tld`), no display name, no CR/LF.
pub fn valid_address(address: &str) -> bool {
    let a = address.trim();
    if a.len() > 254 || a.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return false;
    }
    let Some((local, domain)) = a.rsplit_once('@') else {
        return false;
    };
    !local.is_empty()
        && local.len() <= 64
        && !local.contains(['<', '>', '"', ',', ';', '@'])
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && domain
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
}

fn read_secret(get: &impl Fn(&str) -> Option<String>, name: &str) -> ApiResult<Option<String>> {
    if let Some(path) = get(&format!("{name}_FILE")).filter(|p| !p.trim().is_empty()) {
        let value = std::fs::read_to_string(path.trim()).map_err(|_| {
            ApiError::InternalError(format!("{name}_FILE is set but cannot be read"))
        })?;
        let value = value.trim().to_string();
        if value.is_empty() {
            return Err(ApiError::InternalError(format!("{name}_FILE is empty")));
        }
        return Ok(Some(value));
    }
    Ok(get(name)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty()))
}

impl EmailConfig {
    /// Create EmailConfig from environment variables.
    pub fn from_env() -> ApiResult<Self> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Validated configuration; a selected provider with missing or malformed settings is an
    /// error (the caller decides whether that stops startup).
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> ApiResult<Self> {
        let d = Self::default();
        let var = |k: &str| {
            get(k)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let smtp_host = var("SMTP_HOST").unwrap_or_default();
        let provider = match var("EMAIL_PROVIDER")
            .map(|v| v.to_ascii_lowercase())
            .as_deref()
        {
            Some("resend") => EmailProviderKind::Resend,
            Some("smtp") => EmailProviderKind::Smtp,
            Some("none") | Some("off") | Some("disabled") => EmailProviderKind::None,
            Some(other) => {
                return Err(ApiError::InternalError(format!(
                    "EMAIL_PROVIDER `{other}` is not supported (resend, smtp, none)"
                )))
            }
            None if !smtp_host.is_empty() => EmailProviderKind::Smtp,
            None => EmailProviderKind::None,
        };
        let smtp_from = provider == EmailProviderKind::Smtp;
        let from_email = var("EMAIL_FROM_ADDRESS")
            .or_else(|| smtp_from.then(|| var("SMTP_FROM_EMAIL")).flatten())
            .unwrap_or_else(|| d.from_email.clone());
        let from_name = var("EMAIL_FROM_NAME")
            .or_else(|| smtp_from.then(|| var("SMTP_FROM_NAME")).flatten())
            .unwrap_or_else(|| d.from_name.clone());
        if !valid_address(&from_email) {
            return Err(ApiError::InternalError(
                "EMAIL_FROM_ADDRESS is not a valid address".into(),
            ));
        }
        if from_name
            .chars()
            .any(|c| c.is_control() || matches!(c, '<' | '>' | '"'))
        {
            return Err(ApiError::InternalError(
                "EMAIL_FROM_NAME contains forbidden characters".into(),
            ));
        }
        let reply_to = var("EMAIL_REPLY_TO");
        if reply_to.as_deref().is_some_and(|r| !valid_address(r)) {
            return Err(ApiError::InternalError(
                "EMAIL_REPLY_TO is not a valid address".into(),
            ));
        }
        let num = |k: &str, default: i64, lo: i64, hi: i64| {
            var(k)
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(default)
                .clamp(lo, hi)
        };
        let mut config = Self {
            provider,
            smtp_port: var("SMTP_PORT")
                .map(|p| {
                    p.parse()
                        .map_err(|_| ApiError::InternalError("Invalid SMTP_PORT".to_string()))
                })
                .transpose()?
                .unwrap_or(587),
            smtp_username: var("SMTP_USERNAME").unwrap_or_default(),
            smtp_password: read_secret(&get, "SMTP_PASSWORD")?.unwrap_or_default(),
            smtp_host,
            from_email,
            from_name,
            reply_to,
            app_url: var("APP_URL").unwrap_or_else(|| d.app_url.clone()),
            resend_api_key: String::new(),
            resend_api_url: var("RESEND_API_URL").unwrap_or_else(|| d.resend_api_url.clone()),
            resend_webhook_secret: read_secret(&get, "RESEND_WEBHOOK_SECRET")?,
            max_attempts: num("EMAIL_MAX_ATTEMPTS", d.max_attempts as i64, 1, 20) as i32,
            max_per_second: num("EMAIL_MAX_PER_SECOND", d.max_per_second as i64, 1, 100) as u32,
            batch: num("EMAIL_WORKER_BATCH", d.batch as i64, 1, 100) as u64,
            poll_millis: num("EMAIL_WORKER_POLL_MS", d.poll_millis as i64, 200, 60_000) as u64,
            retention_days: num("EMAIL_LEDGER_RETENTION_DAYS", d.retention_days, 1, 365),
        };
        match provider {
            EmailProviderKind::Resend => {
                let key = read_secret(&get, "RESEND_API_KEY")?.ok_or_else(|| {
                    ApiError::InternalError(
                        "EMAIL_PROVIDER=resend needs RESEND_API_KEY_FILE (or RESEND_API_KEY)"
                            .into(),
                    )
                })?;
                if !key.starts_with("re_") || key.len() < 12 || key.chars().any(char::is_whitespace)
                {
                    return Err(ApiError::InternalError(
                        "RESEND_API_KEY does not look like a Resend API key".into(),
                    ));
                }
                if !config.resend_api_url.starts_with("https://")
                    && !config.resend_api_url.starts_with("http://127.0.0.1")
                {
                    return Err(ApiError::InternalError(
                        "RESEND_API_URL must use https".into(),
                    ));
                }
                config.resend_api_key = key;
            }
            EmailProviderKind::Smtp => {
                if config.smtp_host.is_empty()
                    || config.smtp_username.is_empty()
                    || config.smtp_password.is_empty()
                {
                    return Err(ApiError::InternalError(
                        "EMAIL_PROVIDER=smtp needs SMTP_HOST, SMTP_USERNAME and SMTP_PASSWORD"
                            .into(),
                    ));
                }
            }
            EmailProviderKind::None => {}
        }
        if config
            .resend_webhook_secret
            .as_deref()
            .is_some_and(|s| !s.starts_with("whsec_"))
        {
            return Err(ApiError::InternalError(
                "RESEND_WEBHOOK_SECRET must be the whsec_ signing secret".into(),
            ));
        }
        Ok(config)
    }

    /// `"CF Meet <no-reply@cfmeet.com>"`.
    pub fn from_header(&self) -> String {
        format!("{} <{}>", self.from_name, self.from_email)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn cfg(vars: &[(&str, &str)]) -> ApiResult<EmailConfig> {
        let m: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        EmailConfig::from_lookup(|k| m.get(k).cloned())
    }

    #[test]
    fn defaults_keep_previous_behaviour() {
        assert_eq!(cfg(&[]).unwrap().provider, EmailProviderKind::None);
        let smtp = cfg(&[
            ("SMTP_HOST", "smtp.gmail.com"),
            ("SMTP_USERNAME", "u"),
            ("SMTP_PASSWORD", "p"),
            ("SMTP_FROM_EMAIL", "team@example.org"),
        ])
        .unwrap();
        assert_eq!(smtp.provider, EmailProviderKind::Smtp);
        assert_eq!(smtp.from_email, "team@example.org");
    }

    #[test]
    fn resend_requires_a_key_and_uses_the_cfmeet_sender() {
        assert!(cfg(&[("EMAIL_PROVIDER", "resend")]).is_err());
        assert!(cfg(&[("EMAIL_PROVIDER", "resend"), ("RESEND_API_KEY", "nope")]).is_err());
        let c = cfg(&[
            ("EMAIL_PROVIDER", "resend"),
            ("RESEND_API_KEY", "re_test_1234567890"),
            ("SMTP_HOST", "smtp.gmail.com"),
            ("SMTP_FROM_EMAIL", "someone@gmail.com"),
        ])
        .unwrap();
        assert_eq!(c.provider, EmailProviderKind::Resend);
        assert_eq!(c.from_header(), "CF Meet <no-reply@cfmeet.com>");
    }

    #[test]
    fn key_is_read_from_a_file() {
        let dir = std::env::temp_dir().join(format!("cfmeet-email-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("key");
        std::fs::write(&path, "re_file_key_123456\n").unwrap();
        let c = cfg(&[
            ("EMAIL_PROVIDER", "Resend"),
            ("RESEND_API_KEY_FILE", path.to_str().unwrap()),
        ])
        .unwrap();
        assert_eq!(c.resend_api_key, "re_file_key_123456");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn sender_and_webhook_secret_are_validated() {
        assert!(cfg(&[("EMAIL_FROM_ADDRESS", "a@b")]).is_err());
        assert!(cfg(&[("EMAIL_FROM_NAME", "x\r\nBcc: y")]).is_err());
        assert!(cfg(&[("RESEND_WEBHOOK_SECRET", "plain")]).is_err());
        assert!(cfg(&[("EMAIL_PROVIDER", "carrier-pigeon")]).is_err());
    }

    #[test]
    fn addresses() {
        assert!(valid_address("no-reply@cfmeet.com"));
        assert!(!valid_address("x@cfmeet"));
        assert!(!valid_address("a b@cfmeet.com"));
        assert!(!valid_address("a@cfmeet.com\r\nBcc: z@q.com"));
        assert!(!valid_address("<a>@x.com"));
    }
}
