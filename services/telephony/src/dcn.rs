//! DCN voice activation (Part IV Workstream L).
//!
//! A Digital Communications Number is OWNED by Numbering (assignment, lifecycle, routing
//! revision, provider binding). Telephony only carries its voice traffic, and keeps a
//! local allowlist row for it — `telecom_number_bindings` with `number_kind = 'DCN'` and
//! the Numbering `number_resource_id` — so that:
//!
//! * the SIP edge recognises the dialled number as a DCN (inbound is then resolved by
//!   Numbering per call, never from this row);
//! * an outbound DCN caller identity is presented only on an interconnect the number is
//!   bound to (and only under a consumed Numbering CALLER_IDENTITY grant).
//!
//! Activation is two-sided: Numbering's provider route must be CONFIGURED, and Telephony
//! acknowledges it (`route-readiness`) ONLY while a local enabled VOICE_INBOUND binding
//! for the same Number Resource exists on an enabled route — never a fabricated success.
//! Revocation is the reverse: disabling the local binding stops the number at Telephony;
//! a Numbering-side suspension / release / reassignment is enforced per call by the
//! inbound resolution and the grant verification (current state, not this row).

use chrono::Utc;
use sea_orm::{ConnectionTrait, DatabaseConnection, FromQueryResult, TransactionTrait};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    db,
    error::{TelephonyError, TelephonyResult},
    operator_audit::{self, OperatorAction},
    ports::{NumberingPort, PortError},
};

/// Why an activation step did not happen (bounded codes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DcnActivationError {
    /// Numbering could not be reached (nothing changed).
    NumberingUnavailable,
    /// Numbering refused (unknown number, stale revision, route not configured...).
    NumberingRefused(String),
    /// The local route does not exist, is disabled or cannot carry inbound voice.
    RouteNotUsable,
    /// The number is already bound locally as a relay number.
    NumberKindConflict,
    /// No enabled local VOICE_INBOUND binding: Telephony cannot route this DCN.
    NotBoundLocally,
    /// Numbering has no voice route configuration for this number yet.
    NoVoiceRoute,
    InvalidOperator(&'static str),
    Database(String),
}

impl DcnActivationError {
    pub fn code(&self) -> &str {
        match self {
            Self::NumberingUnavailable => "NUMBERING_UNAVAILABLE",
            Self::NumberingRefused(_) => "NUMBERING_REFUSED",
            Self::RouteNotUsable => "ROUTE_NOT_USABLE",
            Self::NumberKindConflict => "NUMBER_KIND_CONFLICT",
            Self::NotBoundLocally => "DCN_NOT_BOUND",
            Self::NoVoiceRoute => "VOICE_ROUTE_NOT_CONFIGURED",
            Self::InvalidOperator(_) => "OPERATOR_REQUIRED",
            Self::Database(_) => "DATABASE_ERROR",
        }
    }
}

impl std::fmt::Display for DcnActivationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NumberingRefused(reason) => write!(f, "{}: {reason}", self.code()),
            Self::InvalidOperator(reason) => write!(f, "{}: {reason}", self.code()),
            Self::Database(reason) => write!(f, "{}: {reason}", self.code()),
            _ => f.write_str(self.code()),
        }
    }
}

impl From<TelephonyError> for DcnActivationError {
    fn from(e: TelephonyError) -> Self {
        Self::Database(e.to_string())
    }
}

fn port(e: PortError) -> DcnActivationError {
    match e {
        PortError::Unavailable => DcnActivationError::NumberingUnavailable,
        PortError::NotFound => DcnActivationError::NumberingRefused("NOT_FOUND".into()),
        PortError::Denied(reason) => DcnActivationError::NumberingRefused(reason),
    }
}

fn masked(number: &str) -> String {
    let tail: String = number
        .chars()
        .rev()
        .take(2)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("+***{tail}")
}

#[derive(Debug, FromQueryResult)]
struct RouteState {
    capabilities: Vec<String>,
}

#[derive(Debug, FromQueryResult)]
struct KindRow {
    number_kind: String,
    number_resource_id: Option<Uuid>,
}

