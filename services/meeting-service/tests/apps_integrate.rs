//! ADR 0064 / 0065 — CF Meet Apps in the Meeting Service (integrate contract §3), against a
//! scripted Platform Access (`platform-oauth.authorize`) and a scripted Organizations
//! install policy (`organization-apps.evaluate`):
//!
//! * the consent relay: context / approve / deny (account required, the signed-in account
//!   id relayed, the redirect built only for a redirect URI Platform Access validated,
//!   state echoed), workspace installs refused for non-admins by default;
//! * Connected apps (list + revoke own grants only) and organization Apps (admins only);
//! * the `DelegatedUser` actor: OAuth access tokens act for their user on the machine
//!   operations, own meetings only (read also host / co-host), never join or recordings,
//!   audited `via app`, revocation epoch and flags enforced;
//! * the internal userinfo route (email only with `profile:email`);
//! * in-meeting apps: listing, app-context token claims, the public JWKS;
//! * every flag off: unchanged (`FEATURE_DISABLED`, OAuth credentials refused).

mod common;

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

use async_trait::async_trait;
use axum::http::{Method, StatusCode};
use backend::services::{
    actor::PlatformActor,
    apps::{
        context::{self, AppContextSigner},
        oauth::{GrantOwner, OauthAuthority, PaAnswer},
        organizations::{AppOrganizations, InstallDecision},
        Apps,
    },
    platform::authority::{
        Authentication, AuthorityChain, AuthorityUnavailable, OauthCredential, OauthCredentialKind,
    },
};
use cf_service_auth::{
    ApplicationEnvironmentKind, DelegatedPlatformContext, OutboundServiceRequest, ServiceId,
    TrustPolicy,
};
use common::{platform::*, *};
use serde_json::{json, Value};
use uuid::Uuid;

// ---------------------------------------------------------------------------------------
// Fakes
// ---------------------------------------------------------------------------------------

#[derive(Default)]
struct FakeOauth {
    /// Registered apps: client id → redirect URI.
    apps: Mutex<Vec<(Uuid, String)>>,
    grants: Mutex<Vec<Value>>,
    validations: Mutex<Vec<Value>>,
    authorizations: Mutex<Vec<Value>>,
    revoked: Mutex<Vec<Uuid>>,
    calls: AtomicUsize,
}

impl FakeOauth {
    fn register(&self, redirect: &str) -> Uuid {
        let id = Uuid::new_v4();
        self.apps.lock().unwrap().push((id, redirect.to_string()));
        id
    }

    fn grant(&self, g: Value) -> Uuid {
        let id = Uuid::new_v4();
        let mut g = g;
        g["id"] = json!(id);
        self.grants.lock().unwrap().push(g);
        id
    }

    fn check(&self, req: &Value) -> PaAnswer {
        let refuse = |code: &str| PaAnswer {
            status: 400,
            body: json!({ "error": { "code": code, "message": code } }),
        };
        let Some(client) = req["clientId"]
            .as_str()
            .and_then(|c| Uuid::parse_str(c).ok())
        else {
            return refuse("INVALID_CLIENT");
        };
        let apps = self.apps.lock().unwrap();
        let Some((_, redirect)) = apps.iter().find(|(id, _)| *id == client) else {
            return refuse("INVALID_CLIENT");
        };
        if req["redirectUri"] != json!(redirect) {
            return refuse("INVALID_REDIRECT_URI");
        }
        PaAnswer {
            status: 200,
            body: json!({
                "app": { "clientId": client, "name": "Calendar Sync", "distribution": "PUBLIC",
                         "status": "PUBLISHED", "developerName": "Acme" },
                "scopes": [{ "name": "meetings:write", "description": "Create meetings", "who": "user" }],
                "target": req["target"],
            }),
        }
    }
}

