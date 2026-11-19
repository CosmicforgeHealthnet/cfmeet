//! CF Meet Apps — OAuth clients, grants (installations), codes and tokens (ADR 0064;
//! contract `docs/platform/integrate-internal-contracts.md` §0 / §2). Behind the default-off
//! flag `PLATFORM_ACCESS_OAUTH_ENABLED` (the HTTP layer gates every entry point).
//!
//! * **Client profile** (`platform_oauth_clients`): an application of an EXTERNAL tenant
//!   (`client_id` = application id) with exact redirect URIs, requested scopes from the
//!   closed catalogue ([`super::oauth_scopes`]), distribution and status. Client secrets
//!   (`cfcs_<tag>_<24hex>_<64hex>`) use the API credential wire format and HMAC verifier
//!   under their own label; shown once; rotation keeps the previous secret for 24 h.
//! * **Who may authorize** ([`ClientRow::may_authorize`]): SUSPENDED nobody; DRAFT only
//!   `testAccountIds` (≤ 25); PRIVATE (APPROVED / PUBLISHED) the accounts in
//!   `allowedAccountIds` (plus test accounts) and, for organization installs, the
//!   organizations in `allowedOrganizationIds` — both lists are synced by the Developer
//!   Console, which owns workspace membership; PUBLIC + PUBLISHED anyone (PUBLIC +
//!   APPROVED: test accounts only, until published).
//! * **Grant**: USER = (app, account) in the account's personal FIRST_PARTY communications
//!   environment, principal `NATIVE_CF_USER` `cfmeet-user:<accountId>`; ORGANIZATION =
//!   (app, organization) in the organization's FIRST_PARTY triple
//!   (`platform_organization_scopes`, capability `primary`), principal `BOT_OR_SERVICE`
//!   `app:<appId>`. One installation service account `app-<appId hex>` per (app,
//!   environment). Scopes accumulate on the ACTIVE grant; REVOKED is terminal (a later
//!   authorization creates a new grant). Every grant change is an `APP_GRANT` change-feed
//!   entry (database trigger).
//! * **Code**: `cfoc_…`, 60 s, single use, verifier-only, bound to client, redirect URI,
//!   PKCE S256 challenge and grant. Presenting a consumed code revokes the grant.
//! * **Tokens**: access `cfoa_` (1 h) and bot `cfob_` (ORGANIZATION grants with bot scopes,
//!   until uninstall) are `platform_api_credentials` rows of the installation service
//!   account in the target environment, each restricted (existing restriction machinery)
//!   to exactly the grant's principal; `platform_oauth_tokens` marks them. Refresh `cfor_`
//!   (90 d) rotates on every use; presenting a superseded one revokes the grant.
//! * **Pairwise `sub`**: `base64url(HMAC-SHA-256(key, label 0 appId 0 subject))`, subject
//!   = the account id (USER) or `organization:<id>` (ORGANIZATION). See [`PairwiseKey`].

use std::collections::BTreeSet;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Duration, Utc};
use hmac::{Hmac, Mac};
use sea_orm::{ConnectionTrait, DbBackend, DbErr, QueryResult, Statement, TransactionTrait};
use secrecy::{ExposeSecret, SecretSlice};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    credential_format::{self, CredentialFamily, PresentedCredential},
    oauth_scopes::{self, Audience, Target},
    organization_scopes::OrganizationScope,
    principals::NewPrincipal,
    service_accounts::NewServiceAccount,
    verifier::{CredentialPepper, VerifierKeyring, VERIFIER_ALGORITHM},
    PlatformError, PlatformResult, PlatformService,
};
use crate::{
    config::ConfigError,
    domain::{
        CredentialId, EnvironmentId, EnvironmentKind, IssuedSecret, PrincipalKind, PrincipalStatus,
        ServiceAccountStatus,
    },
};

pub(crate) const ACCESS_TOKEN: CredentialFamily = CredentialFamily {
    prefix: "cfoa",
    context: b"cfmeet.platform-access.oauth-access-token.v1",
};
pub(crate) const BOT_TOKEN: CredentialFamily = CredentialFamily {
    prefix: "cfob",
    context: b"cfmeet.platform-access.oauth-bot-token.v1",
};
pub(crate) const REFRESH_TOKEN: CredentialFamily = CredentialFamily {
    prefix: "cfor",
    context: b"cfmeet.platform-access.oauth-refresh-token.v1",
};
pub(crate) const CLIENT_SECRET: CredentialFamily = CredentialFamily {
    prefix: "cfcs",
    context: b"cfmeet.platform-access.oauth-client-secret.v1",
};
pub(crate) const AUTHORIZATION_CODE: CredentialFamily = CredentialFamily {
    prefix: "cfoc",
    context: b"cfmeet.platform-access.oauth-code.v1",
};

pub const CODE_TTL_SECONDS: i64 = 60;
pub const ACCESS_TTL_SECONDS: i64 = 3600;
pub const REFRESH_TTL_DAYS: i64 = 90;
/// Bot tokens live until uninstall; the credential row still needs an expiry.
pub const BOT_TTL_DAYS: i64 = 3650;
pub const SECRET_OVERLAP_HOURS: i64 = 24;
pub const MAX_REDIRECT_URIS: usize = 10;
pub const MAX_TEST_ACCOUNTS: usize = 25;
pub const MAX_ALLOWED_IDS: usize = 1000;
/// The organization-scope capability ORGANIZATION grants target.
pub const ORGANIZATION_CAPABILITY: &str = "primary";

text_enum!(Distribution {
    Private = "PRIVATE",
    Public = "PUBLIC",
});

text_enum!(ClientStatus {
    Draft = "DRAFT",
    Approved = "APPROVED",
    Published = "PUBLISHED",
    Suspended = "SUSPENDED",
});

text_enum!(TokenKind {
    Access = "ACCESS",
    Bot = "BOT",
});

pub fn target_str(target: Target) -> &'static str {
    match target {
        Target::User => "USER",
        Target::Organization => "ORGANIZATION",
    }
}

pub fn parse_target(value: &str) -> Option<Target> {
    match value {
        "USER" => Some(Target::User),
        "ORGANIZATION" => Some(Target::Organization),
        _ => None,
    }
}

/// The principal reference of an account's user principal.
pub fn account_reference(account_id: Uuid) -> String {
    format!("cfmeet-user:{}", account_id.hyphenated())
}

/// The principal reference of an app's bot in an organization.
pub fn bot_reference(app_id: Uuid) -> String {
    format!("app:{}", app_id.hyphenated())
}

/// The installation service account name of an app (unique per environment).
pub fn installation_account_name(app_id: Uuid) -> String {
    format!("app-{}", app_id.simple())
}

// ---------------------------------------------------------------------------
// Pairwise subject
// ---------------------------------------------------------------------------

const PAIRWISE_LABEL: &[u8] = b"cfmeet.platform-access.oauth-pairwise.v1";
const PAIRWISE_DERIVATION_LABEL: &[u8] = b"cfmeet.platform-access.oauth-pairwise-key.v1";

/// The server key of the pairwise `sub`. `Debug` is redacted; memory is zeroized on drop.
pub struct PairwiseKey(SecretSlice<u8>);

impl PairwiseKey {
    /// A configured key (pepper strength rules: ≥ 32 bytes, no placeholder, variety).
    pub fn parse(var: &'static str, raw: &str) -> Result<Self, ConfigError> {
        CredentialPepper::parse(var, raw)?;
        Ok(Self(SecretSlice::from(raw.trim().as_bytes().to_vec())))
    }

    /// The fallback key derived from the current credential pepper.
    pub fn derived(keyring: &VerifierKeyring) -> Self {
        Self(SecretSlice::from(
            keyring.compute_current(PAIRWISE_DERIVATION_LABEL).to_vec(),
        ))
    }

    /// `base64url(HMAC-SHA-256(key, label 0 appId 0 subject))`.
    pub fn sub(&self, app_id: Uuid, subject: &str) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(self.0.expose_secret())
            .expect("HMAC accepts any key length");
        mac.update(PAIRWISE_LABEL);
        mac.update(&[0]);
        mac.update(app_id.hyphenated().to_string().as_bytes());
        mac.update(&[0]);
        mac.update(subject.as_bytes());
        URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    }
}

impl std::fmt::Debug for PairwiseKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PairwiseKey([REDACTED])")
    }
}

