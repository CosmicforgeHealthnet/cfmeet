//! ADR 0064 / Integrate contract §6 at the gateway, against a SYNTHETIC route table (the
//! generated manifest carries no `OAUTH_ACCESS_TOKEN` or `forwardAuthorization` route
//! yet) and a scripted Platform Access:
//!
//! * `OAUTH_ACCESS_TOKEN` (`API_GATEWAY_OAUTH_ENABLED`): `Bearer cfoa_…` / `cfob_…` are
//!   authenticated through Platform Access and forwarded as the signed delegated context
//!   with the token's credential id and its single restricted principal, never the token;
//!   a restriction that is not exactly one principal, or a credential kind that does not
//!   match the prefix, is 401; `cfor_` is always 401; an OAuth token on a route that does
//!   not list the mode is `403 CREDENTIAL_NOT_ACCEPTED` (never transported);
//! * the `/v1/meetings` alias family accepts OAuth tokens;
//! * anonymous Developer Console routes forward `application/x-www-form-urlencoded`
//!   bodies verbatim (transported, no gateway token, no delegated context);
//! * `forwardAuthorization` transports `Authorization` (OAuth bearer, Basic client
//!   credentials) only on the marked route;
//! * flag off: everything as before.

mod common;

use api_gateway::{
    identity::{AuthenticatedCustomer, CredentialKind, OauthCredential, TrustClassification},
    routing::{AuthMode, RouteDefinition},
};
use axum::http::{Method, StatusCode};
use cf_service_auth::{services, ApplicationEnvironmentKind, DelegatedPrincipalKind};
use common::*;
use uuid::Uuid;

const CRED: &str = "cfmk_sbx_aaaaaaaaaaaaaaaaaaaaaaaa_bbbb";
const ACCESS: &str = "cfoa_live_0123456789abcdef01234567_abcd";
const BOT: &str = "cfob_live_0123456789abcdef01234567_abcd";
const REFRESH: &str = "cfor_live_0123456789abcdef01234567_abcd";
const ALIAS: &str = "/v1/meetings/test-spaces/s-1";
const FORM: &str = "grant_type=authorization_code&code=c-1&redirect_uri=https%3A%2F%2Fapp.example%2Fcb&code_verifier=v";

/// The test table plus a `/v1/meetings` alias accepting OAuth and the anonymous
/// Developer Console OAuth / directory routes.
fn routes() -> Vec<RouteDefinition> {
    use AuthMode::*;
    let dp = services::DEVELOPER_PLATFORM;
    let mut alias = route(
        "meetings.platform.getTestSpace",
        "meetings",
        Method::GET,
        ALIAS.replace("s-1", "{id}").as_str(),
        services::MEETING_SERVICE,
        &[PlatformCredential, OauthAccessToken],
        Some("meetings.read"),
    );
    alias.upstream_path = "/api/v2/test-spaces/{id}".into();
    let mut r = test_routes();
    r.push(alias);
    for (id, method, pattern, forward) in [
        (
            "developer-platform.oauthToken",
            Method::POST,
            "/v1/oauth/token",
            true,
        ),
        (
            "developer-platform.oauthUserinfo",
            Method::GET,
            "/v1/oauth/userinfo",
            true,
        ),
        (
            "developer-platform.appDirectory",
            Method::GET,
            "/v1/app-directory",
            false,
        ),
        (
            "developer-platform.oauthMetadata",
            Method::GET,
            "/v1/oauth/metadata",
            false,
        ),
    ] {
        let mut d = route(
            id,
            "developer-platform",
            method,
            pattern,
            dp,
            &[Anonymous],
            None,
        );
        d.forward_authorization = forward;
        r.push(d);
    }
    r
}

async fn gateway(oauth: bool) -> Gateway {
    Gateway::with(Options {
        routes: Some(routes()),
        oauth,
        ..Options::default()
    })
    .await
}

