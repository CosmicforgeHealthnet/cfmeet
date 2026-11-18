//! Immediate revocation of partner session devices from Platform Access's identity
//! change feed (`POST /internal/v1/identity/changes`, `platform-identity.read`).
//!
//! Every validation of a partner session already re-checks its minting credential and
//! resolves its principal (so a revocation applies within the credential cache bound and
//! the re-validation interval of open connections). This watcher makes it immediate:
//!
//! * a `CREDENTIAL` change whose status is not ACTIVE revokes every ACTIVE partner
//!   session device minted by that credential;
//! * a `PRINCIPAL` change whose status is not ACTIVE (suspended, deactivated, deleted)
//!   revokes the principal's partner devices created before the change, and removes its
//!   partner push registrations;
//! * a `PRINCIPAL` snapshot carrying `tokensNotBefore` (`revoke-sessions`) revokes the
//!   principal's partner devices created before that instant.
//!
//! Revoked devices end their client sessions and close their connections through the
//! outbox, exactly like a first-party session-device revocation. Applying a change is
//! idempotent and bounded by the change's own time, so replaying the feed from the start
//! (every process start) never revokes a device created after a re-activation.

use std::time::Duration as StdDuration;

use chrono::{DateTime, Utc};
use sea_orm::{FromQueryResult, TransactionTrait};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    conversations::service::request_context,
    db,
    error::{Error, Result},
    events::outbox::{self, NewEvent},
    realtime::session_devices::PLATFORM_USER_TOKEN,
    state::AppState,
};

/// What one change asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Revocation {
    /// Every partner device minted by this credential.
    Credential(Uuid),
    /// The principal's partner devices created before `before`.
    Principal {
        environment_id: Uuid,
        principal_id: Uuid,
        before: DateTime<Utc>,
        /// The principal is no longer ACTIVE (its push registrations go too).
        inactive: bool,
    },
}

fn uuid_at(v: &Value, key: &str) -> Option<Uuid> {
    v.get(key)?.as_str().and_then(|s| Uuid::parse_str(s).ok())
}

fn time_at(v: &Value, key: &str) -> Option<DateTime<Utc>> {
    v.get(key)?
        .as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
}

/// The revocations a change of the feed asks for (none for every other change).
pub fn revocations(change: &Value) -> Vec<Revocation> {
    let snapshot = &change["snapshot"];
    let deleted = change["operation"].as_str() == Some("DELETE");
    match change["entityKind"].as_str() {
        Some("CREDENTIAL") => match uuid_at(snapshot, "id") {
            Some(id) if deleted || snapshot["status"].as_str() != Some("ACTIVE") => {
                vec![Revocation::Credential(id)]
            }
            _ => Vec::new(),
        },
        Some("PRINCIPAL") => {
            let (Some(environment_id), Some(principal_id)) =
                (uuid_at(snapshot, "environmentId"), uuid_at(snapshot, "id"))
            else {
                return Vec::new();
            };
            let occurred = time_at(change, "occurredAt").unwrap_or_else(Utc::now);
            let mut out = Vec::new();
            if deleted || snapshot["status"].as_str() != Some("ACTIVE") {
                out.push(Revocation::Principal {
                    environment_id,
                    principal_id,
                    before: occurred,
                    inactive: true,
                });
            }
            if let Some(not_before) = time_at(snapshot, "tokensNotBefore") {
                out.push(Revocation::Principal {
                    environment_id,
                    principal_id,
                    before: not_before.min(occurred),
                    inactive: false,
                });
            }
            out
        }
        _ => Vec::new(),
    }
}

#[derive(FromQueryResult)]
struct Revoked {
    id: Uuid,
}

