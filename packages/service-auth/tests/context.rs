//! K. safe delegated context, L. request / correlation binding, M. leakage.

mod common;

use std::{
    io::Write,
    sync::{Arc, Mutex},
};

use cf_service_auth::{
    ApplicationEnvironmentKind, CorrelationId, DelegatedPlatformContext, DelegatedPrincipalKind,
    OutboundServiceRequest, RequestId, RuntimeEnvironment, ServiceId, VerificationFailure as F,
};
use common::*;
use serde_json::json;
use uuid::Uuid;

fn delegated() -> DelegatedPlatformContext {
    DelegatedPlatformContext::new(
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        ApplicationEnvironmentKind::Sandbox,
        Uuid::new_v4(),
    )
    .with_initiating_principal(
        Uuid::new_v4(),
        DelegatedPrincipalKind::ExternalApplicationPrincipal,
    )
}

// ---------------------------------------------------------------- K. delegated context

#[test]
fn k_signed_delegated_context_round_trips() {
    let fleet = Fleet::new(RuntimeEnvironment::Staging);
    let context = delegated();
    let signed = fleet
        .calling
        .sign(
            &OutboundServiceRequest::new_operation(ServiceId::privacy_relay())
                .with_delegated_context(context.clone()),
        )
        .unwrap();
    let ctx = accept(&fleet.at_privacy_relay, &signed);
    assert_eq!(ctx.delegated_platform_context(), Some(&context));
    // Absent when not asserted.
    let plain = accept(
        &fleet.at_privacy_relay,
        &sign_for(&fleet.calling, ServiceId::privacy_relay()),
    );
    assert!(plain.delegated_platform_context().is_none());
}

#[test]
fn k_tampering_with_the_context_after_signing_is_rejected() {
    let fleet = Fleet::new(RuntimeEnvironment::Staging);
    let signed = fleet
        .calling
        .sign(
            &OutboundServiceRequest::new_operation(ServiceId::privacy_relay())
                .with_delegated_context(delegated()),
        )
        .unwrap();
    let token = token_of(&signed);
    let parts: Vec<&str> = token.split('.').collect();
    let mut claims = decode_segment(parts[1]);
    claims["delegated_context"]["tenant_id"] = json!(Uuid::new_v4().to_string());
    let tampered = format!("{}.{}.{}", parts[0], b64(&claims), parts[2]);
    assert_eq!(
        reject(
            &fleet.at_privacy_relay,
            &with_token(&signed.headers(), &tampered)
        ),
        F::BadSignature
    );

    // Unsigned context headers next to the token are not a thing the verifier reads.
    let mut h = signed.headers();
    h.insert(
        "x-cf-tenant-id",
        Uuid::new_v4().to_string().parse().unwrap(),
    );
    let ctx = fleet.at_privacy_relay.verify_headers(&h).unwrap();
    assert_ne!(
        ctx.delegated_platform_context()
            .unwrap()
            .tenant_id()
            .to_string(),
        h["x-cf-tenant-id"].to_str().unwrap()
    );
}

