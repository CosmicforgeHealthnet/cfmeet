//! Blueprint Section 12 follow-up (CAL-007 / ORG-012 / CAL-009): first-party organization
//! requests the Meeting Service forwards to Calling's REAL public routes.
//!
//! The Meeting Service authenticates the human, checks the organization's
//! `calling.enterprise.manage` capability with the Organizations Service, resolves the
//! organization's dedicated FIRST_PARTY scope and its Calling credential (id only) and
//! signs a service token whose delegated context names that credential and the member's
//! NATIVE_CF_USER principal. Calling accepts it ONLY while
//! `CALLING_ORGANIZATION_FORWARDING_ENABLED` is on, from an issuer granted
//! `calling-request.act-for-organization`, re-verifies the credential live with Platform
//! Access, requires a FIRST_PARTY organization tenant (`org-<32 hex>`) and resolves the
//! principal inside the credential's scope — and the enterprise audit then names the
//! human. Every other combination is the normalized 401 (503 when an authority is
//! unreachable).

mod common;

use axum::http::{Method, StatusCode};
use calling::{
    domain::{EnvironmentKind, PrincipalKind, TenantClassification},
    platform::scopes::names,
};
use cf_service_auth::{
    ApplicationEnvironmentKind, DelegatedPlatformContext, DelegatedPrincipalKind,
    OutboundServiceRequest, ServiceId, ServiceTokenSigner,
};
use common::{
    enterprise::{allow_enterprise_scopes, credential, default_rules},
    Consumer, Harness, IssuedCredential, NewPrincipal, TestResponse,
};
use serde_json::{json, Value};
use uuid::Uuid;

const KIND: EnvironmentKind = EnvironmentKind::Production;

/// An organization's dedicated tenant: FIRST_PARTY with the reserved slug.
async fn organization(h: &Harness) -> Consumer {
    let slug = format!("org-{}", Uuid::new_v4().simple());
    h.consumer(&slug, TenantClassification::FirstParty).await
}

/// The organization's Calling credential (enterprise administration scopes).
async fn org_credential(h: &Harness, c: &Consumer) -> IssuedCredential {
    allow_enterprise_scopes(h, c).await;
    credential(
        h,
        c,
        KIND,
        &[
            names::ENTERPRISE_CONFIG_READ,
            names::ENTERPRISE_CONFIG_WRITE,
        ],
    )
    .await
}

/// A principal of `kind` registered in `c`'s production environment.
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
    acting: Option<(Uuid, DelegatedPrincipalKind)>,
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
    if let Some((id, kind)) = acting {
        context = context.with_initiating_principal(id, kind);
    }
    context
}

fn member(id: Uuid) -> Option<(Uuid, DelegatedPrincipalKind)> {
    Some((id, DelegatedPrincipalKind::NativeCfUser))
}

fn signed(signer: &ServiceTokenSigner, context: DelegatedPlatformContext) -> Vec<(String, String)> {
    let request =
        OutboundServiceRequest::new_operation(ServiceId::calling()).with_delegated_context(context);
    signer
        .sign(&request)
        .unwrap()
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap().to_string()))
        .collect()
}

async fn send(
    h: &Harness,
    method: Method,
    path: &str,
    headers: &[(String, String)],
    body: Option<Value>,
) -> TestResponse {
    let refs: Vec<(&str, &str)> = headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    h.request_with(method, path, None, body, &refs).await
}

async fn whoami(h: &Harness, headers: &[(String, String)]) -> TestResponse {
    send(h, Method::GET, "/test/context", headers, None).await
}

fn assert_rejected(response: &TestResponse) {
    assert_eq!(
        response.status,
        StatusCode::UNAUTHORIZED,
        "{}",
        response.text
    );
    assert_eq!(
        response.normalized_error(),
        json!({ "error": {
            "code": "AUTHENTICATION_FAILED",
            "message": "Authentication failed.",
            "requestId": null
        }})
    );
}

async fn enabled() -> Option<Harness> {
    let mut h = Harness::new().await?;
    h.state.organization_forwarding_enabled = true;
    Some(h)
}

#[tokio::test]
async fn an_organization_request_acts_in_the_organization_scope_and_audit_names_the_human() {
    let Some(h) = enabled().await else { return };
    let c = organization(&h).await;
    let issued = org_credential(&h, &c).await;
    let acting = principal(&h, &c, PrincipalKind::NativeCfUser).await;
    let headers = signed(
        &h.peers.meeting_service,
        delegated(&c, Some(issued.credential.id.as_uuid()), member(acting)),
    );

    // The context is the organization's live chain plus the acting member.
    let r = whoami(&h, &headers).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["tenant_id"], json!(c.tenant.id));
    assert_eq!(r.json["tenant_classification"], "FIRST_PARTY");
    assert_eq!(r.json["credential_id"], json!(issued.credential.id));
    assert_eq!(r.json["acting_principal_id"], json!(acting));

    // A real administrative change, audited with the human actor.
    let created = send(
        &h,
        Method::POST,
        "/v1/enterprise/policies",
        &headers,
        Some(json!({ "name": "Head office", "isDefault": true, "rules": default_rules() })),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.text);
    let policy = created.json["policyId"].as_str().unwrap().to_string();
    let r = send(
        &h,
        Method::GET,
        &format!("/v1/enterprise/audit?resourceId={policy}"),
        &headers,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let records = r.json["records"].as_array().unwrap();
    assert_eq!(records.len(), 1, "{}", r.text);
    assert_eq!(records[0]["operation"], "CREATE");
    assert_eq!(records[0]["actorKind"], "CUSTOMER_APPLICATION");
    assert_eq!(
        records[0]["actorCredentialId"],
        issued.credential.id.to_string()
    );
    assert_eq!(records[0]["actorPrincipalId"], acting.to_string());

    // The same credential presented by the API Gateway acts for no human (unchanged).
    let gateway = signed(
        &h.peers.api_gateway,
        delegated(&c, Some(issued.credential.id.as_uuid()), member(acting)),
    );
    let r = whoami(&h, &gateway).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["acting_principal_id"], Value::Null);
}

