//! Routing fence protocol v1 (Notification Service blueprint Document 4 §9): one system
//! per message, decided, recorded and enforced inside the producer's own transaction.
//!
//! Producers (roadmap N7 / N11): the STANDARD categories `MEETING_INVITATION`,
//! `MEETING_NOTIFICATION` and `ORGANIZATION_INVITATION` are routed by
//! [`crate::services::notifications`] through [`current_mode`] and [`route_message_capable`]
//! (publications: [`crate::services::notification_publications`]). The CRITICAL categories
//! have no producer yet: they keep the old path, and their guard triggers stay inert while
//! they are `LEGACY_ONLY`.
//!
//! * [`route_message`] — §9.3: sticky decision per idempotency key (`SELECT … FOR UPDATE`),
//!   mode + canary + CRITICAL `FORCED` fallback, `reason = ROUTED`. The caller writes the
//!   old-path row (route OLD) or the outbox row (route NEW) on the SAME transaction.
//!   [`route_message_capable`] also turns a `NEW` route the event cannot take (audience
//!   larger than one envelope, no complete catalogue envelope) into `OLD` with reason
//!   `RECIPIENT_LIMIT` / `UNPUBLISHABLE`.
//! * [`current_mode`] — the category's mode, read before anything is routed (`None`: no
//!   fence history, exactly as `LEGACY_ONLY`); never fails inside a transaction for a
//!   missing table.
//! * [`reroute_unacked`] — §9.5: NEW → OLD (`REROUTED_UNACKED`) for a decision the service
//!   never acknowledged; the CRITICAL outbox row becomes `SUPERSEDED`. The caller must
//!   first have confirmed with the I1 `dryRun` probe that the service answers `UNKNOWN`,
//!   and writes the old-path row on the same transaction.
//! * [`grant_critical_lease`] (X6), [`complete_critical_send`] (X7) and
//!   [`reroute_critical`] — the CRITICAL lease rules of §9.8.
//! * [`fence_protocol`] — `Some(1)` once the `notification_fence_migrations` history is
//!   fully applied (reported as `routingProtocol` in `/health`).
//!
//! The pure parts ([`decide_route`], [`canary`], [`critical_reroute`]) follow
//! `packages/notification-client/src/routing.rs` exactly (same SHA-256 canary), so a
//! producer and the shared client always agree; the unit tests pin shared vectors.
//!
//! Nothing here logs keys, codes or addresses.

use std::time::Duration;

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbBackend, DbErr, QueryResult, Statement};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// The routing protocol version this binary implements (`routingProtocol`, envelope
/// `route.protocol`).
pub const ROUTING_PROTOCOL: i16 = 1;

/// SQLSTATE raised by the legacy guard triggers (`ROUTING_DECISION_REQUIRED`).
pub const ROUTING_DECISION_REQUIRED_SQLSTATE: &str = "CFN01";
/// SQLSTATE raised by the decision-row guard trigger (F3).
pub const ROUTING_DECISION_IMMUTABLE_SQLSTATE: &str = "CFN02";

/// The X6 lease length (the service's provider call times out after 20 s, inside it).
pub const CRITICAL_LEASE: Duration = Duration::from_secs(30);

/// Maximum idempotency-key length (`notification_route_decisions.idempotency_key`).
pub const MAX_KEY_LEN: usize = 160;

/// The Meeting-owned routing categories (the Notification Service `notif_routing` seed,
/// restricted to event types produced by `cfmeet.meeting-service`). Each has one
/// `notification_routing_state` row, seeded `LEGACY_ONLY` by the fence migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MeetingCategory {
    AuthVerification,
    AuthPasswordReset,
    SecurityDeviceStepUp,
    SecurityPasswordChanged,
    SecurityDeviceLinked,
    MeetingInvitation,
    MeetingNotification,
    OrganizationInvitation,
}

