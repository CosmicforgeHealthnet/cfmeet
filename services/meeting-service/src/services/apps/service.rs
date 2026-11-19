//! The account authority's Apps operations (integrate contract §3).
//!
//! **Consent relay.** The signed-in account (a CF Meet session; first-party direct
//! ingress) asks Platform Access to validate the authorization request with its OWN
//! account id; the browser is only ever redirected to a redirect URI Platform Access just
//! validated for that client (strict echo: if Platform Access names the URI, it must be
//! exactly the requested one). Approval of a workspace (ORGANIZATION) install first asks
//! the Organizations install policy. Approval → `redirect_uri?code=…&state=…`; denial →
//! `redirect_uri?error=access_denied&state=…` (the existing query of the redirect URI is
//! kept). Platform Access refusals are `400 OAUTH_REQUEST_INVALID` whose message starts
//! with Platform Access's reason (`INVALID_CLIENT`, `INVALID_REDIRECT_URI`,
//! `INVALID_SCOPE`, `PKCE_REQUIRED`, `APP_NOT_AVAILABLE`); the frontend must never redirect
//! after `INVALID_CLIENT` / `INVALID_REDIRECT_URI`.
//!
//! **Connected apps / organization Apps.** Grants are listed from Platform Access; a grant
//! can be revoked only by its own account (USER grants) or by an OWNER / ADMIN of its
//! organization (ORGANIZATION grants); anything else is `404 APP_NOT_FOUND`.
//!
//! **In-meeting apps.** Apps with a meeting surface installed for the meeting owner (USER
//! grant) or the meeting's organization, for admitted participants only.

use std::collections::BTreeSet;

use sea_orm::EntityTrait;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    oauth::{Grant, GrantOwner, PaAnswer},
    organizations::InstallDecision,
    Apps,
};
use crate::{
    domain::AdmissionState,
    dto::apps::*,
    error::{ApiError, ApiResult, ErrorCode},
    models::users,
    services::{
        actor::{ActorContext, RegisteredActor},
        meeting_core::{find_conference, find_space, own_participant},
        platform::authority::AuthorityUnavailable,
    },
    state::AppState,
};

fn disabled(what: &str) -> ApiError {
    ApiError::domain(ErrorCode::FeatureDisabled, format!("{what} is not enabled"))
}

fn unavailable(e: AuthorityUnavailable) -> ApiError {
    tracing::warn!(error = %e, "apps dependency unavailable");
    ApiError::domain(
        ErrorCode::ServiceUnavailable,
        "The service is temporarily unavailable. Please retry.",
    )
}

fn invalid(message: impl Into<String>) -> ApiError {
    ApiError::domain(ErrorCode::ValidationError, message)
}

fn oauth_invalid(reason: &str, message: &str) -> ApiError {
    ApiError::domain(
        ErrorCode::OauthRequestInvalid,
        format!("{reason}: {message}"),
    )
}

fn app_not_found() -> ApiError {
    ApiError::domain(ErrorCode::AppNotFound, "App not found")
}

/// A signed-in account whose revocation status was checked, never an app.
fn account(actor: &ActorContext) -> ApiResult<&RegisteredActor> {
    let user = actor.require_registered()?;
    if user.delegation.is_some() {
        return Err(ApiError::domain(
            ErrorCode::PermissionDenied,
            "Apps cannot manage apps",
        ));
    }
    if !user.revocation_checked {
        return Err(ApiError::domain(
            ErrorCode::RevocationCheckUnavailable,
            "Session revocation status is temporarily unavailable; retry later",
        ));
    }
    Ok(user)
}

/// A validated authorization request.
#[derive(Debug, Clone)]
pub struct AuthorizationRequest {
    pub client_id: Uuid,
    pub redirect_uri: String,
    pub scope: String,
    pub state: Option<String>,
    pub code_challenge: String,
    pub code_challenge_method: String,
    pub organization_id: Option<Uuid>,
}