/// The pairwise subject of a grant: its account, or its organization.
pub fn pairwise_subject(grant: &GrantView) -> String {
    match (grant.account_id, grant.organization_id) {
        (Some(account), _) => account.hyphenated().to_string(),
        (None, Some(org)) => format!("organization:{}", org.hyphenated()),
        (None, None) => grant.id.hyphenated().to_string(),
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum OAuthError {
    // Authorization-request validation (`400 {error:{code}}`).
    InvalidClient,
    InvalidRedirectUri,
    InvalidScope,
    PkceRequired,
    AppNotAvailable,
    // Token endpoint (RFC 6749 codes).
    TokenInvalidClient,
    InvalidGrant,
    UnsupportedGrantType,
    InvalidRequest,
    Platform(PlatformError),
}

impl From<PlatformError> for OAuthError {
    fn from(e: PlatformError) -> Self {
        OAuthError::Platform(e)
    }
}

impl From<DbErr> for OAuthError {
    fn from(e: DbErr) -> Self {
        OAuthError::Platform(e.into())
    }
}

pub type OAuthResult<T> = Result<T, OAuthError>;

fn stmt(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

fn uuid_list(row: &QueryResult, col: &str) -> Result<Vec<Uuid>, DbErr> {
    let raw: Vec<String> = row.try_get("", col)?;
    Ok(raw.iter().filter_map(|v| Uuid::parse_str(v).ok()).collect())
}

fn text_list(ids: &[Uuid]) -> sea_orm::Value {
    ids.iter()
        .map(|u| u.to_string())
        .collect::<Vec<String>>()
        .into()
}

// ---------------------------------------------------------------------------
// Client profile
// ---------------------------------------------------------------------------

/// A client profile with the facts the decisions need.
#[derive(Debug, Clone)]
pub struct ClientRow {
    pub application_id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub redirect_uris: Vec<String>,
    pub user_scopes: Vec<String>,
    pub bot_scopes: Vec<String>,
    pub distribution: Distribution,
    pub status: ClientStatus,
    pub test_account_ids: Vec<Uuid>,
    pub allowed_account_ids: Vec<Uuid>,
    pub allowed_organization_ids: Vec<Uuid>,
    pub revision: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// The developer tenant's display name.
    pub developer_name: String,
    /// The app's own application and tenant are ACTIVE.
    pub app_active: bool,
    /// Issue time of the current (non-overlapping) client secret.
    pub secret_issued_at: Option<DateTime<Utc>>,
}

const CLIENT_SELECT: &str = "SELECT c.application_id, c.tenant_id, c.name, c.redirect_uris, c.user_scopes, \
        c.bot_scopes, c.distribution, c.status, c.test_account_ids::text[] AS test_account_ids, \
        c.allowed_account_ids::text[] AS allowed_account_ids, \
        c.allowed_organization_ids::text[] AS allowed_organization_ids, c.revision, c.created_at, \
        c.updated_at, t.display_name AS developer_name, \
        (a.status = 'ACTIVE' AND t.status = 'ACTIVE') AS app_active, \
        (SELECT max(s.issued_at) FROM platform_oauth_client_secrets s \
          WHERE s.application_id = c.application_id AND s.revoked_at IS NULL AND s.valid_until IS NULL) \
          AS secret_issued_at \
     FROM platform_oauth_clients c \
     JOIN platform_applications a ON a.id = c.application_id AND a.tenant_id = c.tenant_id \
     JOIN platform_tenants t ON t.id = c.tenant_id \
     WHERE c.application_id = $1";

fn client_from_row(r: &QueryResult) -> Result<ClientRow, DbErr> {
    let distribution: String = r.try_get("", "distribution")?;
    let status: String = r.try_get("", "status")?;
    let corrupt = || DbErr::Custom("unknown OAuth client vocabulary".into());
    Ok(ClientRow {
        application_id: r.try_get("", "application_id")?,
        tenant_id: r.try_get("", "tenant_id")?,
        name: r.try_get("", "name")?,
        redirect_uris: r.try_get("", "redirect_uris")?,
        user_scopes: r.try_get("", "user_scopes")?,
        bot_scopes: r.try_get("", "bot_scopes")?,
        distribution: Distribution::parse(&distribution).ok_or_else(corrupt)?,
        status: ClientStatus::parse(&status).ok_or_else(corrupt)?,
        test_account_ids: uuid_list(r, "test_account_ids")?,
        allowed_account_ids: uuid_list(r, "allowed_account_ids")?,
        allowed_organization_ids: uuid_list(r, "allowed_organization_ids")?,
        revision: r.try_get("", "revision")?,
        created_at: r.try_get("", "created_at")?,
        updated_at: r.try_get("", "updated_at")?,
        developer_name: r.try_get("", "developer_name")?,
        app_active: r.try_get("", "app_active")?,
        secret_issued_at: r.try_get("", "secret_issued_at")?,
    })
}

impl ClientRow {
    /// The wire view (`OAuthClient`).
    pub fn to_json(&self) -> Value {
        let mut v = json!({
            "clientId": self.application_id,
            "name": self.name,
            "redirectUris": self.redirect_uris,
            "userScopes": self.user_scopes,
            "botScopes": self.bot_scopes,
            "distribution": self.distribution,
            "status": self.status,
            "testAccountIds": self.test_account_ids,
            "allowedAccountIds": self.allowed_account_ids,
            "allowedOrganizationIds": self.allowed_organization_ids,
            "developerName": self.developer_name,
            "revision": self.revision,
            "createdAt": self.created_at,
            "updatedAt": self.updated_at,
        });
        if let Some(at) = self.secret_issued_at {
            v["secretIssuedAt"] = json!(at);
        }
        v
    }

    /// Whether `account` (installing for `target`, in `organization`) may authorize now.
    pub fn may_authorize(&self, target: Target, account: Uuid, organization: Option<Uuid>) -> bool {
        if !self.app_active {
            return false;
        }
        let test = self.test_account_ids.contains(&account);
        match self.status {
            ClientStatus::Suspended => false,
            ClientStatus::Draft => test,
            ClientStatus::Approved | ClientStatus::Published => match self.distribution {
                Distribution::Private => match target {
                    Target::User => test || self.allowed_account_ids.contains(&account),
                    Target::Organization => {
                        organization.is_some_and(|o| self.allowed_organization_ids.contains(&o))
                    }
                },
                Distribution::Public => self.status == ClientStatus::Published || test,
            },
        }
    }

    /// The scopes the client may request for `target`.
    fn requestable(&self, target: Target) -> BTreeSet<&'static str> {
        self.user_scopes
            .iter()
            .chain(self.bot_scopes.iter())
            .filter_map(|s| oauth_scopes::find(s))
            .filter(|s| s.audience.allowed_for(target))
            .map(|s| s.name)
            .collect()
    }
}

/// The client profile as `PUT` by the Developer Console (full replacement).
#[derive(Debug, Clone, Default)]
pub struct ClientInput {
    pub name: String,
    pub redirect_uris: Vec<String>,
    pub user_scopes: Vec<String>,
    pub bot_scopes: Vec<String>,
    pub distribution: String,
    pub status: String,
    pub test_account_ids: Vec<Uuid>,
    pub allowed_account_ids: Vec<Uuid>,
    pub allowed_organization_ids: Vec<Uuid>,
}

fn is_localhost(uri: &str) -> bool {
    uri.starts_with("http://")
}

/// Redirect URI rules: exact `https://` URIs; `http://localhost[:port]/…` only while
/// DRAFT; no fragment, no wildcard, no user info.
pub fn validate_redirect_uri(uri: &str, draft: bool) -> PlatformResult<()> {
    let bad = |m: &str| Err(PlatformError::Validation(format!("redirect URI {m}")));
    if uri.is_empty()
        || uri.len() > 2048
        || uri.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return bad("must be 1-2048 characters without spaces");
    }
    if uri.contains('#') {
        return bad("must not contain a fragment");
    }
    if uri.contains('*') {
        return bad("must not contain a wildcard");
    }
    let (rest, local) = if let Some(r) = uri.strip_prefix("https://") {
        (r, false)
    } else if let Some(r) = uri.strip_prefix("http://") {
        (r, true)
    } else {
        return bad("must use https://");
    };
    let authority = rest.split(['/', '?']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') {
        return bad("must name a host");
    }
    if local {
        let (host, port) = match authority.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (authority, None),
        };
        if host != "localhost"
            || port.is_some_and(|p| {
                p.is_empty() || p.len() > 5 || !p.bytes().all(|b| b.is_ascii_digit())
            })
        {
            return bad("may use http:// only as http://localhost[:port]");
        }
        if !draft {
            return bad("http://localhost is allowed only while the app is DRAFT");
        }
    }
    Ok(())
}

fn scope_list(values: &[String], allowed: &[Audience], field: &str) -> PlatformResult<Vec<String>> {
    if values.len() > oauth_scopes::MAX_REQUESTED {
        return Err(PlatformError::Validation(format!(
            "{field}: too many scopes"
        )));
    }
    let mut out = BTreeSet::new();
    for v in values {
        match oauth_scopes::find(v) {
            Some(s) if allowed.contains(&s.audience) => {
                out.insert(s.name.to_string());
            }
            _ => {
                return Err(PlatformError::Validation(format!(
                    "{field}: `{}` is not a scope of the catalogue for this audience",
                    v.chars().take(64).collect::<String>()
                )))
            }
        }
    }
    Ok(out.into_iter().collect())
}