/// An OAuth credential as Platform Access authenticates it: the install target's
/// FIRST_PARTY chain, the grant's internal scopes and the principal restriction.
fn oauth_customer(kind: CredentialKind, restriction: Vec<Uuid>) -> AuthenticatedCustomer {
    AuthenticatedCustomer {
        environment_kind: ApplicationEnvironmentKind::Production,
        classification: TrustClassification::FirstParty,
        principal_restriction: Some(restriction),
        oauth: Some(OauthCredential {
            kind,
            app_id: Uuid::new_v4(),
            grant_id: Uuid::new_v4(),
        }),
        ..customer(&["meetings.read"])
    }
}

fn calls(g: &Gateway) -> usize {
    *g.authority.calls.lock().unwrap()
}

#[tokio::test]
async fn an_oauth_token_is_authenticated_and_forwarded_with_its_restricted_principal() {
    let g = gateway(true).await;
    for (token, kind, principal_kind) in [
        (
            ACCESS,
            CredentialKind::OauthAccess,
            DelegatedPrincipalKind::NativeCfUser,
        ),
        (
            BOT,
            CredentialKind::OauthBot,
            DelegatedPrincipalKind::BotOrService,
        ),
    ] {
        let principal = Uuid::new_v4();
        let c = oauth_customer(kind, vec![principal]);
        g.authority.add(token, c.clone());
        let r = g.get(ALIAS, Some(token)).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.json);
        let last = g.meeting().last();
        assert_eq!(last.path, "/api/v2/test-spaces/s-1");
        assert_eq!(last.issuer.as_deref(), Some(services::API_GATEWAY));
        assert!(
            last.headers.get("authorization").is_none(),
            "the token itself is never forwarded"
        );
        let d = last.delegated.expect("a signed delegated context");
        assert_eq!(d.initiating_credential_id(), Some(c.credential_id));
        assert_eq!(d.application_environment_id(), c.environment_id);
        assert_eq!(d.initiating_service_account_id(), c.service_account_id);
        let p = d.initiating_principal().expect("the restricted principal");
        assert_eq!(
            (p.principal_id, p.principal_kind),
            (principal, principal_kind)
        );
    }
    // The route scope is pre-checked against the grant's internal scopes.
    let mut narrow = oauth_customer(CredentialKind::OauthAccess, vec![Uuid::new_v4()]);
    narrow.scopes.clear();
    g.authority.add("cfoa_live_narrow", narrow);
    let r = g.get(ALIAS, Some("cfoa_live_narrow")).await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::FORBIDDEN, "INSUFFICIENT_SCOPE")
    );
    // Platform credentials keep working on the same alias.
    g.authority.add(CRED, customer(&["meetings.read"]));
    let r = g.get(ALIAS, Some(CRED)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(
        g.meeting().last().delegated.unwrap().initiating_principal(),
        None
    );
    assert_eq!(g.meeting().count(), 3);
}

