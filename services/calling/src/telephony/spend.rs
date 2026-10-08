//! Telecom spend enforcement (Part E §153–§158).
//!
//! Every paid public leg needs a durable reservation BEFORE Telephony may originate it.
//! A reservation is taken inside the transaction that records the leg, after locking the
//! environment's `call_spend_policies` row (`FOR UPDATE`): two concurrent requests are
//! serialized there, so there is no check-then-spend race. Each check counts what is
//! already reserved or settled:
//!
//! * daily and monthly limits (UTC): RESERVED allowances + SETTLED actual costs — for
//!   the environment, and (Part B foundations, when configured) for the whole
//!   application (`platform_application_limits`) and for the API credential that created
//!   the Call (`platform_credential_limits`);
//! * concurrency: RESERVED reservations (open paid legs);
//! * velocity: reservations created in the last minute (environment), and in the last
//!   hour for the same protected contact, and — when configured — the same requesting
//!   principal, service account or communication context.
//!
//! Lock order (deadlock-free, every reservation takes the same order): the
//! application's limits row, the credential's limits row, then the environment policy.
//!
//! The reserved amount is the policy-defined allowance per leg (the provider's actual
//! cost is known only later); Telephony's USAGE_FINALIZED settles it with the reported
//! or estimated cost, never pretending an unknown cost is known. No environment policy
//! = no public paid leg (fail closed).

use chrono::{DateTime, Datelike, Duration, TimeZone, Utc};
use sea_orm::{ConnectionTrait, DbBackend, DbErr, FromQueryResult, Statement};
use uuid::Uuid;

/// Why a reservation was refused (bounded, safe codes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpendDenial {
    NotConfigured,
    Disabled,
    DailyLimit,
    MonthlyLimit,
    Concurrency,
    Velocity,
    ContactVelocity,
    ApplicationDailyLimit,
    ApplicationMonthlyLimit,
    CredentialDailyLimit,
    CredentialMonthlyLimit,
    PrincipalVelocity,
    ServiceAccountVelocity,
    ContextVelocity,
}

impl SpendDenial {
    pub fn as_str(&self) -> &'static str {
        match self {
            SpendDenial::NotConfigured => "SPEND_POLICY_NOT_CONFIGURED",
            SpendDenial::Disabled => "SPEND_DISABLED",
            SpendDenial::DailyLimit => "DAILY_SPEND_LIMIT",
            SpendDenial::MonthlyLimit => "MONTHLY_SPEND_LIMIT",
            SpendDenial::Concurrency => "CONCURRENT_PUBLIC_LEG_LIMIT",
            SpendDenial::Velocity => "ORIGINATION_VELOCITY_LIMIT",
            SpendDenial::ContactVelocity => "CONTACT_VELOCITY_LIMIT",
            SpendDenial::ApplicationDailyLimit => "APPLICATION_DAILY_SPEND_LIMIT",
            SpendDenial::ApplicationMonthlyLimit => "APPLICATION_MONTHLY_SPEND_LIMIT",
            SpendDenial::CredentialDailyLimit => "CREDENTIAL_DAILY_SPEND_LIMIT",
            SpendDenial::CredentialMonthlyLimit => "CREDENTIAL_MONTHLY_SPEND_LIMIT",
            SpendDenial::PrincipalVelocity => "PRINCIPAL_VELOCITY_LIMIT",
            SpendDenial::ServiceAccountVelocity => "SERVICE_ACCOUNT_VELOCITY_LIMIT",
            SpendDenial::ContextVelocity => "CONTEXT_VELOCITY_LIMIT",
        }
    }
}

/// The spend policy of one application environment.
#[derive(Debug, Clone, PartialEq, Eq, FromQueryResult)]
pub struct SpendPolicy {
    pub currency: String,
    pub leg_allowance_minor: i64,
    pub max_leg_seconds: i32,
    pub daily_limit_minor: i64,
    pub monthly_limit_minor: i64,
    pub max_concurrent_legs: i32,
    pub max_originations_per_minute: i32,
    pub max_per_party_per_hour: i32,
    pub max_per_principal_per_hour: Option<i32>,
    pub max_per_service_account_per_hour: Option<i32>,
    pub max_per_context_per_hour: Option<i32>,
    pub enabled: bool,
}

