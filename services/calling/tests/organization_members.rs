//! Gap audit G3: first-party organization MEMBERS in their organization's own Calling
//! scope, over internal service authentication (`src/organization_members.rs`).
//!
//! * `calling-organization-member.act`: the Meeting Service registers, lists and revokes
//!   the member's own endpoints and mints their signaling credentials. The delegated
//!   context names the organization's Calling credential (re-verified live; its closed
//!   scope set has `calls.read`, never `calls.write`) and the member's NATIVE_CF_USER
//!   principal. Only that principal is ever acted for; a suspended member, a non-organization
//!   tenant, a missing credential or principal, and any other caller are refused.
//! * `calling-organization-member.offboard`: removal / suspension disables the extensions
//!   routed to the member's principal; reactivation restores exactly those.

mod common;

use axum::http::{Method, StatusCode};
use calling::{
    domain::{EnvironmentKind, PrincipalId, PrincipalKind, PrincipalStatus, TenantClassification},
    platform::scopes::names,
};
use cf_service_auth::{
    ApplicationEnvironmentKind, DelegatedPlatformContext, DelegatedPrincipalKind,
    OutboundServiceRequest, ServiceId, ServiceTokenSigner,
};
use common::{
    enterprise::{allow_enterprise_scopes, credential},
    Consumer, Harness, IssuedCredential, NewPrincipal, TestResponse,
};
use serde_json::{json, Value};
use uuid::Uuid;

const KIND: EnvironmentKind = EnvironmentKind::Production;

async fn organization(h: &Harness) -> Consumer {
    let slug = format!("org-{}", Uuid::new_v4().simple());
    h.consumer(&slug, TenantClassification::FirstParty).await
}

/// The organization's Calling binding credential: exactly the closed scope set the
/// Organizations provisioning saga binds (`calls.read`, `enterprise.config.*`).
async fn org_credential(h: &Harness, c: &Consumer) -> IssuedCredential {
    allow_enterprise_scopes(h, c).await;
    credential(
        h,
        c,
        KIND,
        &[
            names::CALLS_READ,
            names::ENTERPRISE_CONFIG_READ,
            names::ENTERPRISE_CONFIG_WRITE,
        ],
    )
    .await
}

async fn principal(h: &Harness, c: &Consumer, kind: PrincipalKind) -> Uuid {
    let (view, _) = h
        .platform
        .register_principal(
            c.env(KIND).id,
            NewPrincipal::External {
                kind,
                external_reference: format!("cfmeet-user:{}", Uuid::new_v4()),
            },
        )
        .await
        .unwrap();
    view.id.as_uuid()
}

fn delegated(
    c: &Consumer,
    credential: Option<Uuid>,
    acting: Option<Uuid>,
) -> DelegatedPlatformContext {
    let mut context = DelegatedPlatformContext::new(
        c.tenant.id.as_uuid(),
        c.application.id.as_uuid(),
        c.env(KIND).id.as_uuid(),
        ApplicationEnvironmentKind::Production,
        c.sa(KIND).as_uuid(),
    );
    if let Some(id) = credential {
        context = context.with_initiating_credential(id);
    }
    if let Some(id) = acting {
        context = context.with_initiating_principal(id, DelegatedPrincipalKind::NativeCfUser);
    }
    context
}

async fn internal(
    h: &Harness,
    signer: &ServiceTokenSigner,
    context: DelegatedPlatformContext,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> TestResponse {
    let request =
        OutboundServiceRequest::new_operation(ServiceId::calling()).with_delegated_context(context);
    let headers: Vec<(String, String)> = signer
        .sign(&request)
        .unwrap()
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap().to_string()))
        .collect();
    let refs: Vec<(&str, &str)> = headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    h.request_with(method, path, None, body, &refs).await
}

fn endpoint_body(key: &str) -> Value {
    json!({ "endpointKey": key, "endpointKind": "WEB", "capabilities": { "audio": true, "video": true } })
}

const ENDPOINTS: &str = "/internal/v1/organization-members/endpoints";
const REVOKE: &str = "/internal/v1/organization-members/endpoints/revoke";
const SIGNALING: &str = "/internal/v1/organization-members/signaling-credentials";
const EXTENSIONS: &str = "/internal/v1/organization-members/extension-status";