impl MeetingCategory {
    pub const ALL: [MeetingCategory; 8] = [
        MeetingCategory::AuthVerification,
        MeetingCategory::AuthPasswordReset,
        MeetingCategory::SecurityDeviceStepUp,
        MeetingCategory::SecurityPasswordChanged,
        MeetingCategory::SecurityDeviceLinked,
        MeetingCategory::MeetingInvitation,
        MeetingCategory::MeetingNotification,
        MeetingCategory::OrganizationInvitation,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            MeetingCategory::AuthVerification => "AUTH_VERIFICATION",
            MeetingCategory::AuthPasswordReset => "AUTH_PASSWORD_RESET",
            MeetingCategory::SecurityDeviceStepUp => "SECURITY_DEVICE_STEP_UP",
            MeetingCategory::SecurityPasswordChanged => "SECURITY_PASSWORD_CHANGED",
            MeetingCategory::SecurityDeviceLinked => "SECURITY_DEVICE_LINKED",
            MeetingCategory::MeetingInvitation => "MEETING_INVITATION",
            MeetingCategory::MeetingNotification => "MEETING_NOTIFICATION",
            MeetingCategory::OrganizationInvitation => "ORGANIZATION_INVITATION",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|c| c.as_str() == value)
    }

    /// CRITICAL lane (codes and security notices: catalogue priority `CRITICAL`).
    pub fn is_critical(self) -> bool {
        matches!(
            self,
            MeetingCategory::AuthVerification
                | MeetingCategory::AuthPasswordReset
                | MeetingCategory::SecurityDeviceStepUp
                | MeetingCategory::SecurityPasswordChanged
                | MeetingCategory::SecurityDeviceLinked
        )
    }
}

/// `notification_routing_state.mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutingMode {
    LegacyOnly,
    Shadow,
    FencedOld,
    FencedNew,
}

impl RoutingMode {
    pub fn as_str(self) -> &'static str {
        match self {
            RoutingMode::LegacyOnly => "LEGACY_ONLY",
            RoutingMode::Shadow => "SHADOW",
            RoutingMode::FencedOld => "FENCED_OLD",
            RoutingMode::FencedNew => "FENCED_NEW",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        [
            RoutingMode::LegacyOnly,
            RoutingMode::Shadow,
            RoutingMode::FencedOld,
            RoutingMode::FencedNew,
        ]
        .into_iter()
        .find(|m| m.as_str() == value)
    }
}

/// `notification_routing_state.critical_fallback` (§10.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CriticalFallback {
    Auto,
    Forced,
    Off,
}

impl CriticalFallback {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "AUTO" => Some(CriticalFallback::Auto),
            "FORCED" => Some(CriticalFallback::Forced),
            "OFF" => Some(CriticalFallback::Off),
            _ => None,
        }
    }
}

/// One `notification_routing_state` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingState {
    pub mode: RoutingMode,
    pub canary_percent: u8,
    pub critical_fallback: CriticalFallback,
    pub routing_version: i32,
}

impl RoutingState {
    /// The inert state the fence migration installs (also assumed for a category without a
    /// row, matching the guard trigger).
    pub fn legacy_only() -> Self {
        Self {
            mode: RoutingMode::LegacyOnly,
            canary_percent: 0,
            critical_fallback: CriticalFallback::Auto,
            routing_version: 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Old,
    New,
}

impl Route {
    pub fn as_str(self) -> &'static str {
        match self {
            Route::Old => "OLD",
            Route::New => "NEW",
        }
    }

    fn parse(value: &str) -> Result<Self, DbErr> {
        match value {
            "OLD" => Ok(Route::Old),
            "NEW" => Ok(Route::New),
            _ => Err(DbErr::Custom("unknown routing route".into())),
        }
    }
}

/// `notification_route_decisions.reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionReason {
    Routed,
    ReroutedUnacked,
    ReroutedCancelled,
    ReroutedCriticalLease,
    AutoFailover,
    ReroutedPossibleDuplicate,
    /// Decided `OLD` although the canary picked `NEW`: the audience does not fit one
    /// Notification Service envelope (fence history migration 2).
    RecipientLimit,
    /// Decided `OLD` although the canary picked `NEW`: the notification has no complete
    /// catalogue envelope (fence history migration 2).
    Unpublishable,
}

impl DecisionReason {
    pub fn as_str(self) -> &'static str {
        match self {
            DecisionReason::RecipientLimit => "RECIPIENT_LIMIT",
            DecisionReason::Unpublishable => "UNPUBLISHABLE",
            DecisionReason::Routed => "ROUTED",
            DecisionReason::ReroutedUnacked => "REROUTED_UNACKED",
            DecisionReason::ReroutedCancelled => "REROUTED_CANCELLED",
            DecisionReason::ReroutedCriticalLease => "REROUTED_CRITICAL_LEASE",
            DecisionReason::AutoFailover => "AUTO_FAILOVER",
            DecisionReason::ReroutedPossibleDuplicate => "REROUTED_POSSIBLE_DUPLICATE",
        }
    }
}

// ---------------------------------------------------------------------------
// Pure rules
// ---------------------------------------------------------------------------

