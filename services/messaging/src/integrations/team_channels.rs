//! Gap audit G7 — team channels: a GROUP conversation of an organization team, in the
//! organization's OWN first-party Messaging scope, whose membership follows the team.
//!
//! The Meeting Service (granted `messaging-organization.govern`) reads the team's ACTIVE
//! members from the Organizations Service, maps them to their principals in the
//! organization's scope and sends the whole SNAPSHOT here:
//!
//! * the channel is resolved idempotently by its opaque team reference (context namespace
//!   [`NAMESPACE`]); the first snapshot creates it (MANAGED: organization governance needs
//!   server-readable content) with the given owner;
//! * snapshots are VERSION-FENCED ([`super::advance_fence`]): one at or below the last
//!   applied version changes nothing, so a delayed or replayed snapshot cannot restore a
//!   former team member;
//! * listed principals that are not active members join as MEMBER with
//!   `authority_source = 'ORGANIZATION_TEAM'` (bans, the lifecycle and the member limit
//!   still apply; a refused principal is reported as skipped);
//! * active members that joined BY THE TEAM and are no longer listed are removed
//!   (`TEAM_MEMBERSHIP_ENDED`). The owner and members added any other way (an admin's
//!   guest, an invitation) are never removed by the sync.
//!
//! Messaging receives no Organizations team event: the sync runs when the Meeting Service
//! sends a snapshot (on channel creation and on an explicit resync).

use std::collections::BTreeSet;

