//! Enterprise calling configuration (Part F §52–§62).
//!
//! Enterprise calling extends the ONE canonical Call architecture of this service: it
//! adds configuration (policies, departments, extensions, ring groups, queues, IVR flows,
//! business hours, forwarding, voicemail mailboxes, recording policies) and routing
//! behaviour on top of the existing Call, ringing, arbitration, control and Telephony
//! primitives. There is no second Call model and no PBX business service (ADR 0018).
//!
//! * The organization boundary is the platform tenant / application / environment
//!   ([`CallScope`]); every resource is confined to it and a resource of another scope is
//!   indistinguishable from a missing one (404).
//! * Mutable resources carry `version >= 1`; every mutation takes `expectedVersion` and a
//!   stale one is refused ([`EnterpriseError::VersionConflict`], 409).
//! * Every administrative change appends durable audit evidence ([`audit`]) in the same
//!   transaction as the change.
//! * This module is HTTP-independent; [`http`] is the thin `/v1/enterprise/**` layer.

pub mod audit;
pub mod departments;
pub mod http;
pub mod policy;

use std::sync::Arc;

use chrono::{DateTime, SecondsFormat, Utc};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, DbErr, SqlErr, Statement,
    TransactionTrait,
};
use uuid::Uuid;

use crate::{
    calls::command::CallScope,
    config::ConfigError,
    domain::{CredentialId, ServiceAccountId, SharedClock},
    error::{CallingError, ErrorCode},
    platform::{context::PlatformAuthContext, error::summarize_db_error},
};

/// Enterprise feature flags (kill switches). Each stops NEW effects only: termination,
/// disabling dangerous configuration, finalizing captured artifacts and retention
/// cleanup are never blocked (§164).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EnterpriseFlags {
    /// `ENTERPRISE_CALLING_ENABLED`: new enterprise call routing.
    pub enterprise_calling: bool,
    /// `VOICEMAIL_ENABLED`: new voicemail routing / capture.
    pub voicemail: bool,
    /// `CALL_RECORDING_ENABLED`: new call recordings.
    pub call_recording: bool,
}

impl EnterpriseFlags {
    /// Every flag on (tests and explicit opt-in deployments).
    pub const ALL_ON: Self = Self {
        enterprise_calling: true,
        voicemail: true,
        call_recording: true,
    };

    /// Reads the three flags; unset = `false` (the rollout convention of kill switches).
    pub fn from_lookup(get: &impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let flag = |var: &'static str| -> Result<bool, ConfigError> {
            match get(var).as_deref().map(str::trim) {
                None | Some("") => Ok(false),
                Some("true") | Some("1") => Ok(true),
                Some("false") | Some("0") => Ok(false),
                Some(_) => Err(ConfigError::invalid(
                    var,
                    "must be true or false".to_string(),
                )),
            }
        };
        Ok(Self {
            enterprise_calling: flag("ENTERPRISE_CALLING_ENABLED")?,
            voicemail: flag("VOICEMAIL_ENABLED")?,
            call_recording: flag("CALL_RECORDING_ENABLED")?,
        })
    }
}