/// The canary is a pure function of the key: `SHA-256(key)`, first 8 bytes as a big-endian
/// `u64`, `mod 100 < percent` (identical to `cf_notification_client::routing::canary`).
pub fn canary(key: &str, percent: u8) -> bool {
    if percent >= 100 {
        return true;
    }
    canary_bucket(key) < u64::from(percent)
}

/// The key's canary bucket (0–99).
pub fn canary_bucket(key: &str) -> u64 {
    let digest = Sha256::digest(key.as_bytes());
    let mut first = [0u8; 8];
    first.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(first) % 100
}

/// §9.3 for a key with no decision yet: `(route, shadow)`.
pub fn decide_route(key: &str, state: &RoutingState, critical: bool) -> (Route, bool) {
    let mut route = match state.mode {
        RoutingMode::LegacyOnly | RoutingMode::Shadow | RoutingMode::FencedOld => Route::Old,
        RoutingMode::FencedNew if canary(key, state.canary_percent) => Route::New,
        RoutingMode::FencedNew => Route::Old,
    };
    if critical && state.critical_fallback == CriticalFallback::Forced {
        route = Route::Old;
    }
    (
        route,
        state.mode == RoutingMode::Shadow && route == Route::Old,
    )
}

/// The CRITICAL decision as seen in the row-locked transaction (§9.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CriticalDecisionState {
    pub route: Route,
    pub lease_count: i32,
    /// `send_lease_until > now` (Meeting DB clock).
    pub lease_live: bool,
    pub completed: bool,
    pub code_expired: bool,
}

/// Verdict of a CRITICAL reroute attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RerouteVerdict {
    Allowed(DecisionReason),
    WaitForLease,
    Refused,
}

/// §9.8 / §10.4 (identical to `cf_notification_client::routing::critical_reroute`, plus
/// `critical_fallback = OFF` disabling the automatic failover).
pub fn critical_reroute(
    state: &CriticalDecisionState,
    automatic: bool,
    fallback: CriticalFallback,
) -> RerouteVerdict {
    if state.route == Route::Old || state.completed || state.code_expired {
        return RerouteVerdict::Refused;
    }
    if state.lease_live {
        return RerouteVerdict::WaitForLease;
    }
    if automatic && fallback == CriticalFallback::Off {
        return RerouteVerdict::Refused;
    }
    if state.lease_count == 0 {
        // The service never had permission to send: no duplicate is possible.
        return RerouteVerdict::Allowed(if automatic {
            DecisionReason::AutoFailover
        } else {
            DecisionReason::ReroutedCriticalLease
        });
    }
    // Leased at least once, lease expired, not completed: manual break-glass only.
    if fallback == CriticalFallback::Forced && !automatic {
        RerouteVerdict::Allowed(DecisionReason::ReroutedPossibleDuplicate)
    } else {
        RerouteVerdict::Refused
    }
}

// ---------------------------------------------------------------------------
// Database operations (all on the caller's transaction)
// ---------------------------------------------------------------------------

fn stmt(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

fn validate_key(key: &str) -> Result<(), DbErr> {
    if key.is_empty() || key.len() > MAX_KEY_LEN || key.chars().any(char::is_control) {
        return Err(DbErr::Custom("invalid routing idempotency key".into()));
    }
    Ok(())
}

/// The routing state of `category` (the inert `LEGACY_ONLY` state when it has no row).
pub async fn routing_state<C: ConnectionTrait>(
    conn: &C,
    category: MeetingCategory,
) -> Result<RoutingState, DbErr> {
    let row = conn
        .query_one(stmt(
            "SELECT mode, canary_percent, critical_fallback, routing_version
               FROM notification_routing_state WHERE category = $1",
            vec![category.as_str().into()],
        ))
        .await?;
    let Some(row) = row else {
        return Ok(RoutingState::legacy_only());
    };
    let mode: String = row.try_get("", "mode")?;
    let fallback: String = row.try_get("", "critical_fallback")?;
    let canary_percent: i16 = row.try_get("", "canary_percent")?;
    Ok(RoutingState {
        mode: RoutingMode::parse(&mode)
            .ok_or_else(|| DbErr::Custom("unknown routing mode".into()))?,
        canary_percent: u8::try_from(canary_percent.clamp(0, 100)).unwrap_or(0),
        critical_fallback: CriticalFallback::parse(&fallback)
            .ok_or_else(|| DbErr::Custom("unknown critical fallback".into()))?,
        routing_version: row.try_get("", "routing_version")?,
    })
}

/// The outcome of [`route_message`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoutedMessage {
    pub decision_id: Uuid,
    pub route: Route,
    pub routing_version: i32,
    pub decision_version: i32,
    /// `true`: an earlier decision was followed. The caller writes nothing new (neither an
    /// old-path row nor an outbox row): the original message already exists.
    pub sticky: bool,
    /// SHADOW mode (route OLD): also write a shadow event the service must never send.
    pub shadow: bool,
}

