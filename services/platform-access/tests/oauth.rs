//! ADR 0064 (CF Meet Apps, "Integrate"): OAuth clients, authorization, grants, the token
//! endpoint backend and app-token authentication, against real PostgreSQL through the real
//! router (contract `docs/platform/integrate-internal-contracts.md` §2).

mod common;

use axum::http::{Method, StatusCode};
use cf_service_auth::DelegatedPlatformContext;
use common::*;
use platform_access::config::platform_build::PlatformBuildConfig;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

/// RFC 7636 appendix B.
const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
const REDIRECT: &str = "https://app.example.com/callback";
const LOCALHOST: &str = "http://localhost:3000/cb";

fn oauth_on() -> PlatformBuildConfig {
    PlatformBuildConfig {
        oauth_enabled: true,
        ..PlatformBuildConfig::default()
    }
}

async fn harness() -> Option<Harness> {
    Harness::with_build(oauth_on()).await
}

async fn dev(h: &Harness, method: Method, path: &str, body: Option<Value>) -> Reply {
    h.call(&h.peers.developer_platform, method, path, body, None, None)
        .await
}

async fn meeting(
    h: &Harness,
    method: Method,
    path: &str,
    body: Option<Value>,
    ctx: Option<DelegatedPlatformContext>,
) -> Reply {
    h.call(&h.peers.meeting, method, path, body, ctx, None)
        .await
}

fn client(status: &str, distribution: &str) -> Value {
    json!({
        "name": "Calendar Sync",
        "redirectUris": [REDIRECT],
        "userScopes": ["meetings:read", "meetings:write", "chat:read", "profile:read"],
        "botScopes": ["chat:read:bot", "chat:write:bot", "events:meetings"],
        "distribution": distribution,
        "status": status,
    })
}

fn with(mut base: Value, extra: Value) -> Value {
    for (k, v) in extra.as_object().unwrap() {
        base[k] = v.clone();
    }
    base
}

