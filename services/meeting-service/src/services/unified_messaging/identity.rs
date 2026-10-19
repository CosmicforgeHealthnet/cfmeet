//! Part V H §104 — the trusted CF Meet login → Platform Access principal → device →
//! short-lived Messaging client token bridge.
//!
//! 1. Meeting verifies the user's login itself (its own HS256 access token, revocation
//!    store checked — fail closed when it cannot be checked).
//! 2. The account is mapped to its `NATIVE_CF_USER` principal in CF Meet's FIRST_PARTY
//!    environment (`POST /internal/v1/principals/ensure`, idempotent). The reference is the
//!    opaque `cfmeet-user:<account id>` — never an email, name or telephone number. Guests
//!    get a `TEMPORARY_EXTERNAL_PARTICIPANT` principal per conference participant.
//! 3. Device registration (proof of possession by the client's own MLS key), KeyPackage
//!    publication, device revocation, client-token issue and session revocation are
//!    RELAYED to Messaging's delegated surface with the principal signed into Meeting's
//!    service token. The client receives Messaging's answer — a device-bound,
//!    short-lived client token at most — and never a service credential.
//! 4. Logging out of CF Meet revokes every Messaging client session of the principal
//!    (durable job, retried until Messaging confirms).

use axum::http::Method;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    all, chat_error, exec, one, Acting, GatewayError, Peer, PrincipalKind, UnifiedMessaging,
};
use crate::{
    domain::IdentityType,
    error::{ApiError, ApiResult, ErrorCode},
    models::conference_participants,
    services::ops_metrics::{inc, OPS_METRICS as OM},
};

pub const USER_REFERENCE_PREFIX: &str = "cfmeet-user:";
pub const PARTICIPANT_REFERENCE_PREFIX: &str = "cfmeet-participant:";

/// The client operations the bridge relays (method, Meeting path suffix → Messaging path).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeOperation {
    RegistrationChallenge,
    RegisterDevice,
    PublishKeyPackages(Uuid),
    RevokeDevice(Uuid),
    IssueClientToken,
    RevokeAllSessions,
}

impl BridgeOperation {
    fn messaging_path(&self) -> String {
        let base = "/internal/v1/delegated/v1/messaging";
        match self {
            BridgeOperation::RegistrationChallenge => {
                format!("{base}/devices/registration-challenges")
            }
            BridgeOperation::RegisterDevice => format!("{base}/devices"),
            BridgeOperation::PublishKeyPackages(d) => format!("{base}/devices/{d}/key-packages"),
            BridgeOperation::RevokeDevice(d) => format!("{base}/devices/{d}/revoke"),
            BridgeOperation::IssueClientToken => format!("{base}/client-tokens"),
            BridgeOperation::RevokeAllSessions => format!("{base}/client-sessions/revoke-all"),
        }
    }
}

