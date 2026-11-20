//! ADR 0064 delegated actor: an app acting for ONE CF Meet user with an OAuth access token
//! (`cfoa_…`, Platform Access `credentialKind = OAUTH_ACCESS`), forwarded by the API
//! Gateway on the `/v1/meetings/*` family exactly like a platform credential.
//!
//! The receiver middleware ([`crate::middleware::platform_auth`]) admits it only when
//! `MEETING_DELEGATED_APPS_ENABLED` is on, and only on the machine-callable operations
//! ([`crate::services::platform::operations`]) — never join, media, recordings, chat or
//! administration. The account comes from the credential's restricted principal
//! (`cfmeet-user:<uuid>`), must be ACTIVE (not a guest) and the credential must postdate
//! the account's revocation epoch (`credentials_not_before`). Meetings:
//! * `meetings:write` (internal `meetings.write`): only meetings the user OWNS;
//! * `meetings:read` (internal `meetings.read`): meetings the user owns, or is an active
//!   HOST / CO_HOST member of.
//!
//! Anything else is 404, exactly as for an unknown id. The request then runs as the
//! user (never as a platform administrator) and every audit record it writes carries
//! `{"via": "app", "viaAppId", "viaGrantId"}` ([`current_via_app`]).

use std::{collections::BTreeSet, future::Future};

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    domain::{MemberStatus, Role},
    error::{ApiError, ApiResult, ErrorCode},
    models::{meeting_spaces, users::UserRole, users::UserStatus},
    services::platform::{authority::OauthCredential, scopes},
};

/// The OAuth scope of an internal meetings scope (§0 mapping).
pub fn oauth_scope_of(internal: &str) -> Option<&'static str> {
    match internal {
        scopes::MEETINGS_READ => Some("meetings:read"),
        scopes::MEETINGS_WRITE => Some("meetings:write"),
        _ => None,
    }
}

/// An app acting for a CF Meet user (ADR 0064 `DelegatedUser`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegatedUser {
    pub account_id: Uuid,
    pub app_id: Uuid,
    pub grant_id: Uuid,
    /// Scopes Platform Access reported for the grant (internal names, OAuth names
    /// accepted too).
    pub scopes: BTreeSet<String>,
}

impl DelegatedUser {
    /// Whether the grant holds the internal meetings scope `internal` (or its OAuth name).
    pub fn has_scope(&self, internal: &str) -> bool {
        self.scopes.contains(internal)
            || oauth_scope_of(internal).is_some_and(|o| self.scopes.contains(o))
    }

    /// Whether this delegated user may act on `space` with `scope`: write = owner only;
    /// read = owner or an active HOST / CO_HOST member.
    pub async fn may_act_on<C: ConnectionTrait>(
        &self,
        conn: &C,
        space: &meeting_spaces::Model,
        scope: &str,
    ) -> ApiResult<bool> {
        if space.owner_user_id == Some(self.account_id) {
            return Ok(true);
        }
        if scope != scopes::MEETINGS_READ || space.owner_user_id.is_none() {
            return Ok(false);
        }
        is_host_member(conn, space.id, self.account_id).await
    }
}

/// Whether `user` is an active HOST or CO_HOST member of `space`.
pub async fn is_host_member<C: ConnectionTrait>(
    conn: &C,
    space: Uuid,
    user: Uuid,
) -> ApiResult<bool> {
    let row = conn
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT count(*) AS n FROM meeting_members \
             WHERE meeting_space_id = $1 AND user_id = $2 AND status = $3 AND role IN ($4, $5)",
            [
                space.into(),
                user.into(),
                MemberStatus::Active.as_str().into(),
                Role::Host.as_str().into(),
                Role::CoHost.as_str().into(),
            ],
        ))
        .await?;
    Ok(row
        .map(|r| r.try_get::<i64>("", "n"))
        .transpose()?
        .unwrap_or(0)
        > 0)
}

/// Why a delegated credential is refused (operator logs only; the caller always gets the
/// normalized 401).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    NotAUserToken,
    PrincipalMismatch,
    UnknownAccount,
    AccountNotActive,
    RevokedByEpoch,
}

impl Refusal {
    pub fn as_str(&self) -> &'static str {
        match self {
            Refusal::NotAUserToken => "oauth_not_a_user_token",
            Refusal::PrincipalMismatch => "oauth_principal_mismatch",
            Refusal::UnknownAccount => "oauth_unknown_account",
            Refusal::AccountNotActive => "oauth_account_not_active",
            Refusal::RevokedByEpoch => "oauth_revoked_by_epoch",
        }
    }
}