/// Errors of the enterprise configuration domain.
#[derive(Debug, thiserror::Error)]
pub enum EnterpriseError {
    /// Missing, or outside the caller's scope (indistinguishable by design).
    #[error("not found")]
    NotFound,
    /// Malformed input; the message is a fixed, safe string.
    #[error("{0}")]
    Validation(&'static str),
    /// `expectedVersion` does not match the stored version.
    #[error("version conflict (current {current})")]
    VersionConflict { current: i64 },
    /// A uniqueness or state conflict (e.g. an active name or extension number in use).
    #[error("{0}")]
    Conflict(&'static str),
    /// The enterprise policy does not permit this configuration or action.
    #[error("{0}")]
    PolicyDenied(&'static str),
    /// A kill switch / capability is off.
    #[error("{0}")]
    Unavailable(&'static str),
    #[error("database error ({summary})")]
    Database { summary: String, connectivity: bool },
}

pub type EnterpriseResult<T> = Result<T, EnterpriseError>;

impl From<DbErr> for EnterpriseError {
    fn from(err: DbErr) -> Self {
        match err.sql_err() {
            Some(SqlErr::UniqueConstraintViolation(_)) => {
                EnterpriseError::Conflict("The resource conflicts with an existing one.")
            }
            Some(SqlErr::ForeignKeyConstraintViolation(_)) => EnterpriseError::NotFound,
            _ => EnterpriseError::Database {
                summary: summarize_db_error(&err),
                connectivity: crate::platform::error::is_connectivity_error(&err),
            },
        }
    }
}

impl From<EnterpriseError> for CallingError {
    fn from(err: EnterpriseError) -> Self {
        match err {
            EnterpriseError::NotFound => CallingError::NotFound,
            EnterpriseError::Validation(message) => CallingError::Validation(message),
            EnterpriseError::VersionConflict { .. } => CallingError::Call {
                code: ErrorCode::VersionConflict,
                status: axum::http::StatusCode::CONFLICT,
            },
            EnterpriseError::Conflict(_) => CallingError::Call {
                code: ErrorCode::Conflict,
                status: axum::http::StatusCode::CONFLICT,
            },
            EnterpriseError::PolicyDenied(reason) => {
                tracing::info!(reason, "enterprise policy refused the operation");
                CallingError::Call {
                    code: ErrorCode::EnterprisePolicyDenied,
                    status: axum::http::StatusCode::FORBIDDEN,
                }
            }
            EnterpriseError::Unavailable(_) => CallingError::Call {
                code: ErrorCode::CapabilityUnavailable,
                status: axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            },
            EnterpriseError::Database {
                summary,
                connectivity,
            } => {
                tracing::error!(%summary, "enterprise database error");
                if connectivity {
                    CallingError::ServiceUnavailable
                } else {
                    CallingError::Internal
                }
            }
        }
    }
}

text_enum!(
    /// Who an administrative change is attributed to.
    AdminActorKind {
        // A customer application credential (the public enterprise API).
        CustomerApplication = "CUSTOMER_APPLICATION",
        // A CF Meet operator (`platform_admin`), e.g. granting high-risk capabilities.
        Operator = "OPERATOR",
        // A Calling worker (retention deletion, finalization).
        System = "SYSTEM",
    }
);

/// Who performs an administrative change (from the authenticated platform context, the
/// operator CLI or a Calling worker — never from request data).
#[derive(Debug, Clone)]
pub struct AdminActor {
    pub scope: CallScope,
    pub kind: AdminActorKind,
    pub service_account_id: Option<ServiceAccountId>,
    pub credential_id: Option<CredentialId>,
    pub request_id: String,
    pub correlation_id: String,
}

impl AdminActor {
    pub fn of(ctx: &PlatformAuthContext) -> Self {
        Self {
            scope: CallScope::of_platform(ctx),
            kind: AdminActorKind::CustomerApplication,
            service_account_id: Some(ctx.service_account_id()),
            credential_id: Some(ctx.credential_id()),
            request_id: ctx.request_id().as_str().to_string(),
            correlation_id: ctx.correlation_id().as_str().to_string(),
        }
    }

    /// A CF Meet operator acting through `platform_admin`.
    pub fn operator(scope: CallScope) -> Self {
        let request = crate::domain::RequestContext::generate();
        Self {
            scope,
            kind: AdminActorKind::Operator,
            service_account_id: None,
            credential_id: None,
            request_id: request.request_id.as_str().to_string(),
            correlation_id: request.correlation_id.as_str().to_string(),
        }
    }

    /// A Calling worker.
    pub fn system(scope: CallScope, correlation_id: &str) -> Self {
        let request = crate::domain::RequestContext::generate();
        Self {
            scope,
            kind: AdminActorKind::System,
            service_account_id: None,
            credential_id: None,
            request_id: request.request_id.as_str().to_string(),
            correlation_id: correlation_id.to_string(),
        }
    }
}

/// The enterprise configuration service (the only writer of enterprise configuration).
#[derive(Clone)]
pub struct EnterpriseService {
    db: DatabaseConnection,
    clock: SharedClock,
    flags: EnterpriseFlags,
}

impl EnterpriseService {
    pub fn new(db: DatabaseConnection, clock: SharedClock, flags: EnterpriseFlags) -> Self {
        Self { db, clock, flags }
    }

    pub fn shared(self) -> Arc<Self> {
        Arc::new(self)
    }

    pub fn flags(&self) -> EnterpriseFlags {
        self.flags
    }

    pub(crate) fn db(&self) -> &DatabaseConnection {
        &self.db
    }

    pub(crate) fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    pub(crate) async fn begin(&self) -> EnterpriseResult<DatabaseTransaction> {
        Ok(self.db.begin().await?)
    }
}

/// A statement with positional values.
pub(crate) fn stmt(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

/// Runs a statement; returns the affected row count.
pub(crate) async fn exec<C: ConnectionTrait>(
    db: &C,
    sql: &str,
    values: Vec<sea_orm::Value>,
) -> EnterpriseResult<u64> {
    Ok(db.execute(stmt(sql, values)).await?.rows_affected())
}

/// Scope values `(tenant, application, environment)` in that order.
pub(crate) fn scope_values(scope: CallScope) -> Vec<sea_orm::Value> {
    vec![
        scope.tenant_id().as_uuid().into(),
        scope.application_id().as_uuid().into(),
        scope.environment_id().as_uuid().into(),
    ]
}

/// RFC 3339 with milliseconds, as every Calling response renders time.
pub(crate) fn time(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// `expectedVersion` against the stored version.
pub(crate) fn check_version(expected: i64, current: i64) -> EnterpriseResult<()> {
    if expected < 1 {
        return Err(EnterpriseError::Validation(
            "expectedVersion must be a positive integer.",
        ));
    }
    if expected != current {
        return Err(EnterpriseError::VersionConflict { current });
    }
    Ok(())
}

/// A bounded, trimmed, control-character-free name.
pub(crate) fn clean_name(
    value: &str,
    max: usize,
    message: &'static str,
) -> EnterpriseResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.chars().count() > max
        || trimmed.chars().any(|c| c.is_control() || is_bidi(c))
        || carries_dialable_number(trimmed)
    {
        return Err(EnterpriseError::Validation(message));
    }
    Ok(trimmed.to_string())
}

fn is_bidi(c: char) -> bool {
    matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}')
}

/// Free text that would carry a telephone number (`+` followed by digits, or seven or
/// more digits in a row). Enterprise configuration never stores raw numbers (§4–§5).
pub fn carries_dialable_number(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    if lower.starts_with("tel:") || lower.starts_with("sip:") || lower.starts_with("sips:") {
        return true;
    }
    let mut run = 0usize;
    let mut plus = false;
    for c in value.chars() {
        if c.is_ascii_digit() {
            run += 1;
            if run >= 7 || plus {
                return true;
            }
        } else if c == '+' {
            plus = true;
            run = 0;
            continue;
        } else if !(c == ' ' || c == '-' || c == '.' || c == '(' || c == ')') || run == 0 {
            run = 0;
        }
        plus = false;
    }
    false
}

/// Opaque keyset cursor `(created_at, id)` of ascending listings.
pub(crate) mod cursor {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    use chrono::{DateTime, Utc};
    use uuid::Uuid;

    use super::{EnterpriseError, EnterpriseResult};

    pub fn encode(at: DateTime<Utc>, id: Uuid) -> String {
        URL_SAFE_NO_PAD.encode(format!("{}|{id}", at.timestamp_micros()))
    }

    pub fn decode(value: &str) -> EnterpriseResult<(DateTime<Utc>, Uuid)> {
        let invalid = EnterpriseError::Validation("cursor is invalid.");
        let bytes = URL_SAFE_NO_PAD.decode(value).map_err(|_| invalid)?;
        let text = String::from_utf8(bytes)
            .map_err(|_| EnterpriseError::Validation("cursor is invalid."))?;
        let (micros, id) = text
            .split_once('|')
            .ok_or(EnterpriseError::Validation("cursor is invalid."))?;
        let micros: i64 = micros
            .parse()
            .map_err(|_| EnterpriseError::Validation("cursor is invalid."))?;
        let at = DateTime::<Utc>::from_timestamp_micros(micros)
            .ok_or(EnterpriseError::Validation("cursor is invalid."))?;
        let id =
            Uuid::parse_str(id).map_err(|_| EnterpriseError::Validation("cursor is invalid."))?;
        Ok((at, id))
    }
}

/// Listing bounds shared by every enterprise collection.
#[derive(Debug, Clone, Default)]
pub struct PageRequest {
    pub limit: Option<u64>,
    pub cursor: Option<String>,
}

impl PageRequest {
    pub(crate) fn limit(&self) -> EnterpriseResult<u64> {
        match self.limit {
            None => Ok(50),
            Some(l) if (1..=100).contains(&l) => Ok(l),
            Some(_) => Err(EnterpriseError::Validation(
                "limit must be between 1 and 100.",
            )),
        }
    }

    pub(crate) fn after(&self) -> EnterpriseResult<Option<(DateTime<Utc>, Uuid)>> {
        self.cursor.as_deref().map(cursor::decode).transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialable_text_is_detected() {
        for v in [
            "+2348012345678",
            "08012345678",
            "tel:123",
            "sip:a@b",
            "+1 202",
            "555-1234567",
        ] {
            assert!(carries_dialable_number(v), "{v}");
        }
        for v in ["Sales", "1001", "Support line 2", "room 101", "abc-123"] {
            assert!(!carries_dialable_number(v), "{v}");
        }
    }

    #[test]
    fn cursors_round_trip_and_reject_garbage() {
        let at = DateTime::parse_from_rfc3339("2031-03-01T10:00:30.123456Z")
            .unwrap()
            .with_timezone(&Utc);
        let id = Uuid::new_v4();
        assert_eq!(cursor::decode(&cursor::encode(at, id)).unwrap(), (at, id));
        assert!(cursor::decode("not-a-cursor").is_err());
    }

    #[test]
    fn versions_are_checked() {
        assert!(check_version(1, 1).is_ok());
        assert!(matches!(
            check_version(1, 2),
            Err(EnterpriseError::VersionConflict { current: 2 })
        ));
        assert!(matches!(
            check_version(0, 1),
            Err(EnterpriseError::Validation(_))
        ));
    }

    #[test]
    fn flags_default_off_and_parse_strictly() {
        let none = EnterpriseFlags::from_lookup(&|_| None).unwrap();
        assert_eq!(none, EnterpriseFlags::default());
        let on = EnterpriseFlags::from_lookup(&|k| {
            (k == "ENTERPRISE_CALLING_ENABLED").then(|| "true".to_string())
        })
        .unwrap();
        assert!(on.enterprise_calling && !on.voicemail && !on.call_recording);
        assert!(EnterpriseFlags::from_lookup(&|_| Some("yes".into())).is_err());
    }
}