#[tokio::test]
async fn a_member_registers_signals_and_revokes_only_their_own_endpoints() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = organization(&h).await;
    let issued = org_credential(&h, &c).await;
    let cred = Some(issued.credential.id.as_uuid());
    let alice = principal(&h, &c, PrincipalKind::NativeCfUser).await;
    let bob = principal(&h, &c, PrincipalKind::NativeCfUser).await;
    let ms = h.peers.meeting_service.clone();
    let as_alice = || delegated(&c, cred, Some(alice));
    let as_bob = || delegated(&c, cred, Some(bob));

    // Register (201), refresh (200): always the member's own principal.
    let r = internal(
        &h,
        &ms,
        as_alice(),
        Method::POST,
        ENDPOINTS,
        Some(endpoint_body("alice-web")),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(r.json["principalId"], alice.to_string());
    assert_eq!(r.json["status"], "ACTIVE");
    let endpoint = r.json["endpointId"].as_str().unwrap().to_string();
    let r = internal(
        &h,
        &ms,
        as_alice(),
        Method::POST,
        ENDPOINTS,
        Some(endpoint_body("alice-web")),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["endpointId"], endpoint);
    // The body never names a principal.
    let mut foreign = endpoint_body("alice-2");
    foreign["principalId"] = json!(bob);
    let r = internal(&h, &ms, as_alice(), Method::POST, ENDPOINTS, Some(foreign)).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);

    // Lists are per member.
    let r = internal(&h, &ms, as_alice(), Method::GET, ENDPOINTS, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["data"].as_array().unwrap().len(), 1);
    let r = internal(&h, &ms, as_bob(), Method::GET, ENDPOINTS, None).await;
    assert_eq!(r.json["data"].as_array().unwrap().len(), 0, "{}", r.text);

    // A signaling credential for the member's own endpoint opens its signaling routes,
    // although the organization credential holds no calls.write.
    let r = internal(
        &h,
        &ms,
        as_alice(),
        Method::POST,
        SIGNALING,
        Some(json!({ "endpointId": endpoint })),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(r.json["principalId"], alice.to_string());
    assert_eq!(r.json["signalingUrl"], "/v1/calling/signaling");
    let token = r.json["token"].as_str().unwrap().to_string();
    assert!(token.starts_with("cfsig_"));
    let r = h
        .request(Method::GET, "/v1/calling/invitations", Some(&token), None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["endpointId"], endpoint);
    // The same signaling credential minted the ordinary way by a calls.read credential
    // would never verify: only the organization member mark relaxes calls.write, and only
    // in an organization tenant (the plain one is refused).
    let keys = h.state.signaling.keys();
    let scope = calling::calls::command::CallScope::of_delegated(&as_alice());
    let plain = keys
        .mint(
            scope,
            PrincipalId::from_uuid(alice),
            calling::endpoints::CallEndpointId::from_uuid(Uuid::parse_str(&endpoint).unwrap()),
            c.sa(KIND),
            issued.credential.id,
            h.state.clock.now(),
        )
        .unwrap();
    use secrecy::ExposeSecret;
    let r = h
        .request(
            Method::GET,
            "/v1/calling/invitations",
            Some(plain.token.expose_secret()),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);

    // Another member can neither mint for nor revoke Alice's endpoint (hidden: 404).
    let r = internal(
        &h,
        &ms,
        as_bob(),
        Method::POST,
        SIGNALING,
        Some(json!({ "endpointId": endpoint })),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    let r = internal(
        &h,
        &ms,
        as_bob(),
        Method::POST,
        REVOKE,
        Some(json!({ "endpointId": endpoint })),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);

    // A suspended member (offboarded principal) is refused, and their signaling stops.
    h.platform
        .set_principal_status(PrincipalId::from_uuid(alice), PrincipalStatus::Suspended)
        .await
        .unwrap();
    let r = internal(&h, &ms, as_alice(), Method::GET, ENDPOINTS, None).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);
    let r = h
        .request(Method::GET, "/v1/calling/invitations", Some(&token), None)
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);
    h.platform
        .set_principal_status(PrincipalId::from_uuid(alice), PrincipalStatus::Active)
        .await
        .unwrap();

    // Revoke: own endpoint only; the credential stops working.
    let r = internal(
        &h,
        &ms,
        as_alice(),
        Method::POST,
        REVOKE,
        Some(json!({ "endpointId": endpoint })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["status"], "REVOKED");
    let r = h
        .request(Method::GET, "/v1/calling/invitations", Some(&token), None)
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);
    let r = internal(
        &h,
        &ms,
        as_alice(),
        Method::POST,
        SIGNALING,
        Some(json!({ "endpointId": endpoint })),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
}