fn id_list(values: &[Uuid], max: usize, field: &str) -> PlatformResult<Vec<Uuid>> {
    let set: BTreeSet<Uuid> = values.iter().copied().collect();
    if set.len() > max || set.iter().any(Uuid::is_nil) {
        return Err(PlatformError::Validation(format!(
            "{field}: at most {max} non-nil UUIDs"
        )));
    }
    Ok(set.into_iter().collect())
}

/// The target scope of a grant: one FIRST_PARTY triple.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetScope {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub kind: EnvironmentKind,
}

/// Issued tokens of one token-endpoint call (the only values carrying the raw tokens).
#[derive(Debug)]
pub struct IssuedTokens {
    pub access: IssuedSecret,
    pub refresh: IssuedSecret,
    pub bot: Option<IssuedSecret>,
    pub grant: GrantView,
}

/// A validated authorization request.
#[derive(Debug, Clone)]
pub struct ValidatedRequest {
    pub client: ClientRow,
    pub target: Target,
    pub scopes: BTreeSet<&'static str>,
    pub organization: Option<OrganizationScope>,
    pub code_challenge: String,
    pub redirect_uri: String,
    pub account_id: Uuid,
}

impl ValidatedRequest {
    /// The consent view (`AuthorizationContext`).
    pub fn to_json(&self) -> Value {
        json!({
            "app": {
                "clientId": self.client.application_id,
                "name": self.client.name,
                "distribution": self.client.distribution,
                "status": self.client.status,
                "developerName": self.client.developer_name,
            },
            "scopes": self.scopes.iter().filter_map(|s| oauth_scopes::find(s)).map(|s| json!({
                "name": s.name,
                "description": s.description,
                "who": s.audience.who(self.target),
            })).collect::<Vec<_>>(),
            "target": target_str(self.target),
        })
    }
}

#[derive(Debug, Clone)]
pub struct AuthorizationRequest {
    pub client_id: String,
    pub redirect_uri: String,
    pub scope: String,
    pub code_challenge: Option<String>,
    pub code_challenge_method: Option<String>,
    pub target: String,
    pub organization_id: Option<Uuid>,
    pub account_id: Uuid,
}

#[derive(Debug, Clone, Default)]
pub struct TokenRequest {
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub grant_type: Option<String>,
    pub code: Option<String>,
    pub redirect_uri: Option<String>,
    pub code_verifier: Option<String>,
    pub refresh_token: Option<String>,
}

// ---------------------------------------------------------------------------
// Grants
// ---------------------------------------------------------------------------

/// A grant (installation) as returned to the callers.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantView {
    pub id: Uuid,
    pub app_id: Uuid,
    pub app_name: String,
    pub target: String,
    pub account_id: Option<Uuid>,
    pub organization_id: Option<Uuid>,
    pub installed_by_account_id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub environment_kind: EnvironmentKind,
    pub principal_id: Uuid,
    pub service_account_id: Uuid,
    pub scopes: Vec<String>,
    pub status: String,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoke_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl GrantView {
    pub fn active(&self) -> bool {
        self.status == "ACTIVE"
    }

    pub fn target(&self) -> Target {
        parse_target(&self.target).unwrap_or(Target::User)
    }
}

const GRANT_SELECT: &str = "SELECT g.id, g.app_id, c.name AS app_name, g.target, g.account_id, \
        g.organization_id, g.installed_by_account_id, g.tenant_id, g.application_id, \
        g.environment_id, e.kind AS environment_kind, g.principal_id, g.service_account_id, \
        g.scopes, g.status, g.revoked_at, g.revoke_reason, g.created_at, g.updated_at \
     FROM platform_oauth_grants g \
     JOIN platform_oauth_clients c ON c.application_id = g.app_id \
     JOIN platform_environments e ON e.id = g.environment_id";

fn grant_from_row(r: &QueryResult) -> Result<GrantView, DbErr> {
    let kind: String = r.try_get("", "environment_kind")?;
    Ok(GrantView {
        id: r.try_get("", "id")?,
        app_id: r.try_get("", "app_id")?,
        app_name: r.try_get("", "app_name")?,
        target: r.try_get("", "target")?,
        account_id: r.try_get("", "account_id")?,
        organization_id: r.try_get("", "organization_id")?,
        installed_by_account_id: r.try_get("", "installed_by_account_id")?,
        tenant_id: r.try_get("", "tenant_id")?,
        application_id: r.try_get("", "application_id")?,
        environment_id: r.try_get("", "environment_id")?,
        environment_kind: EnvironmentKind::parse(&kind)
            .ok_or_else(|| DbErr::Custom("unknown environment kind".into()))?,
        principal_id: r.try_get("", "principal_id")?,
        service_account_id: r.try_get("", "service_account_id")?,
        scopes: r.try_get("", "scopes")?,
        status: r.try_get("", "status")?,
        revoked_at: r.try_get("", "revoked_at")?,
        revoke_reason: r.try_get("", "revoke_reason")?,
        created_at: r.try_get("", "created_at")?,
        updated_at: r.try_get("", "updated_at")?,
    })
}

/// Which grants to list.
#[derive(Debug, Clone, Copy)]
pub enum GrantFilter {
    /// ACTIVE USER grants of an account.
    Account(Uuid),
    /// ACTIVE ORGANIZATION grants of an organization.
    Organization(Uuid),
    /// Every grant of an app (any status), paginated.
    App(Uuid),
}

/// Why a grant was revoked (stored in `revoke_reason`).
pub mod revoke_reason {
    pub const UNINSTALLED: &str = "UNINSTALLED";
    pub const APP_SUSPENDED: &str = "APP_SUSPENDED";
    pub const CODE_REUSE: &str = "CODE_REUSE";
    pub const REFRESH_REUSE: &str = "REFRESH_REUSE";
}

fn pkce_matches(verifier: &str, challenge: &str) -> bool {
    let well_formed = (43..=128).contains(&verifier.len())
        && verifier
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b));
    well_formed && URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())) == challenge
}