impl AuthorizationRequest {
    #[allow(clippy::too_many_arguments)]
    pub fn parse(
        client_id: Option<&str>,
        redirect_uri: Option<&str>,
        scope: Option<&str>,
        state: Option<&str>,
        code_challenge: Option<&str>,
        code_challenge_method: Option<&str>,
        install: Option<&str>,
        organization_id: Option<Uuid>,
    ) -> ApiResult<Self> {
        let client_id = client_id
            .and_then(|c| Uuid::parse_str(c.trim()).ok())
            .ok_or_else(|| oauth_invalid("INVALID_CLIENT", "client_id is missing or invalid"))?;
        let redirect_uri = redirect_uri
            .map(str::trim)
            .filter(|r| !r.is_empty() && r.len() <= 2048 && url::Url::parse(r).is_ok())
            .ok_or_else(|| {
                oauth_invalid("INVALID_REDIRECT_URI", "redirect_uri is missing or invalid")
            })?
            .to_string();
        let scope = scope
            .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|s| !s.is_empty() && s.len() <= 1024)
            .ok_or_else(|| oauth_invalid("INVALID_SCOPE", "scope is missing or too long"))?;
        let state = state.map(str::to_string);
        if state.as_deref().is_some_and(|s| s.len() > 1024) {
            return Err(invalid("state is at most 1024 characters"));
        }
        let code_challenge = code_challenge
            .filter(|c| {
                (43..=128).contains(&c.len())
                    && c.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
            })
            .ok_or_else(|| oauth_invalid("PKCE_REQUIRED", "code_challenge is missing or invalid"))?
            .to_string();
        if code_challenge_method != Some("S256") {
            return Err(oauth_invalid(
                "PKCE_REQUIRED",
                "code_challenge_method must be S256",
            ));
        }
        let organization_id = match install.map(str::to_ascii_lowercase).as_deref() {
            None | Some("") | Some("user") => {
                if organization_id.is_some() {
                    return Err(invalid("organization_id requires install=organization"));
                }
                None
            }
            Some("organization") => Some(organization_id.ok_or_else(|| {
                invalid("install=organization requires organization_id")
            })?),
            Some(_) => return Err(invalid("install must be user or organization")),
        };
        Ok(Self {
            client_id,
            redirect_uri,
            scope,
            state,
            code_challenge,
            code_challenge_method: "S256".into(),
            organization_id,
        })
    }

    pub fn target(&self) -> &'static str {
        if self.organization_id.is_some() {
            "ORGANIZATION"
        } else {
            "USER"
        }
    }

    /// The Platform Access body (`accountId` is always the signed-in account).
    fn platform_body(&self, account: Uuid, decision: Option<&str>) -> Value {
        let mut body = json!({
            "clientId": self.client_id,
            "redirectUri": self.redirect_uri,
            "scope": self.scope,
            "codeChallenge": self.code_challenge,
            "codeChallengeMethod": self.code_challenge_method,
            "target": self.target(),
            "accountId": account,
        });
        if let Some(org) = self.organization_id {
            body["organizationId"] = json!(org);
        }
        if let Some(d) = decision {
            body["decision"] = json!(d);
        }
        body
    }

    /// `redirect_uri` with `params` (and `state`) appended to its query.
    pub fn redirect_with(&self, params: &[(&str, &str)]) -> ApiResult<String> {
        let mut url = url::Url::parse(&self.redirect_uri)
            .map_err(|_| oauth_invalid("INVALID_REDIRECT_URI", "redirect_uri is invalid"))?;
        {
            let mut q = url.query_pairs_mut();
            for (k, v) in params {
                q.append_pair(k, v);
            }
            if let Some(state) = &self.state {
                q.append_pair("state", state);
            }
        }
        Ok(url.to_string())
    }
}

/// Maps a Platform Access refusal of an authorization request.
fn refusal(answer: &PaAnswer) -> ApiError {
    let reason = answer.error_code().filter(|c| {
        matches!(
            c.as_str(),
            "INVALID_CLIENT"
                | "INVALID_REDIRECT_URI"
                | "INVALID_SCOPE"
                | "PKCE_REQUIRED"
                | "APP_NOT_AVAILABLE"
        )
    });
    oauth_invalid(
        reason.as_deref().unwrap_or("APP_NOT_AVAILABLE"),
        "The app's authorization request was refused",
    )
}

fn str_of(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_string)
}

impl Apps {
    fn oauth(&self, enabled: bool, what: &str) -> ApiResult<&super::oauth::SharedOauthAuthority> {
        if !enabled {
            return Err(disabled(what));
        }
        self.oauth.as_ref().ok_or_else(|| disabled(what))
    }