#[tokio::test]
async fn only_the_meeting_service_acts_and_only_for_an_organization_member() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = organization(&h).await;
    let issued = org_credential(&h, &c).await;
    let cred = Some(issued.credential.id.as_uuid());
    let alice = principal(&h, &c, PrincipalKind::NativeCfUser).await;
    let ms = h.peers.meeting_service.clone();
    let body = Some(endpoint_body("probe"));

    // Another trusted service (no grant) and an untrusted one.
    let r = internal(
        &h,
        &h.peers.api_gateway,
        delegated(&c, cred, Some(alice)),
        Method::POST,
        ENDPOINTS,
        body.clone(),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    let r = internal(
        &h,
        &h.peers.untrusted,
        delegated(&c, cred, Some(alice)),
        Method::POST,
        ENDPOINTS,
        body.clone(),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);
    // No credential or no acting member in the delegated context.
    for context in [delegated(&c, None, Some(alice)), delegated(&c, cred, None)] {
        let r = internal(&h, &ms, context, Method::POST, ENDPOINTS, body.clone()).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    }
    // A principal of another kind, or of another scope.
    let member = principal(&h, &c, PrincipalKind::EnterpriseMember).await;
    let r = internal(
        &h,
        &ms,
        delegated(&c, cred, Some(member)),
        Method::POST,
        ENDPOINTS,
        body.clone(),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);
    let other = organization(&h).await;
    let stranger = principal(&h, &other, PrincipalKind::NativeCfUser).await;
    let r = internal(
        &h,
        &ms,
        delegated(&c, cred, Some(stranger)),
        Method::POST,
        ENDPOINTS,
        body.clone(),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);
    // Not an organization tenant: a FIRST_PARTY tenant without the reserved slug.
    let plain = h
        .consumer("first-party-app", TenantClassification::FirstParty)
        .await;
    let plain_cred = org_credential(&h, &plain).await;
    let plain_member = principal(&h, &plain, PrincipalKind::NativeCfUser).await;
    let r = internal(
        &h,
        &ms,
        delegated(
            &plain,
            Some(plain_cred.credential.id.as_uuid()),
            Some(plain_member),
        ),
        Method::POST,
        ENDPOINTS,
        body.clone(),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);
    // A revoked organization credential stops everything at once.
    h.platform
        .revoke_credential(issued.credential.id)
        .await
        .unwrap();
    let r = internal(
        &h,
        &ms,
        delegated(&c, cred, Some(alice)),
        Method::POST,
        ENDPOINTS,
        body,
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);
    // Nothing was registered by any refused request.
    assert_eq!(
        common::scalar_i64(h.conn(), "SELECT count(*) FROM call_endpoints").await,
        0
    );
}

