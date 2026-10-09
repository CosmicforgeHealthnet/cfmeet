//! Server-authoritative limit policy (Part G §70, ADR 0028).
//!
//! Limits are configured by platform operators — never by request input — at five levels:
//!
//! * the **API-client chain** TENANT → APPLICATION → SERVICE_ACCOUNT → CREDENTIAL: the
//!   effective per-credential limit is the most restrictive configured value of the chain
//!   (on a tie the more specific level is named), else the platform default; every
//!   configured parent level is ALSO an aggregate ceiling across everything beneath it
//!   (all of the tenant's applications, the application's service accounts, the service
//!   account's credentials);
//! * the **PRINCIPAL axis**: an independent communication-subject ceiling, evaluated only
//!   for an operation whose principal the server resolved in the caller's own scope. It
//!   is not a child of a credential: every credential / service account acting for the
//!   principal shares it;
//! * telecom spend defaults to zero, and is always zero in an environment that is not
//!   allowed live telecom (SANDBOX / TEST, or PRODUCTION not yet enabled); the
//!   environment's spend policy applies on top.
//!
//! Enforcement: the request-rate limit here (shared PostgreSQL one-minute windows per
//! tenant, application, service account and credential on every authenticated request;
//! per principal on every principal-scoped operation — Call creation by that initiator),
//! the concurrent-communication limit in Call creation (`calls::service`), telecom spend
//! in the Part E reservation (`telephony::spend`).

use chrono::{DateTime, Duration, DurationRound, Utc};
use sea_orm::{
    ActiveValue::Set, ConnectionTrait, DatabaseConnection, DbBackend, DbErr, EntityTrait,
    FromQueryResult, Statement,
};
use serde::Serialize;
use uuid::Uuid;

use super::{context::PlatformAuthContext, PlatformError, PlatformResult, PlatformService};
use crate::{
    domain::{ApplicationId, CredentialId, PrincipalId, ServiceAccountId, SharedClock, TenantId},
    models::{application_limits, credential_limits, environments, tenants},
};

/// Platform defaults used when no level of the API-client chain configures a limit.
/// Telecom spend has no configurable default: it is always zero.
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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, FromQueryResult)]
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
    Tenant,
    Application,
    ServiceAccount,
    Credential,
    /// Forced by the environment's live-telecom policy.
    EnvironmentPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct EffectiveLimit {
    pub value: i64,
    pub source: LimitSource,
}

/// The configured levels of the API-client chain of one credential.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct LimitLevels {
    pub tenant: LimitValues,
    pub application: LimitValues,
    pub service_account: LimitValues,
    pub credential: LimitValues,
}