/// Applies one revocation: the matching ACTIVE partner devices and their ACTIVE client
/// sessions are revoked, and their connections closed after commit. Returns the number
/// of devices revoked.
pub async fn apply(state: &AppState, revocation: &Revocation) -> Result<usize> {
    let now = state.now();
    let revocation = revocation.clone();
    if let Revocation::Credential(id) = &revocation {
        state.partner.forget_credential(*id);
    }
    let n = state
        .db
        .transaction::<_, usize, Error>(move |txn| {
            Box::pin(async move {
                let (sql, values, reason): (&str, Vec<sea_orm::Value>, &str) = match &revocation {
                    Revocation::Credential(credential) => (
                        "UPDATE messaging_session_devices SET status = 'REVOKED', revoked_at = $3, revoked_reason = $4 \
                         WHERE binding_kind = $1 AND minting_credential_id = $2 AND status = 'ACTIVE' \
                         RETURNING id",
                        vec![PLATFORM_USER_TOKEN.into(), (*credential).into(), now.into(), "CREDENTIAL_REVOKED".into()],
                        "CREDENTIAL_REVOKED",
                    ),
                    Revocation::Principal { environment_id, principal_id, before, inactive } => (
                        "UPDATE messaging_session_devices SET status = 'REVOKED', revoked_at = $5, revoked_reason = $6 \
                         WHERE binding_kind = $1 AND environment_id = $2 AND principal_id = $3 AND created_at <= $4 \
                           AND status = 'ACTIVE' \
                         RETURNING id",
                        vec![
                            PLATFORM_USER_TOKEN.into(),
                            (*environment_id).into(),
                            (*principal_id).into(),
                            (*before).into(),
                            now.into(),
                            if *inactive { "PRINCIPAL_INACTIVE" } else { "SESSIONS_REVOKED" }.into(),
                        ],
                        if *inactive { "PRINCIPAL_INACTIVE" } else { "SESSIONS_REVOKED" },
                    ),
                };
                let devices: Vec<Revoked> = db::all(txn, sql, values).await?;
                if let Revocation::Principal { environment_id, principal_id, inactive: true, .. } = &revocation {
                    db::exec(
                        txn,
                        "UPDATE messaging_partner_push_registrations SET status = 'REMOVED', updated_at = $3 \
                         WHERE environment_id = $1 AND principal_id = $2 AND status = 'ACTIVE'",
                        vec![(*environment_id).into(), (*principal_id).into(), now.into()],
                    )
                    .await?;
                }
                if devices.is_empty() {
                    return Ok(0);
                }
                #[derive(FromQueryResult)]
                struct Session {
                    id: Uuid,
                    tenant_id: Uuid,
                    application_id: Uuid,
                    environment_id: Uuid,
                    principal_id: Uuid,
                }
                let ids: Vec<Uuid> = devices.iter().map(|d| d.id).collect();
                let sessions: Vec<Session> = db::all(
                    txn,
                    "UPDATE messaging_client_sessions SET status = 'REVOKED', revoked_at = $2, revoked_reason = $3 \
                     WHERE device_kind = 'SESSION_DEVICE' AND device_id = ANY($1) AND status = 'ACTIVE' \
                     RETURNING id, tenant_id, application_id, environment_id, principal_id",
                    vec![ids.into(), now.into(), reason.into()],
                )
                .await?;
                let request_ctx = request_context();
                for s in &sessions {
                    outbox::enqueue(
                        txn,
                        NewEvent {
                            event_type: "messaging.client-session.revoked",
                            aggregate_type: "CLIENT_SESSION",
                            aggregate_id: s.id,
                            scope: crate::events::outbox::EventScope {
                                tenant_id: s.tenant_id,
                                application_id: s.application_id,
                                environment_id: s.environment_id,
                            },
                            payload: json!({ "session_id": s.id, "principal_id": s.principal_id }),
                        },
                        &request_ctx,
                        now,
                    )
                    .await?;
                }
                Ok(devices.len())
            })
        })
        .await
        .map_err(Error::from)?;
    if n > 0 {
        crate::metrics::inc("messaging_partner_session_devices_revoked_total", &[]);
        state.wake_outbox();
    }
    Ok(n)
}

/// One watcher pass from `after` to the end of the feed; returns the new cursor. A
/// failed page or revocation keeps the cursor (the change is applied again next time).
pub async fn poll(state: &AppState, mut after: i64) -> i64 {
    loop {
        let page = match state.partner.directory.identity_changes(after).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "identity change feed unavailable for partner revocations");
                return after;
            }
        };
        for change in &page.changes {
            for r in revocations(change) {
                if let Err(e) = apply(state, &r).await {
                    tracing::warn!(error = %e, "partner revocation not applied (retried)");
                    return after;
                }
            }
            if let Some(seq) = change["sequence"].as_i64() {
                after = after.max(seq);
            }
        }
        after = after.max(page.next_after);
        if !page.has_more {
            return after;
        }
    }
}

/// Runs the watcher until the process stops (spawned only with the partner flag on).
pub async fn run(state: AppState, interval: StdDuration) {
    let mut after = 0;
    loop {
        after = poll(&state, after).await;
        tokio::time::sleep(interval).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_revoking_changes_revoke() {
        let id = Uuid::new_v4();
        let env = Uuid::new_v4();
        let credential = |status: &str| {
            json!({ "entityKind": "CREDENTIAL", "operation": "UPDATE", "occurredAt": "2026-11-18T10:00:00Z",
            "snapshot": { "id": id, "status": status } })
        };
        assert_eq!(
            revocations(&credential("REVOKED")),
            vec![Revocation::Credential(id)]
        );
        assert!(revocations(&credential("ACTIVE")).is_empty());
        let principal = |status: &str, extra: Value| {
            let mut s = json!({ "id": id, "environmentId": env, "status": status });
            for (k, v) in extra.as_object().unwrap() {
                s[k] = v.clone();
            }
            json!({ "entityKind": "PRINCIPAL", "operation": "UPDATE", "occurredAt": "2026-11-18T10:00:00Z", "snapshot": s })
        };
        assert!(revocations(&principal("ACTIVE", json!({}))).is_empty());
        let r = revocations(&principal("SUSPENDED", json!({})));
        assert!(matches!(
            r.as_slice(),
            [Revocation::Principal { inactive: true, .. }]
        ));
        let r = revocations(&principal(
            "ACTIVE",
            json!({ "tokensNotBefore": "2026-11-18T09:00:00Z" }),
        ));
        assert!(matches!(
            r.as_slice(),
            [Revocation::Principal { inactive: false, before, .. }] if before.to_rfc3339() == "2026-11-18T09:00:00+00:00"
        ));
        assert!(revocations(&json!({ "entityKind": "TENANT", "snapshot": {} })).is_empty());
    }
}