#[tokio::test]
async fn a_restriction_that_is_not_exactly_one_principal_or_a_foreign_kind_is_refused() {
    let g = gateway(true).await;
    let cases: Vec<(&str, AuthenticatedCustomer)> = vec![
        (
            "cfoa_live_none",
            oauth_customer(CredentialKind::OauthAccess, vec![]),
        ),
        (
            "cfoa_live_two",
            oauth_customer(
                CredentialKind::OauthAccess,
                vec![Uuid::new_v4(), Uuid::new_v4()],
            ),
        ),
        (
            "cfoa_live_unrestricted",
            AuthenticatedCustomer {
                principal_restriction: None,
                ..oauth_customer(CredentialKind::OauthAccess, vec![])
            },
        ),
        // The prefix claims an access token; Platform Access says bot.
        (
            "cfoa_live_bot",
            oauth_customer(CredentialKind::OauthBot, vec![Uuid::new_v4()]),
        ),
        // Platform Access says it is no OAuth credential at all.
        ("cfoa_live_apikey", customer(&["meetings.read"])),
    ];
    for (token, c) in cases {
        g.authority.add(token, c);
        let r = g.get(ALIAS, Some(token)).await;
        assert_eq!(
            (r.status, r.code()),
            (StatusCode::UNAUTHORIZED, "AUTHENTICATION_FAILED"),
            "{token}"
        );
    }
    // An API key that Platform Access reports as an OAuth credential is refused too.
    g.authority.add(
        CRED,
        oauth_customer(CredentialKind::OauthAccess, vec![Uuid::new_v4()]),
    );
    assert_eq!(
        g.get(ALIAS, Some(CRED)).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(g.meeting().count(), 0, "nothing was forwarded");
}

#[tokio::test]
async fn a_refresh_token_is_always_refused() {
    let g = gateway(true).await;
    for path in [
        ALIAS,
        "/api/v2/test-spaces/s-1",
        "/v1/oauth/userinfo",
        "/v1/app-directory",
    ] {
        let r = g.get(path, Some(REFRESH)).await;
        assert_eq!(
            (r.status, r.code()),
            (StatusCode::UNAUTHORIZED, "AUTHENTICATION_FAILED"),
            "{path}"
        );
    }
    // Never as anything but a bearer token either.
    let r = g
        .call(
            Method::GET,
            ALIAS,
            &[("authorization", &format!("cf-client-token {ACCESS}"))],
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(calls(&g), 0, "Platform Access never saw a refresh token");
    assert_eq!(g.meeting().count() + g.dp().count(), 0);
}

#[tokio::test]
async fn an_oauth_token_on_a_route_without_the_mode_is_refused_403_and_never_transported() {
    let g = gateway(true).await;
    g.authority.add(
        ACCESS,
        oauth_customer(CredentialKind::OauthAccess, vec![Uuid::new_v4()]),
    );
    for (path, token) in [
        // PLATFORM_CREDENTIAL only.
        ("/v1/test/items/abc", ACCESS),
        // MEETING_SESSION / PARTICIPANT_TOKEN: would otherwise be transported.
        ("/api/v2/test-spaces/s-1", ACCESS),
        ("/v1/test/mixed", BOT),
        // Anonymous but not marked forwardAuthorization.
        ("/v1/app-directory", ACCESS),
    ] {
        let r = g.get(path, Some(token)).await;
        assert_eq!(
            (r.status, r.code()),
            (StatusCode::FORBIDDEN, "CREDENTIAL_NOT_ACCEPTED"),
            "{path}"
        );
    }
    assert_eq!(calls(&g), 0);
    assert_eq!(
        g.calling().count() + g.messaging().count() + g.meeting().count() + g.dp().count(),
        0
    );
}

#[tokio::test]
async fn with_the_flag_off_oauth_tokens_are_handled_exactly_as_before() {
    let g = gateway(false).await;
    g.authority.add(
        ACCESS,
        oauth_customer(CredentialKind::OauthAccess, vec![Uuid::new_v4()]),
    );
    // A route listing OAUTH_ACCESS_TOKEN: an unknown bearer value there (401).
    assert_eq!(
        g.get(ALIAS, Some(ACCESS)).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(calls(&g), 0, "not authenticated");
    // A meeting-session route transports the bearer value as before.
    let r = g.get("/api/v2/test-spaces/s-1", Some(ACCESS)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(
        g.meeting().last().headers.get("authorization").unwrap(),
        &format!("Bearer {ACCESS}")
    );
    // No pass-through on a forwardAuthorization route.
    assert_eq!(
        g.get("/v1/oauth/userinfo", Some(ACCESS)).await.status,
        StatusCode::UNAUTHORIZED
    );
    let r = g
        .call(
            Method::POST,
            "/v1/oauth/token",
            &[
                ("authorization", "Basic Y2xpZW50OnNlY3JldA=="),
                ("content-type", "application/x-www-form-urlencoded"),
            ],
            Some(FORM),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // Platform credentials on the alias: unchanged.
    g.authority.add(CRED, customer(&["meetings.read"]));
    assert_eq!(g.get(ALIAS, Some(CRED)).await.status, StatusCode::OK);
}

#[tokio::test]
async fn anonymous_form_urlencoded_posts_reach_the_developer_console_verbatim() {
    for oauth in [true, false] {
        let g = gateway(oauth).await;
        let r = g
            .call(
                Method::POST,
                "/v1/oauth/token",
                &[("content-type", "application/x-www-form-urlencoded")],
                Some(FORM),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.json);
        let last = g.dp().last();
        assert_eq!(last.path, "/v1/oauth/token");
        assert_eq!(last.raw_body, FORM.as_bytes());
        assert_eq!(
            last.headers.get("content-type").unwrap(),
            "application/x-www-form-urlencoded"
        );
        // Transported: no gateway token, no delegated context, no session.
        assert_eq!(last.issuer, None);
        assert!(last.delegated.is_none());
        assert!(last.headers.get("x-cf-user-session").is_none());
        assert!(last.headers.get("authorization").is_none());
        // Bounded like every body.
        let big = "a=".to_string() + &"x".repeat(8192);
        let r = g
            .call(
                Method::POST,
                "/v1/oauth/token",
                &[("content-type", "application/x-www-form-urlencoded")],
                Some(&big),
            )
            .await;
        assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(g.dp().count(), 1);
        // Anonymous GETs (metadata, directory) as well.
        assert_eq!(
            g.get("/v1/app-directory", None).await.status,
            StatusCode::OK
        );
        assert_eq!(g.dp().last().issuer, None);
    }
}

#[tokio::test]
async fn authorization_is_transported_only_on_the_marked_route() {
    let g = gateway(true).await;
    // Userinfo: the OAuth bearer reaches the Developer Console unauthenticated here.
    let r = g.get("/v1/oauth/userinfo", Some(ACCESS)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let last = g.dp().last();
    assert_eq!(
        last.headers.get("authorization").unwrap(),
        &format!("Bearer {ACCESS}")
    );
    assert!(last.delegated.is_none(), "the gateway grants no authority");
    assert_eq!(
        calls(&g),
        0,
        "the Developer Console introspects the token itself"
    );
    // Without a token: anonymous, nothing transported.
    assert_eq!(
        g.get("/v1/oauth/userinfo", None).await.status,
        StatusCode::OK
    );
    assert!(g.dp().last().headers.get("authorization").is_none());
    // client_secret_basic on the token endpoint.
    let basic = "Basic Y2xpZW50OnNlY3JldA==";
    let r = g
        .call(
            Method::POST,
            "/v1/oauth/token",
            &[
                ("authorization", basic),
                ("content-type", "application/x-www-form-urlencoded"),
            ],
            Some(FORM),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(g.dp().last().headers.get("authorization").unwrap(), basic);
    let forwarded = g.dp().count();
    // Never anything else, and never on an unmarked route.
    for (path, value) in [
        ("/v1/oauth/userinfo", format!("Bearer {CRED}")),
        ("/v1/oauth/userinfo", format!("Bearer {REFRESH}")),
        ("/v1/oauth/userinfo", "Bearer some-session".to_string()),
        ("/v1/app-directory", basic.to_string()),
        ("/v1/oauth/metadata", "Bearer some-session".to_string()),
    ] {
        let r = g
            .call(Method::GET, path, &[("authorization", &value)], None)
            .await;
        assert!(
            r.status == StatusCode::UNAUTHORIZED,
            "{path} {value}: {}",
            r.status
        );
    }
    assert_eq!(
        g.get("/v1/oauth/metadata", Some(ACCESS)).await.code(),
        "CREDENTIAL_NOT_ACCEPTED"
    );
    assert_eq!(g.dp().count(), forwarded);
    assert_eq!(calls(&g), 0);
}