/// A durable reservation (what the originate command proves).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reservation {
    pub id: Uuid,
    pub amount_minor: i64,
    pub currency: String,
    pub max_duration_seconds: i32,
}

/// Whose spend a reservation counts against.
#[derive(Debug, Clone, Copy)]
pub struct SpendScope {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
}

#[derive(Debug, FromQueryResult)]
struct Totals {
    day: i64,
    month: i64,
    open: i64,
    minute: i64,
}

fn stmt(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

fn day_start(now: DateTime<Utc>) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(now.year(), now.month(), now.day(), 0, 0, 0)
        .single()
        .unwrap_or(now)
}

fn month_start(now: DateTime<Utc>) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
        .single()
        .unwrap_or(now)
}

const POLICY_COLUMNS: &str = "currency, leg_allowance_minor, max_leg_seconds, daily_limit_minor, \
    monthly_limit_minor, max_concurrent_legs, max_originations_per_minute, max_per_party_per_hour, \
    max_per_principal_per_hour, max_per_service_account_per_hour, max_per_context_per_hour, enabled";

async fn totals<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    now: DateTime<Utc>,
) -> Result<Totals, DbErr> {
    let row = Totals::find_by_statement(stmt(
        "SELECT \
           COALESCE(SUM(CASE WHEN status = 'RESERVED' THEN amount_minor WHEN status = 'SETTLED' THEN settled_minor ELSE 0 END) \
             FILTER (WHERE created_at >= $2), 0)::bigint AS day, \
           COALESCE(SUM(CASE WHEN status = 'RESERVED' THEN amount_minor WHEN status = 'SETTLED' THEN settled_minor ELSE 0 END) \
             FILTER (WHERE created_at >= $3), 0)::bigint AS month, \
           COUNT(*) FILTER (WHERE status = 'RESERVED')::bigint AS open, \
           COUNT(*) FILTER (WHERE created_at > $4)::bigint AS minute \
         FROM call_spend_reservations WHERE environment_id = $1 AND created_at >= LEAST($3, $4)",
        vec![
            environment_id.into(),
            day_start(now).into(),
            month_start(now).into(),
            (now - Duration::minutes(1)).into(),
        ],
    ))
    .one(db)
    .await?;
    Ok(row.unwrap_or(Totals {
        day: 0,
        month: 0,
        open: 0,
        minute: 0,
    }))
}

/// The environment's policy (no lock).
pub async fn policy<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
) -> Result<Option<SpendPolicy>, DbErr> {
    SpendPolicy::find_by_statement(stmt(
        &format!("SELECT {POLICY_COLUMNS} FROM call_spend_policies WHERE environment_id = $1"),
        vec![environment_id.into()],
    ))
    .one(db)
    .await
}

/// Remaining spend headroom (minor units) for the Routing Engine; a hint only — the
/// reservation decides. `None`: no usable policy (paid routes are denied).
pub async fn headroom<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    now: DateTime<Utc>,
) -> Result<Option<i64>, DbErr> {
    let Some(p) = policy(db, environment_id).await? else {
        return Ok(None);
    };
    if !p.enabled {
        return Ok(Some(0));
    }
    let t = totals(db, environment_id, now).await?;
    Ok(Some(
        (p.daily_limit_minor - t.day).min(p.monthly_limit_minor - t.month),
    ))
}

/// A Part B spend cap row (application or credential), locked while reserving.
#[derive(Debug, FromQueryResult)]
struct Cap {
    telecom_spend_daily_limit_minor: Option<i64>,
    telecom_spend_monthly_limit_minor: Option<i64>,
}

#[derive(Debug, FromQueryResult)]
struct Spent {
    day: i64,
    month: i64,
}

/// Day / month spend of the reservations matching `filter` (`column = $1`).
async fn spent<C: ConnectionTrait>(
    db: &C,
    column: &str,
    subject: Uuid,
    now: DateTime<Utc>,
) -> Result<Spent, DbErr> {
    let row = Spent::find_by_statement(stmt(
        &format!(
            "SELECT \
               COALESCE(SUM(CASE WHEN status = 'RESERVED' THEN amount_minor WHEN status = 'SETTLED' THEN settled_minor ELSE 0 END) \
                 FILTER (WHERE created_at >= $2), 0)::bigint AS day, \
               COALESCE(SUM(CASE WHEN status = 'RESERVED' THEN amount_minor WHEN status = 'SETTLED' THEN settled_minor ELSE 0 END), 0)::bigint AS month \
             FROM call_spend_reservations WHERE {column} = $1 AND created_at >= $3"
        ),
        vec![subject.into(), day_start(now).into(), month_start(now).into()],
    ))
    .one(db)
    .await?;
    Ok(row.unwrap_or(Spent { day: 0, month: 0 }))
}

