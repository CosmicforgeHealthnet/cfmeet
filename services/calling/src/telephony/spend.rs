//! Telecom spend enforcement (Part E §153–§158).
//!
//! Every paid public leg needs a durable reservation BEFORE Telephony may originate it.
//! A reservation is taken inside the transaction that records the leg, after locking the
//! environment's `call_spend_policies` row (`FOR UPDATE`): two concurrent requests are
//! serialized there, so there is no check-then-spend race. Each check counts what is
//! already reserved or settled:
//!
//! * daily and monthly limits (UTC): RESERVED allowances + SETTLED actual costs;
//! * concurrency: RESERVED reservations (open paid legs);
//! * velocity: reservations created in the last minute (environment) and in the last
//!   hour for the same protected contact.
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
    monthly_limit_minor, max_concurrent_legs, max_originations_per_minute, max_per_party_per_hour, enabled";

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

/// Reserves one leg's allowance inside `txn` (which must also record the leg). Locks the
/// environment policy row first: concurrent reservations of one environment serialize.
pub async fn reserve_in<C: ConnectionTrait>(
    txn: &C,
    scope: SpendScope,
    call_id: Uuid,
    call_leg_id: Uuid,
    contact_reference: Option<Uuid>,
    now: DateTime<Utc>,
) -> Result<Result<Reservation, SpendDenial>, DbErr> {
    let p = SpendPolicy::find_by_statement(stmt(
        &format!(
            "SELECT {POLICY_COLUMNS} FROM call_spend_policies WHERE environment_id = $1 FOR UPDATE"
        ),
        vec![scope.environment_id.into()],
    ))
    .one(txn)
    .await?;
    let Some(p) = p else {
        return Ok(Err(SpendDenial::NotConfigured));
    };
    if !p.enabled {
        return Ok(Err(SpendDenial::Disabled));
    }
    let t = totals(txn, scope.environment_id, now).await?;
    if t.day + p.leg_allowance_minor > p.daily_limit_minor {
        return Ok(Err(SpendDenial::DailyLimit));
    }
    if t.month + p.leg_allowance_minor > p.monthly_limit_minor {
        return Ok(Err(SpendDenial::MonthlyLimit));
    }
    if t.open >= i64::from(p.max_concurrent_legs) {
        return Ok(Err(SpendDenial::Concurrency));
    }
    if t.minute >= i64::from(p.max_originations_per_minute) {
        return Ok(Err(SpendDenial::Velocity));
    }
    if let Some(contact) = contact_reference {
        let row = txn
            .query_one(stmt(
                "SELECT count(*) FROM call_spend_reservations \
                 WHERE environment_id = $1 AND contact_reference = $2 AND created_at > $3",
                vec![
                    scope.environment_id.into(),
                    contact.into(),
                    (now - Duration::hours(1)).into(),
                ],
            ))
            .await?;
        let recent: i64 = row.map(|r| r.try_get_by_index(0)).transpose()?.unwrap_or(0);
        if recent >= i64::from(p.max_per_party_per_hour) {
            return Ok(Err(SpendDenial::ContactVelocity));
        }
    }
    let id = Uuid::new_v4();
    txn.execute(stmt(
        "INSERT INTO call_spend_reservations (id, tenant_id, application_id, environment_id, call_id, \
           call_leg_id, contact_reference, amount_minor, currency, max_duration_seconds, status, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'RESERVED', $11)",
        vec![
            id.into(),
            scope.tenant_id.into(),
            scope.application_id.into(),
            scope.environment_id.into(),
            call_id.into(),
            call_leg_id.into(),
            contact_reference.into(),
            p.leg_allowance_minor.into(),
            p.currency.clone().into(),
            p.max_leg_seconds.into(),
            now.into(),
        ],
    ))
    .await?;
    tracing::info!(reservation_id = %id, call_id = %call_id, call_leg_id = %call_leg_id, amount_minor = p.leg_allowance_minor, "telecom spend reserved");
    Ok(Ok(Reservation {
        id,
        amount_minor: p.leg_allowance_minor,
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
               max_originations_per_minute, max_per_party_per_hour, enabled, revision, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, 1, $13, $13) \
             ON CONFLICT (environment_id) DO UPDATE SET currency = EXCLUDED.currency, \
               leg_allowance_minor = EXCLUDED.leg_allowance_minor, max_leg_seconds = EXCLUDED.max_leg_seconds, \
               daily_limit_minor = EXCLUDED.daily_limit_minor, monthly_limit_minor = EXCLUDED.monthly_limit_minor, \
               max_concurrent_legs = EXCLUDED.max_concurrent_legs, \
               max_originations_per_minute = EXCLUDED.max_originations_per_minute, \
               max_per_party_per_hour = EXCLUDED.max_per_party_per_hour, enabled = EXCLUDED.enabled, \
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