const DECISION_COLUMNS: &str = "decision_id, route, routing_version, decision_version, category,
     lease_count, ns_acked_at IS NOT NULL AS acked, ns_completed_at IS NOT NULL AS completed,
     COALESCE(send_lease_until > clock_timestamp(), false) AS lease_live";

#[derive(Debug, Clone)]
struct LockedDecision {
    decision_id: Uuid,
    route: Route,
    routing_version: i32,
    decision_version: i32,
    category: String,
    lease_count: i32,
    acked: bool,
    completed: bool,
    lease_live: bool,
}

impl LockedDecision {
    fn from_row(row: &QueryResult) -> Result<Self, DbErr> {
        let route: String = row.try_get("", "route")?;
        Ok(Self {
            decision_id: row.try_get("", "decision_id")?,
            route: Route::parse(&route)?,
            routing_version: row.try_get("", "routing_version")?,
            decision_version: row.try_get("", "decision_version")?,
            category: row.try_get("", "category")?,
            lease_count: row.try_get("", "lease_count")?,
            acked: row.try_get("", "acked")?,
            completed: row.try_get("", "completed")?,
            lease_live: row.try_get("", "lease_live")?,
        })
    }

    fn sticky(&self) -> RoutedMessage {
        RoutedMessage {
            decision_id: self.decision_id,
            route: self.route,
            routing_version: self.routing_version,
            decision_version: self.decision_version,
            sticky: true,
            shadow: false,
        }
    }
}

async fn lock_decision<C: ConnectionTrait>(
    conn: &C,
    key: &str,
) -> Result<Option<LockedDecision>, DbErr> {
    conn.query_one(stmt(
        &format!(
            "SELECT {DECISION_COLUMNS} FROM notification_route_decisions
              WHERE idempotency_key = $1 FOR UPDATE"
        ),
        vec![key.into()],
    ))
    .await?
    .map(|row| LockedDecision::from_row(&row))
    .transpose()
}

/// §9.3: decide (or follow) the route of the message keyed `key`, inside the caller's
/// transaction. `expires_at` is when the decision may be purged (the message's useful
/// life; the purge keeps it 30 more days).
pub async fn route_message<C: ConnectionTrait>(
    conn: &C,
    key: &str,
    category: MeetingCategory,
    expires_at: DateTime<Utc>,
) -> Result<RoutedMessage, DbErr> {
    route_message_capable(conn, key, category, expires_at, Ok(())).await
}

/// [`route_message`] for a message whose Notification Service event may not exist:
/// `new_capable = Err(reason)` (`RecipientLimit` or `Unpublishable`) turns a `NEW` route
/// into `OLD` recorded with that reason, so the legacy path keeps delivering it whatever
/// the canary says (never dropped). Needs fence history migration 2 for those reasons.
pub async fn route_message_capable<C: ConnectionTrait>(
    conn: &C,
    key: &str,
    category: MeetingCategory,
    expires_at: DateTime<Utc>,
    new_capable: Result<(), DecisionReason>,
) -> Result<RoutedMessage, DbErr> {
    validate_key(key)?;
    if let Some(existing) = lock_decision(conn, key).await? {
        return Ok(existing.sticky());
    }
    let state = routing_state(conn, category).await?;
    let (mut route, mut shadow) = decide_route(key, &state, category.is_critical());
    let mut reason = DecisionReason::Routed;
    if route == Route::New {
        if let Err(r) = new_capable {
            if !matches!(
                r,
                DecisionReason::RecipientLimit | DecisionReason::Unpublishable
            ) {
                return Err(DbErr::Custom("invalid capability reason".into()));
            }
            route = Route::Old;
            reason = r;
            shadow = false;
        }
    }
    let decision_id = Uuid::new_v4();
    let inserted = conn
        .query_one(stmt(
            "INSERT INTO notification_route_decisions
                 (idempotency_key, decision_id, category, route, routing_version, reason, expires_at)
             VALUES ($1, $2, $3, $4, $5, $7, $6)
             ON CONFLICT (idempotency_key) DO NOTHING
             RETURNING decision_id",
            vec![
                key.into(),
                decision_id.into(),
                category.as_str().into(),
                route.as_str().into(),
                state.routing_version.into(),
                expires_at.into(),
                reason.as_str().into(),
            ],
        ))
        .await?;
    if inserted.is_none() {
        // A concurrent producer decided first (and committed): follow its decision.
        return lock_decision(conn, key)
            .await?
            .map(|d| d.sticky())
            .ok_or_else(|| DbErr::Custom("routing decision conflict without a row".into()));
    }
    Ok(RoutedMessage {
        decision_id,
        route,
        routing_version: state.routing_version,
        decision_version: 1,
        sticky: false,
        shadow,
    })
}