async fn recent<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    column: &str,
    subject: Uuid,
    since: DateTime<Utc>,
) -> Result<i64, DbErr> {
    let row = db
        .query_one(stmt(
            &format!(
                "SELECT count(*) FROM call_spend_reservations \
                 WHERE environment_id = $1 AND {column} = $2 AND created_at > $3"
            ),
            vec![environment_id.into(), subject.into(), since.into()],
        ))
        .await?;
    row.map(|r| r.try_get_by_index(0))
        .transpose()
        .map(|v| v.unwrap_or(0))
}

/// Reserves one leg's allowance inside `txn` (which must also record the leg), for the
/// Call `call` (its environment, application, creating credential / service account,
/// requesting principal and communication context) and the protected `contact_reference`.
pub async fn reserve_in<C: ConnectionTrait>(
    txn: &C,
    call: &crate::models::calls::Model,
    call_leg_id: Uuid,
    contact_reference: Option<Uuid>,
    now: DateTime<Utc>,
) -> Result<Result<Reservation, SpendDenial>, DbErr> {
    let result = reserve_checked(txn, call, call_leg_id, contact_reference, now).await?;
    match &result {
        Ok(_) => crate::metrics::inc("calling_telecom_spend_reservations_total", &[]),
        Err(denial) => {
            crate::metrics::inc(
                "calling_telecom_spend_blocks_total",
                &[("reason", denial.as_str())],
            );
            // Abuse hook (§162): the same destination dialed again and again.
            if *denial == SpendDenial::ContactVelocity {
                crate::metrics::inc(
                    "calling_abuse_signals_total",
                    &[("kind", "SAME_DESTINATION_VELOCITY")],
                );
            }
        }
    }
    Ok(result)
}

