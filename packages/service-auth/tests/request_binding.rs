//! SEC-012: request binding (`bnd` / `htm` / `htu`) and the jti replay guard.

mod common;

use std::sync::Arc;

use cf_service_auth::{
    CorrelationId, InMemoryReplayGuard, OutboundServiceRequest, RequestBindingEmission,
    RequestBindingMode, RequestId, RuntimeEnvironment, ServiceId, ServiceTokenSigner,
    ServiceTokenVerifier, SignedServiceRequest, VerificationFailure,
};
use common::*;
use http::{HeaderMap, Method};
use serde_json::json;

const PATH: &str = "/internal/v1/privacy-relay/authorize";

struct Setup {
    calling_key: FleetKey,
    /// Calling's signer with emission ON.
    emitting: ServiceTokenSigner,
    /// Calling's signer as shipped (emission OFF).
    legacy: ServiceTokenSigner,
    relay: ServiceTokenVerifier,
}

fn setup(mode: RequestBindingMode) -> Setup {
    let fleet = Fleet::new(RuntimeEnvironment::Staging);
    let legacy = fleet
        .calling
        .with_request_binding(RequestBindingEmission::Off);
    let emitting = signer(
        &ServiceId::calling(),
        RuntimeEnvironment::Staging,
        fleet.calling_key.signing_key(),
        &fleet.clock,
    )
    .with_request_binding(RequestBindingEmission::Emit);
    Setup {
        relay: fleet
            .at_privacy_relay
            .with_binding_mode(mode)
            .with_replay_guard(Arc::new(InMemoryReplayGuard::new(1_000))),
        calling_key: fleet.calling_key,
        emitting,
        legacy,
    }
}

fn hop() -> OutboundServiceRequest {
    OutboundServiceRequest::new_operation(ServiceId::privacy_relay())
}

fn bound(s: &ServiceTokenSigner, method: Method, path: &str) -> SignedServiceRequest {
    s.sign_request(&hop(), &method, path).expect("signs")
}

async fn verdict(
    v: &ServiceTokenVerifier,
    method: Method,
    path: &str,
    h: &HeaderMap,
) -> Result<(), VerificationFailure> {
    v.verify_request(&method, path, h)
        .await
        .map(|_| ())
        .map_err(|e| e.failure)
}

#[tokio::test]
async fn emitting_signer_binds_method_and_canonical_path() {
    let s = setup(RequestBindingMode::Accept);
    let signed = bound(&s.emitting, Method::POST, "/internal/v1/a%7eb/%2fc");
    let claims = decode_segment(token_of(&signed).split('.').nth(1).unwrap());
    assert_eq!(claims["bnd"], 1);
    assert_eq!(claims["htm"], "POST");
    assert_eq!(claims["htu"], "/internal/v1/a~b/%2Fc");
    // The receiver canonicalizes the path it received the same way.
    let ctx = s
        .relay
        .verify_request(&Method::POST, "/internal/v1/a~b/%2fc", &signed.headers())
        .await
        .unwrap();
    assert!(ctx.is_request_bound());
}

#[tokio::test]
async fn signer_with_emission_off_produces_todays_unbound_tokens() {
    let s = setup(RequestBindingMode::Accept);
    let signed = bound(&s.legacy, Method::POST, PATH);
    let claims = decode_segment(token_of(&signed).split('.').nth(1).unwrap());
    for c in ["bnd", "htm", "htu"] {
        assert!(claims.get(c).is_none(), "{c} emitted while off");
    }
    // Emission off never inspects the path (no new failure mode for today's callers).
    assert!(s
        .legacy
        .sign_request(&hop(), &Method::GET, "not a path")
        .is_ok());
    // Emission on refuses a path it cannot bind.
    assert!(s
        .emitting
        .sign_request(&hop(), &Method::GET, "/internal/v1/../x")
        .is_err());
}

#[tokio::test]
async fn accept_mode_accepts_old_tokens_without_claims() {
    let s = setup(RequestBindingMode::Accept);
    for method in [
        Method::GET,
        Method::POST,
        Method::PUT,
        Method::PATCH,
        Method::DELETE,
    ] {
        // Both the legacy `sign` and `sign_request` with emission off.
        let a = sign_for(&s.legacy, ServiceId::privacy_relay());
        assert_eq!(
            verdict(&s.relay, method.clone(), PATH, &a.headers()).await,
            Ok(())
        );
        let b = bound(&s.legacy, method.clone(), PATH);
        let ctx = s
            .relay
            .verify_request(&method, PATH, &b.headers())
            .await
            .unwrap();
        assert!(!ctx.is_request_bound());
    }
    // verify_headers (no request) still accepts in `accept` mode.
    assert!(s
        .relay
        .verify_headers(&sign_for(&s.legacy, ServiceId::privacy_relay()).headers())
        .is_ok());
}

