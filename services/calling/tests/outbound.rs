//! Section 11 / 14 outbound: safe delegated context from a trusted customer context,
//! request / correlation propagation, and the authenticated internal HTTP client over
//! real loopback HTTP (target selection, safe error mapping, no redirects, timeouts).

mod common;

use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use axum::{routing::any, Router};
use calling::{
    domain::{EnvironmentKind, PrincipalKind, TenantClassification},
    internal_auth::delegation::{delegated_context, outbound_for_customer, DelegationError},
    platform::principals::{NewPrincipal, PrincipalReference},
};
use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient, InternalTargets},
    ApplicationEnvironmentKind, CorrelationId, DelegatedPrincipalKind, OutboundServiceRequest,
    RuntimeEnvironment, ServiceId, ServiceTokenSigner,
};
use common::Harness;
use serde_json::Value;

async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    format!("http://{address}")
}

fn client(
    signer: &Arc<ServiceTokenSigner>,
    calling_base: &str,
    timeout: Duration,
) -> InternalServiceClient {
    let mut targets = InternalTargets::new(RuntimeEnvironment::Test);
    targets.insert(ServiceId::calling(), calling_base).unwrap();
    InternalServiceClient::new(signer.clone(), targets, timeout).unwrap()
}

#[tokio::test]
async fn delegated_context_is_built_only_from_trusted_customer_state() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let other = h.consumer("other", TenantClassification::External).await;
    let credential = h
        .issue(c.sa(EnvironmentKind::Sandbox), &["principals.read"])
        .await;
    let customer = h.ctx(credential.secret.expose_once()).await;

    let plain = delegated_context(&customer, None).unwrap();
    assert_eq!(plain.tenant_id(), c.tenant.id.as_uuid());
    assert_eq!(plain.application_id(), c.application.id.as_uuid());
    assert_eq!(
        plain.application_environment_id(),
        c.env(EnvironmentKind::Sandbox).id.as_uuid()
    );
    assert_eq!(
        plain.application_environment_kind(),
        ApplicationEnvironmentKind::Sandbox
    );
    assert_eq!(
        plain.initiating_service_account_id(),
        c.sa(EnvironmentKind::Sandbox).as_uuid()
    );
    assert!(plain.initiating_principal().is_none());

    // A separately resolved principal of the same scope may be attached.
    let (registered, _) = h
        .platform
        .register_principal(
            c.env(EnvironmentKind::Sandbox).id,
            NewPrincipal::External {
                kind: PrincipalKind::EnterpriseMember,
                external_reference: "member-9".into(),
            },
        )
        .await
        .unwrap();
    let principal = h
        .state
        .principals
        .resolve(&customer, &PrincipalReference::Id(registered.id))
        .await
        .unwrap();
    let with = delegated_context(&customer, Some(&principal)).unwrap();
    let p = with.initiating_principal().unwrap();
    assert_eq!(p.principal_id, registered.id.as_uuid());
    assert_eq!(p.principal_kind, DelegatedPrincipalKind::EnterpriseMember);

    // A principal of another tenant can never be attached.
    let foreign_credential = h
        .issue(other.sa(EnvironmentKind::Sandbox), &["principals.read"])
        .await;
    let foreign_ctx = h.ctx(foreign_credential.secret.expose_once()).await;
    let (foreign, _) = h
        .platform
        .register_principal(
            other.env(EnvironmentKind::Sandbox).id,
            NewPrincipal::External {
                kind: PrincipalKind::NativeCfUser,
                external_reference: "u-1".into(),
            },
        )
        .await
        .unwrap();
    let foreign_principal = h
        .state
        .principals
        .resolve(&foreign_ctx, &PrincipalReference::Id(foreign.id))
        .await
        .unwrap();
    assert_eq!(
        delegated_context(&customer, Some(&foreign_principal)),
        Err(DelegationError::PrincipalOutOfScope)
    );

    // Nothing secret travels: no credential, verifier or contact data.
    let rendered = serde_json::to_string(&with).unwrap();
    assert!(!rendered.contains(cf_service_auth_secret(&credential)));
    for forbidden in ["cfmk_", "verifier", "phone", "secret", "authorization"] {
        assert!(!rendered.to_lowercase().contains(forbidden), "{forbidden}");
    }
}

fn cf_service_auth_secret(credential: &calling::platform::credentials::IssuedCredential) -> &str {
    common::secret_part(credential.secret.expose_once())
}

#[tokio::test]
async fn outbound_tokens_carry_the_customer_correlation_and_verify_downstream() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::FirstParty).await;
    let credential = h.issue(c.sa(EnvironmentKind::Production), &[]).await;
    let customer = h.ctx(credential.secret.expose_once()).await;

    let hop = outbound_for_customer(&customer, ServiceId::privacy_relay(), None).unwrap();
    assert_eq!(hop.correlation_id(), customer.correlation_id());
    assert_ne!(hop.request_id(), customer.request_id());

    let signed = h.state.service_auth.signer().sign(&hop).unwrap();
    // Downstream (a privacy-relay identity fixture) authenticates Calling and receives the
    // delegated context — and only that: no customer credential, no PlatformAuthContext.
    let downstream = h
        .peers
        .privacy_relay_verifier
        .verify_headers(&signed.headers())
        .unwrap();
    assert_eq!(downstream.authenticated_service(), &ServiceId::calling());
    assert_eq!(downstream.correlation_id(), customer.correlation_id());
    let delegated = downstream.delegated_platform_context().unwrap();
    assert_eq!(delegated.tenant_id(), customer.tenant_id().as_uuid());
    assert_eq!(
        delegated.application_environment_kind(),
        ApplicationEnvironmentKind::Production
    );
    let headers = signed.headers();
    assert!(headers.get("authorization").is_none());
    for value in headers.values() {
        assert!(!value
            .to_str()
            .unwrap_or("")
            .contains(credential.secret.expose_once()));
    }
}