    /// Platform Access validation; the answer must be for this client and, when it names a
    /// redirect URI, exactly the requested one.
    async fn validate(
        &self,
        req: &AuthorizationRequest,
        account: Uuid,
    ) -> ApiResult<Value> {
        let pa = self.oauth(self.authorize_enabled, "App authorization")?;
        let answer = pa
            .validate(&req.platform_body(account, None))
            .await
            .map_err(unavailable)?;
        if !answer.ok() {
            return Err(refusal(&answer));
        }
        let body = answer.body;
        let client_ok = body
            .pointer("/app/clientId")
            .and_then(Value::as_str)
            .is_none_or(|c| Uuid::parse_str(c).ok() == Some(req.client_id));
        let redirect_ok = body
            .get("redirectUri")
            .and_then(Value::as_str)
            .is_none_or(|r| r == req.redirect_uri);
        if !client_ok || !redirect_ok || !body.get("app").is_some_and(Value::is_object) {
            tracing::error!("Platform Access validated another authorization request; refused");
            return Err(oauth_invalid(
                "INVALID_REDIRECT_URI",
                "The authorization request could not be validated",
            ));
        }
        Ok(body)
    }

    async fn install_decision(
        &self,
        req: &AuthorizationRequest,
        account: Uuid,
    ) -> ApiResult<Option<InstallDecision>> {
        let Some(org) = req.organization_id else {
            return Ok(None);
        };
        let organizations = self
            .organizations
            .as_ref()
            .ok_or_else(|| disabled("Workspace app installs"))?;
        organizations
            .install_check(org, account, req.client_id)
            .await
            .map(Some)
            .map_err(unavailable)
    }

