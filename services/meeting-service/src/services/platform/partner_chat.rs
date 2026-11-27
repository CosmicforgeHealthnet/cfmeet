//! Partner meeting chat (ADR 0062 §8, `MEETING_PLATFORM_CHAT_ENABLED`, default off).
//!
//! First-party meeting chat (Part V H, [`crate::services::unified_messaging`]) signs only
//! CF Meet's own FIRST_PARTY scope, so an environment-owned meeting has no conversation
//! there (`CHAT_UNAVAILABLE`). Partner meeting chat gives such a meeting a MANAGED
//! Messaging conversation in the PARTNER environment's own scope:
//!
//! 1. **Owner.** The environment's meeting system principal, ensured by Platform Access
//!    (`POST /internal/v1/principals/ensure-meeting-platform`, permission
//!    `platform-principal.ensure-meeting-platform`, body
//!    `{environmentId, kind: "BOT_OR_SERVICE", externalReference: "cfmeet:meeting-system"}`).
//! 2. **Conversation.** Resolved idempotently in Messaging with the SAME request bodies as
//!    first-party meeting chat (`POST /internal/v1/meeting-conversations`,
//!    `{meetingRef: "conference:<id>", ownerPrincipalId, securityProfile: "MANAGED",
//!    managedDisclosureAcknowledged: true}`), but signed with the partner environment's
//!    delegated scope (tenant, application, environment, kind, the service account that
//!    created the meeting). Messaging authorizes it with `messaging-meeting.sync-platform`.
//! 3. **Members.** Partner users who joined with their user token
//!    (`meeting_platform_participants`) are synchronised from their admission state and
//!    role (`PUT /internal/v1/meeting-conversations/{id}/members/{principalId}`,
//!    `{state, role?, sourceVersion, reason}`); they read and send with their Messaging
//!    partner session device. Guests without a partner identity are not members.
//! 4. **Lifecycle.** An ended / cancelled / failed conference closes the conversation
//!    (`POST /internal/v1/meeting-conversations/{id}/lifecycle`).
//!
//! A join never fails because chat is unavailable: the join path syncs best-effort and
//! the background pass ([`PartnerChat::sync_once`]) catches up (admissions by a host,
//! removals, bans, role changes, lifecycle).

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use axum::http::Method;
use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient},
    ApplicationEnvironmentKind, DelegatedPlatformContext, OutboundServiceRequest, ServiceId,
};
use sea_orm::{DatabaseConnection, TransactionTrait};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    domain::{AdmissionState, ConferenceStatus},
    error::{ApiError, ApiResult, ErrorCode},
    services::unified_messaging::{
        all, chat_error, conversation_role, conversation_state, exec, meeting_ref, one,
        GatewayError, Peer,
    },
};

/// Platform Access: ensure the environment's meeting system principal (contract §2.4).
pub const ENSURE_PRINCIPAL_PATH: &str = "/internal/v1/principals/ensure-meeting-platform";
/// The only external reference of the environment's meeting system principal.
pub const SYSTEM_PRINCIPAL_REFERENCE: &str = "cfmeet:meeting-system";
/// Messaging: resolve a meeting's conversation (same body as first-party meeting chat).
pub const CONVERSATIONS_PATH: &str = "/internal/v1/meeting-conversations";

/// The partner environment a call is signed for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartnerScope {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub kind: ApplicationEnvironmentKind,
    /// The service account whose credential created the meeting.
    pub service_account_id: Uuid,
}

pub fn environment_kind(kind: &str) -> Option<ApplicationEnvironmentKind> {
    match kind {
        "SANDBOX" => Some(ApplicationEnvironmentKind::Sandbox),
        "TEST" => Some(ApplicationEnvironmentKind::Test),
        "PRODUCTION" => Some(ApplicationEnvironmentKind::Production),
        _ => None,
    }
}

fn kind_str(kind: ApplicationEnvironmentKind) -> &'static str {
    match kind {
        ApplicationEnvironmentKind::Sandbox => "SANDBOX",
        ApplicationEnvironmentKind::Test => "TEST",
        ApplicationEnvironmentKind::Production => "PRODUCTION",
    }
}

