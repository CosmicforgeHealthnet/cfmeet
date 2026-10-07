//! Server-authoritative limit policy.
//!
//! Limits are configured per application and per credential by platform operators —
//! never by request input. The effective policy of an authenticated credential is:
//!
//! * where both levels configure a limit, the stricter (smaller) value wins;
//! * where one level configures it, that value applies;
//! * where neither does, the platform default applies;
//! * telecom spend defaults to zero, and is always zero in an environment that is not
//!   allowed live telecom (SANDBOX / TEST, or PRODUCTION not yet enabled).
//!
//! Enforcement implemented here: the request-rate limit, counted per credential (against
//! the effective limit) and per application (against the application-wide limit, when
//! configured) in PostgreSQL fixed one-minute windows, so every service replica shares
//! the same counters. Concurrent-communication and telecom-spend limits are resolved
//! here but enforced by the future call / telephony operations that consume them.

use chrono::{DateTime, Duration, DurationRound, Utc};
use sea_orm::{
    ActiveValue::Set, ConnectionTrait, DatabaseConnection, DbBackend, DbErr, EntityTrait,
    FromQueryResult, Statement,
};
use serde::Serialize;

use super::{context::PlatformAuthContext, PlatformError, PlatformResult, PlatformService};
use crate::{
    domain::{ApplicationId, CredentialId, SharedClock, TenantId},
    models::{application_limits, credential_limits, environments, tenants},
};

/// Platform defaults used when neither the application nor the credential configures a
/// limit. Telecom spend has no configurable default: it is always zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LimitDefaults {
    pub requests_per_minute: i64,
    pub max_concurrent_communications: i64,
}

impl Default for LimitDefaults {
    fn default() -> Self {
        Self {
            requests_per_minute: 600,
            max_concurrent_communications: 50,
        }
    }
}

/// Limits configured at one level; `None` = not configured at this level.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct LimitValues {
    pub requests_per_minute: Option<i32>,
    pub max_concurrent_communications: Option<i32>,
    pub telecom_spend_daily_limit_minor: Option<i64>,
    pub telecom_spend_monthly_limit_minor: Option<i64>,
}

