//! Operator audit (Part H §45, §49): the record of every mutating operator action on
//! Telephony's interconnect configuration, destination rules and retention holds.
//!
//! `telephony-admin` writes the audit row in the SAME transaction as the effect, so an
//! effect without its audit row (or the reverse) cannot exist. Rows are append-only
//! (database triggers refuse UPDATE / DELETE / TRUNCATE). The detail is a bounded object
//! of safe operational values: ids, states, counts — never a secret, never a full
//! telephone number (numbers appear masked, as the operator sees them).

use chrono::{DateTime, Utc};
use sea_orm::ConnectionTrait;
use serde_json::Value;
use uuid::Uuid;

use crate::{db, error::TelephonyResult};

/// Operator identity (`--operator` / `CF_OPERATOR_ID`): a short safe token.
pub fn valid_actor(actor: &str) -> bool {
    (2..=64).contains(&actor.len())
        && actor.as_bytes()[0].is_ascii_alphanumeric()
        && actor
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'@' | b':' | b'-'))
}

/// One mutating operator action.
#[derive(Debug, Clone)]
pub struct OperatorAction {
    pub actor: String,
    pub correlation_id: String,
    /// `telephony_operator_audit.action` vocabulary (CHECK constraint).
    pub action: &'static str,
    pub target_kind: &'static str,
    pub target_id: String,
    pub detail: Value,
}

impl OperatorAction {
    /// Builds an action; the actor must be a valid operator token.
    pub fn new(
        actor: &str,
        action: &'static str,
        target_kind: &'static str,
        target_id: &str,
        detail: Value,
    ) -> Result<Self, &'static str> {
        if !valid_actor(actor) {
            return Err("an operator identity is required (--operator or CF_OPERATOR_ID)");
        }
        let target_id: String = target_id.chars().take(64).collect();
        if target_id.is_empty() {
            return Err("the target id is required");
        }
        Ok(Self {
            actor: actor.to_string(),
            correlation_id: format!("op-{}", Uuid::new_v4().simple()),
            action,
            target_kind,
            target_id,
            detail: if detail.is_object() {
                detail
            } else {
                Value::Object(Default::default())
            },
        })
    }
}

/// Appends the audit row (call inside the transaction that applies the effect).
pub async fn record<C: ConnectionTrait>(
    db_: &C,
    action: &OperatorAction,
    at: DateTime<Utc>,
) -> TelephonyResult<Uuid> {
    let id = Uuid::new_v4();
    let detail = if action.detail.to_string().len() > 1800 {
        serde_json::json!({ "truncated": true })
    } else {
        action.detail.clone()
    };
    db::exec(
        db_,
        "INSERT INTO telephony_operator_audit (id, actor, correlation_id, action, target_kind, target_id, detail, occurred_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        vec![
            id.into(),
            action.actor.clone().into(),
            action.correlation_id.clone().into(),
            action.action.into(),
            action.target_kind.into(),
            action.target_id.clone().into(),
            detail.into(),
            at.into(),
        ],
    )
    .await?;
    tracing::info!(
        action = action.action,
        target_kind = action.target_kind,
        "operator action recorded"
    );
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operator_identities_are_short_safe_tokens() {
        assert!(valid_actor("ops.jane"));
        assert!(valid_actor("jane@example.org"));
        assert!(!valid_actor(""));
        assert!(!valid_actor("j"));
        assert!(!valid_actor("-lead"));
        assert!(!valid_actor("jane doe"));
        assert!(!valid_actor(&"x".repeat(65)));
        assert!(OperatorAction::new("", "ROUTE_ENABLE", "ROUTE", "ng-a", Value::Null).is_err());
        let a =
            OperatorAction::new("ops.jane", "ROUTE_ENABLE", "ROUTE", "ng-a", Value::Null).unwrap();
        assert!(a.detail.is_object() && a.correlation_id.starts_with("op-"));
    }
}