/// Replaces every run of 16+ hex digits / dashes (UUIDs, random ids) with `#`, so the
/// forbidden-word scan below sees field names and values, not chance substrings of random
/// identifiers (e.g. `e164` inside a UUID).
fn mask_identifiers(rendered: &str) -> String {
    let mut out = String::with_capacity(rendered.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        if run.len() >= 16 {
            out.push('#');
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for c in rendered.chars() {
        if c.is_ascii_hexdigit() || c == '-' {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    out
}

#[test]
fn identifier_masking_keeps_sensitive_text_visible() {
    let masked = mask_identifiers(
        r#"{"id":"0b1e164a-7f00-4d5e-9c11-aa22bb33cc44","e164":"+2348031234567"}"#,
    );
    assert!(!masked.contains("0b1e164a"));
    assert!(
        masked.contains("\"e164\"") && masked.contains("+234"),
        "{masked}"
    );
}
#[test]
fn k_context_contains_only_safe_identifiers() {
    let fleet = Fleet::new(RuntimeEnvironment::Staging);
    let signed = fleet
        .calling
        .sign(
            &OutboundServiceRequest::new_operation(ServiceId::privacy_relay())
                .with_delegated_context(delegated()),
        )
        .unwrap();
    let claims = decode_segment(token_of(&signed).split('.').nth(1).unwrap());
    let context = claims["delegated_context"].as_object().unwrap();
    let mut keys: Vec<&str> = context.keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "application_environment_id",
            "application_environment_kind",
            "application_id",
            "initiating_principal_id",
            "initiating_principal_kind",
            "initiating_service_account_id",
            "tenant_id"
        ]
    );
    let rendered = mask_identifiers(&serde_json::to_string(&claims).unwrap().to_lowercase());
    for forbidden in [
        "phone",
        "e164",
        "msisdn",
        "secret",
        "password",
        "credential",
        "cfmk_",
        "authorization",
        "cookie",
        "verifier",
        "+234",
        "tel:",
    ] {
        assert!(!rendered.contains(forbidden), "{forbidden}");
    }

    // Forged contexts with extra (sensitive) fields never verify.
    let (rid, cid) = (RequestId::generate(), CorrelationId::generate());
    let mut forged_claims = standard_claims(
        &ServiceId::calling(),
        &ServiceId::privacy_relay(),
        fleet.env,
        fleet.now(),
        fleet.now() + 60,
        &rid,
        &cid,
    );
    let mut ctx = serde_json::to_value(delegated()).unwrap();
    ctx["phone_number"] = json!("+2348012345678");
    forged_claims["delegated_context"] = ctx;
    let token = forge(
        &fleet.calling_key.signing_key(),
        &standard_header(&fleet.calling_key.kid),
        &forged_claims,
    );
    assert_eq!(
        reject(&fleet.at_privacy_relay, &id_headers(&rid, &cid, &token)),
        F::MalformedClaims
    );
}

// ---------------------------------------------------------------- L. request / correlation

#[test]
fn l_id_headers_must_equal_the_signed_claims() {
    let fleet = Fleet::new(RuntimeEnvironment::Staging);
    let signed = sign_for(&fleet.calling, ServiceId::privacy_relay());
    accept(&fleet.at_privacy_relay, &signed);

    let mut h = signed.headers();
    h.insert(
        "x-cf-request-id",
        RequestId::generate().as_str().parse().unwrap(),
    );
    assert_eq!(reject(&fleet.at_privacy_relay, &h), F::RequestIdMismatch);

    let mut h = signed.headers();
    h.insert(
        "x-cf-correlation-id",
        CorrelationId::generate().as_str().parse().unwrap(),
    );
    assert_eq!(
        reject(&fleet.at_privacy_relay, &h),
        F::CorrelationIdMismatch
    );

    let mut h = signed.headers();
    h.remove("x-cf-request-id");
    assert_eq!(reject(&fleet.at_privacy_relay, &h), F::MissingRequestId);
    let mut h = signed.headers();
    h.remove("x-cf-correlation-id");
    assert_eq!(reject(&fleet.at_privacy_relay, &h), F::MissingCorrelationId);
    let mut h = signed.headers();
    h.insert("x-cf-request-id", "client-chosen-id".parse().unwrap());
    assert_eq!(reject(&fleet.at_privacy_relay, &h), F::MalformedRequestId);
    let mut h = signed.headers();
    h.append(
        "x-cf-correlation-id",
        signed.correlation_id().as_str().parse().unwrap(),
    );
    assert_eq!(reject(&fleet.at_privacy_relay, &h), F::DuplicateIdHeader);
}

#[test]
fn l_each_hop_gets_a_new_request_id_and_keeps_the_correlation_id() {
    let fleet = Fleet::new(RuntimeEnvironment::Staging);
    // External request enters Calling: corr_A, req_1 (Calling's own request).
    let corr_a = CorrelationId::generate();
    let req_1 = RequestId::generate();

    // Calling -> Privacy Relay: req_2, corr_A.
    let hop_2 = OutboundServiceRequest::continuing(ServiceId::privacy_relay(), &corr_a)
        .with_delegated_context(delegated());
    let at_relay = accept(
        &fleet.at_privacy_relay,
        &fleet.calling.sign(&hop_2).unwrap(),
    );
    assert_eq!(at_relay.correlation_id(), &corr_a);
    assert_ne!(at_relay.request_id(), &req_1);

    // Privacy Relay -> Calling (callback): req_3, corr_A, same delegated context.
    let hop_3 = OutboundServiceRequest::next_hop_of(&at_relay, ServiceId::calling());
    let at_calling = accept(
        &fleet.at_calling,
        &fleet.privacy_relay.sign(&hop_3).unwrap(),
    );
    assert_eq!(at_calling.correlation_id(), &corr_a);
    assert_ne!(at_calling.request_id(), at_relay.request_id());
    assert_ne!(at_calling.request_id(), &req_1);
    assert_eq!(
        at_calling.delegated_platform_context(),
        at_relay.delegated_platform_context()
    );

    // Telephony -> Privacy Relay starting its own operation: new correlation.
    let independent = accept(
        &fleet.at_privacy_relay,
        &sign_for(&fleet.telephony, ServiceId::privacy_relay()),
    );
    assert_ne!(independent.correlation_id(), &corr_a);
}

// ---------------------------------------------------------------- M. leakage

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn m_tokens_and_keys_never_appear_in_debug_or_errors() {
    let fleet = Fleet::new(RuntimeEnvironment::Staging);
    let signed = sign_for(&fleet.calling, ServiceId::privacy_relay());
    let token = token_of(&signed);
    let signature = token.rsplit('.').next().unwrap().to_string();
    let pem_body = fleet.calling_key.pem.lines().nth(1).unwrap().to_string();

    let ctx = accept(&fleet.at_privacy_relay, &signed);
    let mut wrong = signed.headers();
    wrong.insert(
        "x-cf-request-id",
        RequestId::generate().as_str().parse().unwrap(),
    );
    let error = fleet.at_privacy_relay.verify_headers(&wrong).unwrap_err();
    let presented = cf_service_auth::PresentedServiceRequest {
        service_authorization: &format!("Bearer {token}"),
        request_id: signed.request_id().as_str(),
        correlation_id: signed.correlation_id().as_str(),
    };

    let mut sink = Capture::default();
    for rendered in [
        format!("{signed:?}"),
        format!("{:?}", signed.token()),
        format!("{:?}", signed.headers()),
        format!("{ctx:?}"),
        format!("{error:?} {error}"),
        format!("{presented:?}"),
        format!("{:?}", fleet.calling),
        format!("{:?}", fleet.at_privacy_relay),
        format!("{:?}", fleet.calling_key.signing_key()),
    ] {
        writeln!(sink, "{rendered}").unwrap();
        assert!(
            !rendered.contains(&signature),
            "token signature leaked: {rendered}"
        );
        assert!(!rendered.contains(&token), "token leaked");
        assert!(!rendered.contains(&pem_body), "private key leaked");
    }
    let all = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
    assert!(all.contains("[REDACTED]") || all.contains("Sensitive"));
    // Diagnostics expose only safe, validated values.
    assert_eq!(
        error.diagnostics.kid.as_ref().unwrap().as_str(),
        fleet.calling_key.kid
    );
    assert_eq!(
        error.diagnostics.issuer.as_ref().unwrap(),
        &ServiceId::calling()
    );
}
