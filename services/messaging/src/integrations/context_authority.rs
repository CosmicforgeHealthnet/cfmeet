//! Part V H §103 — Privacy Relay context authority.
//!
//! Privacy Relay owns protected-communication contexts (who may reach whom, through
//! which relay, until when). Messaging never sees the contact values behind them: it
//! receives an opaque context reference and its authority state, version-fenced:
//!
//! | state | effect on conversations with that context |
//! |---|---|
//! | ACTIVE | may be created; a LOCKED-by-authority conversation unlocks |
//! | SUSPENDED | ACTIVE conversations are LOCKED (no new messages) |
//! | REVOKED | conversations are CLOSED and can never be reopened while revoked |
//!
//! A revocation applies even when the owning tenant / application is suspended (a
//! restriction must never depend on the authority it restricts). An update at or below
//! the stored version is stale and ignored.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    audit::{self, AuditEntry},
    conversations::{
        service::{self, request_context, ConversationRow},
        LifecycleState,
    },
    db,
    error::{Error, ErrorCode, Result},
    identity::TrustedContext,
    membership,
    state::AppState,
};

/// Namespaces whose conversations follow a peer's context authority, and the only peer
/// allowed to set it.
pub const AUTHORITY_NAMESPACES: &[(&str, &str)] = &[("privacy-relay", "privacy-relay")];

pub fn controlled(namespace: &str) -> bool {
    AUTHORITY_NAMESPACES.iter().any(|(ns, _)| *ns == namespace)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthorityUpdate {
    pub namespace: String,
    pub reference: String,
    pub state: String,
    pub source_version: i64,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(FromQueryResult)]
struct AuthorityRow {
    state: String,
}

/// The current authority of a controlled context (`None`: never announced).
pub async fn current<C: ConnectionTrait>(
    db: &C,
    environment_id: uuid::Uuid,
    namespace: &str,
    reference: &str,
) -> Result<Option<String>> {
    let row: Option<AuthorityRow> = db::one(
        db,
        "SELECT state FROM messaging_context_authorities WHERE environment_id = $1 AND namespace = $2 AND reference = $3",
        vec![environment_id.into(), namespace.into(), reference.into()],
    )
    .await?;
    Ok(row.map(|r| r.state))
}

/// Refuses an operation that needs an ACTIVE authority for a controlled context.
pub async fn require_active<C: ConnectionTrait>(
    db: &C,
    environment_id: uuid::Uuid,
    namespace: &str,
    reference: &str,
) -> Result<()> {
    if !controlled(namespace) {
        return Ok(());
    }
    match current(db, environment_id, namespace, reference)
        .await?
        .as_deref()
    {
        Some("ACTIVE") => Ok(()),
        Some(_) => Err(Error::reason(
            ErrorCode::ContextNotAllowed,
            "CONTEXT_AUTHORITY_WITHDRAWN",
        )),
        None => Err(Error::reason(
            ErrorCode::ContextNotAllowed,
            "CONTEXT_AUTHORITY_UNKNOWN",
        )),
    }
}

/// `POST /internal/v1/context-authorities` (permission
/// `messaging-context-authority.apply`).
pub async fn apply(
    state: &AppState,
    ctx: &TrustedContext,
    caller_service: &str,
    request: AuthorityUpdate,
) -> Result<Value> {
    let owner = AUTHORITY_NAMESPACES
        .iter()
        .find(|(ns, _)| *ns == request.namespace)
        .map(|(_, owner)| *owner)
        .ok_or_else(|| Error::validation("namespace is not authority-controlled"))?;
    if caller_service != format!("cfmeet.{owner}") {
        // A peer only speaks for the namespace it owns.
        return Err(Error::code(ErrorCode::InternalPermissionDenied));
    }
    crate::conversations::ContextRef {
        namespace: request.namespace.clone(),
        reference: request.reference.clone(),
        version: 1,
    }
    .validate()
    .map_err(Error::validation)?;
    if !matches!(request.state.as_str(), "ACTIVE" | "SUSPENDED" | "REVOKED") {
        return Err(Error::validation(
            "state must be ACTIVE, SUSPENDED or REVOKED",
        ));
    }
    if request.source_version < 0 {
        return Err(Error::validation("sourceVersion must not be negative"));
    }
    let reason = request
        .reason
        .clone()
        .unwrap_or_else(|| match request.state.as_str() {
            "REVOKED" => "CONTEXT_REVOKED".into(),
            "SUSPENDED" => "CONTEXT_SUSPENDED".into(),
            _ => "CONTEXT_ACTIVE".into(),
        });
    if !membership::valid_reason(&reason) {
        return Err(Error::validation(
            "reason must be an UPPER_SNAKE_CASE code of 2-48 characters",
        ));
    }
    let st = state.clone();
    let ctx = ctx.clone();
    let source = owner.to_string();
    state
        .db
        .transaction::<_, Value, Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let rctx = request_context();
                let env = ctx.scope.environment_id;
                let applied = db::exec(
                    txn,
                    "INSERT INTO messaging_context_authorities (environment_id, namespace, reference, tenant_id, application_id, \
                       state, source, source_version, reason, updated_at) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
                     ON CONFLICT (environment_id, namespace, reference) DO UPDATE SET state = EXCLUDED.state, \
                       source_version = EXCLUDED.source_version, reason = EXCLUDED.reason, updated_at = EXCLUDED.updated_at \
                     WHERE messaging_context_authorities.source_version < EXCLUDED.source_version \
                       AND messaging_context_authorities.source = EXCLUDED.source",
                    vec![
                        env.into(),
                        request.namespace.clone().into(),
                        request.reference.clone().into(),
                        ctx.scope.tenant_id.into(),
                        ctx.scope.application_id.into(),
                        request.state.clone().into(),
                        source.clone().into(),
                        request.source_version.into(),
                        reason.clone().into(),
                        now.into(),
                    ],
                )
                .await?;
                if applied != 1 {
                    crate::metrics::inc("messaging_context_authority_updates_total", &[("outcome", "stale")]);
                    return Ok(json!({ "applied": false, "reason": "STALE_VERSION" }));
                }
                let affected = apply_to_conversations(txn, &ctx, &request, now, &rctx).await?;
                audit::record(
                    txn,
                    AuditEntry {
                        ctx: &ctx,
                        principal_id: None,
                        action: "CONTEXT_AUTHORITY_APPLIED",
                        target_kind: "CONTEXT",
                        target_id: uuid::Uuid::nil(),
                        detail: json!({ "namespace": request.namespace, "reference": request.reference,
                                        "state": request.state, "source_version": request.source_version,
                                        "conversations": affected }),
                    },
                    &rctx,
                    now,
                )
                .await?;
                crate::metrics::inc("messaging_context_authority_updates_total", &[("outcome", "applied")]);
                Ok(json!({ "applied": true, "state": request.state, "conversationsAffected": affected }))
            })
        })
        .await
        .map_err(Error::from)
}