/// An EXTERNAL developer tenant's app with `profile` and a client secret.
async fn new_app(h: &Harness, slug: &str, profile: Value) -> (Uuid, String) {
    let c = h.consumer(slug, "EXTERNAL", &[]).await;
    let app = c.application_id;
    let r = dev(
        h,
        Method::PUT,
        &format!("/internal/v1/oauth/clients/{app}"),
        Some(profile),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    assert_eq!(r.body["clientId"], app.to_string());
    let s = dev(
        h,
        Method::POST,
        &format!("/internal/v1/oauth/clients/{app}/secret"),
        None,
    )
    .await;
    assert_eq!(s.status, StatusCode::CREATED, "{}", s.body);
    assert_eq!(s.headers["cache-control"], "no-store");
    let secret = s.body["clientSecret"].as_str().unwrap().to_string();
    assert!(secret.starts_with("cfcs_"), "{secret}");
    (app, secret)
}

/// CF Meet's FIRST_PARTY personal communications scope (what the Meeting Service signs).
async fn personal(h: &Harness) -> Consumer {
    h.consumer("cfmeet-personal", "FIRST_PARTY", &[]).await
}

fn request(app: Uuid, account: Uuid, scope: &str) -> Value {
    json!({
        "clientId": app,
        "redirectUri": REDIRECT,
        "scope": scope,
        "codeChallenge": CHALLENGE,
        "codeChallengeMethod": "S256",
        "target": "USER",
        "accountId": account,
    })
}

fn org_request(app: Uuid, account: Uuid, org: Uuid, scope: &str) -> Value {
    with(
        request(app, account, scope),
        json!({ "target": "ORGANIZATION", "organizationId": org }),
    )
}

async fn validate(h: &Harness, body: Value) -> Reply {
    meeting(
        h,
        Method::POST,
        "/internal/v1/oauth/authorization-requests/validate",
        Some(body),
        None,
    )
    .await
}

async fn authorize(h: &Harness, ctx: Option<DelegatedPlatformContext>, body: Value) -> Reply {
    meeting(
        h,
        Method::POST,
        "/internal/v1/oauth/authorizations",
        Some(with(body, json!({ "decision": "APPROVE" }))),
        ctx,
    )
    .await
}

/// Authorizes for the user in `p`'s personal scope; returns `(code, grantId)`.
async fn user_code(
    h: &Harness,
    p: &Consumer,
    app: Uuid,
    account: Uuid,
    scope: &str,
) -> (String, Uuid) {
    let r = authorize(
        h,
        Some(p.delegated("PRODUCTION")),
        request(app, account, scope),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    assert_eq!(r.body["expiresIn"], 60);
    assert_eq!(r.headers["cache-control"], "no-store");
    (
        r.body["code"].as_str().unwrap().to_string(),
        uuid(&r.body["grantId"]),
    )
}

async fn token(h: &Harness, body: Value) -> Reply {
    dev(h, Method::POST, "/internal/v1/oauth/token", Some(body)).await
}

async fn exchange(h: &Harness, app: Uuid, secret: &str, code: &str) -> Reply {
    token(
        h,
        json!({
            "clientId": app, "clientSecret": secret, "grantType": "authorization_code",
            "code": code, "redirectUri": REDIRECT, "codeVerifier": VERIFIER,
        }),
    )
    .await
}

async fn refresh(h: &Harness, app: Uuid, secret: &str, refresh_token: &str) -> Reply {
    token(
        h,
        json!({
            "clientId": app, "clientSecret": secret, "grantType": "refresh_token",
            "refreshToken": refresh_token,
        }),
    )
    .await
}

fn s(v: &Value, k: &str) -> String {
    v[k].as_str()
        .unwrap_or_else(|| panic!("{k} in {v}"))
        .to_string()
}

async fn introspect(h: &Harness, presented: &str) -> Value {
    let r = dev(
        h,
        Method::POST,
        "/internal/v1/oauth/introspect",
        Some(json!({ "token": presented })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    r.body
}

async fn scalar_text(h: &Harness, sql: String) -> String {
    h.conn()
        .query_one(Statement::from_string(DbBackend::Postgres, sql))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index::<String>(0)
        .unwrap()
}

async fn principal_id(h: &Harness, reference: &str) -> Uuid {
    Uuid::parse_str(
        &scalar_text(
            h,
            format!(
                "SELECT id::text FROM platform_principals WHERE external_reference = '{reference}'"
            ),
        )
        .await,
    )
    .unwrap()
}

async fn grant_status(h: &Harness, grant: Uuid) -> String {
    scalar_text(
        h,
        format!("SELECT status || ':' || coalesce(revoke_reason, '') FROM platform_oauth_grants WHERE id = '{grant}'"),
    )
    .await
}

async fn feed(h: &Harness) -> Vec<Value> {
    let r = h
        .as_calling(
            Method::POST,
            "/internal/v1/identity/changes",
            Some(json!({ "after": 0, "limit": 500 })),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    r.body["changes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["entityKind"] == "APP_GRANT")
        .cloned()
        .collect()
}

#[track_caller]
fn rejected(reply: &Reply, reason: &str) {
    assert_eq!(reply.body["outcome"], "REJECTED", "{}", reply.body);
    assert_eq!(reply.body["reason"], reason, "{}", reply.body);
}

#[tokio::test]
async fn the_full_user_code_flow_validate_authorize_token_authenticate_refresh_revoke() {
    let Some(h) = harness().await else {
        return;
    };
    let p = personal(&h).await;
    let (app, secret) = new_app(&h, "calsync", client("PUBLISHED", "PUBLIC")).await;
    let account = Uuid::new_v4();

    let read_back = dev(
        &h,
        Method::GET,
        &format!("/internal/v1/oauth/clients/{app}"),
        None,
    )
    .await;
    assert_eq!(read_back.status, StatusCode::OK, "{}", read_back.body);
    assert!(read_back.body["secretIssuedAt"].is_string());
    assert_eq!(read_back.body["developerName"], "calsync tenant");

    let ctx = validate(&h, request(app, account, "meetings:read profile:read")).await;
    assert_eq!(ctx.status, StatusCode::OK, "{}", ctx.body);
    assert_eq!(ctx.body["app"]["name"], "Calendar Sync");
    assert_eq!(ctx.body["app"]["developerName"], "calsync tenant");
    assert_eq!(ctx.body["target"], "USER");
    assert_eq!(ctx.body["scopes"].as_array().unwrap().len(), 2);
    assert!(ctx.body["scopes"]
        .as_array()
        .unwrap()
        .iter()
        .all(|s| s["who"] == "user"));

    let (code, grant) = user_code(&h, &p, app, account, "meetings:read profile:read").await;
    assert!(code.starts_with("cfoc_live_"), "{code}");
    let t = exchange(&h, app, &secret, &code).await;
    assert_eq!(t.status, StatusCode::OK, "{}", t.body);
    assert_eq!(t.headers["cache-control"], "no-store");
    assert_eq!(t.body["tokenType"], "Bearer");
    assert_eq!(t.body["expiresIn"], 3600);
    assert_eq!(t.body["scope"], "meetings:read profile:read");
    assert_eq!(t.body["grant"]["id"], grant.to_string());
    assert_eq!(t.body["grant"]["target"], "USER");
    assert!(t.body.get("botToken").is_none());
    let access = s(&t.body, "accessToken");
    let refresh1 = s(&t.body, "refreshToken");
    assert!(access.starts_with("cfoa_live_") && refresh1.starts_with("cfor_live_"));

    // Authentication: the personal FIRST_PARTY triple, the installation service account,
    // mapped scopes, exactly the user principal, and the acting app.
    let user = principal_id(&h, &format!("cfmeet-user:{account}")).await;
    let a = h.authenticate(&access).await;
    assert_eq!(a.body["outcome"], "AUTHENTICATED", "{}", a.body);
    let chain = &a.body["authority"];
    assert_eq!(chain["credentialKind"], "OAUTH_ACCESS");
    assert_eq!(chain["appId"], app.to_string());
    assert_eq!(chain["grantId"], grant.to_string());
    assert_eq!(chain["scopes"], json!(["meetings.read"]));
    assert_eq!(chain["principalRestriction"], json!([user]));
    assert_eq!(chain["tenant"]["id"], p.tenant_id.to_string());
    assert_eq!(chain["tenant"]["classification"], "FIRST_PARTY");
    assert_eq!(chain["environment"]["id"], p.env("PRODUCTION").to_string());
    assert_eq!(
        chain["serviceAccount"]["name"],
        format!("app-{}", app.simple())
    );
    assert_eq!(chain["authorityMode"], "FULL");
    // An API key's chain carries none of the OAuth fields.
    let c = h.consumer("plain", "EXTERNAL", &["calls.read"]).await;
    let (_, key) = h.issue(c.sa("SANDBOX"), &["calls.read"]).await;
    let plain = h.authenticate(&key).await;
    assert_eq!(plain.body["outcome"], "AUTHENTICATED");
    for k in ["credentialKind", "appId", "grantId"] {
        assert!(plain.body["authority"].get(k).is_none(), "{k}");
    }

    // The gateway-forwarded path (delegated authority by credential id) sees the same.
    let sa = uuid(&chain["serviceAccount"]["id"]);
    let fwd = DelegatedPlatformContext::new(
        p.tenant_id,
        p.application_id,
        p.env("PRODUCTION"),
        cf_service_auth::ApplicationEnvironmentKind::Production,
        sa,
    )
    .with_initiating_credential(uuid(&chain["credential"]["id"]));
    let d = h
        .as_calling(
            Method::POST,
            "/internal/v1/credentials/delegated-authority",
            None,
            Some(fwd.clone()),
        )
        .await;
    assert_eq!(d.body["outcome"], "AUTHENTICATED", "{}", d.body);
    assert_eq!(d.body["authority"]["credentialKind"], "OAUTH_ACCESS");

    // Scopes accumulate on the same grant, live for existing tokens.
    let (code2, grant2) = user_code(&h, &p, app, account, "meetings:write").await;
    assert_eq!(grant2, grant);
    let t2 = exchange(&h, app, &secret, &code2).await;
    assert_eq!(
        t2.body["scope"],
        "meetings:read meetings:write profile:read"
    );
    let a = h.authenticate(&access).await;
    assert_eq!(
        a.body["authority"]["scopes"],
        json!(["meetings.read", "meetings.write"])
    );

    // Refresh rotation.
    let r = refresh(&h, app, &secret, &refresh1).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let access2 = s(&r.body, "accessToken");
    let refresh2 = s(&r.body, "refreshToken");
    assert_ne!(refresh2, refresh1);
    assert!(r.body.get("botToken").is_none());
    assert_eq!(
        h.authenticate(&access2).await.body["outcome"],
        "AUTHENTICATED"
    );

    // Listings.
    let mine = meeting(
        &h,
        Method::GET,
        &format!("/internal/v1/oauth/grants?accountId={account}"),
        None,
        None,
    )
    .await;
    assert_eq!(mine.status, StatusCode::OK, "{}", mine.body);
    assert_eq!(mine.body["items"].as_array().unwrap().len(), 1);
    assert_eq!(mine.body["items"][0]["appName"], "Calendar Sync");
    assert_eq!(mine.body["items"][0]["principalId"], user.to_string());
    let by_app = dev(
        &h,
        Method::GET,
        &format!("/internal/v1/oauth/clients/{app}/grants?limit=10"),
        None,
    )
    .await;
    assert_eq!(by_app.status, StatusCode::OK, "{}", by_app.body);
    assert_eq!(by_app.body["items"].as_array().unwrap().len(), 1);
    assert!(by_app.body["nextCursor"].is_null());

    // RFC 7009 revocation of one access token; a foreign client changes nothing.
    let (other_app, other_secret) = new_app(&h, "other", client("PUBLISHED", "PUBLIC")).await;
    for (cid, csecret) in [(other_app, other_secret.as_str()), (app, "cfcs_live_bad")] {
        let r = dev(
            &h,
            Method::POST,
            "/internal/v1/oauth/revoke",
            Some(json!({ "clientId": cid, "clientSecret": csecret, "token": access })),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    }
    assert_eq!(
        h.authenticate(&access).await.body["outcome"],
        "AUTHENTICATED"
    );
    let r = dev(
        &h,
        Method::POST,
        "/internal/v1/oauth/revoke",
        Some(json!({ "clientId": app, "clientSecret": secret, "token": access })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    rejected(&h.authenticate(&access).await, "credential_revoked");

    // Uninstall: every token dies, the refresh token too; the feed records it.
    let del = meeting(
        &h,
        Method::DELETE,
        &format!("/internal/v1/oauth/grants/{grant}?accountId={account}"),
        None,
        None,
    )
    .await;
    assert_eq!(del.status, StatusCode::OK, "{}", del.body);
    assert_eq!(del.body["grant"]["status"], "REVOKED");
    assert_eq!(del.body["grant"]["revokeReason"], "UNINSTALLED");
    rejected(&h.authenticate(&access2).await, "credential_revoked");
    rejected(
        &h.as_calling(
            Method::POST,
            "/internal/v1/credentials/delegated-authority",
            None,
            Some(fwd),
        )
        .await,
        "credential_revoked",
    );
    let r = refresh(&h, app, &secret, &refresh2).await;
    assert_eq!(r.code(), "invalid_grant", "{}", r.body);
    assert_eq!(introspect(&h, &access2).await, json!({ "active": false }));
    let mine = meeting(
        &h,
        Method::GET,
        &format!("/internal/v1/oauth/grants?accountId={account}"),
        None,
        None,
    )
    .await;
    assert!(mine.body["items"].as_array().unwrap().is_empty());
    let changes = feed(&h).await;
    assert!(changes.iter().any(|c| c["operation"] == "INSERT"
        && c["snapshot"]["id"] == grant.to_string()
        && c["snapshot"]["status"] == "ACTIVE"));
    let last = changes
        .iter()
        .rev()
        .find(|c| c["snapshot"]["id"] == grant.to_string())
        .unwrap();
    assert_eq!(last["snapshot"]["status"], "REVOKED");
    assert_eq!(last["snapshot"]["appId"], app.to_string());
    assert_eq!(last["snapshot"]["accountId"], account.to_string());
    assert_eq!(last["snapshot"]["target"], "USER");
    assert_eq!(
        last["snapshot"]["scopes"],
        json!(["meetings:read", "meetings:write", "profile:read"])
    );
}

#[tokio::test]
async fn bad_client_redirect_scope_pkce_and_token_requests_are_refused() {
    let Some(h) = harness().await else {
        return;
    };
    let p = personal(&h).await;
    let (app, secret) = new_app(&h, "refusals", client("PUBLISHED", "PUBLIC")).await;
    let (app2, secret2) = new_app(&h, "second", client("PUBLISHED", "PUBLIC")).await;
    let account = Uuid::new_v4();
    let ok = request(app, account, "meetings:read");
    for (body, code) in [
        (
            with(ok.clone(), json!({ "clientId": Uuid::new_v4() })),
            "INVALID_CLIENT",
        ),
        (
            with(ok.clone(), json!({ "clientId": "not-a-uuid" })),
            "INVALID_CLIENT",
        ),
        (
            with(
                ok.clone(),
                json!({ "redirectUri": "https://app.example.com/other" }),
            ),
            "INVALID_REDIRECT_URI",
        ),
        (
            with(ok.clone(), json!({ "redirectUri": format!("{REDIRECT}/") })),
            "INVALID_REDIRECT_URI",
        ),
        (with(ok.clone(), json!({ "scope": "" })), "INVALID_SCOPE"),
        (
            with(ok.clone(), json!({ "scope": "admin" })),
            "INVALID_SCOPE",
        ),
        // Not requested by the client.
        (
            with(ok.clone(), json!({ "scope": "chat:write" })),
            "INVALID_SCOPE",
        ),
        // A bot scope for a USER grant.
        (
            with(ok.clone(), json!({ "scope": "chat:read:bot" })),
            "INVALID_SCOPE",
        ),
        (
            with(ok.clone(), json!({ "codeChallengeMethod": "plain" })),
            "PKCE_REQUIRED",
        ),
        (
            with(ok.clone(), json!({ "codeChallenge": "short" })),
            "PKCE_REQUIRED",
        ),
        (
            with(
                ok.clone(),
                json!({ "codeChallenge": null, "codeChallengeMethod": null }),
            ),
            "PKCE_REQUIRED",
        ),
    ] {
        let r = validate(&h, body.clone()).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{body}: {}", r.body);
        assert_eq!(r.code(), code, "{body}");
        // /authorizations refuses identically and creates nothing.
        let r = authorize(&h, Some(p.delegated("PRODUCTION")), body.clone()).await;
        assert_eq!(r.code(), code, "{body}");
    }
    let n = scalar_text(
        &h,
        "SELECT count(*)::text FROM platform_oauth_grants".into(),
    )
    .await;
    assert_eq!(n, "0");
    // A denial never reaches Platform Access; ORGANIZATION needs organizationId.
    let r = meeting(
        &h,
        Method::POST,
        "/internal/v1/oauth/authorizations",
        Some(with(ok.clone(), json!({ "decision": "DENY" }))),
        Some(p.delegated("PRODUCTION")),
    )
    .await;
    assert_eq!(r.code(), "VALIDATION_ERROR");
    let r = validate(&h, with(ok.clone(), json!({ "target": "ORGANIZATION" }))).await;
    assert_eq!(r.code(), "VALIDATION_ERROR");

    // PKCE: a wrong verifier is refused (and the code is spent).
    let (code, grant) = user_code(&h, &p, app, account, "meetings:read").await;
    let r = token(
        &h,
        json!({ "clientId": app, "clientSecret": secret, "grantType": "authorization_code",
                "code": code, "redirectUri": REDIRECT, "codeVerifier": VERIFIER.replace('d', "e") }),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.body);
    assert_eq!(r.code(), "invalid_grant");
    // A wrong redirect URI at the token endpoint.
    let (code, _) = user_code(&h, &p, app, account, "meetings:read").await;
    let r = token(
        &h,
        json!({ "clientId": app, "clientSecret": secret, "grantType": "authorization_code",
                "code": code, "redirectUri": "https://app.example.com/other", "codeVerifier": VERIFIER }),
    )
    .await;
    assert_eq!(r.code(), "invalid_grant");
    // Another client's code, presented with that client's own credentials.
    let (code, _) = user_code(&h, &p, app, account, "meetings:read").await;
    let r = exchange(&h, app2, &secret2, &code).await;
    assert_eq!(r.code(), "invalid_grant", "{}", r.body);
    // Client authentication: wrong secret, another client's secret, garbage.
    for (cid, csecret) in [
        (app.to_string(), secret2.clone()),
        (app.to_string(), format!("{}0", &secret[..secret.len() - 1])),
        ("not-a-uuid".to_string(), secret.clone()),
        (app.to_string(), "cfmk_live_x".to_string()),
    ] {
        let r = token(
            &h,
            json!({ "clientId": cid, "clientSecret": csecret, "grantType": "authorization_code",
                    "code": code, "redirectUri": REDIRECT, "codeVerifier": VERIFIER }),
        )
        .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.body);
        assert_eq!(r.code(), "invalid_client");
    }
    let r = token(
        &h,
        json!({ "clientId": app, "clientSecret": secret, "grantType": "password" }),
    )
    .await;
    assert_eq!(r.code(), "unsupported_grant_type");
    let r = token(
        &h,
        json!({ "clientId": app, "grantType": "authorization_code" }),
    )
    .await;
    assert_eq!(r.code(), "invalid_request");
    let r = token(
        &h,
        json!({ "clientId": app, "clientSecret": secret, "grantType": "refresh_token" }),
    )
    .await;
    assert_eq!(r.code(), "invalid_request");
    // The grant is still ACTIVE (none of the above presented a consumed code twice).
    assert_eq!(grant_status(&h, grant).await, "ACTIVE:");

    // Refresh tokens never authenticate; the wrong permission is refused.
    let (code, _) = user_code(&h, &p, app, account, "meetings:read").await;
    let t = exchange(&h, app, &secret, &code).await;
    rejected(
        &h.authenticate(&s(&t.body, "refreshToken")).await,
        "malformed_credential",
    );
    let forged = s(&t.body, "accessToken").replacen("cfoa_", "cfob_", 1);
    assert_eq!(h.authenticate(&forged).await.body["outcome"], "REJECTED");
    let r = h
        .as_calling(
            Method::POST,
            "/internal/v1/oauth/token",
            Some(json!({ "clientId": app })),
            None,
        )
        .await;
    assert_eq!(r.code(), "INTERNAL_PERMISSION_DENIED");
    let r = dev(
        &h,
        Method::POST,
        "/internal/v1/oauth/authorization-requests/validate",
        Some(ok),
    )
    .await;
    assert_eq!(r.code(), "INTERNAL_PERMISSION_DENIED");

    // Rotation: the previous secret keeps working for 24 h, then stops.
    let rotated = dev(
        &h,
        Method::POST,
        &format!("/internal/v1/oauth/clients/{app}/secret"),
        None,
    )
    .await;
    let new_secret = s(&rotated.body, "clientSecret");
    for sec in [&secret, &new_secret] {
        let r = token(
            &h,
            json!({ "clientId": app, "clientSecret": sec, "grantType": "password" }),
        )
        .await;
        assert_eq!(r.code(), "unsupported_grant_type", "authenticates");
    }
    h.clock.advance(chrono::Duration::hours(24));
    let r = token(
        &h,
        json!({ "clientId": app, "clientSecret": secret, "grantType": "password" }),
    )
    .await;
    assert_eq!(r.code(), "invalid_client");
    let r = token(
        &h,
        json!({ "clientId": app, "clientSecret": new_secret, "grantType": "password" }),
    )
    .await;
    assert_eq!(r.code(), "unsupported_grant_type");
}

#[tokio::test]
async fn reusing_a_code_or_a_superseded_refresh_token_revokes_the_grant() {
    let Some(h) = harness().await else {
        return;
    };
    let p = personal(&h).await;
    let (app, secret) = new_app(&h, "reuse", client("PUBLISHED", "PUBLIC")).await;
    let account = Uuid::new_v4();

    let (code, grant) = user_code(&h, &p, app, account, "meetings:read").await;
    let t = exchange(&h, app, &secret, &code).await;
    assert_eq!(t.status, StatusCode::OK, "{}", t.body);
    let access = s(&t.body, "accessToken");
    let again = exchange(&h, app, &secret, &code).await;
    assert_eq!(again.code(), "invalid_grant");
    assert_eq!(grant_status(&h, grant).await, "REVOKED:CODE_REUSE");
    rejected(&h.authenticate(&access).await, "credential_revoked");
    let r = refresh(&h, app, &secret, &s(&t.body, "refreshToken")).await;
    assert_eq!(r.code(), "invalid_grant");

    // A new authorization makes a new grant (REVOKED is terminal).
    let (code, grant2) = user_code(&h, &p, app, account, "meetings:read").await;
    assert_ne!(grant2, grant);
    let t = exchange(&h, app, &secret, &code).await;
    let refresh1 = s(&t.body, "refreshToken");
    let r = refresh(&h, app, &secret, &refresh1).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let access2 = s(&r.body, "accessToken");
    let refresh2 = s(&r.body, "refreshToken");
    // An expired code is refused too.
    let (late, _) = user_code(&h, &p, app, account, "meetings:read").await;
    h.clock.advance(chrono::Duration::seconds(61));
    assert_eq!(
        exchange(&h, app, &secret, &late).await.code(),
        "invalid_grant"
    );
    assert_eq!(grant_status(&h, grant2).await, "ACTIVE:");
    // The access token expires after an hour.
    h.clock.advance(chrono::Duration::seconds(3600));
    rejected(&h.authenticate(&access2).await, "credential_expired");
    // Replaying the superseded refresh token revokes everything.
    let replay = refresh(&h, app, &secret, &refresh1).await;
    assert_eq!(replay.code(), "invalid_grant");
    assert_eq!(grant_status(&h, grant2).await, "REVOKED:REFRESH_REUSE");
    assert_eq!(
        refresh(&h, app, &secret, &refresh2).await.code(),
        "invalid_grant"
    );
}

#[tokio::test]
async fn draft_apps_admit_only_test_accounts_and_private_apps_their_allow_lists() {
    let Some(h) = harness().await else {
        return;
    };
    let p = personal(&h).await;
    let (tester, member, stranger) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let draft = with(
        client("DRAFT", "PUBLIC"),
        json!({ "redirectUris": [REDIRECT, LOCALHOST], "testAccountIds": [tester] }),
    );
    let (app, secret) = new_app(&h, "drafty", draft.clone()).await;
    let path = format!("/internal/v1/oauth/clients/{app}");
    assert_eq!(
        validate(&h, request(app, stranger, "meetings:read"))
            .await
            .code(),
        "APP_NOT_AVAILABLE"
    );
    assert_eq!(
        authorize(
            &h,
            Some(p.delegated("PRODUCTION")),
            request(app, stranger, "meetings:read")
        )
        .await
        .code(),
        "APP_NOT_AVAILABLE"
    );
    let local = with(
        request(app, tester, "meetings:read"),
        json!({ "redirectUri": LOCALHOST }),
    );
    assert_eq!(validate(&h, local.clone()).await.status, StatusCode::OK);
    // The test account completes the flow, localhost included.
    let r = authorize(&h, Some(p.delegated("PRODUCTION")), local).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    let t = token(
        &h,
        json!({ "clientId": app, "clientSecret": secret, "grantType": "authorization_code",
                "code": s(&r.body, "code"), "redirectUri": LOCALHOST, "codeVerifier": VERIFIER }),
    )
    .await;
    assert_eq!(t.status, StatusCode::OK, "{}", t.body);

    // localhost only while DRAFT; at most 25 test accounts; catalogue scopes only.
    let published = with(draft.clone(), json!({ "status": "PUBLISHED" }));
    for bad in [
        published,
        with(
            draft.clone(),
            json!({ "testAccountIds": (0..26).map(|_| Uuid::new_v4()).collect::<Vec<_>>() }),
        ),
        with(
            draft.clone(),
            json!({ "redirectUris": ["https://x.example.com/#f"] }),
        ),
        with(
            draft.clone(),
            json!({ "redirectUris": ["https://*.example.com/cb"] }),
        ),
        with(
            draft.clone(),
            json!({ "redirectUris": (0..11).map(|i| format!("https://x.example.com/{i}")).collect::<Vec<_>>() }),
        ),
        with(draft.clone(), json!({ "userScopes": ["chat:read:bot"] })),
        with(draft.clone(), json!({ "botScopes": ["meetings:read"] })),
        with(draft.clone(), json!({ "userScopes": ["calls.write"] })),
        with(draft.clone(), json!({ "status": "LIVE" })),
    ] {
        let r = dev(&h, Method::PUT, &path, Some(bad.clone())).await;
        assert_eq!(r.code(), "VALIDATION_ERROR", "{bad}: {}", r.body);
    }
    // Once published without localhost, a localhost redirect is refused.
    let r = dev(&h, Method::PUT, &path, Some(client("APPROVED", "PRIVATE"))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let local = with(
        request(app, tester, "meetings:read"),
        json!({ "redirectUri": LOCALHOST }),
    );
    assert_eq!(validate(&h, local).await.code(), "INVALID_REDIRECT_URI");
    // PRIVATE: the synced allow list.
    let private = with(
        client("APPROVED", "PRIVATE"),
        json!({ "allowedAccountIds": [member], "allowedOrganizationIds": [Uuid::new_v4()] }),
    );
    let r = dev(&h, Method::PUT, &path, Some(private)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["allowedAccountIds"], json!([member]));
    assert_eq!(
        validate(&h, request(app, member, "meetings:read"))
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        validate(&h, request(app, stranger, "meetings:read"))
            .await
            .code(),
        "APP_NOT_AVAILABLE"
    );
    // PUBLIC + APPROVED: not yet published, nobody but test accounts.
    let r = dev(&h, Method::PUT, &path, Some(client("APPROVED", "PUBLIC"))).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        validate(&h, request(app, member, "meetings:read"))
            .await
            .code(),
        "APP_NOT_AVAILABLE"
    );

    // Only EXTERNAL applications are apps.
    let fp = h.consumer("firstparty", "FIRST_PARTY", &[]).await;
    let r = dev(
        &h,
        Method::PUT,
        &format!("/internal/v1/oauth/clients/{}", fp.application_id),
        Some(client("DRAFT", "PUBLIC")),
    )
    .await;
    assert_eq!(r.code(), "POLICY_VIOLATION");
    let r = dev(
        &h,
        Method::PUT,
        &format!("/internal/v1/oauth/clients/{}", Uuid::new_v4()),
        Some(client("DRAFT", "PUBLIC")),
    )
    .await;
    assert_eq!(r.code(), "NOT_FOUND");
    let r = dev(
        &h,
        Method::POST,
        &format!("/internal/v1/oauth/clients/{}/secret", Uuid::new_v4()),
        None,
    )
    .await;
    assert_eq!(r.code(), "NOT_FOUND");
}

#[tokio::test]
async fn the_user_target_comes_from_the_signed_first_party_scope_or_the_configured_fallback() {
    let Some(h) = harness().await else {
        return;
    };
    let p = personal(&h).await;
    let (app, _) = new_app(&h, "target", client("PUBLISHED", "PUBLIC")).await;
    let body = request(app, Uuid::new_v4(), "meetings:read");
    // No context, no fallback.
    let r = authorize(&h, None, body.clone()).await;
    assert_eq!(r.code(), "INVALID_STATE", "{}", r.body);
    // An EXTERNAL scope is never a personal target.
    let ext = h.consumer("ext", "EXTERNAL", &[]).await;
    let r = authorize(&h, Some(ext.delegated("PRODUCTION")), body.clone()).await;
    assert_eq!(r.code(), "POLICY_VIOLATION", "{}", r.body);
    // A forged context is refused.
    let forged = DelegatedPlatformContext::new(
        p.tenant_id,
        ext.application_id,
        p.env("PRODUCTION"),
        cf_service_auth::ApplicationEnvironmentKind::Production,
        p.sa("PRODUCTION"),
    );
    let r = authorize(&h, Some(forged), body.clone()).await;
    assert_eq!(r.code(), "DELEGATED_CONTEXT_INVALID", "{}", r.body);
    // The configured fallback environment.
    let fallback = Harness::on_build(
        h.db.clone(),
        PlatformBuildConfig {
            oauth_personal_environment_id: Some(p.env("PRODUCTION")),
            ..oauth_on()
        },
    );
    let r = authorize(&fallback, None, body).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
}

#[tokio::test]
async fn suspending_an_app_revokes_every_grant_and_token() {
    let Some(h) = harness().await else {
        return;
    };
    let p = personal(&h).await;
    let (app, secret) = new_app(&h, "suspend", client("PUBLISHED", "PUBLIC")).await;
    let mut tokens = Vec::new();
    for _ in 0..2 {
        let account = Uuid::new_v4();
        let (code, _) = user_code(&h, &p, app, account, "meetings:read").await;
        let t = exchange(&h, app, &secret, &code).await;
        assert_eq!(t.status, StatusCode::OK, "{}", t.body);
        tokens.push((s(&t.body, "accessToken"), s(&t.body, "refreshToken")));
    }
    let r = dev(
        &h,
        Method::PUT,
        &format!("/internal/v1/oauth/clients/{app}"),
        Some(client("SUSPENDED", "PUBLIC")),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["revokedGrants"], 2);
    for (access, refresh_token) in &tokens {
        rejected(&h.authenticate(access).await, "credential_revoked");
        assert_eq!(
            refresh(&h, app, &secret, refresh_token).await.code(),
            "invalid_client"
        );
    }
    assert_eq!(
        validate(&h, request(app, Uuid::new_v4(), "meetings:read"))
            .await
            .code(),
        "APP_NOT_AVAILABLE"
    );
    let revoked = feed(&h)
        .await
        .into_iter()
        .filter(|c| {
            c["snapshot"]["status"] == "REVOKED" && c["snapshot"]["revokeReason"] == "APP_SUSPENDED"
        })
        .count();
    assert_eq!(revoked, 2);
    // Resuming does not resurrect grants.
    let r = dev(
        &h,
        Method::PUT,
        &format!("/internal/v1/oauth/clients/{app}"),
        Some(client("PUBLISHED", "PUBLIC")),
    )
    .await;
    assert_eq!(r.body["revokedGrants"], 0);
    rejected(&h.authenticate(&tokens[0].0).await, "credential_revoked");
}

#[tokio::test]
async fn an_organization_install_creates_the_bot_the_installation_account_and_a_bot_token() {
    let Some(h) = harness().await else {
        return;
    };
    let org = Uuid::new_v4();
    let provisioned = h
        .call(
            &h.peers.organizations,
            Method::PUT,
            &format!("/internal/v1/organization-scopes/{org}/primary"),
            None,
            None,
            None,
        )
        .await;
    assert_eq!(
        provisioned.status,
        StatusCode::CREATED,
        "{}",
        provisioned.body
    );
    let org_env = uuid(&provisioned.body["environmentId"]);
    let org_tenant = uuid(&provisioned.body["tenantId"]);
    let (app, secret) = new_app(&h, "botapp", client("PUBLISHED", "PUBLIC")).await;
    let admin = Uuid::new_v4();
    let scope = "chat:read:bot chat:write:bot events:meetings";

    let ctx = validate(&h, org_request(app, admin, org, scope)).await;
    assert_eq!(ctx.status, StatusCode::OK, "{}", ctx.body);
    assert_eq!(ctx.body["target"], "ORGANIZATION");
    assert!(ctx.body["scopes"]
        .as_array()
        .unwrap()
        .iter()
        .all(|s| s["who"] == "bot"));
    assert_eq!(
        validate(&h, org_request(app, admin, org, "meetings:read"))
            .await
            .code(),
        "INVALID_SCOPE"
    );
    let unprovisioned = validate(&h, org_request(app, admin, Uuid::new_v4(), scope)).await;
    assert_eq!(unprovisioned.code(), "INVALID_STATE");

    let r = authorize(&h, None, org_request(app, admin, org, scope)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    let grant = uuid(&r.body["grantId"]);
    let t = exchange(&h, app, &secret, &s(&r.body, "code")).await;
    assert_eq!(t.status, StatusCode::OK, "{}", t.body);
    assert_eq!(t.body["grant"]["organizationId"], org.to_string());
    let bot = s(&t.body, "botToken");
    assert!(bot.starts_with("cfob_live_"), "{bot}");
    let access = s(&t.body, "accessToken");

    let bot_principal = principal_id(&h, &format!("app:{app}")).await;
    let kind = scalar_text(
        &h,
        format!("SELECT kind || ':' || environment_id::text FROM platform_principals WHERE id = '{bot_principal}'"),
    )
    .await;
    assert_eq!(kind, format!("BOT_OR_SERVICE:{org_env}"));
    for (presented, kind) in [(&bot, "OAUTH_BOT"), (&access, "OAUTH_ACCESS")] {
        let a = h.authenticate(presented).await;
        assert_eq!(a.body["outcome"], "AUTHENTICATED", "{}", a.body);
        let chain = &a.body["authority"];
        assert_eq!(chain["credentialKind"], kind);
        assert_eq!(chain["tenant"]["id"], org_tenant.to_string());
        assert_eq!(chain["environment"]["id"], org_env.to_string());
        assert_eq!(
            chain["serviceAccount"]["name"],
            format!("app-{}", app.simple())
        );
        assert_eq!(
            chain["serviceAccount"]["environmentId"],
            org_env.to_string()
        );
        assert_eq!(chain["scopes"], json!(["messages.read", "messages.send"]));
        assert_eq!(chain["principalRestriction"], json!([bot_principal]));
        assert_eq!(
            chain["restrictedPrincipal"],
            json!({ "id": bot_principal, "kind": "BOT_OR_SERVICE", "reference": format!("app:{app}") })
        );
        assert_eq!(chain["grantId"], grant.to_string());
    }
    // The bot token is issued once per grant.
    let r = authorize(&h, None, org_request(app, admin, org, "chat:read:bot")).await;
    assert_eq!(uuid(&r.body["grantId"]), grant);
    let t2 = exchange(&h, app, &secret, &s(&r.body, "code")).await;
    assert!(t2.body.get("botToken").is_none(), "{}", t2.body);
    let listed = meeting(
        &h,
        Method::GET,
        &format!("/internal/v1/oauth/grants?organizationId={org}"),
        None,
        None,
    )
    .await;
    assert_eq!(listed.body["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        listed.body["items"][0]["installedByAccountId"],
        admin.to_string()
    );
    assert!(listed.body["items"][0]["accountId"].is_null());

    // Uninstall, scoped to the owner: another owner sees NOT_FOUND.
    let wrong = meeting(
        &h,
        Method::DELETE,
        &format!("/internal/v1/oauth/grants/{grant}?accountId={admin}"),
        None,
        None,
    )
    .await;
    assert_eq!(wrong.code(), "NOT_FOUND");
    let del = meeting(
        &h,
        Method::DELETE,
        &format!("/internal/v1/oauth/grants/{grant}?organizationId={org}"),
        None,
        None,
    )
    .await;
    assert_eq!(del.status, StatusCode::OK, "{}", del.body);
    rejected(&h.authenticate(&bot).await, "credential_revoked");
    let status = scalar_text(
        &h,
        format!("SELECT status FROM platform_principals WHERE id = '{bot_principal}'"),
    )
    .await;
    assert_eq!(status, "DEACTIVATED");
    // Reinstall: the bot comes back, with a new grant and a new bot token.
    let r = authorize(&h, None, org_request(app, admin, org, scope)).await;
    assert_ne!(uuid(&r.body["grantId"]), grant);
    let t3 = exchange(&h, app, &secret, &s(&r.body, "code")).await;
    let bot2 = s(&t3.body, "botToken");
    assert_eq!(h.authenticate(&bot2).await.body["outcome"], "AUTHENTICATED");
    assert_eq!(principal_id(&h, &format!("app:{app}")).await, bot_principal);
}

#[tokio::test]
async fn the_principal_restriction_is_enforced_on_authenticate() {
    let Some(h) = harness().await else {
        return;
    };
    let p = personal(&h).await;
    let (app, secret) = new_app(&h, "restrict", client("PUBLISHED", "PUBLIC")).await;
    let account = Uuid::new_v4();
    let (code, _) = user_code(&h, &p, app, account, "chat:read").await;
    let t = exchange(&h, app, &secret, &code).await;
    let access = s(&t.body, "accessToken");
    let a = h.authenticate(&access).await;
    let user = principal_id(&h, &format!("cfmeet-user:{account}")).await;
    assert_eq!(a.body["authority"]["principalRestriction"], json!([user]));
    assert_eq!(a.body["authority"]["scopes"], json!(["messages.read"]));
    let credential = uuid(&a.body["authority"]["credential"]["id"]);

    // Widening the restriction behind the service's back is a corrupt chain.
    let (other, _) = h
        .state
        .platform
        .register_principal(
            platform_access::domain::EnvironmentId::from_uuid(p.env("PRODUCTION")),
            platform_access::identity::principals::NewPrincipal::External {
                kind: platform_access::domain::PrincipalKind::NativeCfUser,
                external_reference: format!("cfmeet-user:{}", Uuid::new_v4()),
            },
        )
        .await
        .unwrap();
    let insert = format!(
        "INSERT INTO platform_credential_principal_restrictions \
           (credential_id, principal_id, tenant_id, application_id, environment_id) \
         VALUES ('{credential}', '{}', '{}', '{}', '{}')",
        other.id,
        p.tenant_id,
        p.application_id,
        p.env("PRODUCTION")
    );
    h.conn().execute_unprepared(&insert).await.unwrap();
    rejected(&h.authenticate(&access).await, "corrupt_authority_chain");
    h.conn()
        .execute_unprepared(&format!(
            "DELETE FROM platform_credential_principal_restrictions \
             WHERE credential_id = '{credential}' AND principal_id = '{}'",
            other.id
        ))
        .await
        .unwrap();
    assert_eq!(
        h.authenticate(&access).await.body["outcome"],
        "AUTHENTICATED"
    );

    // The account authority suspends / deactivates the account: the token follows.
    for (status, reason) in [
        ("SUSPENDED", "credential_suspended"),
        ("DEACTIVATED", "credential_revoked"),
    ] {
        let r = meeting(
            &h,
            Method::POST,
            "/internal/v1/principals/account-status",
            Some(
                json!({ "externalReference": format!("cfmeet-user:{account}"), "status": status }),
            ),
            Some(p.delegated("PRODUCTION")),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
        rejected(&h.authenticate(&access).await, reason);
    }
    // ... and the account cannot authorize again while inactive.
    let r = authorize(
        &h,
        Some(p.delegated("PRODUCTION")),
        request(app, account, "chat:read"),
    )
    .await;
    assert_eq!(r.code(), "INVALID_STATE", "{}", r.body);
}

#[tokio::test]
async fn introspection_returns_a_pairwise_sub_stable_per_app_and_different_across_apps() {
    let Some(h) = harness().await else {
        return;
    };
    let p = personal(&h).await;
    let (a, secret_a) = new_app(&h, "appa", client("PUBLISHED", "PUBLIC")).await;
    let (b, secret_b) = new_app(&h, "appb", client("PUBLISHED", "PUBLIC")).await;
    let account = Uuid::new_v4();
    let mut subs = Vec::new();
    for (app, secret) in [(a, &secret_a), (b, &secret_b)] {
        let (code, grant) = user_code(&h, &p, app, account, "profile:read meetings:read").await;
        let t = exchange(&h, app, secret, &code).await;
        let first = introspect(&h, &s(&t.body, "accessToken")).await;
        assert_eq!(first["active"], true, "{first}");
        assert_eq!(first["appId"], app.to_string());
        assert_eq!(first["grantId"], grant.to_string());
        assert_eq!(first["accountId"], account.to_string());
        assert_eq!(first["credentialKind"], "OAUTH_ACCESS");
        assert_eq!(first["scopes"], json!(["meetings:read", "profile:read"]));
        assert!(first.get("organizationId").is_none());
        let sub = s(&first, "sub");
        assert_eq!(sub.len(), 43);
        assert!(!sub.contains(&account.to_string()));
        // Stable across tokens of the same app.
        let r = refresh(&h, app, secret, &s(&t.body, "refreshToken")).await;
        let second = introspect(&h, &s(&r.body, "accessToken")).await;
        assert_eq!(second["sub"], first["sub"]);
        // Refresh tokens and garbage are never active.
        assert_eq!(
            introspect(&h, &s(&r.body, "refreshToken")).await,
            json!({ "active": false })
        );
        subs.push(sub);
    }
    assert_ne!(subs[0], subs[1]);
    assert_eq!(introspect(&h, "garbage").await, json!({ "active": false }));
    // Another deployment over the same pepper derives the same key: same sub.
    let again = Harness::on_build(h.db.clone(), oauth_on());
    let (code, _) = user_code(&again, &p, a, account, "profile:read").await;
    let t = exchange(&again, a, &secret_a, &code).await;
    assert_eq!(
        introspect(&again, &s(&t.body, "accessToken")).await["sub"],
        json!(subs[0])
    );
}

#[tokio::test]
async fn with_the_flag_off_every_route_is_unknown_and_app_tokens_never_authenticate() {
    let Some(h) = harness().await else {
        return;
    };
    let p = personal(&h).await;
    let (app, secret) = new_app(&h, "flagged", client("PUBLISHED", "PUBLIC")).await;
    let account = Uuid::new_v4();
    let (code, grant) = user_code(&h, &p, app, account, "meetings:read").await;
    let t = exchange(&h, app, &secret, &code).await;
    let access = s(&t.body, "accessToken");
    let a = h.authenticate(&access).await;
    assert_eq!(a.body["outcome"], "AUTHENTICATED");
    let sa = uuid(&a.body["authority"]["serviceAccount"]["id"]);
    let credential = uuid(&a.body["authority"]["credential"]["id"]);

    let off = Harness::on_build(h.db.clone(), PlatformBuildConfig::default());
    let id = Uuid::new_v4();
    let routes: Vec<(Method, String)> = vec![
        (Method::PUT, format!("/internal/v1/oauth/clients/{app}")),
        (Method::GET, format!("/internal/v1/oauth/clients/{app}")),
        (
            Method::POST,
            format!("/internal/v1/oauth/clients/{app}/secret"),
        ),
        (
            Method::GET,
            format!("/internal/v1/oauth/clients/{app}/grants"),
        ),
        (
            Method::POST,
            "/internal/v1/oauth/authorization-requests/validate".to_string(),
        ),
        (
            Method::POST,
            "/internal/v1/oauth/authorizations".to_string(),
        ),
        (
            Method::GET,
            format!("/internal/v1/oauth/grants?accountId={account}"),
        ),
        (Method::DELETE, format!("/internal/v1/oauth/grants/{grant}")),
        (Method::DELETE, format!("/internal/v1/oauth/grants/{id}")),
        (Method::POST, "/internal/v1/oauth/token".to_string()),
        (Method::POST, "/internal/v1/oauth/revoke".to_string()),
        (Method::POST, "/internal/v1/oauth/introspect".to_string()),
    ];
    for (method, path) in routes {
        for (signer, body) in [
            (
                &off.peers.developer_platform,
                Some(client("PUBLISHED", "PUBLIC")),
            ),
            (&off.peers.meeting, Some(json!({ "garbage": true }))),
            (&off.peers.developer_platform, None),
        ] {
            let body = if method == Method::GET || method == Method::DELETE {
                None
            } else {
                body
            };
            let r = off
                .call(signer, method.clone(), &path, body, None, None)
                .await;
            assert_eq!(
                r.status,
                StatusCode::NOT_FOUND,
                "{method} {path}: {}",
                r.body
            );
            assert_eq!(r.code(), "NOT_FOUND");
        }
    }
    // App tokens are unknown values, by value and by id.
    rejected(&off.authenticate(&access).await, "malformed_credential");
    let fwd = DelegatedPlatformContext::new(
        p.tenant_id,
        p.application_id,
        p.env("PRODUCTION"),
        cf_service_auth::ApplicationEnvironmentKind::Production,
        sa,
    )
    .with_initiating_credential(credential);
    rejected(
        &off.as_calling(
            Method::POST,
            "/internal/v1/credentials/delegated-authority",
            None,
            Some(fwd),
        )
        .await,
        "unknown_credential",
    );
    // API keys are unaffected.
    let c = off.consumer("still", "EXTERNAL", &["calls.read"]).await;
    let (_, key) = off.issue(c.sa("SANDBOX"), &["calls.read"]).await;
    assert_eq!(
        off.authenticate(&key).await.body["outcome"],
        "AUTHENTICATED"
    );
    // Nothing was revoked by the flag: on again, the token works.
    assert_eq!(
        h.authenticate(&access).await.body["outcome"],
        "AUTHENTICATED"
    );
}

#[tokio::test]
async fn presentation_grant_listings_and_the_restricted_principal() {
    let Some(h) = harness().await else {
        return;
    };
    let p = personal(&h).await;
    let presented = with(
        client("PUBLISHED", "PUBLIC"),
        json!({
            "developerName": "Acme Apps",
            "logoUrl": "https://cdn.example.com/logo.png",
            "meetingSurface": {
                "url": "https://meet.example.com/panel",
                "origins": ["https://meet.example.com", "https://meet.example.com", "https://cdn.example.com:8443"],
            },
        }),
    );
    let (app, secret) = new_app(&h, "presented", presented.clone()).await;
    let path = format!("/internal/v1/oauth/clients/{app}");
    let profile = dev(&h, Method::GET, &path, None).await.body;
    assert_eq!(profile["developerName"], "Acme Apps");
    assert_eq!(profile["logoUrl"], "https://cdn.example.com/logo.png");
    let surface = json!({
        "url": "https://meet.example.com/panel",
        "origins": ["https://meet.example.com", "https://cdn.example.com:8443"],
    });
    assert_eq!(profile["meetingSurface"], surface);
    for bad in [
        json!({ "logoUrl": "http://cdn.example.com/logo.png" }),
        json!({ "logoUrl": format!("https://cdn.example.com/{}", "x".repeat(512)) }),
        json!({ "developerName": "" }),
        json!({ "meetingSurface": { "url": "http://meet.example.com", "origins": [] } }),
        json!({ "meetingSurface": { "url": "https://meet.example.com", "origins": ["https://meet.example.com/path"] } }),
        json!({ "meetingSurface": { "url": "https://meet.example.com", "origins": ["http://localhost:3000"] } }),
        json!({ "meetingSurface": { "url": "https://meet.example.com",
            "origins": (0..11).map(|i| format!("https://o{i}.example.com")).collect::<Vec<_>>() } }),
    ] {
        let r = dev(
            &h,
            Method::PUT,
            &path,
            Some(with(presented.clone(), bad.clone())),
        )
        .await;
        assert_eq!(r.code(), "VALIDATION_ERROR", "{bad}: {}", r.body);
    }
    // The consent screen shows the override.
    let account = Uuid::new_v4();
    let ctx = validate(&h, request(app, account, "meetings:read")).await;
    assert_eq!(ctx.body["app"]["developerName"], "Acme Apps");

    let (code, grant) = user_code(&h, &p, app, account, "meetings:read").await;
    let t = exchange(&h, app, &secret, &code).await;
    let access = s(&t.body, "accessToken");
    let sub = s(&introspect(&h, &access).await, "sub");

    // Grant listings: the app as shown, and the pairwise sub of the grant.
    let mine = meeting(
        &h,
        Method::GET,
        &format!("/internal/v1/oauth/grants?accountId={account}"),
        None,
        None,
    )
    .await;
    let item = &mine.body["items"][0];
    assert_eq!(item["id"], grant.to_string());
    assert_eq!(item["appId"], app.to_string());
    assert_eq!(item["target"], "USER");
    assert_eq!(item["accountId"], account.to_string());
    assert_eq!(item["scopes"], json!(["meetings:read"]));
    assert_eq!(item["status"], "ACTIVE");
    assert!(item["createdAt"].is_string());
    assert_eq!(
        item["app"],
        json!({ "name": "Calendar Sync", "developerName": "Acme Apps",
                "logoUrl": "https://cdn.example.com/logo.png", "meetingSurface": surface })
    );
    assert_eq!(item["sub"], json!(sub));
    let by_app = dev(
        &h,
        Method::GET,
        &format!("/internal/v1/oauth/clients/{app}/grants"),
        None,
    )
    .await;
    assert_eq!(by_app.body["items"][0]["sub"], json!(sub));
    assert_eq!(by_app.body["items"][0]["app"]["developerName"], "Acme Apps");

    // Authentication and the credential lookup name the restricted principal and issue time.
    let user = principal_id(&h, &format!("cfmeet-user:{account}")).await;
    let a = h.authenticate(&access).await;
    let chain = &a.body["authority"];
    let restricted = json!({ "id": user, "kind": "NATIVE_CF_USER", "reference": format!("cfmeet-user:{account}") });
    assert_eq!(chain["restrictedPrincipal"], restricted);
    assert_eq!(
        chain["credentialIssuedAt"],
        chain["credential"]["createdAt"]
    );
    let lookup_ctx = DelegatedPlatformContext::new(
        p.tenant_id,
        p.application_id,
        p.env("PRODUCTION"),
        cf_service_auth::ApplicationEnvironmentKind::Production,
        uuid(&chain["serviceAccount"]["id"]),
    );
    let lookup = h
        .as_calling(
            Method::GET,
            &format!("/internal/v1/credentials/{}", s(&chain["credential"], "id")),
            None,
            Some(lookup_ctx),
        )
        .await;
    assert_eq!(lookup.status, StatusCode::OK, "{}", lookup.body);
    assert_eq!(lookup.body["restrictedPrincipal"], restricted);
    assert_eq!(lookup.body["credentialKind"], "OAUTH_ACCESS");
    assert_eq!(
        lookup.body["credentialIssuedAt"],
        chain["credentialIssuedAt"]
    );
    // API keys carry neither.
    let c = h.consumer("keyonly", "EXTERNAL", &["calls.read"]).await;
    let (_, key) = h.issue(c.sa("SANDBOX"), &["calls.read"]).await;
    let plain = h.authenticate(&key).await;
    assert!(plain.body["authority"].get("restrictedPrincipal").is_none());
    assert!(plain.body["authority"].get("credentialIssuedAt").is_none());

    // Uninstall returns the grant with its sub; a full replacement clears the presentation.
    let del = meeting(
        &h,
        Method::DELETE,
        &format!("/internal/v1/oauth/grants/{grant}"),
        None,
        None,
    )
    .await;
    assert_eq!(del.body["grant"]["sub"], json!(sub));
    let r = dev(&h, Method::PUT, &path, Some(client("PUBLISHED", "PUBLIC"))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert!(r.body["logoUrl"].is_null() && r.body["meetingSurface"].is_null());
    assert_eq!(r.body["developerName"], "presented tenant");
}