fn valid_challenge(challenge: &str) -> bool {
    challenge.len() == 43
        && challenge
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl PlatformService {
    fn mint(
        &self,
        family: CredentialFamily,
        kind: EnvironmentKind,
    ) -> (credential_format::GeneratedCredential, Vec<u8>) {
        let generated = credential_format::generate_as(family, kind);
        let message = credential_format::verifier_message_as(
            family,
            kind,
            &generated.public_id,
            &generated.secret,
        );
        let verifier = self.keyring.compute_current(&message).to_vec();
        (generated, verifier)
    }

    fn verify_presented(
        &self,
        family: CredentialFamily,
        presented: &PresentedCredential,
        tag: &str,
        version: i32,
        verifier: &[u8],
    ) -> bool {
        let message = credential_format::verifier_message_as(
            family,
            presented.tag_kind,
            &presented.public_id,
            &presented.secret,
        );
        let ok = self.keyring.verify(version, &message, verifier);
        ok && presented.tag_kind.credential_tag() == tag
    }

    /// Equalises the work of an unknown value with a known one.
    fn burn(&self, family: CredentialFamily, presented: &PresentedCredential) {
        let message = credential_format::verifier_message_as(
            family,
            presented.tag_kind,
            &presented.public_id,
            &presented.secret,
        );
        let _ = self
            .keyring
            .verify(self.keyring.current_version(), &message, &[0u8; 32]);
    }

    pub async fn oauth_client(&self, application_id: Uuid) -> PlatformResult<Option<ClientRow>> {
        load_client(&self.db, application_id).await
    }

    /// Creates or replaces the client profile of an EXTERNAL tenant's application. A
    /// SUSPENDED status revokes every grant of the app in the same transaction. Returns
    /// the profile, whether it was created and how many grants were revoked.
    pub async fn put_oauth_client(
        &self,
        application_id: Uuid,
        input: ClientInput,
    ) -> PlatformResult<(ClientRow, bool, u64)> {
        let distribution = Distribution::parse(&input.distribution).ok_or_else(|| {
            PlatformError::Validation("distribution must be PRIVATE or PUBLIC".into())
        })?;
        let status = ClientStatus::parse(&input.status).ok_or_else(|| {
            PlatformError::Validation(
                "status must be DRAFT, APPROVED, PUBLISHED or SUSPENDED".into(),
            )
        })?;
        let name = super::display_name(&input.name)?;
        let mut redirect_uris: Vec<String> = Vec::new();
        for uri in &input.redirect_uris {
            validate_redirect_uri(uri, status == ClientStatus::Draft)?;
            if !redirect_uris.contains(uri) {
                redirect_uris.push(uri.clone());
            }
        }
        if redirect_uris.is_empty() || redirect_uris.len() > MAX_REDIRECT_URIS {
            return Err(PlatformError::Validation(format!(
                "between 1 and {MAX_REDIRECT_URIS} redirect URIs are required"
            )));
        }
        let user_scopes = scope_list(
            &input.user_scopes,
            &[Audience::User, Audience::Install],
            "userScopes",
        )?;
        let bot_scopes = scope_list(
            &input.bot_scopes,
            &[Audience::Bot, Audience::Install],
            "botScopes",
        )?;
        let test = id_list(&input.test_account_ids, MAX_TEST_ACCOUNTS, "testAccountIds")?;
        let accounts = id_list(
            &input.allowed_account_ids,
            MAX_ALLOWED_IDS,
            "allowedAccountIds",
        )?;
        let orgs = id_list(
            &input.allowed_organization_ids,
            MAX_ALLOWED_IDS,
            "allowedOrganizationIds",
        )?;
        let app = self
            .db
            .query_one(stmt(
                "SELECT a.tenant_id, t.classification FROM platform_applications a \
                 JOIN platform_tenants t ON t.id = a.tenant_id WHERE a.id = $1",
                vec![application_id.into()],
            ))
            .await?
            .ok_or(PlatformError::NotFound("application"))?;
        let tenant_id: Uuid = app.try_get("", "tenant_id")?;
        let classification: String = app.try_get("", "classification")?;
        if classification != "EXTERNAL" {
            return Err(PlatformError::PolicyViolation(
                "OAuth clients are applications of EXTERNAL (developer) tenants".into(),
            ));
        }
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        let created: bool = txn
            .query_one(stmt(
                "INSERT INTO platform_oauth_clients (application_id, tenant_id, name, redirect_uris, \
                   user_scopes, bot_scopes, distribution, status, test_account_ids, \
                   allowed_account_ids, allowed_organization_ids, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::uuid[], $10::uuid[], $11::uuid[], $12, $12) \
                 ON CONFLICT (application_id) DO UPDATE SET name = EXCLUDED.name, \
                   redirect_uris = EXCLUDED.redirect_uris, user_scopes = EXCLUDED.user_scopes, \
                   bot_scopes = EXCLUDED.bot_scopes, distribution = EXCLUDED.distribution, \
                   status = EXCLUDED.status, test_account_ids = EXCLUDED.test_account_ids, \
                   allowed_account_ids = EXCLUDED.allowed_account_ids, \
                   allowed_organization_ids = EXCLUDED.allowed_organization_ids, \
                   updated_at = EXCLUDED.updated_at \
                 RETURNING (xmax = 0) AS created",
                vec![
                    application_id.into(),
                    tenant_id.into(),
                    name.into(),
                    redirect_uris.into(),
                    user_scopes.into(),
                    bot_scopes.into(),
                    distribution.as_str().into(),
                    status.as_str().into(),
                    text_list(&test),
                    text_list(&accounts),
                    text_list(&orgs),
                    now.into(),
                ],
            ))
            .await?
            .map(|r| r.try_get::<bool>("", "created"))
            .transpose()?
            .unwrap_or(false);
        let revoked = if status == ClientStatus::Suspended {
            self.revoke_all_grants_in(&txn, application_id, revoke_reason::APP_SUSPENDED, now)
                .await?
        } else {
            0
        };
        txn.commit().await?;
        tracing::info!(
            application_id = %application_id,
            status = status.as_str(),
            revoked_grants = revoked,
            "OAuth client profile set"
        );
        let client = load_client(&self.db, application_id)
            .await?
            .ok_or(PlatformError::NotFound("OAuth client"))?;
        Ok((client, created, revoked))
    }

    /// Issues a new client secret (returned once). The current secret(s) keep working for
    /// 24 h. Returns `(secret, secretId, issuedAt)`.
    pub async fn issue_oauth_client_secret(
        &self,
        application_id: Uuid,
    ) -> PlatformResult<(IssuedSecret, Uuid, DateTime<Utc>)> {
        if load_client(&self.db, application_id).await?.is_none() {
            return Err(PlatformError::NotFound("OAuth client"));
        }
        let kinds = self
            .db
            .query_all(stmt(
                "SELECT kind FROM platform_environments WHERE application_id = $1",
                vec![application_id.into()],
            ))
            .await?;
        let kind = kinds
            .iter()
            .filter_map(|r| r.try_get::<String>("", "kind").ok())
            .filter_map(|k| EnvironmentKind::parse(&k))
            .max_by_key(|k| match k {
                EnvironmentKind::Sandbox => 0,
                EnvironmentKind::Test => 1,
                EnvironmentKind::Production => 2,
            })
            .unwrap_or(EnvironmentKind::Sandbox);
        let now = self.clock.now();
        let (generated, verifier) = self.mint(CLIENT_SECRET, kind);
        let id = Uuid::new_v4();
        let txn = self.db.begin().await?;
        txn.execute(stmt(
            "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
            vec![format!("platform-oauth-client-secret:{application_id}").into()],
        ))
        .await?;
        txn.execute(stmt(
            "UPDATE platform_oauth_client_secrets SET valid_until = $2 \
             WHERE application_id = $1 AND revoked_at IS NULL AND valid_until IS NULL",
            vec![
                application_id.into(),
                (now + Duration::hours(SECRET_OVERLAP_HOURS)).into(),
            ],
        ))
        .await?;
        txn.execute(stmt(
            "INSERT INTO platform_oauth_client_secrets (id, application_id, public_id, \
               environment_tag, verifier, verifier_key_version, last_four, issued_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            vec![
                id.into(),
                application_id.into(),
                generated.public_id.clone().into(),
                kind.credential_tag().into(),
                verifier.into(),
                self.keyring.current_version().into(),
                generated.last_four.clone().into(),
                now.into(),
            ],
        ))
        .await?;
        txn.commit().await?;
        tracing::info!(application_id = %application_id, secret_id = %id, "OAuth client secret issued");
        Ok((generated.token, id, now))
    }

    /// Client authentication of the token endpoint (`invalid_client` on any failure).
    pub async fn authenticate_oauth_client(
        &self,
        client_id: &str,
        secret: &str,
    ) -> OAuthResult<ClientRow> {
        let id = Uuid::parse_str(client_id).map_err(|_| OAuthError::TokenInvalidClient)?;
        let presented = credential_format::parse_as(CLIENT_SECRET, secret)
            .ok_or(OAuthError::TokenInvalidClient)?;
        let row = self
            .db
            .query_one(stmt(
                "SELECT environment_tag, verifier, verifier_key_version, valid_until, revoked_at \
                 FROM platform_oauth_client_secrets WHERE public_id = $1 AND application_id = $2",
                vec![presented.public_id.clone().into(), id.into()],
            ))
            .await?;
        let Some(row) = row else {
            self.burn(CLIENT_SECRET, &presented);
            return Err(OAuthError::TokenInvalidClient);
        };
        let tag: String = row.try_get("", "environment_tag")?;
        let verifier: Vec<u8> = row.try_get("", "verifier")?;
        let version: i32 = row.try_get("", "verifier_key_version")?;
        let valid_until: Option<DateTime<Utc>> = row.try_get("", "valid_until")?;
        let revoked_at: Option<DateTime<Utc>> = row.try_get("", "revoked_at")?;
        if !self.verify_presented(CLIENT_SECRET, &presented, &tag, version, &verifier) {
            return Err(OAuthError::TokenInvalidClient);
        }
        let now = self.clock.now();
        if revoked_at.is_some() || valid_until.is_some_and(|until| now >= until) {
            return Err(OAuthError::TokenInvalidClient);
        }
        let client = load_client(&self.db, id)
            .await?
            .ok_or(OAuthError::TokenInvalidClient)?;
        if client.status == ClientStatus::Suspended || !client.app_active {
            return Err(OAuthError::TokenInvalidClient);
        }
        Ok(client)
    }

    /// The FIRST_PARTY personal communications environment USER grants target: it must be
    /// a FIRST_PARTY tenant's environment and the whole chain ACTIVE.
    pub async fn oauth_personal_target(&self, environment_id: Uuid) -> PlatformResult<TargetScope> {
        let row = self
            .db
            .query_one(stmt(
                "SELECT e.tenant_id, e.application_id, e.kind, t.classification, \
                        (e.status = 'ACTIVE' AND a.status = 'ACTIVE' AND t.status = 'ACTIVE') AS active \
                 FROM platform_environments e \
                 JOIN platform_applications a ON a.id = e.application_id AND a.tenant_id = e.tenant_id \
                 JOIN platform_tenants t ON t.id = e.tenant_id WHERE e.id = $1",
                vec![environment_id.into()],
            ))
            .await?
            .ok_or(PlatformError::NotFound("environment"))?;
        let classification: String = row.try_get("", "classification")?;
        if classification != "FIRST_PARTY" {
            return Err(PlatformError::PolicyViolation(
                "USER grants target a FIRST_PARTY environment".into(),
            ));
        }
        if !row.try_get::<bool>("", "active")? {
            return Err(PlatformError::InvalidState(
                "the personal communications environment is suspended".into(),
            ));
        }
        let kind: String = row.try_get("", "kind")?;
        Ok(TargetScope {
            tenant_id: row.try_get("", "tenant_id")?,
            application_id: row.try_get("", "application_id")?,
            environment_id,
            kind: EnvironmentKind::parse(&kind)
                .ok_or_else(|| PlatformError::from(DbErr::Custom("environment kind".into())))?,
        })
    }

    /// Validates an authorization request (no side effect).
    pub async fn validate_authorization(
        &self,
        req: &AuthorizationRequest,
    ) -> OAuthResult<ValidatedRequest> {
        let app_id = Uuid::parse_str(&req.client_id).map_err(|_| OAuthError::InvalidClient)?;
        let client = load_client(&self.db, app_id)
            .await?
            .ok_or(OAuthError::InvalidClient)?;
        if !client.redirect_uris.contains(&req.redirect_uri)
            || (is_localhost(&req.redirect_uri) && client.status != ClientStatus::Draft)
        {
            return Err(OAuthError::InvalidRedirectUri);
        }
        let target = parse_target(&req.target).ok_or_else(|| {
            PlatformError::Validation("target must be USER or ORGANIZATION".into())
        })?;
        match (target, req.organization_id) {
            (Target::User, Some(_)) => {
                return Err(PlatformError::Validation(
                    "organizationId is only allowed for ORGANIZATION installs".into(),
                )
                .into())
            }
            (Target::Organization, None) => {
                return Err(PlatformError::Validation(
                    "ORGANIZATION installs need organizationId".into(),
                )
                .into())
            }
            _ => {}
        }
        let scopes =
            oauth_scopes::parse_scope_string(&req.scope).ok_or(OAuthError::InvalidScope)?;
        let requestable = client.requestable(target);
        if !scopes.iter().all(|s| requestable.contains(s)) {
            return Err(OAuthError::InvalidScope);
        }
        let challenge = req.code_challenge.clone().unwrap_or_default();
        if req.code_challenge_method.as_deref() != Some("S256") || !valid_challenge(&challenge) {
            return Err(OAuthError::PkceRequired);
        }
        if !client.may_authorize(target, req.account_id, req.organization_id) {
            return Err(OAuthError::AppNotAvailable);
        }
        let organization = match req.organization_id {
            Some(org) => {
                let scope = self
                    .organization_scope(org, ORGANIZATION_CAPABILITY)
                    .await?
                    .filter(OrganizationScope::verified)
                    .ok_or_else(|| {
                        PlatformError::InvalidState(
                            "the organization has no provisioned FIRST_PARTY scope".into(),
                        )
                    })?;
                if !scope.active() {
                    return Err(PlatformError::InvalidState(
                        "the organization scope is suspended".into(),
                    )
                    .into());
                }
                Some(scope)
            }
            None => None,
        };
        Ok(ValidatedRequest {
            client,
            target,
            scopes,
            organization,
            code_challenge: challenge,
            redirect_uri: req.redirect_uri.clone(),
            account_id: req.account_id,
        })
    }

    async fn ensure_installation_account<C: ConnectionTrait>(
        &self,
        txn: &C,
        app_id: Uuid,
        scope: &TargetScope,
    ) -> PlatformResult<Uuid> {
        txn.execute(stmt(
            "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
            vec![format!(
                "platform-oauth-installation:{app_id}:{}",
                scope.environment_id
            )
            .into()],
        ))
        .await?;
        let name = installation_account_name(app_id);
        if let Some(row) = txn
            .query_one(stmt(
                "SELECT id, status FROM platform_service_accounts WHERE environment_id = $1 AND name = $2",
                vec![scope.environment_id.into(), name.clone().into()],
            ))
            .await?
        {
            let status: String = row.try_get("", "status")?;
            if status != ServiceAccountStatus::Active.as_str() {
                return Err(PlatformError::InvalidState(
                    "the app's installation service account is not ACTIVE".into(),
                ));
            }
            return Ok(row.try_get("", "id")?);
        }
        let account = self
            .create_service_account_in(
                txn,
                EnvironmentId::from_uuid(scope.environment_id),
                NewServiceAccount {
                    name,
                    description: Some(format!(
                        "CF Meet app {app_id} installation (managed by Platform Access)"
                    )),
                },
            )
            .await?;
        Ok(account.id.as_uuid())
    }

    /// Validates the request, ensures the principal and installation service account,
    /// creates or extends the grant and returns a fresh single-use code with the grant id.
    /// `personal`: the USER target (the caller's trusted FIRST_PARTY personal scope).
    pub async fn authorize(
        &self,
        req: &AuthorizationRequest,
        personal: Option<TargetScope>,
    ) -> OAuthResult<(IssuedSecret, Uuid)> {
        let v = self.validate_authorization(req).await?;
        let app_id = v.client.application_id;
        let (scope, kind, reference) = match v.target {
            Target::User => (
                personal.ok_or_else(|| {
                    PlatformError::InvalidState(
                        "no personal communications environment is configured for USER grants"
                            .into(),
                    )
                })?,
                PrincipalKind::NativeCfUser,
                account_reference(v.account_id),
            ),
            Target::Organization => {
                let o = v.organization.as_ref().ok_or(OAuthError::InvalidRequest)?;
                (
                    TargetScope {
                        tenant_id: o.tenant.id.as_uuid(),
                        application_id: o.application.id.as_uuid(),
                        environment_id: o.environment.id.as_uuid(),
                        kind: o.environment.kind,
                    },
                    PrincipalKind::BotOrService,
                    bot_reference(app_id),
                )
            }
        };
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        let key = match v.target {
            Target::User => format!("account:{}", v.account_id),
            Target::Organization => {
                format!("organization:{}", req.organization_id.unwrap_or_default())
            }
        };
        txn.execute(stmt(
            "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
            vec![format!("platform-oauth-grant:{app_id}:{key}").into()],
        ))
        .await?;
        let (principal, _) = self
            .register_principal_in(
                &txn,
                EnvironmentId::from_uuid(scope.environment_id),
                NewPrincipal::External {
                    kind,
                    external_reference: reference,
                },
            )
            .await?;
        if principal.tombstoned_at.is_some() {
            return Err(PlatformError::InvalidState("the principal is tombstoned".into()).into());
        }
        if principal.status != PrincipalStatus::Active {
            match v.target {
                Target::User => {
                    return Err(PlatformError::InvalidState(
                        "the account's principal is not ACTIVE".into(),
                    )
                    .into())
                }
                // A bot deactivated by an earlier uninstall comes back with the reinstall.
                Target::Organization => {
                    self.set_principal_status_in(&txn, principal.id, PrincipalStatus::Active)
                        .await?;
                }
            }
        }
        let service_account = self
            .ensure_installation_account(&txn, app_id, &scope)
            .await?;
        let existing = match v.target {
            Target::User => {
                txn.query_one(stmt(
                    "SELECT id, scopes FROM platform_oauth_grants WHERE app_id = $1 AND target = 'USER' \
                     AND account_id = $2 AND status = 'ACTIVE' FOR UPDATE",
                    vec![app_id.into(), v.account_id.into()],
                ))
                .await?
            }
            Target::Organization => {
                txn.query_one(stmt(
                    "SELECT id, scopes FROM platform_oauth_grants WHERE app_id = $1 \
                     AND target = 'ORGANIZATION' AND organization_id = $2 AND status = 'ACTIVE' FOR UPDATE",
                    vec![app_id.into(), req.organization_id.into()],
                ))
                .await?
            }
        };
        let requested: Vec<String> = v.scopes.iter().map(|s| s.to_string()).collect();
        let grant_id = match existing {
            Some(row) => {
                let id: Uuid = row.try_get("", "id")?;
                let mut scopes: BTreeSet<String> = row
                    .try_get::<Vec<String>>("", "scopes")?
                    .into_iter()
                    .collect();
                scopes.extend(requested.iter().cloned());
                txn.execute(stmt(
                    "UPDATE platform_oauth_grants SET scopes = $2, updated_at = $3 WHERE id = $1",
                    vec![
                        id.into(),
                        scopes.into_iter().collect::<Vec<_>>().into(),
                        now.into(),
                    ],
                ))
                .await?;
                id
            }
            None => {
                let id = Uuid::new_v4();
                let (account, org) = match v.target {
                    Target::User => (Some(v.account_id), None),
                    Target::Organization => (None, req.organization_id),
                };
                txn.execute(stmt(
                    "INSERT INTO platform_oauth_grants (id, app_id, target, account_id, organization_id, \
                       installed_by_account_id, tenant_id, application_id, environment_id, principal_id, \
                       service_account_id, scopes, status, created_at, updated_at) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, 'ACTIVE', $13, $13)",
                    vec![
                        id.into(),
                        app_id.into(),
                        target_str(v.target).into(),
                        account.into(),
                        org.into(),
                        v.account_id.into(),
                        scope.tenant_id.into(),
                        scope.application_id.into(),
                        scope.environment_id.into(),
                        principal.id.as_uuid().into(),
                        service_account.into(),
                        requested.clone().into(),
                        now.into(),
                    ],
                ))
                .await?;
                id
            }
        };
        let (code, verifier) = self.mint(AUTHORIZATION_CODE, scope.kind);
        txn.execute(stmt(
            "INSERT INTO platform_oauth_codes (id, public_id, environment_tag, verifier, \
               verifier_key_version, grant_id, app_id, redirect_uri, code_challenge, scopes, \
               expires_at, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
            vec![
                Uuid::new_v4().into(),
                code.public_id.clone().into(),
                scope.kind.credential_tag().into(),
                verifier.into(),
                self.keyring.current_version().into(),
                grant_id.into(),
                app_id.into(),
                v.redirect_uri.clone().into(),
                v.code_challenge.clone().into(),
                requested.into(),
                (now + Duration::seconds(CODE_TTL_SECONDS)).into(),
                now.into(),
            ],
        ))
        .await?;
        txn.commit().await?;
        tracing::info!(app_id = %app_id, grant_id = %grant_id, target = target_str(v.target), "OAuth authorization granted");
        Ok((code.token, grant_id))
    }

    pub async fn oauth_grant(&self, id: Uuid) -> PlatformResult<Option<GrantView>> {
        let row = self
            .db
            .query_one(stmt(
                &format!("{GRANT_SELECT} WHERE g.id = $1"),
                vec![id.into()],
            ))
            .await?;
        Ok(row.as_ref().map(grant_from_row).transpose()?)
    }

    /// Lists grants; `after` is the id of the last grant of the previous page (App only).
    /// Returns the page and the next cursor.
    pub async fn list_oauth_grants(
        &self,
        filter: GrantFilter,
        after: Option<Uuid>,
        limit: u64,
    ) -> PlatformResult<(Vec<GrantView>, Option<Uuid>)> {
        let limit = limit.clamp(1, 200);
        let (clause, id) = match filter {
            GrantFilter::Account(a) => (
                "g.target = 'USER' AND g.account_id = $1 AND g.status = 'ACTIVE'",
                a,
            ),
            GrantFilter::Organization(o) => (
                "g.target = 'ORGANIZATION' AND g.organization_id = $1 AND g.status = 'ACTIVE'",
                o,
            ),
            GrantFilter::App(app) => ("g.app_id = $1", app),
        };
        let sql = format!(
            "{GRANT_SELECT} WHERE {clause} AND ($2::uuid IS NULL OR (g.created_at, g.id) > \
               (SELECT p.created_at, p.id FROM platform_oauth_grants p WHERE p.id = $2)) \
             ORDER BY g.created_at, g.id LIMIT $3"
        );
        let rows = self
            .db
            .query_all(stmt(
                &sql,
                vec![id.into(), after.into(), (limit as i64 + 1).into()],
            ))
            .await?;
        let mut items = rows
            .iter()
            .map(grant_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        let next = if items.len() as u64 > limit {
            items.truncate(limit as usize);
            items.last().map(|g| g.id)
        } else {
            None
        };
        Ok((items, next))
    }

    /// Revokes a grant inside `txn` (tokens, refresh tokens, pending codes; the bot of an
    /// ORGANIZATION grant is DEACTIVATED). `false` when it was not ACTIVE.
    pub(crate) async fn revoke_grant_in<C: ConnectionTrait>(
        &self,
        txn: &C,
        grant_id: Uuid,
        reason: &str,
        now: DateTime<Utc>,
    ) -> PlatformResult<bool> {
        let Some(row) = txn
            .query_one(stmt(
                "UPDATE platform_oauth_grants SET status = 'REVOKED', revoked_at = $2, \
                   revoke_reason = $3, updated_at = $2 \
                 WHERE id = $1 AND status = 'ACTIVE' RETURNING target, principal_id",
                vec![grant_id.into(), now.into(), reason.into()],
            ))
            .await?
        else {
            return Ok(false);
        };
        txn.execute(stmt(
            "UPDATE platform_api_credentials SET status = 'REVOKED', revoked_at = $2, updated_at = $2 \
             WHERE status = 'ACTIVE' AND id IN \
               (SELECT credential_id FROM platform_oauth_tokens WHERE grant_id = $1)",
            vec![grant_id.into(), now.into()],
        ))
        .await?;
        txn.execute(stmt(
            "UPDATE platform_oauth_refresh_tokens SET revoked_at = $2 \
             WHERE grant_id = $1 AND revoked_at IS NULL",
            vec![grant_id.into(), now.into()],
        ))
        .await?;
        txn.execute(stmt(
            "UPDATE platform_oauth_codes SET consumed_at = $2 WHERE grant_id = $1 AND consumed_at IS NULL",
            vec![grant_id.into(), now.into()],
        ))
        .await?;
        let target: String = row.try_get("", "target")?;
        if target == "ORGANIZATION" {
            let principal: Uuid = row.try_get("", "principal_id")?;
            txn.execute(stmt(
                "UPDATE platform_principals SET status = 'DEACTIVATED', updated_at = $2 \
                 WHERE id = $1 AND status = 'ACTIVE' AND tombstoned_at IS NULL",
                vec![principal.into(), now.into()],
            ))
            .await?;
        }
        crate::metrics::inc(
            "platform_access_oauth_grants_revoked_total",
            &[("reason", reason_label(reason))],
        );
        tracing::info!(grant_id = %grant_id, reason, "OAuth grant revoked");
        Ok(true)
    }

    async fn revoke_all_grants_in<C: ConnectionTrait>(
        &self,
        txn: &C,
        app_id: Uuid,
        reason: &str,
        now: DateTime<Utc>,
    ) -> PlatformResult<u64> {
        let rows = txn
            .query_all(stmt(
                "SELECT id FROM platform_oauth_grants WHERE app_id = $1 AND status = 'ACTIVE' \
                 ORDER BY id FOR UPDATE",
                vec![app_id.into()],
            ))
            .await?;
        let mut n = 0;
        for r in rows {
            let id: Uuid = r.try_get("", "id")?;
            if self.revoke_grant_in(txn, id, reason, now).await? {
                n += 1;
            }
        }
        Ok(n)
    }

    /// Revokes a grant (idempotent). `None`: unknown grant.
    pub async fn revoke_oauth_grant(
        &self,
        grant_id: Uuid,
        reason: &str,
    ) -> PlatformResult<Option<GrantView>> {
        let txn = self.db.begin().await?;
        self.revoke_grant_in(&txn, grant_id, reason, self.clock.now())
            .await?;
        txn.commit().await?;
        self.oauth_grant(grant_id).await
    }

    /// Issues an access or bot token credential for `grant` inside `db`.
    async fn issue_token_credential<C: ConnectionTrait>(
        &self,
        db: &C,
        grant: &GrantView,
        kind: TokenKind,
        now: DateTime<Utc>,
    ) -> PlatformResult<IssuedSecret> {
        let (family, lifetime, label) = match kind {
            TokenKind::Access => (
                ACCESS_TOKEN,
                Duration::seconds(ACCESS_TTL_SECONDS),
                "OAuth access token",
            ),
            TokenKind::Bot => (BOT_TOKEN, Duration::days(BOT_TTL_DAYS), "OAuth bot token"),
        };
        let (generated, verifier) = self.mint(family, grant.environment_kind);
        let id = CredentialId::generate().as_uuid();
        db.execute(stmt(
            "INSERT INTO platform_api_credentials (id, public_id, tenant_id, application_id, \
               environment_id, service_account_id, label, verifier, verifier_algorithm, \
               verifier_key_version, last_four, status, not_before, expires_at, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, 'ACTIVE', $12, $13, $12, $12)",
            vec![
                id.into(),
                generated.public_id.clone().into(),
                grant.tenant_id.into(),
                grant.application_id.into(),
                grant.environment_id.into(),
                grant.service_account_id.into(),
                label.into(),
                verifier.into(),
                VERIFIER_ALGORITHM.into(),
                self.keyring.current_version().into(),
                generated.last_four.clone().into(),
                now.into(),
                (now + lifetime).into(),
            ],
        ))
        .await?;
        db.execute(stmt(
            "INSERT INTO platform_credential_principal_restrictions \
               (credential_id, principal_id, tenant_id, application_id, environment_id, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6)",
            vec![
                id.into(),
                grant.principal_id.into(),
                grant.tenant_id.into(),
                grant.application_id.into(),
                grant.environment_id.into(),
                now.into(),
            ],
        ))
        .await?;
        db.execute(stmt(
            "INSERT INTO platform_oauth_tokens (credential_id, grant_id, kind, created_at) \
             VALUES ($1, $2, $3, $4)",
            vec![id.into(), grant.id.into(), kind.as_str().into(), now.into()],
        ))
        .await?;
        Ok(generated.token)
    }

    async fn issue_refresh_token<C: ConnectionTrait>(
        &self,
        db: &C,
        grant: &GrantView,
        id: Uuid,
        now: DateTime<Utc>,
    ) -> PlatformResult<IssuedSecret> {
        let (generated, verifier) = self.mint(REFRESH_TOKEN, grant.environment_kind);
        db.execute(stmt(
            "INSERT INTO platform_oauth_refresh_tokens (id, public_id, environment_tag, verifier, \
               verifier_key_version, grant_id, expires_at, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            vec![
                id.into(),
                generated.public_id.clone().into(),
                grant.environment_kind.credential_tag().into(),
                verifier.into(),
                self.keyring.current_version().into(),
                grant.id.into(),
                (now + Duration::days(REFRESH_TTL_DAYS)).into(),
                now.into(),
            ],
        ))
        .await?;
        Ok(generated.token)
    }

    async fn revoke_for_reuse(&self, grant_id: Uuid, reason: &str) -> OAuthResult<()> {
        tracing::warn!(grant_id = %grant_id, reason, "OAuth credential reuse: revoking the grant");
        let txn = self.db.begin().await?;
        self.revoke_grant_in(&txn, grant_id, reason, self.clock.now())
            .await?;
        txn.commit().await?;
        Ok(())
    }

    /// The token endpoint backend (authorization_code / refresh_token).
    pub async fn oauth_token(&self, req: TokenRequest) -> OAuthResult<IssuedTokens> {
        let non_empty =
            |v: &Option<String>| v.as_deref().filter(|s| !s.is_empty()).map(str::to_string);
        let (Some(client_id), Some(secret), Some(grant_type)) = (
            non_empty(&req.client_id),
            non_empty(&req.client_secret),
            non_empty(&req.grant_type),
        ) else {
            return Err(OAuthError::InvalidRequest);
        };
        let client = self.authenticate_oauth_client(&client_id, &secret).await?;
        match grant_type.as_str() {
            "authorization_code" => {
                let (Some(code), Some(redirect_uri), Some(verifier)) = (
                    non_empty(&req.code),
                    non_empty(&req.redirect_uri),
                    non_empty(&req.code_verifier),
                ) else {
                    return Err(OAuthError::InvalidRequest);
                };
                self.exchange_code(&client, &code, &redirect_uri, &verifier)
                    .await
            }
            "refresh_token" => {
                let Some(token) = non_empty(&req.refresh_token) else {
                    return Err(OAuthError::InvalidRequest);
                };
                self.exchange_refresh(&client, &token).await
            }
            _ => Err(OAuthError::UnsupportedGrantType),
        }
    }

    async fn locked_active_grant<C: ConnectionTrait>(
        &self,
        txn: &C,
        grant_id: Uuid,
    ) -> OAuthResult<GrantView> {
        txn.execute(stmt(
            "SELECT 1 FROM platform_oauth_grants WHERE id = $1 FOR UPDATE",
            vec![grant_id.into()],
        ))
        .await?;
        let row = txn
            .query_one(stmt(
                &format!("{GRANT_SELECT} WHERE g.id = $1"),
                vec![grant_id.into()],
            ))
            .await?
            .ok_or(OAuthError::InvalidGrant)?;
        let grant = grant_from_row(&row)?;
        if !grant.active() {
            return Err(OAuthError::InvalidGrant);
        }
        Ok(grant)
    }

    async fn exchange_code(
        &self,
        client: &ClientRow,
        code: &str,
        redirect_uri: &str,
        code_verifier: &str,
    ) -> OAuthResult<IssuedTokens> {
        let presented = credential_format::parse_as(AUTHORIZATION_CODE, code)
            .ok_or(OAuthError::InvalidGrant)?;
        let Some(row) = self
            .db
            .query_one(stmt(
                "SELECT id, environment_tag, verifier, verifier_key_version, grant_id, app_id, \
                        redirect_uri, code_challenge, expires_at \
                 FROM platform_oauth_codes WHERE public_id = $1",
                vec![presented.public_id.clone().into()],
            ))
            .await?
        else {
            self.burn(AUTHORIZATION_CODE, &presented);
            return Err(OAuthError::InvalidGrant);
        };
        let tag: String = row.try_get("", "environment_tag")?;
        let verifier: Vec<u8> = row.try_get("", "verifier")?;
        let version: i32 = row.try_get("", "verifier_key_version")?;
        if !self.verify_presented(AUTHORIZATION_CODE, &presented, &tag, version, &verifier) {
            return Err(OAuthError::InvalidGrant);
        }
        let app_id: Uuid = row.try_get("", "app_id")?;
        if app_id != client.application_id {
            return Err(OAuthError::InvalidGrant);
        }
        let code_id: Uuid = row.try_get("", "id")?;
        let grant_id: Uuid = row.try_get("", "grant_id")?;
        let now = self.clock.now();
        let consumed = self
            .db
            .execute(stmt(
                "UPDATE platform_oauth_codes SET consumed_at = $2 WHERE id = $1 AND consumed_at IS NULL",
                vec![code_id.into(), now.into()],
            ))
            .await?
            .rows_affected();
        if consumed == 0 {
            self.revoke_for_reuse(grant_id, revoke_reason::CODE_REUSE)
                .await?;
            return Err(OAuthError::InvalidGrant);
        }
        let expires_at: DateTime<Utc> = row.try_get("", "expires_at")?;
        let bound_redirect: String = row.try_get("", "redirect_uri")?;
        let challenge: String = row.try_get("", "code_challenge")?;
        if now >= expires_at
            || bound_redirect != redirect_uri
            || !pkce_matches(code_verifier, &challenge)
        {
            return Err(OAuthError::InvalidGrant);
        }
        let txn = self.db.begin().await?;
        let grant = self.locked_active_grant(&txn, grant_id).await?;
        let access = self
            .issue_token_credential(&txn, &grant, TokenKind::Access, now)
            .await?;
        let refresh = self
            .issue_refresh_token(&txn, &grant, Uuid::new_v4(), now)
            .await?;
        let bot = if grant.target() == Target::Organization
            && oauth_scopes::has_bot_scopes(grant.scopes.iter().map(String::as_str))
            && txn
                .query_one(stmt(
                    "SELECT 1 AS x FROM platform_oauth_tokens WHERE grant_id = $1 AND kind = 'BOT'",
                    vec![grant.id.into()],
                ))
                .await?
                .is_none()
        {
            Some(
                self.issue_token_credential(&txn, &grant, TokenKind::Bot, now)
                    .await?,
            )
        } else {
            None
        };
        txn.commit().await?;
        crate::metrics::inc(
            "platform_access_oauth_tokens_issued_total",
            &[("grant_type", "authorization_code")],
        );
        Ok(IssuedTokens {
            access,
            refresh,
            bot,
            grant,
        })
    }

    async fn exchange_refresh(&self, client: &ClientRow, token: &str) -> OAuthResult<IssuedTokens> {
        let presented =
            credential_format::parse_as(REFRESH_TOKEN, token).ok_or(OAuthError::InvalidGrant)?;
        let Some(row) = self
            .db
            .query_one(stmt(
                "SELECT r.id, r.environment_tag, r.verifier, r.verifier_key_version, r.grant_id, \
                        r.expires_at, r.superseded_at, r.revoked_at, g.app_id \
                 FROM platform_oauth_refresh_tokens r \
                 JOIN platform_oauth_grants g ON g.id = r.grant_id WHERE r.public_id = $1",
                vec![presented.public_id.clone().into()],
            ))
            .await?
        else {
            self.burn(REFRESH_TOKEN, &presented);
            return Err(OAuthError::InvalidGrant);
        };
        let tag: String = row.try_get("", "environment_tag")?;
        let verifier: Vec<u8> = row.try_get("", "verifier")?;
        let version: i32 = row.try_get("", "verifier_key_version")?;
        if !self.verify_presented(REFRESH_TOKEN, &presented, &tag, version, &verifier) {
            return Err(OAuthError::InvalidGrant);
        }
        let app_id: Uuid = row.try_get("", "app_id")?;
        if app_id != client.application_id {
            return Err(OAuthError::InvalidGrant);
        }
        let id: Uuid = row.try_get("", "id")?;
        let grant_id: Uuid = row.try_get("", "grant_id")?;
        let superseded: Option<DateTime<Utc>> = row.try_get("", "superseded_at")?;
        if superseded.is_some() {
            self.revoke_for_reuse(grant_id, revoke_reason::REFRESH_REUSE)
                .await?;
            return Err(OAuthError::InvalidGrant);
        }
        let revoked: Option<DateTime<Utc>> = row.try_get("", "revoked_at")?;
        let expires_at: DateTime<Utc> = row.try_get("", "expires_at")?;
        let now = self.clock.now();
        if revoked.is_some() || now >= expires_at {
            return Err(OAuthError::InvalidGrant);
        }
        let txn = self.db.begin().await?;
        let grant = self.locked_active_grant(&txn, grant_id).await?;
        let successor = Uuid::new_v4();
        let refresh = self
            .issue_refresh_token(&txn, &grant, successor, now)
            .await?;
        let rotated = txn
            .execute(stmt(
                "UPDATE platform_oauth_refresh_tokens SET superseded_at = $2, superseded_by_id = $3 \
                 WHERE id = $1 AND superseded_at IS NULL AND revoked_at IS NULL",
                vec![id.into(), now.into(), successor.into()],
            ))
            .await?
            .rows_affected();
        if rotated == 0 {
            // A concurrent use of the same refresh token won the race: reuse.
            txn.rollback().await?;
            self.revoke_for_reuse(grant_id, revoke_reason::REFRESH_REUSE)
                .await?;
            return Err(OAuthError::InvalidGrant);
        }
        let access = self
            .issue_token_credential(&txn, &grant, TokenKind::Access, now)
            .await?;
        txn.commit().await?;
        crate::metrics::inc(
            "platform_access_oauth_tokens_issued_total",
            &[("grant_type", "refresh_token")],
        );
        Ok(IssuedTokens {
            access,
            refresh,
            bot: None,
            grant,
        })
    }

    /// RFC 7009 revocation: revokes the presented access / bot credential or refresh token
    /// when the client authenticates and owns it; anything else is silently ignored.
    pub async fn revoke_oauth_token(
        &self,
        client_id: &str,
        secret: &str,
        token: &str,
    ) -> PlatformResult<()> {
        let client = match self.authenticate_oauth_client(client_id, secret).await {
            Ok(c) => c,
            Err(OAuthError::Platform(e)) => return Err(e),
            Err(_) => return Ok(()),
        };
        let now = self.clock.now();
        if let Some(presented) = credential_format::parse_as(REFRESH_TOKEN, token) {
            let Some(row) = self
                .db
                .query_one(stmt(
                    "SELECT r.id, r.environment_tag, r.verifier, r.verifier_key_version, g.app_id \
                     FROM platform_oauth_refresh_tokens r \
                     JOIN platform_oauth_grants g ON g.id = r.grant_id WHERE r.public_id = $1",
                    vec![presented.public_id.clone().into()],
                ))
                .await?
            else {
                return Ok(());
            };
            let tag: String = row.try_get("", "environment_tag")?;
            let verifier: Vec<u8> = row.try_get("", "verifier")?;
            let version: i32 = row.try_get("", "verifier_key_version")?;
            let app: Uuid = row.try_get("", "app_id")?;
            if app == client.application_id
                && self.verify_presented(REFRESH_TOKEN, &presented, &tag, version, &verifier)
            {
                let id: Uuid = row.try_get("", "id")?;
                self.db
                    .execute(stmt(
                        "UPDATE platform_oauth_refresh_tokens SET revoked_at = $2 \
                         WHERE id = $1 AND revoked_at IS NULL",
                        vec![id.into(), now.into()],
                    ))
                    .await?;
            }
            return Ok(());
        }
        let (family, kind) = if token.starts_with("cfoa_") {
            (ACCESS_TOKEN, TokenKind::Access)
        } else if token.starts_with("cfob_") {
            (BOT_TOKEN, TokenKind::Bot)
        } else {
            return Ok(());
        };
        let Some(presented) = credential_format::parse_as(family, token) else {
            return Ok(());
        };
        let Some(row) = self
            .db
            .query_one(stmt(
                "SELECT c.id, c.verifier, c.verifier_key_version, e.kind AS environment_kind, \
                        o.kind AS token_kind, g.app_id \
                 FROM platform_api_credentials c \
                 JOIN platform_oauth_tokens o ON o.credential_id = c.id \
                 JOIN platform_oauth_grants g ON g.id = o.grant_id \
                 JOIN platform_environments e ON e.id = c.environment_id \
                 WHERE c.public_id = $1",
                vec![presented.public_id.clone().into()],
            ))
            .await?
        else {
            return Ok(());
        };
        let env_kind: String = row.try_get("", "environment_kind")?;
        let tag = EnvironmentKind::parse(&env_kind)
            .map(|k| k.credential_tag())
            .unwrap_or("");
        let verifier: Vec<u8> = row.try_get("", "verifier")?;
        let version: i32 = row.try_get("", "verifier_key_version")?;
        let token_kind: String = row.try_get("", "token_kind")?;
        let app: Uuid = row.try_get("", "app_id")?;
        if app == client.application_id
            && token_kind == kind.as_str()
            && self.verify_presented(family, &presented, tag, version, &verifier)
        {
            let id: Uuid = row.try_get("", "id")?;
            self.revoke_credential_in(&self.db, CredentialId::from_uuid(id))
                .await?;
        }
        Ok(())
    }
}