async fn apply_to_conversations<C: ConnectionTrait>(
    txn: &C,
    ctx: &TrustedContext,
    request: &AuthorityUpdate,
    now: DateTime<Utc>,
    rctx: &crate::domain::RequestContext,
) -> Result<i64> {
    let rows: Vec<ConversationRow> =
        service::load_by_context(txn, ctx, &request.namespace, &request.reference).await?;
    let mut affected = 0;
    for row in rows {
        let current = row.state();
        let next = match (request.state.as_str(), current) {
            ("REVOKED", LifecycleState::Active | LifecycleState::Locked) => LifecycleState::Closed,
            ("SUSPENDED", LifecycleState::Active) => LifecycleState::Locked,
            // Unlock only what the authority itself locked.
            ("ACTIVE", LifecycleState::Locked)
                if row.lifecycle_reason.as_deref() == Some("CONTEXT_SUSPENDED") =>
            {
                LifecycleState::Active
            }
            _ => continue,
        };
        let why = match next {
            LifecycleState::Active => "CONTEXT_ACTIVE",
            LifecycleState::Locked => "CONTEXT_SUSPENDED",
            _ => "CONTEXT_REVOKED",
        };
        service::transition(txn, ctx, &row, next, Some(why), None, now, rctx).await?;
        affected += 1;
    }
    Ok(affected)
}
