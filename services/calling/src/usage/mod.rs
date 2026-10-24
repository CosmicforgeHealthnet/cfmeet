//! Part G §69 / §70: the customer-facing usage ledger (`GET /v1/usage`) and limit status
//! (`GET /v1/limits`) of the authenticated application environment (ADR 0028).
//!
//! Usage is a ledger of FINAL consumption facts, never a bill: no price list, no invoice,
//! no tax. A public-network leg's cost is reported with its evidence (KNOWN / ESTIMATED /
//! UNKNOWN); an UNKNOWN cost has no amount. No provider identifier, phone number, SIP URI
//! or object key is part of any view.
//!
//! Limit status shows, per limit, every configured level of the precedence chain
//! (tenant → application → credential, plus the environment's telecom-spend policy) with
//! its ceiling, current consumption and remaining headroom; the binding level is the
//! one with the least headroom. Enforcement itself stays where it was (rate limiter,
//! Call creation, spend reservation) — this is a read model.

pub mod http;

pub mod export;
use chrono::{DateTime, Duration, DurationRound, SecondsFormat, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, FromQueryResult, Statement};
use serde::Serialize;
use uuid::Uuid;

use crate::{
    calls::{command::CallScope, service::TERMINAL_STATES_SQL},
    domain::{PrincipalId, SharedClock},
    error::CallingError,
    platform::{
        context::PlatformAuthContext,
        limits::{self, LimitDefaults, LimitPolicyResolver, LimitValues},
    },
    telephony::spend,
};