    /// `GET /api/v2/oauth/authorize/context`.
    pub async fn authorize_context(
        &self,
        actor: &ActorContext,
        q: &AuthorizeContextQuery,
    ) -> ApiResult<AuthorizeContextResponse> {
        self.oauth(self.authorize_enabled, "App authorization")?;
        let user = account(actor)?;
        let req = AuthorizationRequest::parse(
            q.client_id.as_deref(),
            q.redirect_uri.as_deref(),
            q.scope.as_deref(),
            q.state.as_deref(),
            q.code_challenge.as_deref(),
            q.code_challenge_method.as_deref(),
            q.install.as_deref(),
            q.organization_id,
        )?;
        let body = self.validate(&req, user.user_id).await?;
        let organization_install =
            self.install_decision(&req, user.user_id)
                .await?
                .map(|d| match d {
                    InstallDecision::Allowed => OrganizationInstallDecision {
                        allowed: true,
                        reason: None,
                    },
                    InstallDecision::Refused(r) => OrganizationInstallDecision {
                        allowed: false,
                        reason: Some(r),
                    },
                });
        let app = &body["app"];
        Ok(AuthorizeContextResponse {
            app: OauthAppSummary {
                client_id: req.client_id,
                name: str_of(app, "name").unwrap_or_default(),
                distribution: str_of(app, "distribution"),
                status: str_of(app, "status"),
                developer_name: str_of(app, "developerName"),
            },
            scopes: body
                .get("scopes")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|s| {
                            Some(OauthScopeView {
                                name: str_of(s, "name")?,
                                description: str_of(s, "description"),
                                who: str_of(s, "who"),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
            target: req.target().to_string(),
            organization_id: req.organization_id,
            redirect_uri: req.redirect_uri.clone(),
            state: req.state.clone(),
            organization_install,
        })
    }

    /// `POST /api/v2/oauth/authorize/approve` (approve or deny).
    pub async fn authorize_decision(
        &self,
        actor: &ActorContext,
        body: &AuthorizeDecisionRequest,
    ) -> ApiResult<AuthorizeDecisionResponse> {
        let pa = self.oauth(self.authorize_enabled, "App authorization")?;
        let user = account(actor)?;
        let req = AuthorizationRequest::parse(
            Some(&body.client_id),
            Some(&body.redirect_uri),
            Some(&body.scope),
            body.state.as_deref(),
            Some(&body.code_challenge),
            Some(&body.code_challenge_method),
            body.install.as_deref(),
            body.organization_id,
        )?;
        let approve = match body.decision.as_str() {
            "APPROVE" => true,
            "DENY" => false,
            _ => return Err(invalid("decision must be APPROVE or DENY")),
        };
        // Even a denial redirects only to a redirect URI Platform Access just validated.
        self.validate(&req, user.user_id).await?;
        if !approve {
            tracing::info!(app_id = %req.client_id, target = req.target(), "app authorization denied by the account");
            return Ok(AuthorizeDecisionResponse {
                redirect_to: req.redirect_with(&[("error", "access_denied")])?,
            });
        }
        if let Some(InstallDecision::Refused(reason)) =
            self.install_decision(&req, user.user_id).await?
        {
            return Err(ApiError::domain(
                ErrorCode::PermissionDenied,
                format!("{reason}: the organization does not allow you to install this app"),
            ));
        }
        let answer = pa
            .authorize(&req.platform_body(user.user_id, Some("APPROVE")))
            .await
            .map_err(unavailable)?;
        if !answer.ok() {
            return Err(refusal(&answer));
        }
        let code = answer
            .body
            .get("code")
            .and_then(Value::as_str)
            .filter(|c| !c.is_empty() && c.len() <= 512)
            .ok_or_else(|| unavailable(AuthorityUnavailable("authorization without code".into())))?;
        let grant = answer
            .body
            .get("grantId")
            .and_then(|g| g.as_str())
            .unwrap_or("")
            .to_string();
        tracing::info!(app_id = %req.client_id, target = req.target(),
            organization_id = ?req.organization_id, grant_id = %grant,
            "app authorized by the account");
        Ok(AuthorizeDecisionResponse {
            redirect_to: req.redirect_with(&[("code", code)])?,
        })
    }

    async fn grants(&self, owner: GrantOwner) -> ApiResult<Vec<Grant>> {
        let pa = self.oauth(self.authorize_enabled || self.in_meeting_enabled, "Apps")?;
        let answer = pa.grants(owner).await.map_err(unavailable)?;
        if !answer.ok() {
            return Err(unavailable(AuthorityUnavailable(format!(
                "grant listing refused: HTTP {}",
                answer.status
            ))));
        }
        let grants = Grant::list_of(&answer.body)
            .ok_or_else(|| unavailable(AuthorityUnavailable("malformed grant list".into())))?;
        // Defence in depth: only the owner's own grants.
        Ok(grants
            .into_iter()
            .filter(|g| match owner {
                GrantOwner::Account(a) => {
                    g.target == "USER" && g.account_id.is_none_or(|x| x == a)
                }
                GrantOwner::Organization(o) => {
                    g.target == "ORGANIZATION" && g.organization_id.is_none_or(|x| x == o)
                }
            })
            .collect())
    }

    fn view(g: &Grant) -> ConnectedApp {
        ConnectedApp {
            grant_id: g.id,
            app_id: g.app_id,
            name: g.app_name.clone(),
            developer_name: g.developer_name.clone(),
            logo_url: g.logo_url.clone(),
            target: g.target.clone(),
            organization_id: g.organization_id,
            scopes: g.scopes.clone(),
            status: g.status.clone(),
            created_at: g.created_at.clone(),
            has_meeting_surface: g.meeting_surface.is_some(),
        }
    }

    async fn revoke_listed(&self, owner: GrantOwner, grant_id: Uuid) -> ApiResult<()> {
        let pa = self.oauth(self.authorize_enabled, "App authorization")?;
        if !self.grants(owner).await?.iter().any(|g| g.id == grant_id) {
            return Err(app_not_found());
        }
        let answer = pa.revoke(grant_id).await.map_err(unavailable)?;
        match answer.status {
            200..=299 | 404 => {
                tracing::info!(%grant_id, ?owner, "app grant revoked");
                Ok(())
            }
            s => Err(unavailable(AuthorityUnavailable(format!(
                "grant revocation refused: HTTP {s}"
            )))),
        }
    }

    /// `GET /api/v2/me/apps`.
    pub async fn my_apps(&self, actor: &ActorContext) -> ApiResult<ConnectedAppList> {
        self.oauth(self.authorize_enabled, "App authorization")?;
        let user = account(actor)?;
        let grants = self.grants(GrantOwner::Account(user.user_id)).await?;
        Ok(ConnectedAppList {
            items: grants.iter().map(Self::view).collect(),
        })
    }

    /// `DELETE /api/v2/me/apps/{grantId}`.
    pub async fn revoke_my_app(&self, actor: &ActorContext, grant_id: Uuid) -> ApiResult<()> {
        self.oauth(self.authorize_enabled, "App authorization")?;
        let user = account(actor)?;
        self.revoke_listed(GrantOwner::Account(user.user_id), grant_id)
            .await
    }

    async fn require_organization_admin(
        &self,
        actor: &ActorContext,
        organization: Uuid,
    ) -> ApiResult<()> {
        self.oauth(self.authorize_enabled, "App authorization")?;
        let user = account(actor)?;
        let organizations = self
            .organizations
            .as_ref()
            .ok_or_else(|| disabled("Workspace apps"))?;
        if organizations
            .is_admin(organization, user.user_id)
            .await
            .map_err(unavailable)?
        {
            Ok(())
        } else {
            Err(ApiError::domain(
                ErrorCode::PermissionDenied,
                "Organization administrators only",
            ))
        }
    }

    /// `GET /api/v2/organizations/{id}/apps`.
    pub async fn organization_apps(
        &self,
        actor: &ActorContext,
        organization: Uuid,
    ) -> ApiResult<ConnectedAppList> {
        self.require_organization_admin(actor, organization).await?;
        let grants = self.grants(GrantOwner::Organization(organization)).await?;
        Ok(ConnectedAppList {
            items: grants.iter().map(Self::view).collect(),
        })
    }

    /// `DELETE /api/v2/organizations/{id}/apps/{grantId}`.
    pub async fn revoke_organization_app(
        &self,
        actor: &ActorContext,
        organization: Uuid,
        grant_id: Uuid,
    ) -> ApiResult<()> {
        self.require_organization_admin(actor, organization).await?;
        self.revoke_listed(GrantOwner::Organization(organization), grant_id)
            .await
    }

    // ---------------------------------------------------------------------------------
    // In-meeting apps (ADR 0065 I7)
    // ---------------------------------------------------------------------------------

    fn in_meeting(&self) -> ApiResult<&super::context::AppContextSigner> {
        self.oauth(self.in_meeting_enabled, "In-meeting apps")?;
        self.context
            .as_deref()
            .ok_or_else(|| disabled("In-meeting apps"))
    }

    /// The caller's ADMITTED participant of the conference (404 otherwise) and the
    /// conference's apps.
    async fn conference_apps_for(
        &self,
        state: &AppState,
        actor: &ActorContext,
        conference_id: Uuid,
    ) -> ApiResult<(
        crate::models::conference_participants::Model,
        Vec<(ConferenceApp, Grant)>,
    )> {
        if actor.registered().is_some_and(|r| r.delegation.is_some()) {
            return Err(ApiError::domain(
                ErrorCode::PermissionDenied,
                "Apps cannot open in-meeting apps",
            ));
        }
        let db = &state.meeting.core.db;
        let not_found = || ApiError::domain(ErrorCode::ConferenceNotFound, "Conference not found");
        let conference = find_conference(db, conference_id).await?;
        let participant = own_participant(db, conference_id, actor, false)
            .await?
            .filter(|p| p.admission_state == AdmissionState::Admitted)
            .ok_or_else(not_found)?;
        let space = find_space(db, conference.meeting_space_id).await?;
        let mut out: Vec<(ConferenceApp, Grant)> = vec![];
        let mut seen = BTreeSet::new();
        let mut owners: Vec<(GrantOwner, &str)> = vec![];
        if let Some(owner) = space.owner_user_id {
            owners.push((GrantOwner::Account(owner), "OWNER"));
        }
        if let Some(org) = space.organization_id {
            owners.push((GrantOwner::Organization(org), "ORGANIZATION"));
        }
        for (owner, installed_for) in owners {
            for g in self.grants(owner).await? {
                let Some(surface) = g.meeting_surface.clone().filter(|_| g.is_active()) else {
                    continue;
                };
                if !seen.insert(g.app_id) {
                    continue;
                }
                out.push((
                    ConferenceApp {
                        app_id: g.app_id,
                        name: g.app_name.clone(),
                        developer_name: g.developer_name.clone(),
                        logo_url: g.logo_url.clone(),
                        surface: MeetingSurfaceView {
                            url: surface.url,
                            origins: surface.origins,
                        },
                        installed_for: installed_for.to_string(),
                    },
                    g,
                ));
            }
        }
        Ok((participant, out))
    }

    /// `GET /api/v2/conferences/{id}/apps`.
    pub async fn conference_apps(
        &self,
        state: &AppState,
        actor: &ActorContext,
        conference_id: Uuid,
    ) -> ApiResult<ConferenceAppList> {
        self.in_meeting()?;
        let (_, apps) = self.conference_apps_for(state, actor, conference_id).await?;
        Ok(ConferenceAppList {
            items: apps.into_iter().map(|(a, _)| a).collect(),
        })
    }

    /// `POST /api/v2/conferences/{id}/apps/{appId}/context`.
    pub async fn app_context(
        &self,
        state: &AppState,
        actor: &ActorContext,
        conference_id: Uuid,
        app_id: Uuid,
    ) -> ApiResult<AppContextTokenResponse> {
        let signer = self.in_meeting()?;
        let (participant, apps) = self.conference_apps_for(state, actor, conference_id).await?;
        if !apps.iter().any(|(a, _)| a.app_id == app_id) {
            return Err(app_not_found());
        }
        // The user's pairwise subject only if THIS user granted the app.
        let sub = match actor.registered() {
            Some(user) => self
                .grants(GrantOwner::Account(user.user_id))
                .await?
                .into_iter()
                .find(|g| g.app_id == app_id && g.is_active())
                .and_then(|g| g.sub),
            None => None,
        };
        let token = signer.mint(
            app_id,
            conference_id,
            participant.id,
            participant.role.as_str(),
            sub.as_deref(),
        );
        tracing::info!(%conference_id, %app_id, with_sub = sub.is_some(), "app-context token minted");
        Ok(AppContextTokenResponse {
            context_token: token,
            expires_in: super::context::CONTEXT_TTL_SECONDS,
        })
    }

    /// `GET /api/v2/app-context-keys`.
    pub fn app_context_keys(&self) -> ApiResult<Value> {
        Ok(self.in_meeting()?.jwks())
    }
}

/// `GET /internal/v1/oauth/userinfo` — the profile of `account` for an app holding
/// `scopes` (`email` only with `profile:email`, and only a verified address).
pub async fn userinfo(state: &AppState, account: Uuid, scopes: &[&str]) -> ApiResult<Option<Value>> {
    let Some(user) = users::Entity::find_by_id(account).one(&state.db).await? else {
        return Ok(None);
    };
    if matches!(
        user.status,
        users::UserStatus::Inactive | users::UserStatus::Suspended
    ) || user.role == users::UserRole::Guest
    {
        return Ok(None);
    }
    let mut body = json!({
        "displayName": user.presentation_name(),
        "avatarUrl": user.avatar_url,
    });
    if scopes.contains(&"profile:email") && user.email_verified(state.require_email_verification)
    {
        body["email"] = json!(user.email.to_ascii_lowercase());
    }
    Ok(Some(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(state: Option<&str>, redirect: &str) -> AuthorizationRequest {
        AuthorizationRequest::parse(
            Some(&Uuid::new_v4().to_string()),
            Some(redirect),
            Some("profile:read  meetings:write"),
            state,
            Some(&"a".repeat(43)),
            Some("S256"),
            None,
            None,
        )
        .unwrap()
    }

    #[test]
    fn redirects_keep_the_registered_query_and_echo_the_state() {
        let r = req(Some("x y&z"), "https://app.example/cb?tenant=1");
        assert_eq!(r.scope, "profile:read meetings:write");
        assert_eq!(
            r.redirect_with(&[("code", "c1")]).unwrap(),
            "https://app.example/cb?tenant=1&code=c1&state=x+y%26z"
        );
        let r = req(None, "https://app.example/cb");
        assert_eq!(
            r.redirect_with(&[("error", "access_denied")]).unwrap(),
            "https://app.example/cb?error=access_denied"
        );
    }

    #[test]
    fn requests_need_pkce_s256_and_a_consistent_install_target() {
        let id = Uuid::new_v4().to_string();
        let c = "a".repeat(43);
        let parse = |method: &str, install: Option<&str>, org: Option<Uuid>| {
            AuthorizationRequest::parse(
                Some(&id),
                Some("https://a.example/cb"),
                Some("profile:read"),
                None,
                Some(&c),
                Some(method),
                install,
                org,
            )
        };
        assert!(parse("plain", None, None).is_err());
        assert!(parse("S256", Some("organization"), None).is_err());
        assert!(parse("S256", None, Some(Uuid::new_v4())).is_err());
        assert!(parse("S256", Some("team"), None).is_err());
        let org = Uuid::new_v4();
        let r = parse("S256", Some("organization"), Some(org)).unwrap();
        assert_eq!(r.target(), "ORGANIZATION");
        assert_eq!(r.platform_body(Uuid::nil(), None)["organizationId"], json!(org));
        assert!(AuthorizationRequest::parse(
            Some("nope"),
            Some("https://a.example/cb"),
            Some("x"),
            None,
            Some(&c),
            Some("S256"),
            None,
            None
        )
        .is_err());
    }
}