async fn reserve_checked<C: ConnectionTrait>(
    txn: &C,
    call: &crate::models::calls::Model,
    call_leg_id: Uuid,
    contact_reference: Option<Uuid>,
    now: DateTime<Utc>,
) -> Result<Result<Reservation, SpendDenial>, DbErr> {
    // 1. Part B caps (locked first, in a fixed order): application, then credential.
    let app_cap = Cap::find_by_statement(stmt(
        "SELECT telecom_spend_daily_limit_minor, telecom_spend_monthly_limit_minor \
         FROM platform_application_limits WHERE application_id = $1 FOR UPDATE",
        vec![call.application_id.into()],
    ))
    .one(txn)
    .await?;
    let credential_cap = Cap::find_by_statement(stmt(
        "SELECT telecom_spend_daily_limit_minor, telecom_spend_monthly_limit_minor \
         FROM platform_credential_limits WHERE credential_id = $1 FOR UPDATE",
        vec![call.created_by_credential_id.into()],
    ))
    .one(txn)
    .await?;
    // 2. The environment policy (serializes every reservation of the environment).
    let p = SpendPolicy::find_by_statement(stmt(
        &format!(
            "SELECT {POLICY_COLUMNS} FROM call_spend_policies WHERE environment_id = $1 FOR UPDATE"
        ),
        vec![call.environment_id.into()],
    ))
    .one(txn)
    .await?;
    let Some(p) = p else {
        return Ok(Err(SpendDenial::NotConfigured));
    };
    if !p.enabled {
        return Ok(Err(SpendDenial::Disabled));
    }
    let allowance = p.leg_allowance_minor;
    let t = totals(txn, call.environment_id, now).await?;
    if t.day + allowance > p.daily_limit_minor {
        return Ok(Err(SpendDenial::DailyLimit));
    }
    if t.month + allowance > p.monthly_limit_minor {
        return Ok(Err(SpendDenial::MonthlyLimit));
    }
    for (cap, column, subject, day_denial, month_denial) in [
        (
            &app_cap,
            "application_id",
            call.application_id,
            SpendDenial::ApplicationDailyLimit,
            SpendDenial::ApplicationMonthlyLimit,
        ),
        (
            &credential_cap,
            "credential_id",
            call.created_by_credential_id,
            SpendDenial::CredentialDailyLimit,
            SpendDenial::CredentialMonthlyLimit,
        ),
    ] {
        let Some(cap) = cap else { continue };
        if cap.telecom_spend_daily_limit_minor.is_none()
            && cap.telecom_spend_monthly_limit_minor.is_none()
        {
            continue;
        }
        let used = spent(txn, column, subject, now).await?;
        if cap
            .telecom_spend_daily_limit_minor
            .is_some_and(|l| used.day + allowance > l)
        {
            return Ok(Err(day_denial));
        }
        if cap
            .telecom_spend_monthly_limit_minor
            .is_some_and(|l| used.month + allowance > l)
        {
            return Ok(Err(month_denial));
        }
    }
    if t.open >= i64::from(p.max_concurrent_legs) {
        return Ok(Err(SpendDenial::Concurrency));
    }
    if t.minute >= i64::from(p.max_originations_per_minute) {
        return Ok(Err(SpendDenial::Velocity));
    }
    let hour = now - Duration::hours(1);
    if let Some(contact) = contact_reference {
        if recent(txn, call.environment_id, "contact_reference", contact, hour).await?
            >= i64::from(p.max_per_party_per_hour)
        {
            return Ok(Err(SpendDenial::ContactVelocity));
        }
    }
    for (limit, column, subject, denial) in [
        (
            p.max_per_principal_per_hour,
            "principal_id",
            Some(call.initiator_principal_id),
            SpendDenial::PrincipalVelocity,
        ),
        (
            p.max_per_service_account_per_hour,
            "service_account_id",
            Some(call.created_by_service_account_id),
            SpendDenial::ServiceAccountVelocity,
        ),
        (
            p.max_per_context_per_hour,
            "context_ref",
            call.communication_context_ref,
            SpendDenial::ContextVelocity,
        ),
    ] {
        if let (Some(limit), Some(subject)) = (limit, subject) {
            if recent(txn, call.environment_id, column, subject, hour).await? >= i64::from(limit) {
                return Ok(Err(denial));
            }
        }
    }
    let id = Uuid::new_v4();
    txn.execute(stmt(
        "INSERT INTO call_spend_reservations (id, tenant_id, application_id, environment_id, call_id, \
           call_leg_id, contact_reference, principal_id, service_account_id, credential_id, context_ref, \
           amount_minor, currency, max_duration_seconds, status, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, 'RESERVED', $15)",
        vec![
            id.into(),
            call.tenant_id.into(),
            call.application_id.into(),
            call.environment_id.into(),
            call.id.into(),
            call_leg_id.into(),
            contact_reference.into(),
            call.initiator_principal_id.into(),
            call.created_by_service_account_id.into(),
            call.created_by_credential_id.into(),
            call.communication_context_ref.into(),
            allowance.into(),
            p.currency.clone().into(),
            p.max_leg_seconds.into(),
            now.into(),
        ],
    ))
    .await?;
    tracing::info!(reservation_id = %id, call_id = %call.id, call_leg_id = %call_leg_id, amount_minor = allowance, "telecom spend reserved");
    Ok(Ok(Reservation {
        id,
        amount_minor: allowance,
        currency: p.currency,
        max_duration_seconds: p.max_leg_seconds,
    }))
}

/// The reservation of a leg.
pub async fn reservation_of<C: ConnectionTrait>(
    db: &C,
    call_leg_id: Uuid,
) -> Result<Option<Reservation>, DbErr> {
    #[derive(FromQueryResult)]
    struct Row {
        id: Uuid,
        amount_minor: i64,
        currency: String,
        max_duration_seconds: i32,
    }
    Ok(Row::find_by_statement(stmt(
        "SELECT id, amount_minor, currency, max_duration_seconds FROM call_spend_reservations \
         WHERE call_leg_id = $1",
        vec![call_leg_id.into()],
    ))
    .one(db)
    .await?
    .map(|r| Reservation {
        id: r.id,
        amount_minor: r.amount_minor,
        currency: r.currency,
        max_duration_seconds: r.max_duration_seconds,
    }))
}

