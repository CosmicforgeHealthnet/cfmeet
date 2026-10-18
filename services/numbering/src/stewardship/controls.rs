//! Workstream W — emergency kill switches.
//!
//! An operator engages a control and Numbering refuses the controlled operation at once,
//! service-wide (it is consulted on every resolution / grant / provisioning / port /
//! lease). Every change is appended to `number_control_events`; the current state is one
//! row per control. Restrictive only: a control never authorizes anything.
//!
//! * `DCN_INBOUND_ROUTING` — inbound voice / SMS resolution answers `ROUTING_DISABLED`
//!   (Telephony / the SMS Gateway refuse the traffic);
//! * `DCN_CALLER_IDENTITY`, `DCN_SMS_SENDER` — no new outbound grant;
//! * `NUMBER_PROVISIONING`, `NUMBER_PORTING`, `RELAY_LEASES` — no new provisioning,
//!   port request or relay lease.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde::Serialize;
use uuid::Uuid;

use super::Stewardship;
use crate::numbers::{stmt, Actor, ActorKind, DomainError, DomainResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Control {
    DcnInboundRouting,
    DcnCallerIdentity,
    DcnSmsSender,
    NumberProvisioning,
    NumberPorting,
    RelayLeases,
}

impl Control {
    pub const ALL: &'static [Control] = &[
        Control::DcnInboundRouting,
        Control::DcnCallerIdentity,
        Control::DcnSmsSender,
        Control::NumberProvisioning,
        Control::NumberPorting,
        Control::RelayLeases,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Control::DcnInboundRouting => "DCN_INBOUND_ROUTING",
            Control::DcnCallerIdentity => "DCN_CALLER_IDENTITY",
            Control::DcnSmsSender => "DCN_SMS_SENDER",
            Control::NumberProvisioning => "NUMBER_PROVISIONING",
            Control::NumberPorting => "NUMBER_PORTING",
            Control::RelayLeases => "RELAY_LEASES",
        }
    }

    pub fn parse(v: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|c| c.as_str() == v)
    }
}

#[derive(Debug, Clone, PartialEq, FromQueryResult, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlRow {
    pub control_key: String,
    pub engaged: bool,
    pub reason_code: String,
    pub changed_by: String,
    pub changed_at: DateTime<Utc>,
    pub revision: i64,
}

/// Whether a control is engaged (a read failure is refused by the caller's query anyway).
pub async fn engaged<C: ConnectionTrait>(db: &C, control: Control) -> DomainResult<bool> {
    Ok(ControlRow::find_by_statement(stmt(
        "SELECT * FROM number_operational_controls WHERE control_key = $1",
        vec![control.as_str().into()],
    ))
    .one(db)
    .await?
    .is_some_and(|c| c.engaged))
}

/// Refuses with `OPERATION_DISABLED` while `control` is engaged.
pub async fn require_open<C: ConnectionTrait>(db: &C, control: Control) -> DomainResult<()> {
    if engaged(db, control).await? {
        crate::metrics::inc(
            "numbering_kill_switch_refusals_total",
            &[("control", control.as_str())],
        );
        return Err(DomainError::precondition(
            "OPERATION_DISABLED",
            "the operation is disabled by an operator control",
        ));
    }
    Ok(())
}

impl Stewardship {
    /// Engages / releases a control (operators only; audited).
    pub async fn set_control(
        &self,
        operator: &Actor,
        control: Control,
        engage: bool,
        reason_code: &str,
    ) -> DomainResult<ControlRow> {
        if operator.kind != ActorKind::Operator {
            return Err(DomainError::precondition(
                "PRIVILEGED_ACCESS_REQUIRED",
                "controls are changed by an operator",
            ));
        }
        crate::numbers::types::reason_code(reason_code)?;
        let now = self.now();
        let txn = self.numbers.db().begin().await?;
        let row = ControlRow::find_by_statement(stmt(
            "INSERT INTO number_operational_controls (control_key, engaged, reason_code, changed_by, changed_at, revision) \
             VALUES ($1, $2, $3, $4, $5, 1) \
             ON CONFLICT (control_key) DO UPDATE SET engaged = EXCLUDED.engaged, reason_code = EXCLUDED.reason_code, \
               changed_by = EXCLUDED.changed_by, changed_at = EXCLUDED.changed_at, \
               revision = number_operational_controls.revision + 1 \
             RETURNING *",
            vec![
                control.as_str().into(),
                engage.into(),
                reason_code.into(),
                operator.reference.clone().into(),
                now.into(),
            ],
        ))
        .one(&txn)
        .await?
        .expect("upsert returns the row");
        txn.execute(stmt(
            "INSERT INTO number_control_events (id, control_key, engaged, reason_code, changed_by, changed_at, revision) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
            vec![
                Uuid::new_v4().into(),
                row.control_key.clone().into(),
                row.engaged.into(),
                row.reason_code.clone().into(),
                row.changed_by.clone().into(),
                now.into(),
                row.revision.into(),
            ],
        ))
        .await?;
        txn.commit().await?;
        tracing::warn!(
            control = control.as_str(),
            engaged = engage,
            "operational control changed"
        );
        Ok(row)
    }

    /// Every control's current state (unset controls are open).
    pub async fn controls(&self) -> DomainResult<Vec<ControlRow>> {
        Ok(ControlRow::find_by_statement(stmt(
            "SELECT * FROM number_operational_controls ORDER BY control_key",
            vec![],
        ))
        .all(self.numbers.db())
        .await?)
    }
}
