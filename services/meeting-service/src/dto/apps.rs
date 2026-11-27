//! ADR 0064 / 0065 (CF Meet Apps): OAuth consent relay, connected apps, organization apps
//! and in-meeting apps. JSON is camelCase; the authorize CONTEXT query keeps the OAuth wire
//! names (snake_case, RFC 6749) because the frontend forwards the authorization request
//! unchanged.

use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

/// The authorization request as the app sent it to `https://cfmeet.com/oauth/authorize`.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct AuthorizeContextQuery {
    /// The app's OAuth client id (= app id).
    pub client_id: Option<String>,
    /// Must exactly match a redirect URI registered for the app.
    pub redirect_uri: Option<String>,
    /// Space-separated OAuth scopes (`profile:read meetings:write …`).
    pub scope: Option<String>,
    /// Opaque value echoed back to the app.
    pub state: Option<String>,
    /// PKCE S256 challenge (43-128 characters).
    pub code_challenge: Option<String>,
    /// Must be `S256`.
    pub code_challenge_method: Option<String>,
    /// `organization` for a workspace install (default: a personal install).
    pub install: Option<String>,
    /// The organization of a workspace install.
    pub organization_id: Option<Uuid>,
    /// Ignored (the response type is always `code`).
    pub response_type: Option<String>,
}

/// `POST /api/v2/oauth/authorize/approve`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthorizeDecisionRequest {
    pub client_id: String,
    pub redirect_uri: String,
    pub scope: String,
    pub state: Option<String>,
    pub code_challenge: String,
    pub code_challenge_method: String,
    /// `organization` for a workspace install; absent or `user` for a personal install.
    pub install: Option<String>,
    pub organization_id: Option<Uuid>,
    /// `APPROVE` or `DENY`.
    pub decision: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OauthAppSummary {
    pub client_id: Uuid,
    pub name: String,
    /// `PRIVATE` or `PUBLIC`.
    pub distribution: Option<String>,
    /// `DRAFT`, `APPROVED`, `PUBLISHED` (or `SUSPENDED`, never authorizable).
    pub status: Option<String>,
    pub developer_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OauthScopeView {
    pub name: String,
    pub description: Option<String>,
    /// `user` or `bot`.
    pub who: Option<String>,
}

/// The organization's install decision for a workspace install (Organizations policy).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrganizationInstallDecision {
    pub allowed: bool,
    /// `NOT_A_MEMBER`, `ROLE_REQUIRED`, `NOT_ALLOWLISTED` or `ORGANIZATION_NOT_READY`.
    pub reason: Option<String>,
}

/// Consent data for the consent page.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizeContextResponse {
    pub app: OauthAppSummary,
    pub scopes: Vec<OauthScopeView>,
    /// `USER` or `ORGANIZATION`.
    pub target: String,
    pub organization_id: Option<Uuid>,
    /// The redirect URI Platform Access validated (exactly the requested one).
    pub redirect_uri: String,
    pub state: Option<String>,
    /// Present for workspace installs: whether the signed-in account may install there.
    pub organization_install: Option<OrganizationInstallDecision>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizeDecisionResponse {
    /// Where the browser goes next: `redirect_uri?code=…&state=…` on approval,
    /// `redirect_uri?error=access_denied&state=…` on denial.
    pub redirect_to: String,
}

/// One installed app (a grant).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConnectedApp {
    pub grant_id: Uuid,
    pub app_id: Uuid,
    pub name: Option<String>,
    pub developer_name: Option<String>,
    pub logo_url: Option<String>,
    /// `USER` or `ORGANIZATION`.
    pub target: String,
    pub organization_id: Option<Uuid>,
    pub scopes: Vec<String>,
    pub status: String,
    pub created_at: Option<String>,
    /// Whether the app has an in-meeting panel.
    pub has_meeting_surface: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConnectedAppList {
    pub items: Vec<ConnectedApp>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MeetingSurfaceView {
    /// The iframe URL (`https://`).
    pub url: String,
    /// Origins the panel's messaging bridge accepts.
    pub origins: Vec<String>,
}

/// An app with a meeting surface installed for the meeting's owner or organization.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConferenceApp {
    pub app_id: Uuid,
    pub name: Option<String>,
    pub developer_name: Option<String>,
    pub logo_url: Option<String>,
    pub surface: MeetingSurfaceView,
    /// `OWNER` (the meeting owner's personal install) or `ORGANIZATION`.
    pub installed_for: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConferenceAppList {
    pub items: Vec<ConferenceApp>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AppContextTokenResponse {
    /// Ed25519 JWS (verify with `GET /api/v2/app-context-keys`).
    pub context_token: String,
    /// Seconds (300).
    pub expires_in: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AppContextJwk {
    pub kty: String,
    pub crv: String,
    pub x: String,
    pub kid: String,
    #[serde(rename = "use")]
    pub use_: String,
    pub alg: String,
}

/// JWKS of the app-context signing keys.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AppContextKeySet {
    pub keys: Vec<AppContextJwk>,
}