fn ts(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn db_err(e: sea_orm::DbErr) -> CallingError {
    tracing::error!(error = %e, "usage read failed");
    CallingError::Internal
}

fn stmt(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

/// Resource class of a usage type (the `resourceClass` filter).
pub const RESOURCE_CLASSES: &[(&str, &[&str])] = &[
    (
        "APP_MEDIA",
        &[
            "CALL_CONNECTED_TIME",
            "PARTICIPANT_CONNECTED_TIME",
            "SESSION_CONNECTED_TIME",
        ],
    ),
    ("PUBLIC_NETWORK", &["LEG_CONNECTED_TIME"]),
    ("STORAGE", &["VOICEMAIL_ARTIFACT", "RECORDING_ARTIFACT"]),
];

fn resource_class(usage_type: &str) -> &'static str {
    RESOURCE_CLASSES
        .iter()
        .find(|(_, types)| types.contains(&usage_type))
        .map(|(class, _)| *class)
        .unwrap_or("APP_MEDIA")
}

/// Filters of `GET /v1/usage` (already validated).
#[derive(Debug, Default, Clone)]
pub struct UsageFilter {
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    /// `None` = every type; `Some(empty)` = nothing matches.
    pub usage_types: Option<Vec<&'static str>>,
}

#[derive(Debug, FromQueryResult)]
struct UsageRow {
    id: Uuid,
    call_id: Uuid,
    participant_id: Option<Uuid>,
    session_id: Option<Uuid>,
    leg_id: Option<Uuid>,
    usage_type: String,
    quantity: i64,
    unit: String,
    usage_started_at: Option<DateTime<Utc>>,
    usage_ended_at: Option<DateTime<Utc>>,
    finalized_at: DateTime<Utc>,
    route_class: Option<String>,
    destination_country: Option<String>,
    billable_seconds: Option<i32>,
    cost_status: Option<String>,
    cost_minor: Option<i64>,
    cost_currency: Option<String>,
    size_bytes: Option<i64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageCost {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount_minor: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecordView {
    pub id: Uuid,
    pub call_id: Uuid,
    pub usage_type: String,
    pub resource_class: &'static str,
    pub quantity: i64,
    pub unit: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub participant_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leg_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route_class: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination_country: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub billable_seconds: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<UsageCost>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_ended_at: Option<String>,
    pub recorded_at: String,
}

impl From<UsageRow> for UsageRecordView {
    fn from(r: UsageRow) -> Self {
        Self {
            id: r.id,
            call_id: r.call_id,
            resource_class: resource_class(&r.usage_type),
            usage_type: r.usage_type,
            quantity: r.quantity,
            unit: r.unit,
            participant_id: r.participant_id,
            session_id: r.session_id,
            leg_id: r.leg_id,
            route_class: r.route_class,
            destination_country: r.destination_country,
            billable_seconds: r.billable_seconds,
            cost: r.cost_status.map(|status| UsageCost {
                status,
                amount_minor: r.cost_minor,
                currency: r.cost_currency,
            }),
            size_bytes: r.size_bytes,
            usage_started_at: r.usage_started_at.map(ts),
            usage_ended_at: r.usage_ended_at.map(ts),
            recorded_at: ts(r.finalized_at),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageList {
    pub data: Vec<UsageRecordView>,
    pub next_cursor: Option<String>,
}

/// One level of a limit.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LimitLevelView {
    pub scope: &'static str,
    pub ceiling: i64,
    pub current: i64,
    pub remaining: i64,
}

impl LimitLevelView {
    fn new(scope: &'static str, ceiling: i64, current: i64) -> Self {
        Self {
            scope,
            ceiling,
            current,
            remaining: (ceiling - current).max(0),
        }
    }
}

/// One limit: its window, the binding (least headroom) level and every level.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LimitView {
    pub limit: &'static str,
    pub window: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    pub scope: &'static str,
    pub ceiling: i64,
    pub current: i64,
    pub remaining: i64,
    pub levels: Vec<LimitLevelView>,
}

impl LimitView {
    fn new(
        limit: &'static str,
        window: &'static str,
        resets_at: Option<DateTime<Utc>>,
        currency: Option<String>,
        levels: Vec<LimitLevelView>,
    ) -> Self {
        // The binding level: least remaining; on a tie the broader level (listed first).
        let binding = levels
            .iter()
            .fold(None::<&LimitLevelView>, |best, l| match best {
                Some(b) if b.remaining <= l.remaining => Some(b),
                _ => Some(l),
            })
            .cloned()
            .unwrap_or(LimitLevelView::new("PLATFORM_DEFAULT", 0, 0));
        Self {
            limit,
            window,
            resets_at: resets_at.map(ts),
            currency,
            scope: binding.scope,
            ceiling: binding.ceiling,
            current: binding.current,
            remaining: binding.remaining,
            levels,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LimitsView {
    pub generated_at: String,
    pub limits: Vec<LimitView>,
}

pub struct UsageService {
    db: DatabaseConnection,
    clock: SharedClock,
    defaults: LimitDefaults,
}

impl UsageService {
    pub fn new(db: DatabaseConnection, clock: SharedClock, defaults: LimitDefaults) -> Self {
        Self {
            db,
            clock,
            defaults,
        }
    }

    /// Final usage facts of the scope, oldest first by finalization time (keyset).
    pub async fn list(
        &self,
        scope: CallScope,
        filter: &UsageFilter,
        limit: u64,
        cursor: Option<(DateTime<Utc>, Uuid)>,
    ) -> Result<UsageList, CallingError> {
        let mut sql = String::from(
            "SELECT id, call_id, participant_id, session_id, leg_id, usage_type, quantity, unit, \
               usage_started_at, usage_ended_at, finalized_at, route_class, \
               destination_country::text AS destination_country, billable_seconds, cost_status, \
               cost_minor, cost_currency::text AS cost_currency, size_bytes \
             FROM call_usage_records \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 \
               AND finalized_at IS NOT NULL",
        );
        let mut values: Vec<sea_orm::Value> = vec![
            scope.tenant_id().as_uuid().into(),
            scope.application_id().as_uuid().into(),
            scope.environment_id().as_uuid().into(),
        ];
        let mut bind = |sql: &mut String, clause: &str, v: sea_orm::Value| {
            values.push(v);
            sql.push_str(&clause.replace('?', &format!("${}", values.len())));
        };
        if let Some(from) = filter.from {
            bind(&mut sql, " AND finalized_at >= ?", from.into());
        }
        if let Some(to) = filter.to {
            bind(&mut sql, " AND finalized_at < ?", to.into());
        }
        if let Some(types) = &filter.usage_types {
            let types: Vec<String> = types.iter().map(|t| t.to_string()).collect();
            bind(&mut sql, " AND usage_type = ANY(?)", types.into());
        }
        if let Some((at, id)) = cursor {
            bind(&mut sql, " AND (finalized_at, id) > (?", at.into());
            bind(&mut sql, ", ?)", id.into());
        }
        sql.push_str(&format!(
            " ORDER BY finalized_at, id LIMIT {}",
            limit.saturating_add(1)
        ));
        let mut rows = UsageRow::find_by_statement(stmt(&sql, values))
            .all(&self.db)
            .await
            .map_err(db_err)?;
        let more = rows.len() as u64 > limit;
        rows.truncate(limit as usize);
        let next_cursor = more
            .then(|| {
                rows.last()
                    .map(|r| crate::enterprise::cursor::encode(r.finalized_at, r.id))
            })
            .flatten();
        Ok(UsageList {
            data: rows.into_iter().map(UsageRecordView::from).collect(),
            next_cursor,
        })
    }

    async fn count(&self, sql: &str, values: Vec<sea_orm::Value>) -> Result<i64, CallingError> {
        Ok(self
            .db
            .query_one(stmt(sql, values))
            .await
            .map_err(db_err)?
            .map(|r| r.try_get_by_index::<i64>(0))
            .transpose()
            .map_err(db_err)?
            .unwrap_or(0))
    }

    /// The authenticated credential's limit status: every configured level of the
    /// API-client chain (TENANT → APPLICATION → SERVICE_ACCOUNT → CREDENTIAL; the
    /// environment's spend policy for spend) and, when `principal` is given — a principal
    /// ALREADY resolved in the caller's own scope — that principal's own levels.
    pub async fn limits(
        &self,
        ctx: &PlatformAuthContext,
        principal: Option<PrincipalId>,
    ) -> Result<LimitsView, CallingError> {
        let now = self.clock.now();
        let levels = limits::load_levels(
            &self.db,
            ctx.tenant_id(),
            ctx.application_id(),
            ctx.service_account_id(),
            ctx.credential_id(),
        )
        .await
        .map_err(db_err)?;
        let principal_values = match principal {
            Some(p) => Some((
                p.as_uuid(),
                limits::principal_level(&self.db, p).await.map_err(db_err)?,
            )),
            None => None,
        };
        let effective = LimitPolicyResolver::new(self.db.clone(), self.defaults)
            .effective_for(ctx)
            .await
            .map_err(|_| CallingError::Internal)?;
        // (scope code, configured values, rate subject, calls column, spend column, id)
        let chain: Vec<(&'static str, LimitValues, &'static str, Uuid)> = vec![
            (
                "TENANT",
                levels.tenant,
                "tenant_id",
                ctx.tenant_id().as_uuid(),
            ),
            (
                "APPLICATION",
                levels.application,
                "application_id",
                ctx.application_id().as_uuid(),
            ),
            (
                "SERVICE_ACCOUNT",
                levels.service_account,
                "service_account_id",
                ctx.service_account_id().as_uuid(),
            ),
        ];

        // Request rate (fixed one-minute windows).
        let window = now.duration_trunc(Duration::minutes(1)).unwrap_or(now);
        let rate_count = |kind: &'static str, subject: Uuid| {
            self.count(
                "SELECT COALESCE(MAX(request_count), 0)::bigint FROM platform_request_rate_windows \
                 WHERE subject_kind = $1 AND subject_id = $2 AND window_start = $3",
                vec![kind.into(), subject.into(), window.into()],
            )
        };
        let mut rate = Vec::new();
        for (scope, values, _, id) in &chain {
            if let Some(l) = values.requests_per_minute {
                rate.push(LimitLevelView::new(
                    scope,
                    l.into(),
                    rate_count(scope, *id).await?,
                ));
            }
        }
        rate.push(LimitLevelView::new(
            "CREDENTIAL",
            effective.requests_per_minute.value,
            rate_count("CREDENTIAL", ctx.credential_id().as_uuid()).await?,
        ));

        // Concurrent Calls (non-terminal Calls).
        let active = |column: &'static str, subject: Uuid| async move {
            let sql = format!(
                "SELECT count(*) FROM calls WHERE {column} = $1 \
                 AND current_state NOT IN {TERMINAL_STATES_SQL}"
            );
            self.count(&sql, vec![subject.into()]).await
        };
        let calls_column = |scope: &str| match scope {
            "TENANT" => "tenant_id",
            "APPLICATION" => "application_id",
            _ => "created_by_service_account_id",
        };
        let mut concurrent = Vec::new();
        for (scope, values, _, id) in &chain {
            if let Some(l) = values.max_concurrent_communications {
                concurrent.push(LimitLevelView::new(
                    scope,
                    l.into(),
                    active(calls_column(scope), *id).await?,
                ));
            }
        }
        concurrent.push(LimitLevelView::new(
            "CREDENTIAL",
            effective.max_concurrent_communications.value,
            active("created_by_credential_id", ctx.credential_id().as_uuid()).await?,
        ));

        // Telecom spend (reserved + settled, minor units), per UTC day and month.
        let env = ctx.environment_id().as_uuid();
        let policy = spend::policy(&self.db, env).await.map_err(db_err)?;
        let totals = spend::totals(&self.db, env, now).await.map_err(db_err)?;
        let live = ctx.live_telecom_allowed();
        let (env_day, env_month) = match (&policy, live) {
            (Some(p), true) if p.enabled => (p.daily_limit_minor, p.monthly_limit_minor),
            _ => (0, 0),
        };
        let currency = policy
            .as_ref()
            .map(|p| p.currency.clone())
            .unwrap_or_else(|| effective.telecom_spend_currency.clone());
        let mut day = vec![LimitLevelView::new("ENVIRONMENT", env_day, totals.day)];
        let mut month = vec![LimitLevelView::new("ENVIRONMENT", env_month, totals.month)];
        let mut spend_levels = chain.clone();
        spend_levels.push((
            "CREDENTIAL",
            levels.credential,
            "credential_id",
            ctx.credential_id().as_uuid(),
        ));
        if let Some((id, values)) = principal_values {
            spend_levels.push(("PRINCIPAL", values, "principal_id", id));
        }
        for (scope, values, column, subject) in spend_levels {
            if values.telecom_spend_daily_limit_minor.is_none()
                && values.telecom_spend_monthly_limit_minor.is_none()
            {
                continue;
            }
            let used = spend::spent(&self.db, column, subject, now)
                .await
                .map_err(db_err)?;
            if let Some(l) = values.telecom_spend_daily_limit_minor {
                day.push(LimitLevelView::new(scope, l, used.day));
            }
            if let Some(l) = values.telecom_spend_monthly_limit_minor {
                month.push(LimitLevelView::new(scope, l, used.month));
            }
        }

        // The principal axis (rate of its principal-scoped operations, its Calls).
        if let Some((id, values)) = principal_values {
            if let Some(l) = values.requests_per_minute {
                rate.push(LimitLevelView::new(
                    "PRINCIPAL",
                    l.into(),
                    rate_count("PRINCIPAL", id).await?,
                ));
            }
            if let Some(l) = values.max_concurrent_communications {
                let sql = format!(
                    "SELECT count(*) FROM calls c WHERE c.current_state NOT IN {TERMINAL_STATES_SQL} \
                       AND (c.initiator_principal_id = $1 OR EXISTS (SELECT 1 FROM call_participants p \
                            WHERE p.call_id = c.id AND p.principal_id = $1))"
                );
                concurrent.push(LimitLevelView::new(
                    "PRINCIPAL",
                    l.into(),
                    self.count(&sql, vec![id.into()]).await?,
                ));
            }
        }
        let next_day = spend::day_start(now) + Duration::days(1);
        let next_month = spend::month_start(spend::month_start(now) + Duration::days(32));
        Ok(LimitsView {
            generated_at: ts(now),
            limits: vec![
                LimitView::new(
                    "REQUEST_RATE",
                    "MINUTE",
                    Some(window + Duration::minutes(1)),
                    None,
                    rate,
                ),
                LimitView::new("CONCURRENT_CALLS", "NONE", None, None, concurrent),
                LimitView::new(
                    "TELECOM_SPEND_DAILY",
                    "DAY",
                    Some(next_day),
                    Some(currency.clone()),
                    day,
                ),
                LimitView::new(
                    "TELECOM_SPEND_MONTHLY",
                    "MONTH",
                    Some(next_month),
                    Some(currency),
                    month,
                ),
            ],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_level_has_the_least_headroom() {
        let v = LimitView::new(
            "CONCURRENT_CALLS",
            "NONE",
            None,
            None,
            vec![
                LimitLevelView::new("TENANT", 100, 90),
                LimitLevelView::new("APPLICATION", 20, 5),
                LimitLevelView::new("CREDENTIAL", 10, 8),
            ],
        );
        assert_eq!((v.scope, v.remaining), ("CREDENTIAL", 2));
        let over = LimitLevelView::new("TENANT", 5, 9);
        assert_eq!(over.remaining, 0, "never negative");
    }

    #[test]
    fn every_usage_type_has_one_resource_class() {
        for t in [
            "CALL_CONNECTED_TIME",
            "PARTICIPANT_CONNECTED_TIME",
            "SESSION_CONNECTED_TIME",
            "LEG_CONNECTED_TIME",
            "VOICEMAIL_ARTIFACT",
            "RECORDING_ARTIFACT",
        ] {
            let n = RESOURCE_CLASSES
                .iter()
                .filter(|(_, types)| types.contains(&t))
                .count();
            assert_eq!(n, 1, "{t}");
        }
    }
}