use sea_orm::{FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::advance_fence;
use crate::{
    audit::{self, AuditEntry},
    conversations::{
        service::{self, request_context, CreateRequest},
        ContextClass, ContextRef,
    },
    db,
    domain::SecurityProfile,
    error::{Error, ErrorCode, Result},
    http::middleware::Acting,
    identity::TrustedContext,
    membership,
    state::AppState,
};

/// Context namespace of team channels (reserved: customers cannot create one in a
/// first-party environment).
pub const NAMESPACE: &str = "organization-team";
/// Fence source of team snapshots.
pub const SOURCE: &str = "organization-team";
pub const AUTHORITY_SOURCE: &str = "ORGANIZATION_TEAM";
pub const MAX_MEMBERS: usize = 1000;
const FENCE_KEY: &str = "team-members";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TeamSnapshot {
    /// Opaque team reference (`[A-Za-z0-9._:/-]{1,256}`), e.g. `organization:{id}:team:{id}`.
    pub team_ref: String,
    /// The channel owner when the snapshot creates it (ignored afterwards).
    pub owner_principal_id: Uuid,
    /// Display title when the snapshot creates the channel.
    #[serde(default)]
    pub title: Option<String>,
    /// Every ACTIVE team member's principal in the organization's scope.
    pub member_principal_ids: Vec<Uuid>,
    /// Monotonic version of the snapshot (e.g. the time it was read, in milliseconds).
    pub source_version: i64,
}

impl TeamSnapshot {
    pub fn validate(&self) -> Result<()> {
        if self.member_principal_ids.len() > MAX_MEMBERS {
            return Err(Error::validation(format!(
                "memberPrincipalIds may list at most {MAX_MEMBERS} principals"
            )));
        }
        if self.source_version < 0 {
            return Err(Error::validation("sourceVersion must not be negative"));
        }
        ContextRef {
            namespace: NAMESPACE.into(),
            reference: self.team_ref.clone(),
            version: 1,
        }
        .validate()
        .map_err(Error::validation)
    }
}

#[derive(Debug, FromQueryResult)]
struct TeamMember {
    principal_id: Uuid,
}

/// `PUT /internal/v1/team-channels`. `owner` is verified ACTIVE; `inactive` lists the
/// snapshot principals Platform Access no longer reports ACTIVE (never added).
pub async fn sync(
    state: &AppState,
    ctx: &TrustedContext,
    owner: &Acting,
    snapshot: TeamSnapshot,
    inactive: BTreeSet<Uuid>,
) -> Result<(u16, Value)> {
    snapshot.validate()?;
    let key = format!(
        "team-channel.{}",
        &hex::encode(Sha256::digest(snapshot.team_ref.as_bytes()))[..40]
    );
    let created = service::create(
        state,
        ctx,
        owner,
        CreateRequest {
            context_class: ContextClass::Group,
            security_profile: Some(SecurityProfile::Managed),
            counterpart_principal_id: None,
            context: Some(ContextRef {
                namespace: NAMESPACE.into(),
                reference: snapshot.team_ref.clone(),
                version: 1,
            }),
            expires_at: None,
            expiry_grace_seconds: None,
            retention_seconds: None,
            predecessor_conversation_id: None,
            profile_change_disclosure_acknowledged: false,
            read_receipts: None,
            expected_policy_version: None,
            managed_disclosure_acknowledged: true,
            title: snapshot.title.clone(),
            avatar_ref: None,
        },
        &key,
    )
    .await?;
    let conversation_id = created.body["conversationId"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or(Error::code(ErrorCode::InternalError))?;
    let st = state.clone();
    let ctx2 = ctx.clone();
    let snap = snapshot.clone();
    let result = state
        .db
        .transaction::<_, Value, Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let rctx = request_context();
                let conversation = service::load(txn, &ctx2, conversation_id, true)
                    .await?
                    .filter(|c| c.context_namespace.as_deref() == Some(NAMESPACE))
                    .ok_or(Error::code(ErrorCode::NotFound))?;
                if !advance_fence(txn, conversation.id, FENCE_KEY, SOURCE, snap.source_version, now).await? {
                    crate::metrics::inc("messaging_sync_updates_total", &[("outcome", "stale")]);
                    return Ok(json!({ "applied": false, "reason": "STALE_VERSION" }));
                }
                let wanted: BTreeSet<Uuid> = snap.member_principal_ids.iter().copied().collect();
                let by_team: Vec<TeamMember> = db::all(
                    txn,
                    "SELECT principal_id FROM messaging_conversation_members WHERE conversation_id = $1 \
                       AND state = 'ACTIVE' AND authority_source = $2 AND role <> 'OWNER' ORDER BY principal_id",
                    vec![conversation.id.into(), AUTHORITY_SOURCE.into()],
                )
                .await?;
                let mut removed = Vec::new();
                for m in by_team {
                    if wanted.contains(&m.principal_id) && !inactive.contains(&m.principal_id) {
                        continue;
                    }
                    membership::remove_in(txn, &ctx2, &conversation, m.principal_id, false, "TEAM_MEMBERSHIP_ENDED", None, &rctx, now)
                        .await?;
                    removed.push(m.principal_id);
                }
                let mut added = Vec::new();
                let mut skipped = Vec::new();
                for p in &wanted {
                    if inactive.contains(p) {
                        skipped.push(json!({ "principalId": p, "reason": "PRINCIPAL_INACTIVE" }));
                        continue;
                    }
                    match membership::activate_from_authority(txn, &st, &ctx2, &conversation, *p, "MEMBER", AUTHORITY_SOURCE, &rctx, now)
                        .await
                    {
                        Ok((201, _)) => added.push(*p),
                        Ok(_) => {}
                        // A refusal (banned, member limit, closed channel) skips the
                        // principal; the refusal checks run before anything is written.
                        Err(Error::Code { code, reason, .. }) if code != ErrorCode::InternalError => {
                            skipped.push(json!({ "principalId": p, "reason": reason.unwrap_or(code.as_str()) }))
                        }
                        Err(e) => return Err(e),
                    }
                }
                audit::record(
                    txn,
                    AuditEntry {
                        ctx: &ctx2,
                        principal_id: None,
                        action: "TEAM_CHANNEL_SYNCED",
                        target_kind: "CONVERSATION",
                        target_id: conversation.id,
                        detail: json!({ "source_version": snap.source_version, "listed": wanted.len(),
                                        "added": added.len(), "removed": removed.len(), "skipped": skipped.len() }),
                    },
                    &rctx,
                    now,
                )
                .await?;
                crate::metrics::inc("messaging_sync_updates_total", &[("outcome", "applied")]);
                Ok(json!({ "applied": true, "added": added, "removed": removed, "skipped": skipped }))
            })
        })
        .await
        .map_err(Error::from)?;
    let conversation = service::load(&state.db, ctx, conversation_id, false)
        .await?
        .ok_or(Error::code(ErrorCode::NotFound))?;
    let mut body = result;
    body["created"] = json!(created.status == 201);
    body["conversation"] = service::full_view(&state.db, &conversation).await?;
    Ok((created.status, body))
}