impl UnifiedMessaging {
    async fn ensure_principal(
        &self,
        subject_kind: &'static str,
        subject_id: Uuid,
        kind: PrincipalKind,
        reference: String,
    ) -> ApiResult<Uuid> {
        let env = self.environment_id();
        if let Some(r) = one(
            &self.db,
            "SELECT principal_id FROM communication_principal_links \
             WHERE subject_kind = $1 AND subject_id = $2 AND environment_id = $3",
            vec![subject_kind.into(), subject_id.into(), env.into()],
        )
        .await?
        {
            return Ok(r.try_get_by_index(0)?);
        }
        let kind_str = match kind {
            PrincipalKind::NativeCfUser => "NATIVE_CF_USER",
            PrincipalKind::TemporaryExternalParticipant => "TEMPORARY_EXTERNAL_PARTICIPANT",
            _ => return Err(ApiError::InternalError("unsupported principal kind".into())),
        };
        let v = self
            .gateway
            .call(
                Peer::PlatformAccess,
                None,
                Method::POST,
                "/internal/v1/principals/ensure",
                &[],
                Some(&json!({ "kind": kind_str, "externalReference": reference })),
            )
            .await
            .map_err(|e| match &e {
                GatewayError::Refused { code, .. } if code == "INVALID_STATE" => ApiError::domain(
                    ErrorCode::PermissionDenied,
                    "Messaging is not available for this account",
                ),
                _ => chat_error(&e),
            })?;
        let principal = v["principal"]["id"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .ok_or_else(|| {
                ApiError::domain(
                    ErrorCode::ChatUnavailable,
                    "Unexpected Platform Access answer",
                )
            })?;
        exec(
            &self.db,
            "INSERT INTO communication_principal_links (subject_kind, subject_id, environment_id, principal_id, principal_kind) \
             VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING",
            vec![
                subject_kind.into(),
                subject_id.into(),
                env.into(),
                principal.into(),
                kind_str.into(),
            ],
        )
        .await?;
        inc(&OM.unified_messaging_principals_ensured);
        Ok(principal)
    }

    /// The `NATIVE_CF_USER` principal of a CF Meet account (created on first use).
    pub async fn principal_for_user(&self, user_id: Uuid) -> ApiResult<Uuid> {
        self.ensure_principal(
            "USER",
            user_id,
            PrincipalKind::NativeCfUser,
            format!("{USER_REFERENCE_PREFIX}{user_id}"),
        )
        .await
    }

    /// The principal a conference participant acts as: the account's own principal for
    /// registered users, a per-participant temporary principal otherwise.
    pub async fn principal_for_participant(
        &self,
        p: &conference_participants::Model,
    ) -> ApiResult<(Uuid, PrincipalKind)> {
        match (p.identity_type, p.user_id) {
            (IdentityType::RegisteredUser, Some(user)) => Ok((
                self.principal_for_user(user).await?,
                PrincipalKind::NativeCfUser,
            )),
            _ => Ok((
                self.ensure_principal(
                    "PARTICIPANT",
                    p.id,
                    PrincipalKind::TemporaryExternalParticipant,
                    format!("{PARTICIPANT_REFERENCE_PREFIX}{}", p.id),
                )
                .await?,
                PrincipalKind::TemporaryExternalParticipant,
            )),
        }
    }

    /// Relays one client operation for a VERIFIED CF Meet account and returns Messaging's
    /// status and body unchanged (its typed error envelope included).
    pub async fn bridge(
        &self,
        user_id: Uuid,
        op: BridgeOperation,
        body: Option<Value>,
    ) -> ApiResult<(u16, Value)> {
        let principal = self.principal_for_user(user_id).await?;
        let acting = Acting {
            principal_id: principal,
            kind: PrincipalKind::NativeCfUser,
        };
        let body = match (op, body) {
            (BridgeOperation::RevokeAllSessions | BridgeOperation::RevokeDevice(_), _) => None,
            (_, Some(b)) => Some(b),
            (_, None) => Some(json!({})),
        };
        self.gateway
            .call_raw(
                Peer::Messaging,
                Some(acting),
                Method::POST,
                &op.messaging_path(),
                &[],
                body.as_ref(),
            )
            .await
            .map_err(|e| chat_error(&e))
    }

    /// Logout: queue (and try now) the revocation of every Messaging client session of
    /// the account's principal. Never fails the logout itself.
    pub async fn revoke_sessions_for_user(&self, user_id: Uuid, reason: &'static str) {
        let env = self.environment_id();
        let principal: Option<Uuid> = match one(
            &self.db,
            "SELECT principal_id FROM communication_principal_links \
             WHERE subject_kind = 'USER' AND subject_id = $1 AND environment_id = $2",
            vec![user_id.into(), env.into()],
        )
        .await
        {
            Ok(r) => r.and_then(|r| r.try_get_by_index(0).ok()),
            Err(e) => {
                tracing::error!(error = %e, "messaging logout revocation could not be queued");
                return;
            }
        };
        // No principal: this account never used Messaging — nothing to revoke.
        let Some(principal) = principal else { return };
        if let Err(e) = exec(
            &self.db,
            "INSERT INTO communication_session_revocations (id, principal_id, environment_id, reason) VALUES ($1, $2, $3, $4)",
            vec![Uuid::new_v4().into(), principal.into(), env.into(), reason.into()],
        )
        .await
        {
            tracing::error!(error = %e, "messaging logout revocation could not be queued");
            return;
        }
        let _ = self.process_revocations(5).await;
    }

    /// Due revocation jobs (row-locked; several instances never double-process one).
    pub async fn process_revocations(&self, limit: u64) -> ApiResult<u32> {
        use sea_orm::TransactionTrait;
        let mut done = 0;
        for _ in 0..limit {
            let txn = self.db.begin().await?;
            let Some(r) = one(
                &txn,
                "SELECT id, principal_id FROM communication_session_revocations \
                 WHERE status = 'PENDING' AND next_attempt_at <= now() ORDER BY next_attempt_at \
                 LIMIT 1 FOR UPDATE SKIP LOCKED",
                vec![],
            )
            .await?
            else {
                break;
            };
            let id: Uuid = r.try_get_by_index(0)?;
            let principal: Uuid = r.try_get_by_index(1)?;
            let result = self
                .gateway
                .call(
                    Peer::Messaging,
                    Some(Acting {
                        principal_id: principal,
                        kind: PrincipalKind::NativeCfUser,
                    }),
                    Method::POST,
                    &BridgeOperation::RevokeAllSessions.messaging_path(),
                    &[],
                    None,
                )
                .await;
            match result {
                // A principal Messaging no longer authorizes has no live session either.
                Ok(_) | Err(GatewayError::Refused { status: 403, .. }) => {
                    exec(
                        &txn,
                        "UPDATE communication_session_revocations SET status = 'DONE', done_at = now(), attempts = attempts + 1 WHERE id = $1",
                        vec![id.into()],
                    )
                    .await?;
                    inc(&OM.unified_messaging_revocations_done);
                    done += 1;
                }
                Err(e) => {
                    let code = e
                        .code()
                        .unwrap_or("UNAVAILABLE")
                        .chars()
                        .take(64)
                        .collect::<String>();
                    exec(
                        &txn,
                        "UPDATE communication_session_revocations SET attempts = attempts + 1, last_error = $2, \
                           next_attempt_at = now() + make_interval(secs => LEAST(300, 2 ^ LEAST(attempts, 8))) WHERE id = $1",
                        vec![id.into(), code.into()],
                    )
                    .await?;
                    txn.commit().await?;
                    inc(&OM.unified_messaging_revocations_failed);
                    break;
                }
            }
            txn.commit().await?;
        }
        Ok(done)
    }

    /// Pending revocations (operations evidence).
    pub async fn pending_revocations(&self) -> ApiResult<i64> {
        Ok(all(
            &self.db,
            "SELECT count(*) FROM communication_session_revocations WHERE status = 'PENDING'",
            vec![],
        )
        .await?
        .first()
        .map(|r| r.try_get_by_index::<i64>(0))
        .transpose()?
        .unwrap_or(0))
    }
}