#[tokio::test]
async fn accept_mode_verifies_claims_when_present() {
    let s = setup(RequestBindingMode::Accept);
    let signed = bound(&s.emitting, Method::POST, PATH);
    assert_eq!(
        verdict(&s.relay, Method::PUT, PATH, &signed.headers()).await,
        Err(VerificationFailure::RequestMethodMismatch)
    );
    assert_eq!(
        verdict(
            &s.relay,
            Method::POST,
            "/internal/v1/privacy-relay/admin",
            &signed.headers()
        )
        .await,
        Err(VerificationFailure::RequestPathMismatch)
    );
    // Trailing slash is significant; a non-canonicalizable received path never matches.
    assert_eq!(
        verdict(
            &s.relay,
            Method::POST,
            &format!("{PATH}/"),
            &signed.headers()
        )
        .await,
        Err(VerificationFailure::RequestPathMismatch)
    );
    assert_eq!(
        verdict(
            &s.relay,
            Method::POST,
            "/internal/v1/x/../privacy-relay/authorize",
            &signed.headers()
        )
        .await,
        Err(VerificationFailure::RequestPathMismatch)
    );
    assert_eq!(
        verdict(&s.relay, Method::POST, PATH, &signed.headers()).await,
        Ok(())
    );
}

#[tokio::test]
async fn require_mode_rejects_missing_or_mismatching_claims() {
    let s = setup(RequestBindingMode::Require);
    let old = sign_for(&s.legacy, ServiceId::privacy_relay());
    assert_eq!(
        verdict(&s.relay, Method::GET, PATH, &old.headers()).await,
        Err(VerificationFailure::RequestBindingMissing)
    );
    let signed = bound(&s.emitting, Method::GET, PATH);
    assert_eq!(
        verdict(&s.relay, Method::DELETE, PATH, &signed.headers()).await,
        Err(VerificationFailure::RequestMethodMismatch)
    );
    assert_eq!(
        verdict(
            &s.relay,
            Method::GET,
            "/internal/v1/other",
            &signed.headers()
        )
        .await,
        Err(VerificationFailure::RequestPathMismatch)
    );
    assert_eq!(
        verdict(&s.relay, Method::GET, PATH, &signed.headers()).await,
        Ok(())
    );
    // Without the request nothing can be compared: refused, never silently accepted.
    assert_eq!(
        reject(&s.relay, &signed.headers()),
        VerificationFailure::RequestBindingUnchecked
    );
}

#[tokio::test]
async fn off_mode_ignores_well_formed_claims_but_not_malformed_ones() {
    let s = setup(RequestBindingMode::Off);
    let signed = bound(&s.emitting, Method::GET, PATH);
    // Mismatch ignored.
    let ctx = s
        .relay
        .verify_request(&Method::POST, "/internal/v1/elsewhere", &signed.headers())
        .await
        .unwrap();
    assert!(!ctx.is_request_bound());

    // Malformed binding groups are refused in every mode.
    let (rid, cid) = (RequestId::generate(), CorrelationId::generate());
    let base = standard_claims(
        &ServiceId::calling(),
        &ServiceId::privacy_relay(),
        RuntimeEnvironment::Staging,
        T0,
        T0 + 120,
        &rid,
        &cid,
    );
    let malformed = [
        json!({"htm": "GET", "htu": PATH}),                    // no bnd
        json!({"bnd": 1, "htm": "GET"}),                       // no htu
        json!({"bnd": 1, "htu": PATH}),                        // no htm
        json!({"bnd": 2, "htm": "GET", "htu": PATH}),          // unknown version
        json!({"bnd": 1, "htm": "get", "htu": PATH}),          // lower-case method
        json!({"bnd": 1, "htm": "GET", "htu": "/a/../b"}),     // non-canonical path
        json!({"bnd": 1, "htm": "GET", "htu": "/a%7e"}),       // non-canonical escape
        json!({"bnd": 1, "htm": "GET", "htu": "/a?q=1"}),      // query
        json!({"bnd": 1, "htm": "GET", "htu": "https://h/a"}), // not a path
    ];
    for mode in [
        RequestBindingMode::Off,
        RequestBindingMode::Accept,
        RequestBindingMode::Require,
    ] {
        let f = setup(mode);
        let (v, k) = (f.relay, f.calling_key);
        let key = k.signing_key();
        for extra in &malformed {
            let mut claims = base.clone();
            claims
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            let token = forge(&key, &standard_header(&k.kid), &claims);
            let h = id_headers(&rid, &cid, &token);
            assert_eq!(
                verdict(&v, Method::GET, PATH, &h).await,
                Err(VerificationFailure::MalformedClaims),
                "{mode} {extra}"
            );
        }
    }
}