async fn reroute_to_old<C: ConnectionTrait>(
    conn: &C,
    key: &str,
    decision_id: Uuid,
    reason: DecisionReason,
) -> Result<i32, DbErr> {
    let row = conn
        .query_one(stmt(
            "UPDATE notification_route_decisions
                SET route = 'OLD', decision_version = decision_version + 1, reason = $2
              WHERE idempotency_key = $1
              RETURNING decision_version",
            vec![key.into(), reason.as_str().into()],
        ))
        .await?
        .ok_or_else(|| DbErr::Custom("routing decision vanished during reroute".into()))?;
    conn.execute(stmt(
        "UPDATE account_notification_outbox SET state = 'SUPERSEDED'
          WHERE decision_id = $1 AND state IN ('PENDING', 'ACKED')",
        vec![decision_id.into()],
    ))
    .await?;
    // A STANDARD publication still waiting for I1 is never posted after the reroute.
    if relation_exists(conn, "meeting_notification_publications").await? {
        conn.execute(stmt(
            "UPDATE meeting_notification_publications
                SET state = 'SUPERSEDED', last_error = 'SUPERSEDED', completed_at = now(),
                    envelope = envelope - 'sealed'
              WHERE decision_id = $1 AND state = 'PENDING'",
            vec![decision_id.into()],
        ))
        .await?;
    }
    row.try_get("", "decision_version")
}

/// Whether `public.<table>` exists (a read that cannot fail for a missing table, so it is
/// safe inside a transaction).
async fn relation_exists<C: ConnectionTrait>(conn: &C, table: &str) -> Result<bool, DbErr> {
    conn.query_one(stmt(
        "SELECT to_regclass('public.' || $1) IS NOT NULL AS present",
        vec![table.into()],
    ))
    .await?
    .map(|r| r.try_get::<bool>("", "present"))
    .transpose()
    .map(|p| p.unwrap_or(false))
}

/// The routing mode of `category`, read before a producer routes anything. `None` when
/// the fence history is not applied (no `notification_routing_state`) or the category has
/// no row: the pre-fence behaviour, exactly as `LEGACY_ONLY`. Safe inside the caller's
/// transaction (a missing table is detected without a failing statement).
pub async fn current_mode<C: ConnectionTrait>(
    conn: &C,
    category: MeetingCategory,
) -> Result<Option<RoutingMode>, DbErr> {
    if !relation_exists(conn, "notification_routing_state").await? {
        return Ok(None);
    }
    let Some(row) = conn
        .query_one(stmt(
            "SELECT mode FROM notification_routing_state WHERE category = $1",
            vec![category.as_str().into()],
        ))
        .await?
    else {
        return Ok(None);
    };
    let mode: String = row.try_get("", "mode")?;
    RoutingMode::parse(&mode)
        .map(Some)
        .ok_or_else(|| DbErr::Custom("unknown routing mode".into()))
}

/// Outcome of [`reroute_unacked`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnackedReroute {
    /// Decision now OLD (`REROUTED_UNACKED`); the caller writes the old-path row now.
    Rerouted {
        decision_id: Uuid,
        decision_version: i32,
    },
    NotFound,
    /// Already OLD: the old path owns it.
    AlreadyOld,
    /// The service acknowledged it (or leased / completed it): only §9.8 may move it.
    Acknowledged,
}