async fn extension(h: &Harness, admin: &str, number: &str, principal: Uuid) -> Value {
    let r = h
        .request(
            Method::POST,
            "/v1/enterprise/extensions",
            Some(admin),
            Some(json!({
                "extensionNumber": number,
                "displayLabel": format!("Extension {number}"),
                "destination": { "type": "PRINCIPAL", "id": principal.to_string() },
            })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    r.json
}

async fn status_of(h: &Harness, admin: &str, id: &str) -> String {
    let r = h
        .request(
            Method::GET,
            &format!("/v1/enterprise/extensions/{id}"),
            Some(admin),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    r.json["status"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn offboarding_disables_and_reactivation_restores_only_offboarded_extensions() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = organization(&h).await;
    let issued = org_credential(&h, &c).await;
    let admin = issued.secret.expose_once().to_string();
    let alice = principal(&h, &c, PrincipalKind::NativeCfUser).await;
    let bob = principal(&h, &c, PrincipalKind::NativeCfUser).await;
    let ms = h.peers.meeting_service.clone();
    let scope = || delegated(&c, None, None);
    let apply = |status: &'static str, who: Uuid| {
        let h = &h;
        let ms = ms.clone();
        let context = scope();
        async move {
            internal(
                h,
                &ms,
                context,
                Method::POST,
                EXTENSIONS,
                Some(json!({ "principalId": who, "status": status })),
            )
            .await
        }
    };

    let a1 = extension(&h, &admin, "1001", alice).await;
    let a2 = extension(&h, &admin, "1002", alice).await;
    let a3 = extension(&h, &admin, "1003", alice).await;
    let b1 = extension(&h, &admin, "2001", bob).await;
    let id = |v: &Value| v["extensionId"].as_str().unwrap().to_string();
    // An administrator disabled 1002 themselves: offboarding never touches it again, and
    // restoration never undoes the administrator's decision.
    let r = h
        .request(
            Method::DELETE,
            &format!("/v1/enterprise/extensions/{}?expectedVersion=1", id(&a2)),
            Some(&admin),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);

    // Removal / suspension: Alice's ACTIVE extensions are disabled, idempotently.
    let r = apply("SUSPENDED", alice).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        (r.json["disabled"].as_u64(), r.json["restored"].as_u64()),
        (Some(2), Some(0))
    );
    let r = apply("SUSPENDED", alice).await;
    assert_eq!(r.json["disabled"], 0, "{}", r.text);
    for (ext, expected) in [
        (&a1, "DISABLED"),
        (&a2, "DISABLED"),
        (&a3, "DISABLED"),
        (&b1, "ACTIVE"),
    ] {
        assert_eq!(status_of(&h, &admin, &id(ext)).await, expected);
    }
    // Meanwhile 1003 is given to Bob.
    let b3 = extension(&h, &admin, "1003", bob).await;

    // Reactivation: exactly the offboarded ones, where the number is still free.
    let r = apply("ACTIVE", alice).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        (
            r.json["restored"].as_u64(),
            r.json["notRestored"].as_u64(),
            r.json["disabled"].as_u64()
        ),
        (Some(1), Some(1), Some(0))
    );
    for (ext, expected) in [
        (&a1, "ACTIVE"),
        (&a2, "DISABLED"),
        (&a3, "DISABLED"),
        (&b1, "ACTIVE"),
        (&b3, "ACTIVE"),
    ] {
        assert_eq!(status_of(&h, &admin, &id(ext)).await, expected);
    }
    let r = apply("ACTIVE", alice).await;
    assert_eq!(r.json["restored"], 0, "{}", r.text);

    // System-audited with the reason.
    let reasons = common::enterprise::strings(
        h.conn(),
        &format!(
            "SELECT operation || ':' || actor_kind || ':' || (details ->> 'reason') FROM enterprise_admin_audit \
             WHERE resource_id = '{}' AND actor_kind = 'SYSTEM' ORDER BY seq",
            id(&a1)
        ),
    )
    .await;
    assert_eq!(
        reasons,
        vec![
            "DISABLE:SYSTEM:MEMBER_OFFBOARDED",
            "ACTIVATE:SYSTEM:MEMBER_RESTORED"
        ]
    );

    // Only the Meeting Service, only an organization tenant, valid bodies only.
    let r = internal(
        &h,
        &h.peers.api_gateway,
        scope(),
        Method::POST,
        EXTENSIONS,
        Some(json!({ "principalId": alice, "status": "SUSPENDED" })),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    let r = internal(
        &h,
        &ms,
        scope(),
        Method::POST,
        EXTENSIONS,
        Some(json!({ "principalId": alice, "status": "REMOVED" })),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    let external = h
        .consumer("external-co", TenantClassification::External)
        .await;
    let r = internal(
        &h,
        &ms,
        delegated(&external, None, None),
        Method::POST,
        EXTENSIONS,
        Some(json!({ "principalId": alice, "status": "SUSPENDED" })),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    assert_eq!(status_of(&h, &admin, &id(&a1)).await, "ACTIVE");
}