#[tokio::test]
async fn replayed_token_is_rejected_on_second_mutating_request() {
    let s = setup(RequestBindingMode::Accept);
    for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
        for signed in [
            sign_for(&s.legacy, ServiceId::privacy_relay()),
            bound(&s.emitting, method.clone(), PATH),
        ] {
            let h = signed.headers();
            assert_eq!(verdict(&s.relay, method.clone(), PATH, &h).await, Ok(()));
            assert_eq!(
                verdict(&s.relay, method.clone(), PATH, &h).await,
                Err(VerificationFailure::Replayed),
                "{method}"
            );
        }
    }
    // A fresh token for the same call (what every retry is) is accepted.
    let again = sign_for(&s.legacy, ServiceId::privacy_relay());
    assert_eq!(
        verdict(&s.relay, Method::POST, PATH, &again.headers()).await,
        Ok(())
    );
}

#[tokio::test]
async fn safe_methods_are_not_replay_checked() {
    let s = setup(RequestBindingMode::Accept);
    let signed = sign_for(&s.legacy, ServiceId::privacy_relay());
    for _ in 0..3 {
        assert_eq!(
            verdict(&s.relay, Method::GET, PATH, &signed.headers()).await,
            Ok(())
        );
        assert_eq!(
            verdict(&s.relay, Method::HEAD, PATH, &signed.headers()).await,
            Ok(())
        );
    }
    // A GET token is still single-use if re-aimed at a mutating method of an unbound
    // route: the first POST records it, the second is a replay.
    assert_eq!(
        verdict(&s.relay, Method::POST, PATH, &signed.headers()).await,
        Ok(())
    );
    assert_eq!(
        verdict(&s.relay, Method::POST, PATH, &signed.headers()).await,
        Err(VerificationFailure::Replayed)
    );
}

#[tokio::test]
async fn replay_guard_can_be_disabled_and_failed_tokens_are_not_recorded() {
    let s = setup(RequestBindingMode::Accept);
    let signed = bound(&s.emitting, Method::POST, PATH);
    // A mismatching attempt does not consume the token.
    assert_eq!(
        verdict(
            &s.relay,
            Method::POST,
            "/internal/v1/other",
            &signed.headers()
        )
        .await,
        Err(VerificationFailure::RequestPathMismatch)
    );
    assert_eq!(
        verdict(&s.relay, Method::POST, PATH, &signed.headers()).await,
        Ok(())
    );

    let unguarded = Fleet::new(RuntimeEnvironment::Staging);
    let v = unguarded.at_privacy_relay.without_replay_guard();
    assert!(!v.has_replay_guard());
    let t = sign_for(&unguarded.calling, ServiceId::privacy_relay());
    assert_eq!(verdict(&v, Method::POST, PATH, &t.headers()).await, Ok(()));
    assert_eq!(verdict(&v, Method::POST, PATH, &t.headers()).await, Ok(()));
}

#[tokio::test]
async fn replay_cache_is_bounded() {
    let fleet = Fleet::new(RuntimeEnvironment::Staging);
    let guard = Arc::new(InMemoryReplayGuard::new(2));
    let v = fleet.at_privacy_relay.with_replay_guard(guard.clone());
    let tokens: Vec<_> = (0..3)
        .map(|_| sign_for(&fleet.calling, ServiceId::privacy_relay()))
        .collect();
    for t in &tokens {
        assert_eq!(verdict(&v, Method::POST, PATH, &t.headers()).await, Ok(()));
    }
    let stats = guard.stats();
    assert_eq!(stats.entries, 2);
    assert_eq!(stats.evicted_live, 1);
    // The two most recent tokens are still single-use.
    for t in &tokens[1..] {
        assert_eq!(
            verdict(&v, Method::POST, PATH, &t.headers()).await,
            Err(VerificationFailure::Replayed)
        );
    }
}

#[tokio::test]
async fn binding_failures_have_distinct_telemetry_codes() {
    let codes = [
        VerificationFailure::RequestBindingMissing,
        VerificationFailure::RequestBindingUnchecked,
        VerificationFailure::RequestMethodMismatch,
        VerificationFailure::RequestPathMismatch,
        VerificationFailure::Replayed,
        VerificationFailure::ReplayStoreUnavailable,
    ]
    .map(|f| f.as_str());
    let unique: std::collections::BTreeSet<_> = codes.iter().collect();
    assert_eq!(unique.len(), codes.len());
}