/// §9.5: unilateral reroute of a NEW decision the service never acknowledged. Precondition
/// (the caller's job): the I1 `dryRun` probe for the event answered `UNKNOWN`.
pub async fn reroute_unacked<C: ConnectionTrait>(
    conn: &C,
    key: &str,
) -> Result<UnackedReroute, DbErr> {
    validate_key(key)?;
    let Some(d) = lock_decision(conn, key).await? else {
        return Ok(UnackedReroute::NotFound);
    };
    if d.route == Route::Old {
        return Ok(UnackedReroute::AlreadyOld);
    }
    if d.acked || d.completed || d.lease_count > 0 {
        return Ok(UnackedReroute::Acknowledged);
    }
    let outbox_acked = conn
        .query_one(stmt(
            "SELECT state FROM account_notification_outbox WHERE decision_id = $1 FOR UPDATE",
            vec![d.decision_id.into()],
        ))
        .await?
        .map(|r| r.try_get::<String>("", "state"))
        .transpose()?
        .is_some_and(|s| s == "ACKED");
    if outbox_acked {
        return Ok(UnackedReroute::Acknowledged);
    }
    let decision_version =
        reroute_to_old(conn, key, d.decision_id, DecisionReason::ReroutedUnacked).await?;
    Ok(UnackedReroute::Rerouted {
        decision_id: d.decision_id,
        decision_version,
    })
}

/// X6 answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseGrant {
    Granted {
        lease_count: i32,
        until: DateTime<Utc>,
    },
    /// The decision is OLD (rerouted): the service must not send.
    Revoked,
    /// Already completed (terminal).
    Completed,
    /// Another lease is live.
    Busy,
    /// Not a CRITICAL decision.
    NotCritical,
    NotFound,
}

/// X6: grant a send lease on a CRITICAL NEW decision (row-locked; Meeting DB clock).
pub async fn grant_critical_lease<C: ConnectionTrait>(
    conn: &C,
    key: &str,
    lease: Duration,
) -> Result<LeaseGrant, DbErr> {
    validate_key(key)?;
    let Some(d) = lock_decision(conn, key).await? else {
        return Ok(LeaseGrant::NotFound);
    };
    if !MeetingCategory::parse(&d.category).is_some_and(MeetingCategory::is_critical) {
        return Ok(LeaseGrant::NotCritical);
    }
    if d.route == Route::Old {
        return Ok(LeaseGrant::Revoked);
    }
    if d.completed {
        return Ok(LeaseGrant::Completed);
    }
    if d.lease_live {
        return Ok(LeaseGrant::Busy);
    }
    let millis = i64::try_from(lease.as_millis()).unwrap_or(i64::MAX).max(1);
    let row = conn
        .query_one(stmt(
            "UPDATE notification_route_decisions
                SET send_lease_until = clock_timestamp() + $2 * interval '1 millisecond',
                    lease_count = lease_count + 1
              WHERE idempotency_key = $1
              RETURNING lease_count, send_lease_until",
            vec![key.into(), millis.into()],
        ))
        .await?
        .ok_or_else(|| DbErr::Custom("routing decision vanished during lease".into()))?;
    Ok(LeaseGrant::Granted {
        lease_count: row.try_get("", "lease_count")?,
        until: row.try_get("", "send_lease_until")?,
    })
}

/// X7 answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completion {
    Completed,
    AlreadyCompleted,
    /// The decision is OLD: the completion is refused (recorded by the caller).
    Revoked,
    /// No lease was ever granted: a send cannot have happened under the protocol.
    NotLeased,
    NotFound,
}

/// X7: the service reports the provider accepted the CRITICAL send (terminal).
pub async fn complete_critical_send<C: ConnectionTrait>(
    conn: &C,
    key: &str,
) -> Result<Completion, DbErr> {
    validate_key(key)?;
    let Some(d) = lock_decision(conn, key).await? else {
        return Ok(Completion::NotFound);
    };
    if d.completed {
        return Ok(Completion::AlreadyCompleted);
    }
    if d.route == Route::Old {
        return Ok(Completion::Revoked);
    }
    if d.lease_count == 0 {
        return Ok(Completion::NotLeased);
    }
    conn.execute(stmt(
        "UPDATE notification_route_decisions SET ns_completed_at = clock_timestamp()
          WHERE idempotency_key = $1",
        vec![key.into()],
    ))
    .await?;
    Ok(Completion::Completed)
}

/// Outcome of [`reroute_critical`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CriticalReroute {
    /// Decision now OLD with this reason; the caller writes the old-ledger row now (the
    /// same code, from `fallback_sealed`).
    Rerouted {
        reason: DecisionReason,
        decision_id: Uuid,
    },
    WaitForLease,
    Refused,
    NotCritical,
    NotFound,
}