#[tokio::test]
async fn internal_client_round_trip_over_http() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let base = serve(h.router()).await;
    let telephony = client(&h.peers.telephony, &base, Duration::from_secs(5));

    let correlation = CorrelationId::generate();
    let hop = OutboundServiceRequest::continuing(ServiceId::calling(), &correlation);
    let response = telephony
        .get(&hop, "/internal/v1/test/caller")
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.request_id(), hop.request_id());
    let body: Value = response.json().unwrap();
    assert_eq!(body["service"], "cfmeet.telephony");
    assert_eq!(body["correlation_id"], correlation.as_str());
    assert_eq!(body["request_id"], hop.request_id().as_str());

    let ok = telephony
        .post_json(
            &OutboundServiceRequest::continuing(ServiceId::calling(), &correlation),
            "/internal/v1/test/callback",
            &serde_json::json!({}),
        )
        .await
        .unwrap();
    assert_eq!(ok.json::<Value>().unwrap()["accepted"], true);

    // Receiver-side denial maps to a safe status error carrying only the stable code.
    let denied = telephony
        .post_json(
            &OutboundServiceRequest::new_operation(ServiceId::calling()),
            "/internal/v1/test/never-granted",
            &serde_json::json!({}),
        )
        .await
        .unwrap_err();
    assert_eq!(
        denied,
        InternalCallError::Status {
            target: ServiceId::calling(),
            status: 403,
            code: Some("INTERNAL_PERMISSION_DENIED".into())
        }
    );

    // An untrusted signer is refused by Calling.
    let untrusted = client(&h.peers.untrusted, &base, Duration::from_secs(5));
    let refused = untrusted
        .get(
            &OutboundServiceRequest::new_operation(ServiceId::calling()),
            "/internal/v1/test/caller",
        )
        .await
        .unwrap_err();
    assert!(
        matches!(refused, InternalCallError::Status { status: 401, ref code, .. } if code.as_deref() == Some("INTERNAL_AUTHENTICATION_FAILED"))
    );
    assert!(
        !refused.to_string().contains("eyJ"),
        "error leaks a token: {refused}"
    );

    // Target selection is by audience and configuration only; paths are confined.
    let unknown = telephony
        .get(
            &OutboundServiceRequest::new_operation(ServiceId::privacy_relay()),
            "/internal/v1/x",
        )
        .await
        .unwrap_err();
    assert_eq!(
        unknown,
        InternalCallError::UnknownTarget(ServiceId::privacy_relay())
    );
    for path in [
        "/api/v1/calls",
        "/internal/v1/../admin",
        "/internal/v1/x?y=1",
    ] {
        let invalid = telephony
            .get(
                &OutboundServiceRequest::new_operation(ServiceId::calling()),
                path,
            )
            .await
            .unwrap_err();
        assert_eq!(invalid, InternalCallError::InvalidPath, "{path}");
    }

    // Calling's own client has no targets configured in tests.
    let none = h
        .state
        .service_auth
        .client()
        .get(
            &OutboundServiceRequest::new_operation(ServiceId::telephony()),
            "/internal/v1/x",
        )
        .await
        .unwrap_err();
    assert_eq!(
        none,
        InternalCallError::UnknownTarget(ServiceId::telephony())
    );
}

#[tokio::test]
async fn internal_client_never_follows_redirects_and_times_out() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let seen = Arc::new(AtomicUsize::new(0));
    let recorder = {
        let seen = seen.clone();
        Router::new().route(
            "/*rest",
            any(move |headers: axum::http::HeaderMap| {
                let seen = seen.clone();
                async move {
                    if headers.contains_key("x-cf-service-authorization") {
                        seen.fetch_add(1, Ordering::SeqCst);
                    }
                    "captured"
                }
            }),
        )
    };
    let elsewhere = serve(recorder).await;
    common::REDIRECT_TARGET
        .set(format!("{elsewhere}/internal/v1/capture"))
        .unwrap();
    let base = serve(h.router()).await;

    let telephony = client(&h.peers.telephony, &base, Duration::from_secs(5));
    let redirected = telephony
        .get(
            &OutboundServiceRequest::new_operation(ServiceId::calling()),
            "/internal/v1/test/redirect",
        )
        .await
        .unwrap_err();
    assert!(
        matches!(redirected, InternalCallError::Status { status: 307, .. }),
        "{redirected:?}"
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        seen.load(Ordering::SeqCst),
        0,
        "the service token was forwarded to another host"
    );

    let impatient = client(&h.peers.telephony, &base, Duration::from_millis(500));
    let slow = impatient
        .get(
            &OutboundServiceRequest::new_operation(ServiceId::calling()),
            "/internal/v1/test/slow",
        )
        .await
        .unwrap_err();
    assert_eq!(
        slow,
        InternalCallError::Timeout {
            target: ServiceId::calling()
        }
    );
}