impl LimitValues {
    pub fn validate(&self) -> PlatformResult<()> {
        if self.requests_per_minute.is_some_and(|v| v <= 0) {
            return Err(PlatformError::Validation(
                "requests_per_minute must be positive".to_string(),
            ));
        }
        if self.max_concurrent_communications.is_some_and(|v| v < 0)
            || self.telecom_spend_daily_limit_minor.is_some_and(|v| v < 0)
            || self
                .telecom_spend_monthly_limit_minor
                .is_some_and(|v| v < 0)
        {
            return Err(PlatformError::Validation(
                "limits must not be negative".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LimitSource {
    PlatformDefault,
    Application,
    Credential,
    /// Forced by the environment's live-telecom policy.
    EnvironmentPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct EffectiveLimit {
    pub value: i64,
    pub source: LimitSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EffectiveLimits {
    pub requests_per_minute: EffectiveLimit,
    pub max_concurrent_communications: EffectiveLimit,
    pub telecom_spend_daily_limit_minor: EffectiveLimit,
    pub telecom_spend_monthly_limit_minor: EffectiveLimit,
    /// Currency of the telecom-spend limits (the tenant's billing currency).
    pub telecom_spend_currency: String,
    /// Application-level configuration: also the aggregate ceiling across all of the
    /// application's credentials.
    pub application: LimitValues,
    /// Credential-level configuration.
    pub credential: LimitValues,
}

fn stricter(application: Option<i64>, credential: Option<i64>, default: i64) -> EffectiveLimit {
    let (value, source) = match (application, credential) {
        (None, None) => (default, LimitSource::PlatformDefault),
        (Some(a), None) => (a, LimitSource::Application),
        (None, Some(c)) => (c, LimitSource::Credential),
        (Some(a), Some(c)) if c <= a => (c, LimitSource::Credential),
        (Some(a), Some(_)) => (a, LimitSource::Application),
    };
    EffectiveLimit { value, source }
}

/// Pure resolution of the effective policy.
pub fn resolve_effective_limits(
    application: LimitValues,
    credential: LimitValues,
    defaults: LimitDefaults,
    live_telecom_allowed: bool,
    telecom_spend_currency: String,
) -> EffectiveLimits {
    let spend = |a: Option<i64>, c: Option<i64>| {
        if live_telecom_allowed {
            stricter(a, c, 0)
        } else {
            EffectiveLimit {
                value: 0,
                source: LimitSource::EnvironmentPolicy,
            }
        }
    };
    EffectiveLimits {
        requests_per_minute: stricter(
            application.requests_per_minute.map(i64::from),
            credential.requests_per_minute.map(i64::from),
            defaults.requests_per_minute,
        ),
        max_concurrent_communications: stricter(
            application.max_concurrent_communications.map(i64::from),
            credential.max_concurrent_communications.map(i64::from),
            defaults.max_concurrent_communications,
        ),
        telecom_spend_daily_limit_minor: spend(
            application.telecom_spend_daily_limit_minor,
            credential.telecom_spend_daily_limit_minor,
        ),
        telecom_spend_monthly_limit_minor: spend(
            application.telecom_spend_monthly_limit_minor,
            credential.telecom_spend_monthly_limit_minor,
        ),
        telecom_spend_currency,
        application,
        credential,
    }
}

impl From<application_limits::Model> for LimitValues {
    fn from(m: application_limits::Model) -> Self {
        Self {
            requests_per_minute: m.requests_per_minute,
            max_concurrent_communications: m.max_concurrent_communications,
            telecom_spend_daily_limit_minor: m.telecom_spend_daily_limit_minor,
            telecom_spend_monthly_limit_minor: m.telecom_spend_monthly_limit_minor,
        }
    }
}

impl From<credential_limits::Model> for LimitValues {
    fn from(m: credential_limits::Model) -> Self {
        Self {
            requests_per_minute: m.requests_per_minute,
            max_concurrent_communications: m.max_concurrent_communications,
            telecom_spend_daily_limit_minor: m.telecom_spend_daily_limit_minor,
            telecom_spend_monthly_limit_minor: m.telecom_spend_monthly_limit_minor,
        }
    }
}

async fn load_levels<C: ConnectionTrait>(
    db: &C,
    application_id: ApplicationId,
    credential_id: CredentialId,
) -> Result<(LimitValues, LimitValues), DbErr> {
    let application = application_limits::Entity::find_by_id(application_id.as_uuid())
        .one(db)
        .await?
        .map(LimitValues::from)
        .unwrap_or_default();
    let credential = credential_limits::Entity::find_by_id(credential_id.as_uuid())
        .one(db)
        .await?
        .map(LimitValues::from)
        .unwrap_or_default();
    Ok((application, credential))
}

async fn tenant_currency<C: ConnectionTrait>(
    db: &C,
    tenant_id: TenantId,
) -> PlatformResult<String> {
    tenants::Entity::find_by_id(tenant_id.as_uuid())
        .one(db)
        .await?
        .map(|t| t.billing_currency)
        .ok_or(PlatformError::NotFound("tenant"))
}

/// Resolves the effective limit policy of an authenticated context.
#[derive(Clone)]
pub struct LimitPolicyResolver {
    db: DatabaseConnection,
    defaults: LimitDefaults,
}

impl LimitPolicyResolver {
    pub fn new(db: DatabaseConnection, defaults: LimitDefaults) -> Self {
        Self { db, defaults }
    }

    pub async fn effective_for(
        &self,
        ctx: &PlatformAuthContext,
    ) -> PlatformResult<EffectiveLimits> {
        let (application, credential) =
            load_levels(&self.db, ctx.application_id(), ctx.credential_id()).await?;
        let currency = tenant_currency(&self.db, ctx.tenant_id()).await?;
        Ok(resolve_effective_limits(
            application,
            credential,
            self.defaults,
            ctx.live_telecom_allowed(),
            currency,
        ))
    }
}

impl PlatformService {
    /// Replaces the application-level limit policy (`None` = not configured).
    pub async fn set_application_limits(
        &self,
        application_id: ApplicationId,
        values: LimitValues,
    ) -> PlatformResult<LimitValues> {
        values.validate()?;
        let application = self.application(application_id).await?;
        let now = self.clock.now();
        application_limits::Entity::insert(application_limits::ActiveModel {
            application_id: Set(application.id.as_uuid()),
            tenant_id: Set(application.tenant_id.as_uuid()),
            requests_per_minute: Set(values.requests_per_minute),
            max_concurrent_communications: Set(values.max_concurrent_communications),
            telecom_spend_daily_limit_minor: Set(values.telecom_spend_daily_limit_minor),
            telecom_spend_monthly_limit_minor: Set(values.telecom_spend_monthly_limit_minor),
            created_at: Set(now),
            updated_at: Set(now),
        })
        .on_conflict(
            sea_orm::sea_query::OnConflict::column(application_limits::Column::ApplicationId)
                .update_columns([
                    application_limits::Column::RequestsPerMinute,
                    application_limits::Column::MaxConcurrentCommunications,
                    application_limits::Column::TelecomSpendDailyLimitMinor,
                    application_limits::Column::TelecomSpendMonthlyLimitMinor,
                    application_limits::Column::UpdatedAt,
                ])
                .to_owned(),
        )
        .exec_without_returning(&self.db)
        .await?;
        tracing::info!(application_id = %application_id, "application limit policy set");
        Ok(values)
    }

    /// Replaces the credential-level limit policy (`None` = not configured).
    pub async fn set_credential_limits(
        &self,
        credential_id: CredentialId,
        values: LimitValues,
    ) -> PlatformResult<LimitValues> {
        values.validate()?;
        let credential = self.credential(credential_id).await?;
        let now = self.clock.now();
        credential_limits::Entity::insert(credential_limits::ActiveModel {
            credential_id: Set(credential.id.as_uuid()),
            tenant_id: Set(credential.tenant_id.as_uuid()),
            application_id: Set(credential.application_id.as_uuid()),
            requests_per_minute: Set(values.requests_per_minute),
            max_concurrent_communications: Set(values.max_concurrent_communications),
            telecom_spend_daily_limit_minor: Set(values.telecom_spend_daily_limit_minor),
            telecom_spend_monthly_limit_minor: Set(values.telecom_spend_monthly_limit_minor),
            created_at: Set(now),
            updated_at: Set(now),
        })
        .on_conflict(
            sea_orm::sea_query::OnConflict::column(credential_limits::Column::CredentialId)
                .update_columns([
                    credential_limits::Column::RequestsPerMinute,
                    credential_limits::Column::MaxConcurrentCommunications,
                    credential_limits::Column::TelecomSpendDailyLimitMinor,
                    credential_limits::Column::TelecomSpendMonthlyLimitMinor,
                    credential_limits::Column::UpdatedAt,
                ])
                .to_owned(),
        )
        .exec_without_returning(&self.db)
        .await?;
        tracing::info!(credential_id = %credential_id, "credential limit policy set");
        Ok(values)
    }

    /// Effective policy of a credential, as authentication would resolve it.
    pub async fn effective_limits(
        &self,
        credential_id: CredentialId,
    ) -> PlatformResult<EffectiveLimits> {
        let credential = self.credential(credential_id).await?;
        let environment = environments::Entity::find_by_id(credential.environment_id.as_uuid())
            .one(&self.db)
            .await?
            .ok_or(PlatformError::NotFound("environment"))?;
        let (application, levels) =
            load_levels(&self.db, credential.application_id, credential.id).await?;
        let currency = tenant_currency(&self.db, credential.tenant_id).await?;
        Ok(resolve_effective_limits(
            application,
            levels,
            self.limit_defaults,
            environment.live_telecom_allowed && environment.kind.may_allow_live_telecom(),
            currency,
        ))
    }
}

/// Outcome of a request-rate check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateDecision {
    Allowed,
    Limited { retry_after_secs: u64 },
}

#[derive(FromQueryResult)]
struct ConfiguredRates {
    application_rpm: Option<i32>,
    credential_rpm: Option<i32>,
}

#[derive(FromQueryResult)]
struct WindowCount {
    subject_kind: String,
    request_count: i64,
}

const CONFIGURED_RATES_SQL: &str = r#"
SELECT
    (SELECT requests_per_minute FROM platform_application_limits WHERE application_id = $1) AS application_rpm,
    (SELECT requests_per_minute FROM platform_credential_limits WHERE credential_id = $2) AS credential_rpm
"#;

const COUNT_SQL: &str = r#"
INSERT INTO platform_request_rate_windows (subject_kind, subject_id, window_start, request_count)
VALUES ('CREDENTIAL', $1, $3, 1), ('APPLICATION', $2, $3, 1)
ON CONFLICT (subject_kind, subject_id, window_start)
DO UPDATE SET request_count = platform_request_rate_windows.request_count + 1
RETURNING subject_kind, request_count
"#;

/// Enforces the effective request-rate limit with shared PostgreSQL counters.
#[derive(Clone)]
pub struct RequestRateLimiter {
    db: DatabaseConnection,
    defaults: LimitDefaults,
    clock: SharedClock,
}

impl RequestRateLimiter {
    pub fn new(db: DatabaseConnection, defaults: LimitDefaults, clock: SharedClock) -> Self {
        Self {
            db,
            defaults,
            clock,
        }
    }

    pub async fn check(&self, ctx: &PlatformAuthContext) -> Result<RateDecision, DbErr> {
        let configured = ConfiguredRates::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            CONFIGURED_RATES_SQL,
            [
                ctx.application_id().as_uuid().into(),
                ctx.credential_id().as_uuid().into(),
            ],
        ))
        .one(&self.db)
        .await?
        .unwrap_or(ConfiguredRates {
            application_rpm: None,
            credential_rpm: None,
        });
        let per_credential = stricter(
            configured.application_rpm.map(i64::from),
            configured.credential_rpm.map(i64::from),
            self.defaults.requests_per_minute,
        )
        .value;

        let now = self.clock.now();
        let window_start = now.duration_trunc(Duration::minutes(1)).unwrap_or(now);
        let counts = WindowCount::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            COUNT_SQL,
            [
                ctx.credential_id().as_uuid().into(),
                ctx.application_id().as_uuid().into(),
                window_start.into(),
            ],
        ))
        .all(&self.db)
        .await?;

        let mut limited = false;
        for count in counts {
            match count.subject_kind.as_str() {
                "CREDENTIAL" => limited |= count.request_count > per_credential,
                "APPLICATION" => {
                    limited |= configured
                        .application_rpm
                        .is_some_and(|limit| count.request_count > i64::from(limit))
                }
                _ => {}
            }
        }
        if limited {
            let retry_after = (window_start + Duration::minutes(1) - now)
                .num_seconds()
                .max(1) as u64;
            tracing::info!(
                credential_id = %ctx.credential_id(),
                application_id = %ctx.application_id(),
                "request rate limit reached"
            );
            return Ok(RateDecision::Limited {
                retry_after_secs: retry_after,
            });
        }
        Ok(RateDecision::Allowed)
    }

    /// Deletes rate windows that ended before `older_than`.
    pub async fn prune(&self, older_than: DateTime<Utc>) -> Result<u64, DbErr> {
        Ok(self
            .db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "DELETE FROM platform_request_rate_windows WHERE window_start < $1",
                [older_than.into()],
            ))
            .await?
            .rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(rpm: Option<i32>, concurrent: Option<i32>, daily: Option<i64>) -> LimitValues {
        LimitValues {
            requests_per_minute: rpm,
            max_concurrent_communications: concurrent,
            telecom_spend_daily_limit_minor: daily,
            telecom_spend_monthly_limit_minor: None,
        }
    }

    #[test]
    fn stricter_configured_limit_wins() {
        let defaults = LimitDefaults::default();
        let eff = resolve_effective_limits(
            values(Some(100), Some(10), Some(5_000)),
            values(Some(40), Some(20), Some(9_000)),
            defaults,
            true,
            "USD".into(),
        );
        assert_eq!(
            eff.requests_per_minute,
            EffectiveLimit {
                value: 40,
                source: LimitSource::Credential
            }
        );
        assert_eq!(
            eff.max_concurrent_communications,
            EffectiveLimit {
                value: 10,
                source: LimitSource::Application
            }
        );
        assert_eq!(
            eff.telecom_spend_daily_limit_minor,
            EffectiveLimit {
                value: 5_000,
                source: LimitSource::Application
            }
        );
    }

    #[test]
    fn single_level_and_defaults() {
        let defaults = LimitDefaults::default();
        let eff = resolve_effective_limits(
            values(None, None, None),
            values(Some(7), None, None),
            defaults,
            true,
            "USD".into(),
        );
        assert_eq!(
            eff.requests_per_minute,
            EffectiveLimit {
                value: 7,
                source: LimitSource::Credential
            }
        );
        assert_eq!(
            eff.max_concurrent_communications,
            EffectiveLimit {
                value: defaults.max_concurrent_communications,
                source: LimitSource::PlatformDefault
            }
        );
        // Telecom spend defaults to zero (fail closed).
        assert_eq!(
            eff.telecom_spend_monthly_limit_minor,
            EffectiveLimit {
                value: 0,
                source: LimitSource::PlatformDefault
            }
        );
    }

    #[test]
    fn non_live_environments_never_get_telecom_spend() {
        let eff = resolve_effective_limits(
            values(None, None, Some(1_000_000)),
            values(None, None, Some(1_000_000)),
            LimitDefaults::default(),
            false,
            "USD".into(),
        );
        assert_eq!(
            eff.telecom_spend_daily_limit_minor,
            EffectiveLimit {
                value: 0,
                source: LimitSource::EnvironmentPolicy
            }
        );
    }

    #[test]
    fn limit_values_validation() {
        assert!(values(Some(0), None, None).validate().is_err());
        assert!(values(None, Some(-1), None).validate().is_err());
        assert!(values(None, None, Some(-5)).validate().is_err());
        assert!(values(Some(1), Some(0), Some(0)).validate().is_ok());
    }
}