/// The signed boundary to Platform Access and Messaging in a PARTNER scope.
#[async_trait]
pub trait PartnerChatGateway: Send + Sync {
    /// One signed call; 2xx answers are `Ok`, every other answer a [`GatewayError`].
    async fn call(
        &self,
        peer: Peer,
        scope: &PartnerScope,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, GatewayError>;
}

pub type SharedPartnerChatGateway = Arc<dyn PartnerChatGateway>;

/// Production gateway: `cf-service-auth` signed internal HTTP with the partner
/// environment's delegated scope signed into every token.
pub struct ServiceAuthPartnerChatGateway {
    client: Arc<InternalServiceClient>,
}

impl ServiceAuthPartnerChatGateway {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl PartnerChatGateway for ServiceAuthPartnerChatGateway {
    async fn call(
        &self,
        peer: Peer,
        scope: &PartnerScope,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, GatewayError> {
        let audience = match peer {
            Peer::PlatformAccess => ServiceId::platform_access(),
            Peer::Messaging => ServiceId::messaging(),
        };
        let delegated = DelegatedPlatformContext::new(
            scope.tenant_id,
            scope.application_id,
            scope.environment_id,
            scope.kind,
            scope.service_account_id,
        );
        let request =
            OutboundServiceRequest::new_operation(audience).with_delegated_context(delegated);
        let response = self
            .client
            .exchange_json(&request, method, path, body)
            .await
            .map_err(|e| match e {
                InternalCallError::Timeout { .. } => GatewayError::Unavailable("timeout"),
                InternalCallError::Connect { .. } => GatewayError::Unavailable("connect"),
                _ => GatewayError::Unavailable("transport"),
            })?;
        let status = response.status();
        let value = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body())
                .map_err(|_| GatewayError::Unavailable("invalid response body"))?
        };
        if (200..300).contains(&status) {
            Ok(value)
        } else {
            Err(GatewayError::from_response(status, &value))
        }
    }
}

/// A conference's partner conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartnerChatLink {
    pub conference_id: Uuid,
    pub conversation_id: Uuid,
    pub owner_principal_id: Uuid,
    pub scope: PartnerScope,
}

/// Counts of one background pass.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PartnerChatReport {
    pub conversations_linked: u32,
    pub members_synced: u32,
    pub lifecycles_closed: u32,
    pub failures: u32,
}

pub struct PartnerChat {
    db: DatabaseConnection,
    gateway: SharedPartnerChatGateway,
}

pub type SharedPartnerChat = Arc<PartnerChat>;

fn unavailable(why: &str) -> ApiError {
    tracing::warn!(why, "partner meeting chat unavailable");
    ApiError::domain(
        ErrorCode::ChatUnavailable,
        "Chat is temporarily unavailable; the meeting continues",
    )
}

impl PartnerChat {
    pub fn new(db: DatabaseConnection, gateway: SharedPartnerChatGateway) -> Self {
        Self { db, gateway }
    }

    pub async fn link_of(&self, conference_id: Uuid) -> ApiResult<Option<PartnerChatLink>> {
        let Some(r) = one(
            &self.db,
            "SELECT conversation_id, owner_principal_id, tenant_id, application_id, environment_id, \
                    environment_kind, service_account_id \
               FROM meeting_platform_chat_links WHERE conference_id = $1",
            vec![conference_id.into()],
        )
        .await?
        else {
            return Ok(None);
        };
        let kind: String = r.try_get_by_index(5)?;
        Ok(Some(PartnerChatLink {
            conference_id,
            conversation_id: r.try_get_by_index(0)?,
            owner_principal_id: r.try_get_by_index(1)?,
            scope: PartnerScope {
                tenant_id: r.try_get_by_index(2)?,
                application_id: r.try_get_by_index(3)?,
                environment_id: r.try_get_by_index(4)?,
                kind: environment_kind(&kind).ok_or_else(|| unavailable("stored kind"))?,
                service_account_id: r.try_get_by_index(6)?,
            },
        }))
    }

    /// The partner scope of an environment-owned conference (`None`: user-owned).
    async fn scope_of(
        &self,
        conference_id: Uuid,
        kind: ApplicationEnvironmentKind,
    ) -> ApiResult<Option<PartnerScope>> {
        let row = one(
            &self.db,
            "SELECT s.platform_tenant_id, s.platform_application_id, s.platform_environment_id, \
                    s.created_by_service_account_id \
               FROM conference_sessions c JOIN meeting_spaces s ON s.id = c.meeting_space_id \
              WHERE c.id = $1 AND s.owner_user_id IS NULL AND s.platform_environment_id IS NOT NULL",
            vec![conference_id.into()],
        )
        .await?;
        let Some(r) = row else { return Ok(None) };
        Ok(Some(PartnerScope {
            tenant_id: r.try_get_by_index(0)?,
            application_id: r.try_get_by_index(1)?,
            environment_id: r.try_get_by_index(2)?,
            kind,
            service_account_id: r.try_get_by_index(3)?,
        }))
    }