/// The CF Meet account of an OAuth access credential: the restricted principal's
/// `cfmeet-user:<uuid>` reference when Platform Access reports it, otherwise the
/// account linked to that principal (`communication_principal_links`, USER).
pub async fn account_of<C: ConnectionTrait>(
    conn: &C,
    credential: &OauthCredential,
) -> ApiResult<Result<Uuid, Refusal>> {
    if let Some(reference) = &credential.principal_reference {
        return Ok(OauthCredential::account_of_reference(reference).ok_or(Refusal::NotAUserToken));
    }
    let Some(principal) = credential.restricted_principal else {
        return Ok(Err(Refusal::NotAUserToken));
    };
    let rows = conn
        .query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT DISTINCT subject_id FROM communication_principal_links \
             WHERE subject_kind = 'USER' AND principal_kind = 'NATIVE_CF_USER' AND principal_id = $1",
            [principal.into()],
        ))
        .await?;
    match rows.as_slice() {
        [one] => Ok(Ok(one.try_get::<Uuid>("", "subject_id")?)),
        _ => Ok(Err(Refusal::UnknownAccount)),
    }
}

/// The account must be ACTIVE (not a guest) and the credential issued after the account's
/// revocation epoch (`cutoff`, e.g. "sign out everywhere").
pub fn check_account(
    status: UserStatus,
    role: UserRole,
    cutoff: Option<DateTime<Utc>>,
    issued_at: Option<DateTime<Utc>>,
) -> Result<(), Refusal> {
    if matches!(status, UserStatus::Inactive | UserStatus::Suspended) || role == UserRole::Guest {
        return Err(Refusal::AccountNotActive);
    }
    match (cutoff, issued_at) {
        (Some(c), Some(i)) if i <= c => Err(Refusal::RevokedByEpoch),
        // Without an issue time the epoch cannot be proven: fail closed.
        (Some(_), None) => Err(Refusal::RevokedByEpoch),
        _ => Ok(()),
    }
}

tokio::task_local! {
    static VIA_APP: (Uuid, Uuid);
}

/// Runs `fut` with the acting app recorded for audit (`via = app`).
pub async fn with_via_app<F: Future>(app_id: Uuid, grant_id: Uuid, fut: F) -> F::Output {
    VIA_APP.scope((app_id, grant_id), fut).await
}

/// `(appId, grantId)` of the app the current request runs for, if any.
pub fn current_via_app() -> Option<(Uuid, Uuid)> {
    VIA_APP.try_with(|v| *v).ok()
}

/// Audit metadata with the acting app (unchanged outside a delegated request).
pub fn audit_metadata(metadata: Value) -> Value {
    let Some((app, grant)) = current_via_app() else {
        return metadata;
    };
    let mut m = match metadata {
        Value::Object(m) => m,
        Value::Null => Default::default(),
        other => {
            let mut m = serde_json::Map::new();
            m.insert("value".into(), other);
            m
        }
    };
    m.insert("via".into(), json!("app"));
    m.insert("viaAppId".into(), json!(app));
    m.insert("viaGrantId".into(), json!(grant));
    Value::Object(m)
}

/// The normalized refusal of a delegated request on a meeting it may not touch.
pub fn not_found(conference: bool) -> ApiError {
    if conference {
        ApiError::domain(ErrorCode::ConferenceNotFound, "Conference not found")
    } else {
        ApiError::domain(ErrorCode::MeetingNotFound, "Meeting not found")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_accept_internal_and_oauth_names() {
        let mut d = DelegatedUser {
            account_id: Uuid::new_v4(),
            app_id: Uuid::new_v4(),
            grant_id: Uuid::new_v4(),
            scopes: ["meetings.read".to_string()].into(),
        };
        assert!(d.has_scope(scopes::MEETINGS_READ));
        assert!(!d.has_scope(scopes::MEETINGS_WRITE));
        d.scopes = ["meetings:write".to_string()].into();
        assert!(d.has_scope(scopes::MEETINGS_WRITE));
        assert!(!d.has_scope("messages.send"));
    }

    #[test]
    fn accounts_must_be_active_and_newer_than_the_epoch() {
        let now = Utc::now();
        let earlier = now - chrono::Duration::minutes(5);
        assert_eq!(
            check_account(UserStatus::Active, UserRole::User, None, None),
            Ok(())
        );
        assert_eq!(
            check_account(UserStatus::Active, UserRole::User, Some(earlier), Some(now)),
            Ok(())
        );
        assert_eq!(
            check_account(UserStatus::Active, UserRole::User, Some(now), Some(earlier)),
            Err(Refusal::RevokedByEpoch)
        );
        assert_eq!(
            check_account(UserStatus::Active, UserRole::User, Some(now), None),
            Err(Refusal::RevokedByEpoch)
        );
        assert_eq!(
            check_account(UserStatus::Suspended, UserRole::User, None, None),
            Err(Refusal::AccountNotActive)
        );
        assert_eq!(
            check_account(UserStatus::Active, UserRole::Guest, None, None),
            Err(Refusal::AccountNotActive)
        );
    }

    #[tokio::test]
    async fn audit_metadata_records_the_acting_app_only_inside_a_delegated_request() {
        assert_eq!(audit_metadata(json!({ "a": 1 })), json!({ "a": 1 }));
        let (app, grant) = (Uuid::new_v4(), Uuid::new_v4());
        let m = with_via_app(app, grant, async { audit_metadata(json!({ "a": 1 })) }).await;
        assert_eq!(m["via"], "app");
        assert_eq!(m["viaAppId"], json!(app));
        assert_eq!(m["viaGrantId"], json!(grant));
        assert_eq!(m["a"], 1);
    }
}