fn reason_label(reason: &str) -> &'static str {
    match reason {
        revoke_reason::UNINSTALLED => "uninstalled",
        revoke_reason::APP_SUSPENDED => "app_suspended",
        revoke_reason::CODE_REUSE => "code_reuse",
        revoke_reason::REFRESH_REUSE => "refresh_reuse",
        _ => "other",
    }
}

async fn load_client<C: ConnectionTrait>(db: &C, id: Uuid) -> PlatformResult<Option<ClientRow>> {
    let row = db.query_one(stmt(CLIENT_SELECT, vec![id.into()])).await?;
    Ok(row.as_ref().map(client_from_row).transpose()?)
}

/// Deletes authorization codes that expired before `before` (they can never be used).
pub async fn prune_codes<C: ConnectionTrait>(db: &C, before: DateTime<Utc>) -> Result<u64, DbErr> {
    Ok(db
        .execute(stmt(
            "DELETE FROM platform_oauth_codes WHERE expires_at < $1",
            vec![before.into()],
        ))
        .await?
        .rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_uris_follow_the_rules() {
        for ok in [
            "https://app.example.com/callback",
            "https://app.example.com:8443/cb?x=1",
            "https://app.example.com",
        ] {
            assert!(validate_redirect_uri(ok, false).is_ok(), "{ok}");
        }
        for ok in [
            "http://localhost/cb",
            "http://localhost:3000/cb",
            "http://localhost",
        ] {
            assert!(validate_redirect_uri(ok, true).is_ok(), "{ok}");
            assert!(validate_redirect_uri(ok, false).is_err(), "{ok}");
        }
        for bad in [
            "",
            "ftp://x/y",
            "https://app.example.com/cb#frag",
            "https://*.example.com/cb",
            "https:///cb",
            "https://user@app.example.com/cb",
            "http://127.0.0.1/cb",
            "http://localhost:abc/cb",
            "http://example.com/cb",
            "https://app.example.com/c b",
        ] {
            assert!(validate_redirect_uri(bad, true).is_err(), "{bad}");
        }
    }

    #[test]
    fn pkce_is_s256() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        assert!(valid_challenge(challenge));
        assert!(pkce_matches(verifier, challenge));
        assert!(!pkce_matches(&verifier.replace('d', "e"), challenge));
        assert!(!pkce_matches("short", challenge));
        assert!(!valid_challenge("plain"));
    }

    #[test]
    fn pairwise_subjects_are_stable_per_app_and_distinct_across_apps() {
        let key =
            PairwiseKey::parse("K", "pairwise-unit-key-0123456789abcdefghijklmnopqrstuv").unwrap();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let account = Uuid::new_v4().to_string();
        assert_eq!(key.sub(a, &account), key.sub(a, &account));
        assert_ne!(key.sub(a, &account), key.sub(b, &account));
        assert_eq!(key.sub(a, &account).len(), 43);
        assert!(!format!("{key:?}").contains("pairwise-unit"));
    }

    fn client(status: ClientStatus, distribution: Distribution) -> ClientRow {
        ClientRow {
            application_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            name: "App".into(),
            redirect_uris: vec![],
            user_scopes: vec![],
            bot_scopes: vec![],
            distribution,
            status,
            test_account_ids: vec![],
            allowed_account_ids: vec![],
            allowed_organization_ids: vec![],
            revision: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            developer_name: "Dev".into(),
            app_active: true,
            secret_issued_at: None,
        }
    }

    #[test]
    fn distribution_rules() {
        let (tester, member, stranger, org) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        let mut c = client(ClientStatus::Draft, Distribution::Public);
        c.test_account_ids = vec![tester];
        c.allowed_account_ids = vec![member];
        c.allowed_organization_ids = vec![org];
        assert!(c.may_authorize(Target::User, tester, None));
        assert!(!c.may_authorize(Target::User, member, None));
        c.status = ClientStatus::Approved;
        assert!(!c.may_authorize(Target::User, stranger, None));
        c.status = ClientStatus::Published;
        assert!(c.may_authorize(Target::User, stranger, None));
        c.distribution = Distribution::Private;
        assert!(c.may_authorize(Target::User, member, None));
        assert!(!c.may_authorize(Target::User, stranger, None));
        assert!(c.may_authorize(Target::Organization, stranger, Some(org)));
        assert!(!c.may_authorize(Target::Organization, member, Some(Uuid::new_v4())));
        c.status = ClientStatus::Suspended;
        assert!(!c.may_authorize(Target::User, tester, None));
        c.status = ClientStatus::Published;
        c.app_active = false;
        assert!(!c.may_authorize(Target::User, member, None));
    }
}