    /// Resolves (idempotently) and records the conference's partner conversation.
    /// `kind` is the environment kind Platform Access reported for the joining user.
    pub async fn ensure_link(
        &self,
        conference_id: Uuid,
        kind: ApplicationEnvironmentKind,
    ) -> ApiResult<PartnerChatLink> {
        if let Some(l) = self.link_of(conference_id).await? {
            return Ok(l);
        }
        let scope = self.scope_of(conference_id, kind).await?.ok_or_else(|| {
            ApiError::domain(
                ErrorCode::ChatUnavailable,
                "Partner meeting chat is only available for meetings owned by an application environment",
            )
        })?;
        let ensured = self
            .gateway
            .call(
                Peer::PlatformAccess,
                &scope,
                Method::POST,
                ENSURE_PRINCIPAL_PATH,
                Some(&json!({
                    "environmentId": scope.environment_id,
                    "kind": "BOT_OR_SERVICE",
                    "externalReference": SYSTEM_PRINCIPAL_REFERENCE,
                })),
            )
            .await
            .map_err(|e| chat_error(&e))?;
        let owner = ensured["principalId"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .ok_or_else(|| unavailable("ensure answer"))?;
        if ensured["environmentId"].as_str() != Some(scope.environment_id.to_string().as_str()) {
            return Err(unavailable("ensured principal of another environment"));
        }
        let v = self
            .gateway
            .call(
                Peer::Messaging,
                &scope,
                Method::POST,
                CONVERSATIONS_PATH,
                Some(&json!({
                    "meetingRef": meeting_ref(conference_id),
                    "ownerPrincipalId": owner,
                    "securityProfile": "MANAGED",
                    "managedDisclosureAcknowledged": true,
                })),
            )
            .await
            .map_err(|e| chat_error(&e))?;
        let conversation_id = v["conversation"]["conversationId"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .ok_or_else(|| unavailable("conversation answer"))?;
        exec(
            &self.db,
            "INSERT INTO meeting_platform_chat_links (conference_id, tenant_id, application_id, environment_id, \
                 environment_kind, service_account_id, conversation_id, owner_principal_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) ON CONFLICT (conference_id) DO NOTHING",
            vec![
                conference_id.into(),
                scope.tenant_id.into(),
                scope.application_id.into(),
                scope.environment_id.into(),
                kind_str(scope.kind).into(),
                scope.service_account_id.into(),
                conversation_id.into(),
                owner.into(),
            ],
        )
        .await?;
        self.link_of(conference_id)
            .await?
            .ok_or_else(|| ApiError::InternalError("partner chat link vanished".into()))
    }

    /// Applies a partner user's CURRENT admission state / role to the conversation.
    /// Returns whether anything was sent.
    pub async fn sync_participant(
        &self,
        link: &PartnerChatLink,
        participant_id: Uuid,
    ) -> ApiResult<bool> {
        let txn = self.db.begin().await?;
        exec(
            &txn,
            "SELECT 1 FROM meeting_platform_chat_links WHERE conference_id = $1 FOR UPDATE",
            vec![link.conference_id.into()],
        )
        .await?;
        let Some(row) = one(
            &txn,
            "SELECT m.principal_id, p.admission_state, p.role, s.synced_state, s.synced_role \
               FROM meeting_platform_participants m \
               JOIN conference_participants p ON p.id = m.participant_id \
               LEFT JOIN meeting_platform_chat_members s ON s.participant_id = m.participant_id \
              WHERE m.participant_id = $1 AND m.conference_id = $2",
            vec![participant_id.into(), link.conference_id.into()],
        )
        .await?
        else {
            return Ok(false);
        };
        let principal: Uuid = row.try_get_by_index(0)?;
        let state: String = row.try_get_by_index(1)?;
        let role: String = row.try_get_by_index(2)?;
        let prev_state: Option<String> = row.try_get_by_index(3)?;
        let prev_role: Option<String> = row.try_get_by_index(4)?;
        let admission = AdmissionState::parse(&state)
            .ok_or_else(|| ApiError::InternalError("admission state".into()))?;
        let target = match conversation_state(admission) {
            Some(s) => s,
            None if prev_state.as_deref() == Some("ACTIVE") => "REMOVED",
            None => return Ok(false),
        };
        let role = conversation_role(
            crate::domain::Role::parse(&role)
                .ok_or_else(|| ApiError::InternalError("role".into()))?,
        );
        if prev_state.as_deref() == Some(target)
            && (target != "ACTIVE" || prev_role.as_deref() == Some(role))
        {
            return Ok(false);
        }
        let version: i64 = one(
            &txn,
            "SELECT nextval('meeting_platform_chat_sync_version_seq')",
            vec![],
        )
        .await?
        .ok_or_else(|| ApiError::InternalError("sequence".into()))?
        .try_get_by_index(0)?;
        let mut body = json!({
            "state": target,
            "sourceVersion": version,
            "reason": match target {
                "ACTIVE" => "MEETING_ADMITTED",
                "BANNED" => "MEETING_BANNED",
                _ => "MEETING_REMOVED",
            },
        });
        if target == "ACTIVE" {
            body["role"] = json!(role);
        }
        let path = format!(
            "{CONVERSATIONS_PATH}/{}/members/{principal}",
            link.conversation_id
        );
        self.gateway
            .call(
                Peer::Messaging,
                &link.scope,
                Method::PUT,
                &path,
                Some(&body),
            )
            .await
            .map_err(|e| chat_error(&e))?;
        exec(
            &txn,
            "INSERT INTO meeting_platform_chat_members (participant_id, conference_id, principal_id, synced_state, synced_role, synced_version, synced_at) \
             VALUES ($1, $2, $3, $4, $5, $6, now()) ON CONFLICT (participant_id) DO UPDATE SET \
               synced_state = EXCLUDED.synced_state, synced_role = EXCLUDED.synced_role, \
               synced_version = EXCLUDED.synced_version, synced_at = now()",
            vec![
                participant_id.into(),
                link.conference_id.into(),
                principal.into(),
                target.into(),
                (target == "ACTIVE").then_some(role).map(str::to_string).into(),
                version.into(),
            ],
        )
        .await?;
        txn.commit().await?;
        Ok(true)
    }

    /// Fast path after a partner user's join: link the conversation and admit the member.
    pub async fn after_join(
        &self,
        conference_id: Uuid,
        participant_id: Uuid,
        kind: ApplicationEnvironmentKind,
    ) -> ApiResult<PartnerChatLink> {
        let link = self.ensure_link(conference_id, kind).await?;
        self.sync_participant(&link, participant_id).await?;
        Ok(link)
    }

    /// Partner users of linked conferences whose admission / role changed since the last
    /// applied sync.
    async fn sync_members(&self, limit: u64) -> ApiResult<u32> {
        let rows = all(
            &self.db,
            "SELECT p.id, p.conference_id FROM meeting_platform_participants m \
               JOIN conference_participants p ON p.id = m.participant_id \
               JOIN meeting_platform_chat_links l ON l.conference_id = m.conference_id \
               LEFT JOIN meeting_platform_chat_members s ON s.participant_id = p.id \
             WHERE (s.participant_id IS NULL AND p.admission_state IN ('ADMITTED','REMOVED','BANNED')) \
                OR (s.participant_id IS NOT NULL AND ( \
                     (p.admission_state = 'ADMITTED' AND (s.synced_state <> 'ACTIVE' OR s.synced_role IS DISTINCT FROM \
                        CASE WHEN p.role IN ('OWNER','HOST','CO_HOST','MODERATOR') THEN 'ADMIN' ELSE 'MEMBER' END)) \
                  OR (p.admission_state = 'REMOVED' AND s.synced_state <> 'REMOVED') \
                  OR (p.admission_state = 'BANNED' AND s.synced_state <> 'BANNED') \
                  OR (p.admission_state IN ('WAITING','DENIED') AND s.synced_state = 'ACTIVE'))) \
             ORDER BY p.updated_at LIMIT $1",
            vec![(limit as i64).into()],
        )
        .await?;
        let mut synced = 0;
        for r in rows {
            let pid: Uuid = r.try_get_by_index(0)?;
            let cid: Uuid = r.try_get_by_index(1)?;
            let Some(link) = self.link_of(cid).await? else {
                continue;
            };
            if self.sync_participant(&link, pid).await? {
                synced += 1;
            }
        }
        Ok(synced)
    }

    /// Ended / cancelled / failed conferences close their partner conversation (once).
    async fn close_lifecycles(&self, limit: u64) -> ApiResult<u32> {
        let rows = all(
            &self.db,
            "SELECT l.conference_id, c.version, c.status FROM meeting_platform_chat_links l \
               JOIN conference_sessions c ON c.id = l.conference_id \
             WHERE c.status IN ('ENDED','CANCELLED','FAILED') AND l.lifecycle_synced_state IS DISTINCT FROM 'CLOSED' \
             LIMIT $1",
            vec![(limit as i64).into()],
        )
        .await?;
        let mut n = 0;
        for r in rows {
            let conference_id: Uuid = r.try_get_by_index(0)?;
            let version: i32 = r.try_get_by_index(1)?;
            let status: String = r.try_get_by_index(2)?;
            let Some(link) = self.link_of(conference_id).await? else {
                continue;
            };
            let body = json!({
                "lifecycleState": "CLOSED",
                "sourceVersion": version as i64,
                "reason": match ConferenceStatus::parse(&status) {
                    Some(ConferenceStatus::Cancelled) => "MEETING_CANCELLED",
                    Some(ConferenceStatus::Failed) => "MEETING_FAILED",
                    _ => "MEETING_ENDED",
                },
            });
            let path = format!("{CONVERSATIONS_PATH}/{}/lifecycle", link.conversation_id);
            match self
                .gateway
                .call(
                    Peer::Messaging,
                    &link.scope,
                    Method::POST,
                    &path,
                    Some(&body),
                )
                .await
            {
                // Applied, or already closed by a newer update: either way it is done.
                Ok(_) | Err(GatewayError::Refused { .. }) => {}
                Err(e) => return Err(chat_error(&e)),
            }
            exec(
                &self.db,
                "UPDATE meeting_platform_chat_links SET lifecycle_synced_state = 'CLOSED', updated_at = now() \
                 WHERE conference_id = $1",
                vec![conference_id.into()],
            )
            .await?;
            n += 1;
        }
        Ok(n)
    }

    /// Live conferences with an admitted partner user but no conversation yet (a join
    /// whose fast path failed). The environment kind is the one Platform Access reported
    /// at join time.
    async fn link_pending(&self, limit: u64) -> ApiResult<u32> {
        let rows = all(
            &self.db,
            "SELECT m.conference_id, min(m.environment_kind) FROM meeting_platform_participants m \
               JOIN conference_sessions c ON c.id = m.conference_id \
               JOIN conference_participants p ON p.id = m.participant_id \
               LEFT JOIN meeting_platform_chat_links l ON l.conference_id = m.conference_id \
             WHERE l.conference_id IS NULL AND c.status = 'LIVE' AND p.admission_state = 'ADMITTED' \
             GROUP BY m.conference_id LIMIT $1",
            vec![(limit as i64).into()],
        )
        .await?;
        let mut n = 0;
        for r in rows {
            let cid: Uuid = r.try_get_by_index(0)?;
            let kind: String = r.try_get_by_index(1)?;
            if let Some(kind) = environment_kind(&kind) {
                self.ensure_link(cid, kind).await?;
                n += 1;
            }
        }
        Ok(n)
    }

    /// One pass of every background responsibility (idempotent, safe on several
    /// instances at once).
    pub async fn sync_once(&self) -> PartnerChatReport {
        let mut report = PartnerChatReport::default();
        macro_rules! step {
            ($field:ident, $e:expr) => {
                match $e {
                    Ok(n) => report.$field += n,
                    Err(e) => {
                        report.failures += 1;
                        tracing::warn!(error = %e, step = stringify!($field), "partner meeting chat sync step deferred");
                    }
                }
            };
        }
        step!(conversations_linked, self.link_pending(50).await);
        step!(members_synced, self.sync_members(200).await);
        step!(lifecycles_closed, self.close_lifecycles(50).await);
        report
    }
}

/// Runs [`PartnerChat::sync_once`] every `interval` until the process exits.
pub fn spawn_partner_chat_sync(
    chat: SharedPartnerChat,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let report = chat.sync_once().await;
            if report.failures > 0 {
                tracing::warn!(?report, "partner meeting chat pass had deferred steps");
            }
        }
    })
}