/// §9.8 lease-fenced reroute of a CRITICAL decision. `automatic` is the relay's AUTO
/// failover; otherwise it is an operator reroute (break-glass needs `FORCED`).
pub async fn reroute_critical<C: ConnectionTrait>(
    conn: &C,
    key: &str,
    automatic: bool,
) -> Result<CriticalReroute, DbErr> {
    validate_key(key)?;
    let Some(d) = lock_decision(conn, key).await? else {
        return Ok(CriticalReroute::NotFound);
    };
    let Some(category) = MeetingCategory::parse(&d.category).filter(|c| c.is_critical()) else {
        return Ok(CriticalReroute::NotCritical);
    };
    let fallback = routing_state(conn, category).await?.critical_fallback;
    let code_expired = conn
        .query_one(stmt(
            "SELECT expires_at <= clock_timestamp() AS expired
               FROM account_notification_outbox WHERE decision_id = $1 FOR UPDATE",
            vec![d.decision_id.into()],
        ))
        .await?
        .map(|r| r.try_get::<bool>("", "expired"))
        .transpose()?
        .unwrap_or(false);
    let state = CriticalDecisionState {
        route: d.route,
        lease_count: d.lease_count,
        lease_live: d.lease_live,
        completed: d.completed,
        code_expired,
    };
    match critical_reroute(&state, automatic, fallback) {
        RerouteVerdict::Allowed(reason) => {
            reroute_to_old(conn, key, d.decision_id, reason).await?;
            Ok(CriticalReroute::Rerouted {
                reason,
                decision_id: d.decision_id,
            })
        }
        RerouteVerdict::WaitForLease => Ok(CriticalReroute::WaitForLease),
        RerouteVerdict::Refused => Ok(CriticalReroute::Refused),
    }
}

/// `Some(1)` when the notification-fence migration history is fully applied (and carries
/// no version unknown to this binary); `None` otherwise or when it cannot be read.
pub async fn fence_protocol<C: ConnectionTrait>(conn: &C) -> Option<i16> {
    match migration::notification_fence_ready(conn).await {
        Ok(true) => Some(ROUTING_PROTOCOL),
        _ => None,
    }
}

fn sqlstate(err: &DbErr) -> Option<String> {
    let runtime = match err {
        DbErr::Query(e) | DbErr::Exec(e) | DbErr::Conn(e) => e,
        _ => return None,
    };
    match runtime {
        sea_orm::RuntimeErr::SqlxError(e) => e
            .as_database_error()
            .and_then(|d| d.code())
            .map(|c| c.into_owned()),
        _ => None,
    }
}

/// The insert was refused by a legacy guard trigger (`ROUTING_DECISION_REQUIRED`).
pub fn is_routing_decision_required(err: &DbErr) -> bool {
    sqlstate(err).as_deref() == Some(ROUTING_DECISION_REQUIRED_SQLSTATE)
}