/// Binds a DCN to a local interconnect route. The canonical number comes from Numbering
/// (`GET /internal/v1/number-resources/{id}`), never from the operator. Audited.
pub async fn bind(
    db: &DatabaseConnection,
    numbering: &dyn NumberingPort,
    operator: &str,
    number_resource_id: Uuid,
    route_id: &str,
    capabilities: &[String],
) -> Result<Value, DcnActivationError> {
    let allowed = ["VOICE_INBOUND", "VOICE_OUTBOUND"];
    if capabilities.is_empty() || capabilities.iter().any(|c| !allowed.contains(&c.as_str())) {
        return Err(DcnActivationError::RouteNotUsable);
    }
    let action = OperatorAction::new(
        operator,
        "DCN_BIND",
        "NUMBER_BINDING",
        &number_resource_id.to_string(),
        json!({ "route": route_id, "capabilities": capabilities }),
    )
    .map_err(DcnActivationError::InvalidOperator)?;
    let (number, _) = numbering
        .voice_route(number_resource_id)
        .await
        .map_err(port)?;
    let now = Utc::now();
    let txn = db.begin().await.map_err(TelephonyError::from)?;
    let route = db::one::<RouteState, _>(
        &txn,
        "SELECT capabilities FROM telecom_routes WHERE id = $1",
        vec![route_id.into()],
    )
    .await?
    .ok_or(DcnActivationError::RouteNotUsable)?;
    if capabilities
        .iter()
        .any(|c| !route.capabilities.iter().any(|r| r == c))
    {
        return Err(DcnActivationError::RouteNotUsable);
    }
    let kinds = db::all::<KindRow, _>(
        &txn,
        "SELECT number_kind, number_resource_id FROM telecom_number_bindings WHERE public_e164 = $1",
        vec![number.expose().into()],
    )
    .await?;
    if kinds
        .iter()
        .any(|k| k.number_kind != "DCN" || k.number_resource_id != Some(number_resource_id))
    {
        return Err(DcnActivationError::NumberKindConflict);
    }
    db::exec(
        &txn,
        "INSERT INTO telecom_number_bindings (public_e164, route_id, capabilities, status, number_kind, \
           number_resource_id, created_at, updated_at) \
         VALUES ($1, $2, $3, 'ENABLED', 'DCN', $4, $5, $5) \
         ON CONFLICT (public_e164, route_id) DO UPDATE SET capabilities = EXCLUDED.capabilities, \
           status = 'ENABLED', updated_at = EXCLUDED.updated_at",
        vec![
            number.expose().into(),
            route_id.into(),
            capabilities.to_vec().into(),
            number_resource_id.into(),
            now.into(),
        ],
    )
    .await?;
    operator_audit::record(&txn, &action, now).await?;
    txn.commit().await.map_err(TelephonyError::from)?;
    tracing::info!(number_resource_id = %number_resource_id, route = route_id, "DCN bound for voice");
    Ok(json!({
        "numberResourceId": number_resource_id,
        "number": masked(number.expose()),
        "route": route_id,
        "status": "ENABLED",
    }))
}

/// Disables the local DCN binding(s): Telephony stops carrying the number (voice
/// revocation on the Telephony side). Audited.
pub async fn disable(
    db: &DatabaseConnection,
    operator: &str,
    number_resource_id: Uuid,
) -> Result<Value, DcnActivationError> {
    let action = OperatorAction::new(
        operator,
        "DCN_DISABLE",
        "NUMBER_BINDING",
        &number_resource_id.to_string(),
        json!({ "status": "DISABLED" }),
    )
    .map_err(DcnActivationError::InvalidOperator)?;
    let now = Utc::now();
    let txn = db.begin().await.map_err(TelephonyError::from)?;
    let n = db::exec(
        &txn,
        "UPDATE telecom_number_bindings SET status = 'DISABLED', updated_at = $2 \
         WHERE number_kind = 'DCN' AND number_resource_id = $1 AND status = 'ENABLED'",
        vec![number_resource_id.into(), now.into()],
    )
    .await?;
    operator_audit::record(&txn, &action, now).await?;
    txn.commit().await.map_err(TelephonyError::from)?;
    Ok(json!({ "numberResourceId": number_resource_id, "disabled": n }))
}