#[async_trait]
impl OauthAuthority for FakeOauth {
    async fn validate(&self, request: &Value) -> Result<PaAnswer, AuthorityUnavailable> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.validations.lock().unwrap().push(request.clone());
        Ok(self.check(request))
    }

    async fn authorize(&self, request: &Value) -> Result<PaAnswer, AuthorityUnavailable> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.authorizations.lock().unwrap().push(request.clone());
        let checked = self.check(request);
        if checked.status != 200 {
            return Ok(checked);
        }
        Ok(PaAnswer {
            status: 201,
            body: json!({ "code": "code-123", "expiresIn": 60, "grantId": Uuid::new_v4() }),
        })
    }

    async fn grants(&self, owner: GrantOwner) -> Result<PaAnswer, AuthorityUnavailable> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let items: Vec<Value> = self
            .grants
            .lock()
            .unwrap()
            .iter()
            .filter(|g| match owner {
                GrantOwner::Account(a) => g["accountId"] == json!(a) && g["target"] == "USER",
                GrantOwner::Organization(o) => {
                    g["organizationId"] == json!(o) && g["target"] == "ORGANIZATION"
                }
            })
            .cloned()
            .collect();
        Ok(PaAnswer {
            status: 200,
            body: json!({ "items": items }),
        })
    }

    async fn revoke(&self, grant_id: Uuid) -> Result<PaAnswer, AuthorityUnavailable> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.revoked.lock().unwrap().push(grant_id);
        self.grants
            .lock()
            .unwrap()
            .retain(|g| g["id"] != json!(grant_id));
        Ok(PaAnswer {
            status: 204,
            body: Value::Null,
        })
    }
}

/// Organizations: who administers which organization, and the default ADMINS tier.
#[derive(Default)]
struct FakeOrganizations {
    admins: Mutex<Vec<(Uuid, Uuid)>>,
    checks: AtomicUsize,
}

#[async_trait]
impl AppOrganizations for FakeOrganizations {
    async fn install_check(
        &self,
        organization: Uuid,
        account: Uuid,
        _app: Uuid,
    ) -> Result<InstallDecision, AuthorityUnavailable> {
        self.checks.fetch_add(1, Ordering::SeqCst);
        Ok(
            if self
                .admins
                .lock()
                .unwrap()
                .contains(&(organization, account))
            {
                InstallDecision::Allowed
            } else {
                InstallDecision::Refused("ROLE_REQUIRED".into())
            },
        )
    }

    async fn is_admin(
        &self,
        organization: Uuid,
        account: Uuid,
    ) -> Result<bool, AuthorityUnavailable> {
        Ok(self
            .admins
            .lock()
            .unwrap()
            .contains(&(organization, account)))
    }
}

struct Fakes {
    oauth: Arc<FakeOauth>,
    orgs: Arc<FakeOrganizations>,
    signer: Arc<AppContextSigner>,
}

fn apps(all: bool) -> (Apps, Fakes) {
    let oauth = Arc::new(FakeOauth::default());
    let orgs = Arc::new(FakeOrganizations::default());
    let signer = Arc::new(AppContextSigner::from_signing_key(
        ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]),
    ));
    (
        Apps {
            authorize_enabled: all,
            delegated_enabled: all,
            in_meeting_enabled: all,
            oauth: Some(oauth.clone()),
            organizations: Some(orgs.clone()),
            context: Some(signer.clone()),
        },
        Fakes {
            oauth,
            orgs,
            signer,
        },
    )
}

async fn app_with(all: bool) -> Option<(TestApp, Fakes)> {
    let (apps, fakes) = apps(all);
    let app = TestApp::with(TestOptions {
        apps: Some(apps),
        ..Default::default()
    })
    .await?;
    Some((app, fakes))
}

const REDIRECT: &str = "https://calendar.example/oauth/callback?tenant=7";
const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

fn context_uri(client: Uuid, extra: &str) -> String {
    format!(
        "/api/v2/oauth/authorize/context?client_id={client}&redirect_uri={}&scope=meetings%3Awrite&state=st%201&code_challenge={CHALLENGE}&code_challenge_method=S256{extra}",
        urlencoding::encode(REDIRECT)
    )
}

fn decision(client: Uuid, decision: &str) -> Value {
    json!({
        "clientId": client, "redirectUri": REDIRECT, "scope": "meetings:write", "state": "st 1",
        "codeChallenge": CHALLENGE, "codeChallengeMethod": "S256", "decision": decision,
    })
}