/// Settles a leg's reservation with the usage fact (once; a repeat is a no-op).
pub async fn settle_in<C: ConnectionTrait>(
    txn: &C,
    call_leg_id: Uuid,
    cost_minor: i64,
    cost_known: bool,
    billable_seconds: i32,
    now: DateTime<Utc>,
) -> Result<bool, DbErr> {
    let result = txn
        .execute(stmt(
            "UPDATE call_spend_reservations SET status = 'SETTLED', settled_minor = $2, cost_known = $3, \
               billable_seconds = $4, settled_at = GREATEST($5, created_at) \
             WHERE call_leg_id = $1 AND status = 'RESERVED'",
            vec![
                call_leg_id.into(),
                cost_minor.max(0).into(),
                cost_known.into(),
                billable_seconds.max(0).into(),
                now.into(),
            ],
        ))
        .await?;
    Ok(result.rows_affected() == 1)
}

/// Releases a reservation whose leg never reached Telephony (nothing can be billed).
pub async fn release_in<C: ConnectionTrait>(
    txn: &C,
    call_leg_id: Uuid,
    now: DateTime<Utc>,
) -> Result<bool, DbErr> {
    let result = txn
        .execute(stmt(
            "UPDATE call_spend_reservations SET status = 'RELEASED', settled_at = GREATEST($2, created_at) \
             WHERE call_leg_id = $1 AND status = 'RESERVED'",
            vec![call_leg_id.into(), now.into()],
        ))
        .await?;
    Ok(result.rows_affected() == 1)
}

/// Upserts an environment's spend policy (operator CLI). Returns the new revision.
pub async fn store_policy<C: ConnectionTrait>(
    db: &C,
    scope: SpendScope,
    p: &SpendPolicy,
    now: DateTime<Utc>,
) -> Result<i32, DbErr> {
    let row = db
        .query_one(stmt(
            "INSERT INTO call_spend_policies (tenant_id, application_id, environment_id, currency, \
               leg_allowance_minor, max_leg_seconds, daily_limit_minor, monthly_limit_minor, max_concurrent_legs, \
               max_originations_per_minute, max_per_party_per_hour, max_per_principal_per_hour, \
               max_per_service_account_per_hour, max_per_context_per_hour, enabled, revision, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $14, $15, $16, $12, 1, $13, $13) \
             ON CONFLICT (environment_id) DO UPDATE SET currency = EXCLUDED.currency, \
               leg_allowance_minor = EXCLUDED.leg_allowance_minor, max_leg_seconds = EXCLUDED.max_leg_seconds, \
               daily_limit_minor = EXCLUDED.daily_limit_minor, monthly_limit_minor = EXCLUDED.monthly_limit_minor, \
               max_concurrent_legs = EXCLUDED.max_concurrent_legs, \
               max_originations_per_minute = EXCLUDED.max_originations_per_minute, \
               max_per_party_per_hour = EXCLUDED.max_per_party_per_hour, \
               max_per_principal_per_hour = EXCLUDED.max_per_principal_per_hour, \
               max_per_service_account_per_hour = EXCLUDED.max_per_service_account_per_hour, \
               max_per_context_per_hour = EXCLUDED.max_per_context_per_hour, enabled = EXCLUDED.enabled, \
               revision = call_spend_policies.revision + 1, \
               updated_at = GREATEST(EXCLUDED.updated_at, call_spend_policies.updated_at) \
             RETURNING revision",
            vec![
                scope.tenant_id.into(),
                scope.application_id.into(),
                scope.environment_id.into(),
                p.currency.clone().into(),
                p.leg_allowance_minor.into(),
                p.max_leg_seconds.into(),
                p.daily_limit_minor.into(),
                p.monthly_limit_minor.into(),
                p.max_concurrent_legs.into(),
                p.max_originations_per_minute.into(),
                p.max_per_party_per_hour.into(),
                p.enabled.into(),
                now.into(),
                p.max_per_principal_per_hour.into(),
                p.max_per_service_account_per_hour.into(),
                p.max_per_context_per_hour.into(),
            ],
        ))
        .await?;
    row.map(|r| r.try_get_by_index::<i32>(0))
        .transpose()?
        .ok_or_else(|| DbErr::Custom("spend policy upsert returned nothing".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_are_utc_day_and_month() {
        let now = Utc.with_ymd_and_hms(2026, 10, 8, 17, 45, 3).unwrap();
        assert_eq!(
            day_start(now),
            Utc.with_ymd_and_hms(2026, 10, 8, 0, 0, 0).unwrap()
        );
        assert_eq!(
            month_start(now),
            Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap()
        );
    }
}