/// The change was refused by the decision-row guard trigger (F3).
pub fn is_routing_decision_immutable(err: &DbErr) -> bool {
    sqlstate(err).as_deref() == Some(ROUTING_DECISION_IMMUTABLE_SQLSTATE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(mode: RoutingMode, canary_percent: u8, fallback: CriticalFallback) -> RoutingState {
        RoutingState {
            mode,
            canary_percent,
            critical_fallback: fallback,
            routing_version: 7,
        }
    }

    #[test]
    fn canary_vectors_match_the_shared_client() {
        // SHA-256(key)[..8] big-endian mod 100 (same vectors as cf-notification-client).
        assert_eq!(canary_bucket("otp:verify:abc:def"), 43);
        assert_eq!(
            canary_bucket("reminder:00000000-0000-0000-0000-000000000001:1700000000"),
            83
        );
        assert_eq!(
            canary_bucket("device-linked:6f1c8a52-3c2a-4a43-9a3e-0d6c1f2b9e10"),
            62
        );
        assert_eq!(canary_bucket("meeting-invitation:x"), 25);
        assert!(canary("otp:verify:abc:def", 44));
        assert!(!canary("otp:verify:abc:def", 43));
        assert!(canary("anything", 100));
        assert!(!canary("anything", 0));
    }

    #[test]
    fn canary_is_deterministic_and_roughly_proportional() {
        let keys: Vec<String> = (0..2000).map(|i| format!("otp:verify:{i}")).collect();
        let first: Vec<bool> = keys.iter().map(|k| canary(k, 30)).collect();
        let second: Vec<bool> = keys.iter().map(|k| canary(k, 30)).collect();
        assert_eq!(first, second);
        let share = first.iter().filter(|b| **b).count();
        assert!((450..750).contains(&share), "share {share}");
        // Monotone in the percentage: a key in the 30 % canary stays in at 60 %.
        for k in &keys {
            if canary(k, 30) {
                assert!(canary(k, 60));
            }
        }
    }

    #[test]
    fn decide_route_follows_mode_canary_and_forced_fallback() {
        let k = "otp:verify:abc:def"; // bucket 43
        for mode in [
            RoutingMode::LegacyOnly,
            RoutingMode::Shadow,
            RoutingMode::FencedOld,
        ] {
            assert_eq!(
                decide_route(k, &state(mode, 100, CriticalFallback::Auto), false).0,
                Route::Old
            );
        }
        assert_eq!(
            decide_route(
                k,
                &state(RoutingMode::Shadow, 0, CriticalFallback::Auto),
                false
            ),
            (Route::Old, true)
        );
        let new = state(RoutingMode::FencedNew, 44, CriticalFallback::Auto);
        assert_eq!(decide_route(k, &new, true), (Route::New, false));
        let out = state(RoutingMode::FencedNew, 43, CriticalFallback::Auto);
        assert_eq!(decide_route(k, &out, false).0, Route::Old);
        let forced = state(RoutingMode::FencedNew, 100, CriticalFallback::Forced);
        assert_eq!(decide_route(k, &forced, true).0, Route::Old);
        assert_eq!(decide_route(k, &forced, false).0, Route::New);
    }

    #[test]
    fn critical_reroute_rules() {
        let base = CriticalDecisionState {
            route: Route::New,
            lease_count: 0,
            lease_live: false,
            completed: false,
            code_expired: false,
        };
        use CriticalFallback::*;
        assert_eq!(
            critical_reroute(&base, true, Auto),
            RerouteVerdict::Allowed(DecisionReason::AutoFailover)
        );
        assert_eq!(
            critical_reroute(&base, false, Auto),
            RerouteVerdict::Allowed(DecisionReason::ReroutedCriticalLease)
        );
        assert_eq!(critical_reroute(&base, true, Off), RerouteVerdict::Refused);
        let live = CriticalDecisionState {
            lease_count: 1,
            lease_live: true,
            ..base
        };
        assert_eq!(
            critical_reroute(&live, false, Forced),
            RerouteVerdict::WaitForLease
        );
        let lapsed = CriticalDecisionState {
            lease_count: 1,
            ..base
        };
        assert_eq!(
            critical_reroute(&lapsed, true, Auto),
            RerouteVerdict::Refused
        );
        assert_eq!(
            critical_reroute(&lapsed, false, Auto),
            RerouteVerdict::Refused
        );
        assert_eq!(
            critical_reroute(&lapsed, true, Forced),
            RerouteVerdict::Refused
        );
        assert_eq!(
            critical_reroute(&lapsed, false, Forced),
            RerouteVerdict::Allowed(DecisionReason::ReroutedPossibleDuplicate)
        );
        let done = CriticalDecisionState {
            lease_count: 1,
            completed: true,
            ..base
        };
        assert_eq!(
            critical_reroute(&done, false, Forced),
            RerouteVerdict::Refused
        );
        let expired = CriticalDecisionState {
            code_expired: true,
            ..base
        };
        assert_eq!(
            critical_reroute(&expired, false, Forced),
            RerouteVerdict::Refused
        );
        let old = CriticalDecisionState {
            route: Route::Old,
            ..base
        };
        assert_eq!(
            critical_reroute(&old, false, Forced),
            RerouteVerdict::Refused
        );
    }

    #[test]
    fn categories_round_trip_and_lanes() {
        for c in MeetingCategory::ALL {
            assert_eq!(MeetingCategory::parse(c.as_str()), Some(c));
        }
        let critical: Vec<_> = MeetingCategory::ALL
            .iter()
            .filter(|c| c.is_critical())
            .map(|c| c.as_str())
            .collect();
        assert_eq!(
            critical,
            [
                "AUTH_VERIFICATION",
                "AUTH_PASSWORD_RESET",
                "SECURITY_DEVICE_STEP_UP",
                "SECURITY_PASSWORD_CHANGED",
                "SECURITY_DEVICE_LINKED"
            ]
        );
        assert!(validate_key("").is_err());
        assert!(validate_key(&"k".repeat(161)).is_err());
        assert!(validate_key("otp:verify:a:b").is_ok());
    }
}