/// Whether Telephony can carry inbound voice for the DCN: an ENABLED local binding with
/// VOICE_INBOUND on an ENABLED route.
pub async fn routable_locally<C: ConnectionTrait>(
    db: &C,
    number_resource_id: Uuid,
) -> TelephonyResult<bool> {
    Ok(db::scalar_i64(
        db,
        "SELECT count(*) FROM telecom_number_bindings b JOIN telecom_routes r ON r.id = b.route_id \
         WHERE b.number_kind = 'DCN' AND b.number_resource_id = $1 AND b.status = 'ENABLED' \
           AND 'VOICE_INBOUND' = ANY(b.capabilities) AND r.status = 'ENABLED'",
        vec![number_resource_id.into()],
    )
    .await?
        > 0)
}

/// Acknowledges Numbering's CURRENT voice route configuration for the DCN, only when
/// Telephony can actually route it (see [`routable_locally`]). `operator` = `None` is the
/// automatic reconciler (not an operator action, not audited as one).
pub async fn acknowledge(
    db: &DatabaseConnection,
    numbering: &dyn NumberingPort,
    operator: Option<&str>,
    number_resource_id: Uuid,
) -> Result<Value, DcnActivationError> {
    let action = match operator {
        Some(op) => Some(
            OperatorAction::new(
                op,
                "DCN_ACKNOWLEDGE",
                "NUMBER_BINDING",
                &number_resource_id.to_string(),
                json!({ "family": "VOICE" }),
            )
            .map_err(DcnActivationError::InvalidOperator)?,
        ),
        None => None,
    };
    if !routable_locally(db, number_resource_id).await? {
        return Err(DcnActivationError::NotBoundLocally);
    }
    let (_, route) = numbering
        .voice_route(number_resource_id)
        .await
        .map_err(port)?;
    let route = route.ok_or(DcnActivationError::NoVoiceRoute)?;
    numbering
        .acknowledge_voice_route(number_resource_id, &route)
        .await
        .map_err(port)?;
    if let Some(action) = action {
        let txn = db.begin().await.map_err(TelephonyError::from)?;
        operator_audit::record(&txn, &action, Utc::now()).await?;
        txn.commit().await.map_err(TelephonyError::from)?;
    }
    crate::metrics::inc("telephony_dcn_route_acknowledgements_total", &[]);
    Ok(json!({
        "numberResourceId": number_resource_id,
        "family": "VOICE",
        "routeRef": route.route_ref,
        "routeRevision": route.route_revision,
        "status": "READY",
    }))
}

#[derive(Debug, FromQueryResult)]
struct Pending {
    number_resource_id: Uuid,
}

/// The automatic side of activation continuity: every locally routable DCN whose
/// Numbering voice route is CONFIGURED is (re-)acknowledged — a re-provisioned route
/// revision becomes ready without an operator, and an unchanged one is an idempotent
/// no-op at Numbering. Bounded; a Numbering outage stops the pass (no partial guessing).
pub async fn reconcile(
    db: &DatabaseConnection,
    numbering: &dyn NumberingPort,
    limit: u64,
) -> TelephonyResult<u64> {
    let pending = db::all::<Pending, _>(
        db,
        "SELECT DISTINCT b.number_resource_id FROM telecom_number_bindings b \
           JOIN telecom_routes r ON r.id = b.route_id \
         WHERE b.number_kind = 'DCN' AND b.status = 'ENABLED' AND r.status = 'ENABLED' \
           AND 'VOICE_INBOUND' = ANY(b.capabilities) \
         ORDER BY b.number_resource_id LIMIT $1",
        vec![(limit.clamp(1, 500) as i64).into()],
    )
    .await?;
    let mut acknowledged = 0;
    for p in pending {
        let (_, route) = match numbering.voice_route(p.number_resource_id).await {
            Ok(r) => r,
            Err(PortError::Unavailable) => break,
            Err(_) => continue,
        };
        let Some(route) = route.filter(|r| r.status == "CONFIGURED") else {
            continue;
        };
        match numbering
            .acknowledge_voice_route(p.number_resource_id, &route)
            .await
        {
            Ok(()) => acknowledged += 1,
            Err(PortError::Unavailable) => break,
            Err(_) => continue,
        }
    }
    Ok(acknowledged)
}