impl LimitLevels {
    fn chain(
        &self,
        value: impl Fn(&LimitValues) -> Option<i64>,
    ) -> [(Option<i64>, LimitSource); 4] {
        [
            (value(&self.tenant), LimitSource::Tenant),
            (value(&self.application), LimitSource::Application),
            (value(&self.service_account), LimitSource::ServiceAccount),
            (value(&self.credential), LimitSource::Credential),
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EffectiveLimits {
    pub requests_per_minute: EffectiveLimit,
    pub max_concurrent_communications: EffectiveLimit,
    pub telecom_spend_daily_limit_minor: EffectiveLimit,
    pub telecom_spend_monthly_limit_minor: EffectiveLimit,
    /// Currency of the telecom-spend limits (the tenant's billing currency).
    pub telecom_spend_currency: String,
    /// Tenant-level configuration: also the aggregate ceiling across all of the
    /// tenant's applications.
    pub tenant: LimitValues,
    /// Application-level configuration: also the aggregate ceiling across all of the
    /// application's service accounts and credentials.
    pub application: LimitValues,
    /// Service-account-level configuration: also the aggregate ceiling across all of the
    /// service account's credentials.
    pub service_account: LimitValues,
    /// Credential-level configuration.
    pub credential: LimitValues,
}

/// The most restrictive configured level (on a tie, the more specific level is named),
/// else the platform default.
fn strictest(levels: &[(Option<i64>, LimitSource)], default: i64) -> EffectiveLimit {
    levels
        .iter()
        .filter_map(|(v, source)| {
            v.map(|value| EffectiveLimit {
                value,
                source: *source,
            })
        })
        .fold(None, |best: Option<EffectiveLimit>, level| match best {
            Some(b) if b.value < level.value => Some(b),
            _ => Some(level),
        })
        .unwrap_or(EffectiveLimit {
            value: default,
            source: LimitSource::PlatformDefault,
        })
}

fn rpm(v: &LimitValues) -> Option<i64> {
    v.requests_per_minute.map(i64::from)
}

fn concurrent(v: &LimitValues) -> Option<i64> {
    v.max_concurrent_communications.map(i64::from)
}

/// Pure resolution of the effective per-credential policy.
pub fn resolve_effective_limits(
    levels: LimitLevels,
    defaults: LimitDefaults,
    live_telecom_allowed: bool,
    telecom_spend_currency: String,
) -> EffectiveLimits {
    let spend = |value: fn(&LimitValues) -> Option<i64>| {
        if live_telecom_allowed {
            strictest(&levels.chain(value), 0)
        } else {
            EffectiveLimit {
                value: 0,
                source: LimitSource::EnvironmentPolicy,
            }
        }
    };
    EffectiveLimits {
        requests_per_minute: strictest(&levels.chain(rpm), defaults.requests_per_minute),
        max_concurrent_communications: strictest(
            &levels.chain(concurrent),
            defaults.max_concurrent_communications,
        ),
        telecom_spend_daily_limit_minor: spend(|v| v.telecom_spend_daily_limit_minor),
        telecom_spend_monthly_limit_minor: spend(|v| v.telecom_spend_monthly_limit_minor),
        telecom_spend_currency,
        tenant: levels.tenant,
        application: levels.application,
        service_account: levels.service_account,
        credential: levels.credential,
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

const LIMIT_COLUMNS: &str = "requests_per_minute, max_concurrent_communications, \
    telecom_spend_daily_limit_minor, telecom_spend_monthly_limit_minor";

/// One level stored by key (`table`, `key` column).
async fn level_of<C: ConnectionTrait>(
    db: &C,
    table: &str,
    key: &str,
    id: Uuid,
) -> Result<LimitValues, DbErr> {
    Ok(
        LimitValues::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!("SELECT {LIMIT_COLUMNS} FROM {table} WHERE {key} = $1"),
            [id.into()],
        ))
        .one(db)
        .await?
        .unwrap_or_default(),
    )
}

/// The configured PRINCIPAL level (independent axis).
pub(crate) async fn principal_level<C: ConnectionTrait>(
    db: &C,
    principal_id: PrincipalId,
) -> Result<LimitValues, DbErr> {
    level_of(
        db,
        "platform_principal_limits",
        "principal_id",
        principal_id.as_uuid(),
    )
    .await
}

/// The configured levels of the API-client chain.
pub(crate) async fn load_levels<C: ConnectionTrait>(
    db: &C,
    tenant_id: TenantId,
    application_id: ApplicationId,
    service_account_id: ServiceAccountId,
    credential_id: CredentialId,
) -> Result<LimitLevels, DbErr> {
    let tenant = level_of(
        db,
        "platform_tenant_limits",
        "tenant_id",
        tenant_id.as_uuid(),
    )
    .await?;
    let application = application_limits::Entity::find_by_id(application_id.as_uuid())
        .one(db)
        .await?
        .map(LimitValues::from)
        .unwrap_or_default();
    let service_account = level_of(
        db,
        "platform_service_account_limits",
        "service_account_id",
        service_account_id.as_uuid(),
    )
    .await?;
    let credential = credential_limits::Entity::find_by_id(credential_id.as_uuid())
        .one(db)
        .await?
        .map(LimitValues::from)
        .unwrap_or_default();
    Ok(LimitLevels {
        tenant,
        application,
        service_account,
        credential,
    })
}

/// Concurrent-communication limits of a Call creation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ConcurrencyLimits {
    /// The effective per-credential limit (most restrictive level, else the default).
    pub per_credential: i64,
    /// The service-account aggregate ceiling, when configured.
    pub service_account: Option<i64>,
    /// The application-wide aggregate ceiling, when configured.
    pub application: Option<i64>,
    /// The tenant-wide aggregate ceiling, when configured.
    pub tenant: Option<i64>,
    /// The initiating principal's ceiling, when configured.
    pub principal: Option<i64>,
}

pub(crate) async fn concurrent_communication_limits<C: ConnectionTrait>(
    db: &C,
    tenant_id: TenantId,
    application_id: ApplicationId,
    service_account_id: ServiceAccountId,
    credential_id: CredentialId,
    principal_id: PrincipalId,
    defaults: LimitDefaults,
) -> Result<ConcurrencyLimits, DbErr> {
    let levels = load_levels(
        db,
        tenant_id,
        application_id,
        service_account_id,
        credential_id,
    )
    .await?;
    let principal = principal_level(db, principal_id).await?;
    Ok(ConcurrencyLimits {
        per_credential: strictest(
            &levels.chain(concurrent),
            defaults.max_concurrent_communications,
        )
        .value,
        service_account: concurrent(&levels.service_account),
        application: concurrent(&levels.application),
        tenant: concurrent(&levels.tenant),
        principal: concurrent(&principal),
    })
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
        let levels = load_levels(
            &self.db,
            ctx.tenant_id(),
            ctx.application_id(),
            ctx.service_account_id(),
            ctx.credential_id(),
        )
        .await?;
        let currency = tenant_currency(&self.db, ctx.tenant_id()).await?;
        Ok(resolve_effective_limits(
            levels,
            self.defaults,
            ctx.live_telecom_allowed(),
            currency,
        ))
    }
}

impl PlatformService {
    /// Replaces the tenant-level limit policy (`None` = not configured).
    pub async fn set_tenant_limits(
        &self,
        tenant_id: TenantId,
        values: LimitValues,
    ) -> PlatformResult<LimitValues> {
        values.validate()?;
        let tenant = self.tenant(tenant_id).await?;
        let now = self.clock.now();
        self.db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO platform_tenant_limits (tenant_id, requests_per_minute, \
                   max_concurrent_communications, telecom_spend_daily_limit_minor, \
                   telecom_spend_monthly_limit_minor, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $6) \
                 ON CONFLICT (tenant_id) DO UPDATE SET \
                   requests_per_minute = EXCLUDED.requests_per_minute, \
                   max_concurrent_communications = EXCLUDED.max_concurrent_communications, \
                   telecom_spend_daily_limit_minor = EXCLUDED.telecom_spend_daily_limit_minor, \
                   telecom_spend_monthly_limit_minor = EXCLUDED.telecom_spend_monthly_limit_minor, \
                   updated_at = EXCLUDED.updated_at",
                [
                    tenant.id.as_uuid().into(),
                    values.requests_per_minute.into(),
                    values.max_concurrent_communications.into(),
                    values.telecom_spend_daily_limit_minor.into(),
                    values.telecom_spend_monthly_limit_minor.into(),
                    now.into(),
                ],
            ))
            .await?;
        tracing::info!(tenant_id = %tenant_id, "tenant limit policy set");
        Ok(values)
    }

    /// Upserts one owned limit row (service account / principal).
    async fn upsert_owned_limits(
        &self,
        table: &str,
        key: &str,
        owner: (TenantId, ApplicationId, crate::domain::EnvironmentId, Uuid),
        values: LimitValues,
    ) -> PlatformResult<()> {
        let now = self.clock.now();
        self.db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                format!(
                    "INSERT INTO {table} ({key}, tenant_id, application_id, environment_id, \
                       requests_per_minute, max_concurrent_communications, \
                       telecom_spend_daily_limit_minor, telecom_spend_monthly_limit_minor, \
                       created_at, updated_at) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9) \
                     ON CONFLICT ({key}) DO UPDATE SET \
                       requests_per_minute = EXCLUDED.requests_per_minute, \
                       max_concurrent_communications = EXCLUDED.max_concurrent_communications, \
                       telecom_spend_daily_limit_minor = EXCLUDED.telecom_spend_daily_limit_minor, \
                       telecom_spend_monthly_limit_minor = EXCLUDED.telecom_spend_monthly_limit_minor, \
                       updated_at = EXCLUDED.updated_at"
                ),
                [
                    owner.3.into(),
                    owner.0.as_uuid().into(),
                    owner.1.as_uuid().into(),
                    owner.2.as_uuid().into(),
                    values.requests_per_minute.into(),
                    values.max_concurrent_communications.into(),
                    values.telecom_spend_daily_limit_minor.into(),
                    values.telecom_spend_monthly_limit_minor.into(),
                    now.into(),
                ],
            ))
            .await?;
        Ok(())
    }

    /// Replaces the service-account-level limit policy (`None` = not configured): an
    /// aggregate ceiling across every credential of the service account.
    pub async fn set_service_account_limits(
        &self,
        service_account_id: ServiceAccountId,
        values: LimitValues,
    ) -> PlatformResult<LimitValues> {
        values.validate()?;
        let sa = self.service_account(service_account_id).await?;
        self.upsert_owned_limits(
            "platform_service_account_limits",
            "service_account_id",
            (
                sa.tenant_id,
                sa.application_id,
                sa.environment_id,
                sa.id.as_uuid(),
            ),
            values,
        )
        .await?;
        tracing::info!(service_account_id = %service_account_id, "service account limit policy set");
        Ok(values)
    }

    /// Replaces the principal-level limit policy (`None` = not configured): the
    /// principal's own ceiling, shared by every credential acting for it.
    pub async fn set_principal_limits(
        &self,
        principal_id: PrincipalId,
        values: LimitValues,
    ) -> PlatformResult<LimitValues> {
        values.validate()?;
        let p = self.principal(principal_id).await?;
        self.upsert_owned_limits(
            "platform_principal_limits",
            "principal_id",
            (
                p.tenant_id,
                p.application_id,
                p.environment_id,
                p.id.as_uuid(),
            ),
            values,
        )
        .await?;
        tracing::info!(principal_id = %principal_id, "principal limit policy set");
        Ok(values)
    }

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
        let levels = load_levels(
            &self.db,
            credential.tenant_id,
            credential.application_id,
            credential.service_account_id,
            credential.id,
        )
        .await?;
        let currency = tenant_currency(&self.db, credential.tenant_id).await?;
        Ok(resolve_effective_limits(
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
    tenant_rpm: Option<i32>,
    application_rpm: Option<i32>,
    service_account_rpm: Option<i32>,
    credential_rpm: Option<i32>,
}

#[derive(FromQueryResult)]
struct WindowCount {
    subject_kind: String,
    request_count: i64,
}

const CONFIGURED_RATES_SQL: &str = r#"
SELECT
    (SELECT requests_per_minute FROM platform_tenant_limits WHERE tenant_id = $3) AS tenant_rpm,
    (SELECT requests_per_minute FROM platform_application_limits WHERE application_id = $1) AS application_rpm,
    (SELECT requests_per_minute FROM platform_service_account_limits WHERE service_account_id = $4) AS service_account_rpm,
    (SELECT requests_per_minute FROM platform_credential_limits WHERE credential_id = $2) AS credential_rpm
"#;

/// One atomic statement increments every applicable window (no read-then-write race:
/// each concurrent request sees a distinct count per subject).
const COUNT_SQL: &str = r#"
INSERT INTO platform_request_rate_windows (subject_kind, subject_id, window_start, request_count)
VALUES ('CREDENTIAL', $1, $3, 1), ('APPLICATION', $2, $3, 1), ('TENANT', $4, $3, 1),
       ('SERVICE_ACCOUNT', $5, $3, 1)
ON CONFLICT (subject_kind, subject_id, window_start)
DO UPDATE SET request_count = platform_request_rate_windows.request_count + 1
RETURNING subject_kind, request_count
"#;

const COUNT_ONE_SQL: &str = r#"
INSERT INTO platform_request_rate_windows (subject_kind, subject_id, window_start, request_count)
VALUES ($1, $2, $3, 1)
ON CONFLICT (subject_kind, subject_id, window_start)
DO UPDATE SET request_count = platform_request_rate_windows.request_count + 1
RETURNING subject_kind, request_count
"#;

fn window_of(now: DateTime<Utc>) -> DateTime<Utc> {
    now.duration_trunc(Duration::minutes(1)).unwrap_or(now)
}

fn retry_after(window_start: DateTime<Utc>, now: DateTime<Utc>) -> u64 {
    (window_start + Duration::minutes(1) - now)
        .num_seconds()
        .max(1) as u64
}

/// Compares one counted window against its ceiling: `true` = over the ceiling. Pushes
/// the first refusal of the window (`ceiling + 1`, seen by exactly one request) as
/// `limit.exceeded` and the crossing of the warning threshold as `limit.warning`.
fn judge(
    level: crate::webhooks::notices::LimitLevel,
    subject: Uuid,
    count: i64,
    ceiling: i64,
    window_start: DateTime<Utc>,
    notices: &mut Vec<crate::webhooks::notices::LimitNotice>,
) -> bool {
    use crate::webhooks::notices::{LimitKind, LimitNotice};
    if count > ceiling {
        if count == ceiling + 1 {
            notices.push(LimitNotice::exceeded(
                LimitKind::RequestRate,
                level,
                subject,
                count,
                ceiling,
                window_start,
            ));
        }
        return true;
    }
    notices.extend(LimitNotice::warning_if_crossed(
        LimitKind::RequestRate,
        level,
        subject,
        count - 1,
        count,
        ceiling,
        window_start,
    ));
    false
}

/// Part G §70: the request-rate ceiling of a principal-scoped operation (a Call created
/// for a server-resolved initiator). Counted only when the principal configures a
/// ceiling; every credential / service account acting for the principal shares the
/// window. `scope` = (tenant, application, environment) receiving the notices.
pub(crate) async fn check_principal_rate<C: ConnectionTrait>(
    db: &C,
    now: DateTime<Utc>,
    scope: (Uuid, Uuid, Uuid),
    principal_id: PrincipalId,
) -> Result<RateDecision, DbErr> {
    use crate::webhooks::notices::{self, LimitLevel};
    let Some(ceiling) = rpm(&principal_level(db, principal_id).await?) else {
        return Ok(RateDecision::Allowed);
    };
    let window_start = window_of(now);
    let count = WindowCount::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        COUNT_ONE_SQL,
        [
            "PRINCIPAL".into(),
            principal_id.as_uuid().into(),
            window_start.into(),
        ],
    ))
    .one(db)
    .await?
    .map(|c| c.request_count)
    .unwrap_or(1);
    let mut found = Vec::new();
    let limited = judge(
        LimitLevel::Principal,
        principal_id.as_uuid(),
        count,
        ceiling,
        window_start,
        &mut found,
    );
    notices::emit(db, now, scope, &found).await;
    Ok(if limited {
        tracing::info!(principal_id = %principal_id, "principal request rate limit reached");
        RateDecision::Limited {
            retry_after_secs: retry_after(window_start, now),
        }
    } else {
        RateDecision::Allowed
    })
}

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

    /// Counts the request against its credential, service account, application and
    /// tenant windows (all from the authenticated context) and refuses it when any
    /// configured ceiling — or the effective per-credential limit — is exceeded.
    pub async fn check(&self, ctx: &PlatformAuthContext) -> Result<RateDecision, DbErr> {
        use crate::webhooks::notices::{self, LimitLevel};
        let configured = ConfiguredRates::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            CONFIGURED_RATES_SQL,
            [
                ctx.application_id().as_uuid().into(),
                ctx.credential_id().as_uuid().into(),
                ctx.tenant_id().as_uuid().into(),
                ctx.service_account_id().as_uuid().into(),
            ],
        ))
        .one(&self.db)
        .await?
        .unwrap_or(ConfiguredRates {
            tenant_rpm: None,
            application_rpm: None,
            service_account_rpm: None,
            credential_rpm: None,
        });
        let configured_values = |v: Option<i32>| LimitValues {
            requests_per_minute: v,
            ..Default::default()
        };
        let levels = LimitLevels {
            tenant: configured_values(configured.tenant_rpm),
            application: configured_values(configured.application_rpm),
            service_account: configured_values(configured.service_account_rpm),
            credential: configured_values(configured.credential_rpm),
        };
        let per_credential = strictest(&levels.chain(rpm), self.defaults.requests_per_minute).value;

        let now = self.clock.now();
        let window_start = window_of(now);
        let counts = WindowCount::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            COUNT_SQL,
            [
                ctx.credential_id().as_uuid().into(),
                ctx.application_id().as_uuid().into(),
                window_start.into(),
                ctx.tenant_id().as_uuid().into(),
                ctx.service_account_id().as_uuid().into(),
            ],
        ))
        .all(&self.db)
        .await?;

        let mut limited = false;
        let mut found = Vec::new();
        for count in counts {
            let (level, subject, ceiling) = match count.subject_kind.as_str() {
                "CREDENTIAL" => (
                    LimitLevel::Credential,
                    ctx.credential_id().as_uuid(),
                    Some(per_credential),
                ),
                "SERVICE_ACCOUNT" => (
                    LimitLevel::ServiceAccount,
                    ctx.service_account_id().as_uuid(),
                    configured.service_account_rpm.map(i64::from),
                ),
                "APPLICATION" => (
                    LimitLevel::Application,
                    ctx.application_id().as_uuid(),
                    configured.application_rpm.map(i64::from),
                ),
                "TENANT" => (
                    LimitLevel::Tenant,
                    ctx.tenant_id().as_uuid(),
                    configured.tenant_rpm.map(i64::from),
                ),
                _ => continue,
            };
            let Some(ceiling) = ceiling else { continue };
            limited |= judge(
                level,
                subject,
                count.request_count,
                ceiling,
                window_start,
                &mut found,
            );
        }
        if !found.is_empty() {
            let scope = (
                ctx.tenant_id().as_uuid(),
                ctx.application_id().as_uuid(),
                ctx.environment_id().as_uuid(),
            );
            notices::emit(&self.db, now, scope, &found).await;
        }
        if limited {
            tracing::info!(
                credential_id = %ctx.credential_id(),
                application_id = %ctx.application_id(),
                "request rate limit reached"
            );
            return Ok(RateDecision::Limited {
                retry_after_secs: retry_after(window_start, now),
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

    fn levels(
        tenant: LimitValues,
        application: LimitValues,
        credential: LimitValues,
    ) -> LimitLevels {
        LimitLevels {
            tenant,
            application,
            service_account: LimitValues::default(),
            credential,
        }
    }

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
            levels(
                LimitValues::default(),
                values(Some(100), Some(10), Some(5_000)),
                values(Some(40), Some(20), Some(9_000)),
            ),
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
            levels(
                LimitValues::default(),
                values(None, None, None),
                values(Some(7), None, None),
            ),
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
            levels(
                LimitValues::default(),
                values(None, None, Some(1_000_000)),
                values(None, None, Some(1_000_000)),
            ),
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
    fn tenant_level_is_part_of_the_precedence_chain() {
        let eff = resolve_effective_limits(
            levels(
                values(Some(30), Some(4), Some(1_000)),
                values(Some(100), Some(10), Some(5_000)),
                values(Some(40), Some(20), None),
            ),
            LimitDefaults::default(),
            true,
            "USD".into(),
        );
        for (got, value) in [
            (eff.requests_per_minute, 30),
            (eff.max_concurrent_communications, 4),
            (eff.telecom_spend_daily_limit_minor, 1_000),
        ] {
            assert_eq!(
                got,
                EffectiveLimit {
                    value,
                    source: LimitSource::Tenant
                }
            );
        }
        // On a tie the more specific level is named.
        let all = |c: Option<i64>| {
            [
                (Some(5), LimitSource::Tenant),
                (Some(5), LimitSource::Application),
                (Some(5), LimitSource::ServiceAccount),
                (c, LimitSource::Credential),
            ]
        };
        assert_eq!(strictest(&all(Some(5)), 9).source, LimitSource::Credential);
        assert_eq!(strictest(&all(None), 9).source, LimitSource::ServiceAccount);
    }

    #[test]
    fn service_account_level_is_part_of_the_chain() {
        let eff = resolve_effective_limits(
            LimitLevels {
                tenant: values(Some(100), Some(10), Some(5_000)),
                application: values(Some(90), None, None),
                service_account: values(Some(20), Some(3), Some(700)),
                credential: values(Some(50), Some(5), None),
            },
            LimitDefaults::default(),
            true,
            "USD".into(),
        );
        for (got, value) in [
            (eff.requests_per_minute, 20),
            (eff.max_concurrent_communications, 3),
            (eff.telecom_spend_daily_limit_minor, 700),
        ] {
            assert_eq!(
                got,
                EffectiveLimit {
                    value,
                    source: LimitSource::ServiceAccount
                }
            );
        }
    }

    #[test]
    fn limit_values_validation() {
        assert!(values(Some(0), None, None).validate().is_err());
        assert!(values(None, Some(-1), None).validate().is_err());
        assert!(values(None, None, Some(-5)).validate().is_err());
        assert!(values(Some(1), Some(0), Some(0)).validate().is_ok());
    }
}