#[tokio::test]
async fn organization_forwarding_is_refused_while_the_flag_is_off() {
    let Some(h) = Harness::new().await else {
        return;
    };
    assert!(!h.state.organization_forwarding_enabled, "default is off");
    let c = organization(&h).await;
    let issued = org_credential(&h, &c).await;
    let acting = principal(&h, &c, PrincipalKind::NativeCfUser).await;
    let headers = signed(
        &h.peers.meeting_service,
        delegated(&c, Some(issued.credential.id.as_uuid()), member(acting)),
    );
    assert_rejected(&whoami(&h, &headers).await);
    assert_rejected(&send(&h, Method::GET, "/v1/enterprise/audit", &headers, None).await);
}

#[tokio::test]
async fn an_organization_request_needs_the_credential_and_a_native_member() {
    let Some(h) = enabled().await else { return };
    let c = organization(&h).await;
    let issued = org_credential(&h, &c).await;
    let credential = Some(issued.credential.id.as_uuid());
    let acting = principal(&h, &c, PrincipalKind::NativeCfUser).await;

    // No acting principal.
    let headers = signed(&h.peers.meeting_service, delegated(&c, credential, None));
    assert_rejected(&whoami(&h, &headers).await);
    // An acting principal that is not a human member.
    let headers = signed(
        &h.peers.meeting_service,
        delegated(
            &c,
            credential,
            Some((acting, DelegatedPrincipalKind::ServiceAccount)),
        ),
    );
    assert_rejected(&whoami(&h, &headers).await);
    // No credential id.
    let headers = signed(
        &h.peers.meeting_service,
        delegated(&c, None, member(acting)),
    );
    assert_rejected(&whoami(&h, &headers).await);
    // A credential Platform Access does not know.
    let headers = signed(
        &h.peers.meeting_service,
        delegated(&c, Some(Uuid::new_v4()), member(acting)),
    );
    assert_rejected(&whoami(&h, &headers).await);
}

#[tokio::test]
async fn only_first_party_organization_tenants_are_reachable() {
    let Some(h) = enabled().await else { return };
    // FIRST_PARTY but not an organization tenant (e.g. the shared personal scope).
    let personal = h
        .consumer("cfmeet-personal", TenantClassification::FirstParty)
        .await;
    // The reserved slug on an EXTERNAL tenant.
    let external = h
        .consumer(
            &format!("org-{}", Uuid::new_v4().simple()),
            TenantClassification::External,
        )
        .await;
    for c in [&personal, &external] {
        let issued = org_credential(&h, c).await;
        let acting = principal(&h, c, PrincipalKind::NativeCfUser).await;
        let headers = signed(
            &h.peers.meeting_service,
            delegated(c, Some(issued.credential.id.as_uuid()), member(acting)),
        );
        assert_rejected(&whoami(&h, &headers).await);
        assert_rejected(&send(&h, Method::GET, "/v1/enterprise/audit", &headers, None).await);
    }
}

#[tokio::test]
async fn the_acting_principal_must_belong_to_the_organization_scope() {
    let Some(h) = enabled().await else { return };
    let c = organization(&h).await;
    let other = organization(&h).await;
    let issued = org_credential(&h, &c).await;
    let credential = Some(issued.credential.id.as_uuid());

    // A member principal of ANOTHER organization's scope.
    let foreign = principal(&h, &other, PrincipalKind::NativeCfUser).await;
    let headers = signed(
        &h.peers.meeting_service,
        delegated(&c, credential, member(foreign)),
    );
    assert_rejected(&whoami(&h, &headers).await);
    // A principal id nobody registered.
    let headers = signed(
        &h.peers.meeting_service,
        delegated(&c, credential, member(Uuid::new_v4())),
    );
    assert_rejected(&whoami(&h, &headers).await);
    // A principal of this scope that is not a CF Meet user, claimed as one.
    let not_native = principal(&h, &c, PrincipalKind::EnterpriseMember).await;
    let headers = signed(
        &h.peers.meeting_service,
        delegated(&c, credential, member(not_native)),
    );
    assert_rejected(&whoami(&h, &headers).await);
}

#[tokio::test]
async fn the_gateway_grant_does_not_open_the_organization_path_and_vice_versa() {
    let Some(h) = enabled().await else { return };
    let c = organization(&h).await;
    let issued = org_credential(&h, &c).await;
    let acting = principal(&h, &c, PrincipalKind::NativeCfUser).await;
    let context = delegated(&c, Some(issued.credential.id.as_uuid()), member(acting));
    // Trusted issuers holding neither forwarding grant, and an untrusted signer.
    for peer in [
        &h.peers.privacy_relay,
        &h.peers.telephony,
        &h.peers.communications,
        &h.peers.untrusted,
    ] {
        assert_rejected(&whoami(&h, &signed(peer, context.clone())).await);
    }
    // The genuine organization forwarder is accepted.
    let r = whoami(&h, &signed(&h.peers.meeting_service, context)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
}