// ---------------------------------------------------------------------------------------
// Consent relay
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn consent_context_approve_and_deny_build_the_redirect_for_the_signed_in_account() {
    let Some((app, f)) = app_with(true).await else {
        return;
    };
    let user = app.user("consent").await;
    let client = f.oauth.register(REDIRECT);

    // An account session is required.
    let (s, v) = app.get(context_uri(client, ""), Auth::None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");
    assert_eq!(f.oauth.calls.load(Ordering::SeqCst), 0);

    let (s, v) = app
        .get(context_uri(client, ""), Auth::Bearer(&user.token))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["app"]["name"], "Calendar Sync");
    assert_eq!(v["target"], "USER");
    assert_eq!(v["redirectUri"], REDIRECT);
    assert_eq!(v["state"], "st 1");
    assert_eq!(v["scopes"][0]["name"], "meetings:write");
    let sent = f.oauth.validations.lock().unwrap()[0].clone();
    assert_eq!(sent["accountId"], json!(user.id), "the signed-in account");
    assert_eq!(sent["codeChallengeMethod"], "S256");

    // Approve: code + state appended to the registered redirect URI (its query kept).
    let (s, v) = app
        .post(
            "/api/v2/oauth/authorize/approve",
            Auth::Bearer(&user.token),
            decision(client, "APPROVE"),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        v["redirectTo"],
        "https://calendar.example/oauth/callback?tenant=7&code=code-123&state=st+1"
    );
    let authz = f.oauth.authorizations.lock().unwrap()[0].clone();
    assert_eq!(authz["decision"], "APPROVE");
    assert_eq!(authz["accountId"], json!(user.id));
    assert_eq!(authz["target"], "USER");

    // Deny: access_denied, nothing authorized.
    let (s, v) = app
        .post(
            "/api/v2/oauth/authorize/approve",
            Auth::Bearer(&user.token),
            decision(client, "DENY"),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        v["redirectTo"],
        "https://calendar.example/oauth/callback?tenant=7&error=access_denied&state=st+1"
    );
    assert_eq!(f.oauth.authorizations.lock().unwrap().len(), 1);

    // An unregistered redirect URI is never redirected to (not even on denial).
    let mut bad = decision(client, "DENY");
    bad["redirectUri"] = json!("https://evil.example/cb");
    let (s, v) = app
        .post(
            "/api/v2/oauth/authorize/approve",
            Auth::Bearer(&user.token),
            bad,
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(v["error"]["code"], "OAUTH_REQUEST_INVALID");
    assert!(v["error"]["message"]
        .as_str()
        .unwrap()
        .starts_with("INVALID_REDIRECT_URI"));
    assert!(v.get("redirectTo").is_none());
    // Unknown client, plain PKCE.
    let (s, v) = app
        .get(context_uri(Uuid::new_v4(), ""), Auth::Bearer(&user.token))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    assert!(v["error"]["message"]
        .as_str()
        .unwrap()
        .starts_with("INVALID_CLIENT"));
    let plain = context_uri(client, "").replace("method=S256", "method=plain");
    let (s, v) = app.get(plain, Auth::Bearer(&user.token)).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    assert!(v["error"]["message"]
        .as_str()
        .unwrap()
        .starts_with("PKCE_REQUIRED"));
}

#[tokio::test]
async fn organization_installs_need_the_organization_policy() {
    let Some((app, f)) = app_with(true).await else {
        return;
    };
    let member = app.user("orgmember").await;
    let admin = app.user("orgadmin").await;
    let org = Uuid::new_v4();
    f.orgs.admins.lock().unwrap().push((org, admin.id));
    let client = f.oauth.register(REDIRECT);
    let extra = format!("&install=organization&organization_id={org}");

    // The consent page learns the decision up front.
    let (s, v) = app
        .get(context_uri(client, &extra), Auth::Bearer(&member.token))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["target"], "ORGANIZATION");
    assert_eq!(
        v["organizationInstall"],
        json!({ "allowed": false, "reason": "ROLE_REQUIRED" })
    );
    // A member (default policy: ADMINS) is refused and nothing is authorized.
    let mut body = decision(client, "APPROVE");
    body["install"] = json!("organization");
    body["organizationId"] = json!(org);
    let (s, v) = app
        .post(
            "/api/v2/oauth/authorize/approve",
            Auth::Bearer(&member.token),
            body.clone(),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert!(v["error"]["message"]
        .as_str()
        .unwrap()
        .starts_with("ROLE_REQUIRED"));
    assert!(f.oauth.authorizations.lock().unwrap().is_empty());
    // An admin installs.
    let (s, v) = app
        .post(
            "/api/v2/oauth/authorize/approve",
            Auth::Bearer(&admin.token),
            body,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v["redirectTo"].as_str().unwrap().contains("code=code-123"));
    let authz = f.oauth.authorizations.lock().unwrap()[0].clone();
    assert_eq!(authz["target"], "ORGANIZATION");
    assert_eq!(authz["organizationId"], json!(org));
    assert_eq!(authz["accountId"], json!(admin.id));
    assert!(f.orgs.checks.load(Ordering::SeqCst) >= 2);
}

// ---------------------------------------------------------------------------------------
// Connected apps and organization apps
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn connected_apps_list_and_revoke_only_own_grants() {
    let Some((app, f)) = app_with(true).await else {
        return;
    };
    let alice = app.user("alice-apps").await;
    let bob = app.user("bob-apps").await;
    let a_app = Uuid::new_v4();
    let mine = f.oauth.grant(
        json!({ "appId": a_app, "target": "USER", "accountId": alice.id,
        "scopes": ["meetings:read"], "app": { "name": "Cal" } }),
    );
    let bobs = f.oauth.grant(
        json!({ "appId": Uuid::new_v4(), "target": "USER", "accountId": bob.id, "scopes": [] }),
    );
    let (s, v) = app.get("/api/v2/me/apps", Auth::Bearer(&alice.token)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["grantId"], json!(mine));
    assert_eq!(items[0]["name"], "Cal");
    assert_eq!(items[0]["hasMeetingSurface"], false);
    // Bob's grant does not exist for Alice.
    let (s, v) = app
        .delete(
            format!("/api/v2/me/apps/{bobs}"),
            Auth::Bearer(&alice.token),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{v}");
    assert_eq!(v["error"]["code"], "APP_NOT_FOUND");
    assert!(f.oauth.revoked.lock().unwrap().is_empty());
    let (s, _) = app
        .delete(
            format!("/api/v2/me/apps/{mine}"),
            Auth::Bearer(&alice.token),
        )
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(*f.oauth.revoked.lock().unwrap(), vec![mine]);
    let (_, v) = app.get("/api/v2/me/apps", Auth::Bearer(&alice.token)).await;
    assert_eq!(v["items"], json!([]));
    let (s, _) = app.get("/api/v2/me/apps", Auth::None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn organization_apps_are_for_organization_admins() {
    let Some((app, f)) = app_with(true).await else {
        return;
    };
    let admin = app.user("orgapps-admin").await;
    let member = app.user("orgapps-member").await;
    let org = Uuid::new_v4();
    f.orgs.admins.lock().unwrap().push((org, admin.id));
    let g = f
        .oauth
        .grant(json!({ "appId": Uuid::new_v4(), "target": "ORGANIZATION",
        "organizationId": org, "scopes": ["chat:write:bot"] }));
    let uri = format!("/api/v2/organizations/{org}/apps");
    let (s, v) = app.get(&uri, Auth::Bearer(&member.token)).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    let (s, v) = app
        .delete(format!("{uri}/{g}"), Auth::Bearer(&member.token))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    let (s, v) = app.get(&uri, Auth::Bearer(&admin.token)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["items"][0]["grantId"], json!(g));
    assert_eq!(v["items"][0]["target"], "ORGANIZATION");
    // Another organization's grant is unknown here.
    let other = f
        .oauth
        .grant(json!({ "appId": Uuid::new_v4(), "target": "ORGANIZATION",
        "organizationId": Uuid::new_v4() }));
    let (s, _) = app
        .delete(format!("{uri}/{other}"), Auth::Bearer(&admin.token))
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = app
        .delete(format!("{uri}/{g}"), Auth::Bearer(&admin.token))
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(*f.oauth.revoked.lock().unwrap(), vec![g]);
}

// ---------------------------------------------------------------------------------------
// In-meeting apps
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn in_meeting_apps_mint_context_tokens_verifiable_with_the_jwks() {
    let Some((app, f)) = app_with(true).await else {
        return;
    };
    let owner = app.user("host-apps").await;
    let (_, conference) = app
        .live_conference(&owner, json!({ "title": "Apps", "accessPolicy": "PUBLIC" }))
        .await;
    let panel = Uuid::new_v4();
    f.oauth.grant(json!({ "appId": panel, "target": "USER", "accountId": owner.id,
        "scopes": ["meetings:read"], "sub": "owner-pairwise-sub",
        "app": { "name": "Whiteboard", "meetingSurface": { "url": "https://wb.example/panel", "origins": ["https://wb.example"] } } }));
    // A grant without a surface is not an in-meeting app.
    f.oauth
        .grant(json!({ "appId": Uuid::new_v4(), "target": "USER", "accountId": owner.id }));

    // Not a participant yet: the conference does not exist for the caller.
    let uri = format!("/api/v2/conferences/{conference}/apps");
    let (s, v) = app.get(&uri, Auth::Bearer(&owner.token)).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{v}");

    let (s, j) = app
        .join(conference, Auth::Bearer(&owner.token), json!({}))
        .await;
    assert_eq!(s, StatusCode::OK, "{j}");
    let (s, v) = app.get(&uri, Auth::Bearer(&owner.token)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{v}");
    assert_eq!(items[0]["appId"], json!(panel));
    assert_eq!(items[0]["installedFor"], "OWNER");
    assert_eq!(items[0]["surface"]["url"], "https://wb.example/panel");

    let (s, keys) = app.get("/api/v2/app-context-keys", Auth::None).await;
    assert_eq!(s, StatusCode::OK, "{keys}");
    assert_eq!(keys, f.signer.jwks());

    // The owner granted the app: `sub` is present.
    let (s, t) = app
        .post(
            &format!("{uri}/{panel}/context"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{t}");
    assert_eq!(t["expiresIn"], 300);
    let claims = context::verify(t["contextToken"].as_str().unwrap(), &keys).unwrap();
    assert_eq!(claims["aud"], panel.to_string());
    assert_eq!(claims["conference"], conference.to_string());
    assert_eq!(claims["sub"], "owner-pairwise-sub");
    assert_eq!(
        claims["exp"].as_i64().unwrap() - claims["iat"].as_i64().unwrap(),
        300
    );
    let owner_participant = uuid(&j["participant"]["id"]);
    assert_eq!(
        claims["participant"],
        f.signer.pairwise_participant(panel, owner_participant)
    );
    assert_eq!(claims["role"], j["participant"]["role"]);

    // A guest sees the same apps; its token has no `sub` (it never granted the app).
    let (s, g) = app
        .join(conference, Auth::None, json!({ "displayName": "Guest" }))
        .await;
    assert_eq!(s, StatusCode::OK, "{g}");
    let guest = g["participantToken"].as_str().unwrap().to_string();
    let (s, t) = app
        .post(
            &format!("{uri}/{panel}/context"),
            Auth::Participant(&guest),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{t}");
    let claims = context::verify(t["contextToken"].as_str().unwrap(), &keys).unwrap();
    assert!(claims.get("sub").is_none(), "{claims}");
    assert_ne!(claims["participant"], json!(owner_participant.to_string()));
    // An app that is not installed for this meeting.
    let (s, v) = app
        .post(
            &format!("{uri}/{}/context", Uuid::new_v4()),
            Auth::Participant(&guest),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{v}");
    assert_eq!(v["error"]["code"], "APP_NOT_FOUND");
}

// ---------------------------------------------------------------------------------------
// Flags off
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn with_every_flag_off_nothing_changes() {
    let Some((app, f)) = app_with(false).await else {
        return;
    };
    let user = app.user("flagsoff").await;
    let client = f.oauth.register(REDIRECT);
    for (method, uri) in [
        (Method::GET, context_uri(client, "")),
        (Method::GET, "/api/v2/me/apps".to_string()),
        (
            Method::DELETE,
            format!("/api/v2/me/apps/{}", Uuid::new_v4()),
        ),
        (
            Method::GET,
            format!("/api/v2/organizations/{}/apps", Uuid::new_v4()),
        ),
        (
            Method::GET,
            format!("/api/v2/conferences/{}/apps", Uuid::new_v4()),
        ),
        (Method::GET, "/api/v2/app-context-keys".to_string()),
    ] {
        let (s, v) = app
            .call(method.clone(), &uri, Auth::Bearer(&user.token), None)
            .await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{method} {uri}: {v}");
        assert_eq!(v["error"]["code"], "FEATURE_DISABLED", "{method} {uri}");
    }
    let (s, v) = app
        .post(
            "/api/v2/oauth/authorize/approve",
            Auth::Bearer(&user.token),
            decision(client, "APPROVE"),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{v}");
    assert_eq!(
        f.oauth.calls.load(Ordering::SeqCst),
        0,
        "nothing was called"
    );
}

// ---------------------------------------------------------------------------------------
// Delegated actor
// ---------------------------------------------------------------------------------------

struct OauthToken {
    machine: Machine,
    app_id: Uuid,
    grant_id: Uuid,
    principal: Uuid,
}

impl OauthToken {
    fn new(scopes: &[&str]) -> Self {
        let mut machine = Machine::new(scopes);
        machine.actor.environment_kind = "PRODUCTION".into();
        Self {
            machine,
            app_id: Uuid::new_v4(),
            grant_id: Uuid::new_v4(),
            principal: Uuid::new_v4(),
        }
    }

    fn chain(
        &self,
        account: Uuid,
        kind: OauthCredentialKind,
        issued: chrono::DateTime<chrono::Utc>,
    ) -> Authentication {
        Authentication::Authenticated(Box::new(AuthorityChain {
            actor: PlatformActor {
                environment_kind: "PRODUCTION".into(),
                ..self.machine.actor.clone()
            },
            full_authority: true,
            oauth: Some(Ok(OauthCredential {
                kind,
                app_id: self.app_id,
                grant_id: self.grant_id,
                restricted_principal: Some(self.principal),
                principal_reference: Some(format!("cfmeet-user:{account}")),
                issued_at: Some(issued),
            })),
        }))
    }

    fn delegated(&self) -> DelegatedPlatformContext {
        let a = &self.machine.actor;
        DelegatedPlatformContext::new(
            a.tenant_id,
            a.application_id,
            a.environment_id,
            ApplicationEnvironmentKind::Production,
            a.service_account_id,
        )
        .with_initiating_credential(a.credential_id)
        .with_initiating_principal(
            self.principal,
            cf_service_auth::DelegatedPrincipalKind::NativeCfUser,
        )
    }
}

async fn delegated_harness(enabled: bool) -> Option<(Harness, Fakes)> {
    let (mut apps, fakes) = apps(false);
    apps.delegated_enabled = enabled;
    let h = Harness::with(TestOptions {
        apps: Some(apps),
        ..Default::default()
    })
    .await?;
    Some((h, fakes))
}

async fn as_app(
    h: &Harness,
    t: &OauthToken,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let (s, v, _) = h
        .forwarded_by(&h.gateway, method, path, body, Some(t.delegated()), &[])
        .await;
    (s, v)
}

fn register(h: &Harness, t: &OauthToken, account: Uuid, kind: OauthCredentialKind) {
    h.pa.answers.lock().unwrap().insert(
        t.machine.actor.credential_id,
        t.chain(account, kind, chrono::Utc::now()),
    );
}

#[tokio::test]
async fn a_delegated_user_manages_only_its_own_meetings_and_never_joins() {
    let Some((h, _f)) = delegated_harness(true).await else {
        return;
    };
    let alice = h.app.user("deleg-alice").await;
    let bob = h.app.user("deleg-bob").await;
    let t = OauthToken::new(&["meetings.read", "meetings.write"]);
    register(&h, &t, alice.id, OauthCredentialKind::Access);

    // Create an instant meeting AS Alice (she owns it), audited via the app.
    let (s, v) = as_app(
        &h,
        &t,
        Method::POST,
        "/api/v2/instant-meetings",
        Some(json!({ "title": "Booked by app" })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let space = uuid(&v["meetingSpace"]["id"]);
    let conference = uuid(&v["conference"]["id"]);
    assert_eq!(v["meetingSpace"]["ownerUserId"], json!(alice.id));
    assert_eq!(
        h.app
            .scalar(&format!(
                "SELECT count(*) FROM conference_audit_events WHERE meeting_space_id = '{space}' \
                 AND actor_type = 'USER' AND actor_ref = 'user:{}' AND metadata->>'via' = 'app' \
                 AND metadata->>'viaAppId' = '{}' AND metadata->>'viaGrantId' = '{}'",
                alice.id, t.app_id, t.grant_id
            ))
            .await
            >= 1,
        true
    );
    // Read and update her meeting.
    let (s, v) = as_app(
        &h,
        &t,
        Method::GET,
        &format!("/api/v2/meeting-spaces/{space}"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let version = v["version"].as_i64().unwrap();
    let (s, v) = as_app(
        &h,
        &t,
        Method::PATCH,
        &format!("/api/v2/meeting-spaces/{space}"),
        Some(json!({ "version": version, "title": "Renamed by app" })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");

    // Bob's meeting does not exist for the app (read and write); Bob's co-hosted meeting
    // with Alice as co-host is readable but not writable.
    let bobs = uuid(&h.app.create_space(&bob, json!({ "title": "Bob" })).await["id"]);
    let (s, _) = as_app(
        &h,
        &t,
        Method::GET,
        &format!("/api/v2/meeting-spaces/{bobs}"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = as_app(
        &h,
        &t,
        Method::POST,
        &format!("/api/v2/meeting-spaces/{bobs}/archive"),
        Some(json!({})),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    h.app
        .add_member(bobs, &bob, &alice, backend::domain::Role::CoHost)
        .await;
    let (s, v) = as_app(
        &h,
        &t,
        Method::GET,
        &format!("/api/v2/meeting-spaces/{bobs}"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "co-host read: {v}");
    let (s, _) = as_app(
        &h,
        &t,
        Method::POST,
        &format!("/api/v2/meeting-spaces/{bobs}/archive"),
        Some(json!({})),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "co-host never writes");
    // A participant-only membership is not listed / readable.
    let carol = h.app.user("deleg-carol").await;
    let carols = uuid(
        &h.app
            .create_space(&carol, json!({ "title": "Carol" }))
            .await["id"],
    );
    h.app
        .add_member(carols, &carol, &alice, backend::domain::Role::Participant)
        .await;
    let (s, v) = as_app(&h, &t, Method::GET, "/api/v2/meeting-spaces", None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let ids: Vec<Uuid> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| uuid(&i["id"]))
        .collect();
    assert!(ids.contains(&space) && ids.contains(&bobs), "{v}");
    assert!(
        !ids.contains(&carols),
        "participant membership not listed: {v}"
    );
    let (s, _) = as_app(
        &h,
        &t,
        Method::GET,
        &format!("/api/v2/meeting-spaces/{carols}"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Never join, never recordings (not machine operations).
    let (s, _) = as_app(
        &h,
        &t,
        Method::POST,
        &format!("/api/v2/conferences/{conference}/join"),
        Some(json!({})),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = as_app(
        &h,
        &t,
        Method::GET,
        &format!("/api/v2/conferences/{conference}/recordings"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // Never the Apps routes either.
    let (s, _) = as_app(&h, &t, Method::GET, "/api/v2/me/apps", None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // Its own conference lifecycle works.
    let (s, v) = as_app(
        &h,
        &t,
        Method::POST,
        &format!("/api/v2/conferences/{conference}/end"),
        Some(json!({})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
}

#[tokio::test]
async fn delegated_scopes_epoch_bots_and_the_flag_are_enforced() {
    let Some((h, _f)) = delegated_harness(true).await else {
        return;
    };
    let alice = h.app.user("deleg-scope").await;
    // Read-only grant: no writes.
    let ro = OauthToken::new(&["meetings:read"]);
    register(&h, &ro, alice.id, OauthCredentialKind::Access);
    let (s, v) = as_app(&h, &ro, Method::GET, "/api/v2/meeting-spaces", None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = as_app(
        &h,
        &ro,
        Method::POST,
        "/api/v2/instant-meetings",
        Some(json!({ "title": "x" })),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    // Bot tokens never act on meetings.
    let bot = OauthToken::new(&["meetings.read"]);
    register(&h, &bot, alice.id, OauthCredentialKind::Bot);
    let (s, _) = as_app(&h, &bot, Method::GET, "/api/v2/meeting-spaces", None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // The gateway's principal must be the credential's.
    let t = OauthToken::new(&["meetings.read"]);
    register(&h, &t, alice.id, OauthCredentialKind::Access);
    let mut other = OauthToken::new(&["meetings.read"]);
    other.machine = t.machine.clone();
    let (s, _) = as_app(&h, &other, Method::GET, "/api/v2/meeting-spaces", None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "principal mismatch");
    // Sign out everywhere (revocation epoch) after the token was issued: refused.
    h.app
        .scalar(&format!(
            "WITH u AS (INSERT INTO account_security_state (user_id, credentials_not_before) \
             VALUES ('{}', now() + interval '1 second') ON CONFLICT (user_id) DO UPDATE \
             SET credentials_not_before = EXCLUDED.credentials_not_before RETURNING 1) SELECT count(*) FROM u",
            alice.id
        ))
        .await;
    let (s, _) = as_app(&h, &t, Method::GET, "/api/v2/meeting-spaces", None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "revoked by epoch");
    // Unknown / suspended accounts.
    let ghost = OauthToken::new(&["meetings.read"]);
    register(&h, &ghost, Uuid::new_v4(), OauthCredentialKind::Access);
    let (s, _) = as_app(&h, &ghost, Method::GET, "/api/v2/meeting-spaces", None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn oauth_credentials_are_refused_while_delegated_apps_are_off() {
    let Some((h, _f)) = delegated_harness(false).await else {
        return;
    };
    let alice = h.app.user("deleg-off").await;
    let t = OauthToken::new(&["meetings.read", "meetings.write"]);
    register(&h, &t, alice.id, OauthCredentialKind::Access);
    let (s, v) = as_app(&h, &t, Method::GET, "/api/v2/meeting-spaces", None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");
    assert_eq!(v["error"]["code"], "AUTHENTICATION_REQUIRED");
    // Platform credentials are unchanged.
    let m = Machine::new(&["meetings.read", "meetings.write"]);
    h.register(&m);
    let (s, v) = h
        .machine(
            &m,
            Method::POST,
            "/api/v2/instant-meetings",
            Some(json!({ "title": "m" })),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert!(v["meetingSpace"]["ownerUserId"].is_null());
}

// ---------------------------------------------------------------------------------------
// Userinfo
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn userinfo_returns_the_email_only_with_profile_email() {
    let Some((app, _f)) = app_with(true).await else {
        return;
    };
    let user = app.user_with_email("info", "Info.User@Example.com").await;
    let dp = key("developer-platform-userinfo-1");
    let other = key("calling-userinfo-1");
    let bundle = json!({
        "bundle_version": 1,
        "receiver": cf_service_auth::services::MEETING_SERVICE,
        "runtime_environment": ENV,
        "trusted_issuers": [{
            "issuer": cf_service_auth::services::DEVELOPER_PLATFORM,
            "audience": cf_service_auth::services::MEETING_SERVICE,
            "runtime_environment": ENV,
            "keys": [dp.public_jwk()],
            "permissions": ["meeting-oauth-userinfo.read"],
        }, {
            "issuer": cf_service_auth::services::API_GATEWAY,
            "audience": cf_service_auth::services::MEETING_SERVICE,
            "runtime_environment": ENV,
            "keys": [other.public_jwk()],
            "permissions": ["meeting-request.forward"],
        }],
    })
    .to_string();
    let policy = TrustPolicy::from_json(&bundle, &ServiceId::meeting_service(), ENV).unwrap();
    let verifier = Arc::new(cf_service_auth::ServiceTokenVerifier::new(
        policy,
        Arc::new(cf_service_auth::SystemClock),
    ));
    let router = backend::services::apps::userinfo::router(app.state.clone(), verifier);
    let dp = signer(ServiceId::developer_platform(), dp);
    let other = signer(ServiceId::api_gateway(), other);
    let call = |signer: &cf_service_auth::ServiceTokenSigner, query: String| {
        let signed = signer
            .sign(&OutboundServiceRequest::new_operation(
                ServiceId::meeting_service(),
            ))
            .unwrap();
        let mut req = axum::http::Request::builder()
            .method(Method::GET)
            .uri(format!("/internal/v1/oauth/userinfo?{query}"));
        for (k, v) in signed.headers().iter() {
            req = req.header(k, v);
        }
        let req = req.body(axum::body::Body::empty()).unwrap();
        let router = router.clone();
        async move {
            use tower::ServiceExt;
            let resp = router.oneshot(req).await.unwrap();
            let status = resp.status();
            let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap();
            (
                status,
                serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null),
            )
        }
    };
    let (s, v) = call(&dp, format!("accountId={}&scopes=profile%3Aread", user.id)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v["displayName"].is_string());
    assert!(v.get("email").is_none(), "{v}");
    let (s, v) = call(
        &dp,
        format!(
            "accountId={}&scopes=profile%3Aread%20profile%3Aemail",
            user.id
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["email"], "info.user@example.com");
    let (s, _) = call(
        &dp,
        format!("accountId={}&scopes=profile%3Aread", Uuid::new_v4()),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(&dp, format!("accountId={}&scopes=meetings%3Aread", user.id)).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "profile scope required");
    let (s, v) = call(
        &other,
        format!("accountId={}&scopes=profile%3Aread", user.id),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["error"]["code"], "INTERNAL_PERMISSION_DENIED");
}
