//! A. valid interoperability, B. no network trust, C. audience, D. runtime environment,
//! E. lifetime, F. signature / algorithm safety, H. customer/service separation.

mod common;

use cf_service_auth::{
    CorrelationId, OutboundServiceRequest, RequestId, RuntimeEnvironment, ServiceId,
    VerificationFailure as F,
};
use common::*;
use http::HeaderMap;
use serde_json::json;

// ---------------------------------------------------------------- A. valid auth

#[test]
fn a_calling_to_privacy_relay_is_accepted() {
    let fleet = Fleet::new(RuntimeEnvironment::Staging);
    let signed = sign_for(&fleet.calling, ServiceId::privacy_relay());
    let ctx = accept(&fleet.at_privacy_relay, &signed);
    assert_eq!(ctx.authenticated_service(), &ServiceId::calling());
    assert_eq!(ctx.audience(), &ServiceId::privacy_relay());
    assert_eq!(ctx.runtime_environment(), RuntimeEnvironment::Staging);
    assert_eq!(ctx.key_id().as_str(), "calling-staging-1");
    assert_eq!(ctx.request_id(), signed.request_id());
    assert_eq!(ctx.correlation_id(), signed.correlation_id());
    assert_eq!(ctx.expires_at() - ctx.issued_at(), 120);
    assert_eq!(
        ctx.permissions().names(),
        vec!["communication.authorize", "communication-context.read"]
    );
}

#[test]
fn a_privacy_relay_to_calling_and_telephony_to_privacy_relay_are_accepted() {
    let fleet = Fleet::new(RuntimeEnvironment::Production);
    let ctx = accept(
        &fleet.at_calling,
        &sign_for(&fleet.privacy_relay, ServiceId::calling()),
    );
    assert_eq!(ctx.authenticated_service(), &ServiceId::privacy_relay());
    assert_eq!(
        ctx.permissions().names(),
        vec!["operational-callback.deliver"]
    );

    let ctx = accept(
        &fleet.at_privacy_relay,
        &sign_for(&fleet.telephony, ServiceId::privacy_relay()),
    );
    assert_eq!(ctx.authenticated_service(), &ServiceId::telephony());
    assert!(ctx
        .permissions()
        .names()
        .contains(&"protected-contact.resolve-for-execution"));

    let ctx = accept(
        &fleet.at_telephony,
        &sign_for(&fleet.calling, ServiceId::telephony()),
    );
    assert_eq!(
        ctx.permissions().names(),
        vec!["call-leg.execute", "call-leg-status.read"]
    );
}

// ---------------------------------------------------------------- B. no network trust

#[test]
fn b_requests_without_a_service_token_are_rejected() {
    let fleet = Fleet::new(RuntimeEnvironment::Staging);
    assert_eq!(
        reject(&fleet.at_privacy_relay, &HeaderMap::new()),
        F::MissingServiceToken
    );

    // Id headers alone (as a request from "inside the network" would carry) are not
    // authentication.
    let signed = sign_for(&fleet.calling, ServiceId::privacy_relay());
    let mut ids_only = signed.headers();
    ids_only.remove("x-cf-service-authorization");
    assert_eq!(
        reject(&fleet.at_privacy_relay, &ids_only),
        F::MissingServiceToken
    );

    for bad in [
        "",
        "Bearer",
        "Bearer ",
        "Basic abc",
        "bearer  two  spaces",
        "token",
    ] {
        let mut h = signed.headers();
        h.insert("x-cf-service-authorization", bad.parse().unwrap());
        assert_eq!(
            reject(&fleet.at_privacy_relay, &h),
            F::MalformedAuthorization,
            "{bad:?}"
        );
    }
    let mut dup = signed.headers();
    dup.append(
        "x-cf-service-authorization",
        format!("Bearer {}", token_of(&signed)).parse().unwrap(),
    );
    assert_eq!(
        reject(&fleet.at_privacy_relay, &dup),
        F::DuplicateServiceToken
    );
}

// ---------------------------------------------------------------- C. audience

#[test]
fn c_tokens_work_only_for_their_exact_audience() {
    let fleet = Fleet::new(RuntimeEnvironment::Staging);
    // calling -> privacy-relay presented to telephony (calling is trusted there!).
    let for_relay = sign_for(&fleet.calling, ServiceId::privacy_relay());
    assert_eq!(
        reject(&fleet.at_telephony, &for_relay.headers()),
        F::AudienceMismatch
    );
    // calling -> telephony presented to privacy-relay.
    let for_telephony = sign_for(&fleet.calling, ServiceId::telephony());
    assert_eq!(
        reject(&fleet.at_privacy_relay, &for_telephony.headers()),
        F::AudienceMismatch
    );
    // A signer refuses to address itself.
    assert!(fleet
        .calling
        .sign(&OutboundServiceRequest::new_operation(ServiceId::calling()))
        .is_err());
}

#[test]
fn c_wildcard_and_list_audiences_are_rejected() {
    let fleet = Fleet::new(RuntimeEnvironment::Staging);
    let key = common::key("calling-staging-1");
    // Rebuild a fleet-compatible verifier trusting this key for calling.
    let relay = ServiceId::privacy_relay();
    let v = verifier(
        &bundle(
            &relay,
            fleet.env,
            &[Grant {
                issuer: &ServiceId::calling(),
                keys: vec![key.public_jwk()],
                permissions: &["communication.authorize"],
            }],
        ),
        &fleet.clock,
    );
    let (rid, cid) = (RequestId::generate(), CorrelationId::generate());
    for aud in [
        json!("*"),
        json!("cfmeet.*"),
        json!(["cfmeet.privacy-relay"]),
        json!(["cfmeet.privacy-relay", "cfmeet.telephony"]),
    ] {
        let mut claims = standard_claims(
            &ServiceId::calling(),
            &relay,
            fleet.env,
            T0,
            T0 + 60,
            &rid,
            &cid,
        );
        claims["aud"] = aud.clone();
        let token = forge(&key, &standard_header("calling-staging-1"), &claims);
        assert_eq!(
            reject(&v, &id_headers(&rid, &cid, &token)),
            F::MalformedClaims,
            "{aud}"
        );
    }
}

// ---------------------------------------------------------------- D. runtime environment

#[test]
fn d_tokens_never_cross_runtime_environments() {
    let staging = Fleet::new(RuntimeEnvironment::Staging);
    let production = Fleet::new(RuntimeEnvironment::Production);
    // Different environments have different keys: unknown key.
    let s = sign_for(&staging.calling, ServiceId::privacy_relay());
    assert_eq!(
        reject(&production.at_privacy_relay, &s.headers()),
        F::UnknownKey
    );
    let p = sign_for(&production.calling, ServiceId::privacy_relay());
    assert_eq!(
        reject(&staging.at_privacy_relay, &p.headers()),
        F::UnknownKey
    );

    // Even with the SAME key trusted in both, the signed environment must match the
    // receiver's configured environment.
    let key = common::key("shared-misconfigured-key");
    let relay = ServiceId::privacy_relay();
    let prod_verifier = verifier(
        &bundle(
            &relay,
            RuntimeEnvironment::Production,
            &[Grant {
                issuer: &ServiceId::calling(),
                keys: vec![key.public_jwk()],
                permissions: &["communication.authorize"],
            }],
        ),
        &staging.clock,
    );
    let staging_signer = signer(
        &ServiceId::calling(),
        RuntimeEnvironment::Staging,
        key,
        &staging.clock,
    );
    let signed = sign_for(&staging_signer, relay);
    assert_eq!(
        reject(&prod_verifier, &signed.headers()),
        F::RuntimeEnvironmentMismatch
    );
}

// ---------------------------------------------------------------- E. lifetime

#[test]
fn e_expiry_not_before_and_maximum_lifetime() {
    let fleet = Fleet::new(RuntimeEnvironment::Staging);
    let signed = sign_for(&fleet.calling, ServiceId::privacy_relay());
    let headers = signed.headers();

    // Valid while short-lived, including 30 s of skew after expiry.
    fleet.clock.advance(119);
    accept(&fleet.at_privacy_relay, &signed);
    fleet.clock.advance(30);
    accept(&fleet.at_privacy_relay, &signed);
    fleet.clock.advance(1);
    assert_eq!(reject(&fleet.at_privacy_relay, &headers), F::Expired);

    // Not yet valid (beyond skew), and issued in the future.
    let key = common::key("calling-staging-1b");
    let relay = ServiceId::privacy_relay();
    let v = verifier(
        &bundle(
            &relay,
            fleet.env,
            &[Grant {
                issuer: &ServiceId::calling(),
                keys: vec![key.public_jwk()],
                permissions: &["communication.authorize"],
            }],
        ),
        &fleet.clock,
    );
    let now = fleet.now();
    let (rid, cid) = (RequestId::generate(), CorrelationId::generate());
    let mut claims = standard_claims(
        &ServiceId::calling(),
        &relay,
        fleet.env,
        now,
        now + 100,
        &rid,
        &cid,
    );
    claims["nbf"] = json!(now + 31);
    let token = forge(&key, &standard_header("calling-staging-1b"), &claims);
    assert_eq!(reject(&v, &id_headers(&rid, &cid, &token)), F::NotYetValid);

    let claims = standard_claims(
        &ServiceId::calling(),
        &relay,
        fleet.env,
        now + 31,
        now + 100,
        &rid,
        &cid,
    );
    let token = forge(&key, &standard_header("calling-staging-1b"), &claims);
    assert_eq!(
        reject(&v, &id_headers(&rid, &cid, &token)),
        F::IssuedInFuture
    );

    // A lifetime above 300 s is refused even with a perfectly valid signature.
    for lifetime in [301, 3600, 86_400 * 30] {
        let claims = standard_claims(
            &ServiceId::calling(),
            &relay,
            fleet.env,
            now,
            now + lifetime,
            &rid,
            &cid,
        );
        let token = forge(&key, &standard_header("calling-staging-1b"), &claims);
        assert_eq!(
            reject(&v, &id_headers(&rid, &cid, &token)),
            F::LifetimeTooLong,
            "{lifetime}"
        );
    }
    // Exactly 300 s is the maximum.
    let claims = standard_claims(
        &ServiceId::calling(),
        &relay,
        fleet.env,
        now,
        now + 300,
        &rid,
        &cid,
    );
    let token = forge(&key, &standard_header("calling-staging-1b"), &claims);
    assert!(v.verify_headers(&id_headers(&rid, &cid, &token)).is_ok());

    // Incoherent time claims.
    let mut claims = standard_claims(
        &ServiceId::calling(),
        &relay,
        fleet.env,
        now,
        now + 60,
        &rid,
        &cid,
    );
    claims["nbf"] = json!(now - 10);
    let token = forge(&key, &standard_header("calling-staging-1b"), &claims);
    assert_eq!(
        reject(&v, &id_headers(&rid, &cid, &token)),
        F::InvalidTimeClaims
    );

    // Signers cannot be configured beyond the maximum either.
    assert!(cf_service_auth::TokenTtl::seconds(301).is_err());
    assert!(cf_service_auth::TokenTtl::seconds(5).is_err());
    assert!(cf_service_auth::TokenTtl::seconds(300).is_ok());
}

// ---------------------------------------------------------------- F. signature / algorithm

#[test]
fn f_signature_and_algorithm_attacks_are_rejected() {
    let fleet = Fleet::new(RuntimeEnvironment::Staging);
    let signed = sign_for(&fleet.calling, ServiceId::privacy_relay());
    let headers = signed.headers();
    let token = token_of(&signed);
    let (h, p, s) = {
        let mut parts = token.split('.');
        (
            parts.next().unwrap().to_string(),
            parts.next().unwrap().to_string(),
            parts.next().unwrap().to_string(),
        )
    };

    // Wrong key: same issuer name and kid, different private key.
    let impostor = signer(
        &ServiceId::calling(),
        fleet.env,
        common::key("calling-staging-1"),
        &fleet.clock,
    );
    let forged = sign_for(&impostor, ServiceId::privacy_relay());
    assert_eq!(
        reject(&fleet.at_privacy_relay, &forged.headers()),
        F::BadSignature
    );

    // Unknown kid.
    let unknown = signer(
        &ServiceId::calling(),
        fleet.env,
        common::key("calling-staging-9"),
        &fleet.clock,
    );
    assert_eq!(
        reject(
            &fleet.at_privacy_relay,
            &sign_for(&unknown, ServiceId::privacy_relay()).headers()
        ),
        F::UnknownKey
    );

    // Tampered payload: elevate audience/permissions after signing.
    let mut claims = decode_segment(&p);
    claims["exp"] = json!(claims["exp"].as_i64().unwrap() + 3600);
    let tampered = format!("{h}.{}.{s}", b64(&claims));
    assert_eq!(
        reject(&fleet.at_privacy_relay, &with_token(&headers, &tampered)),
        F::BadSignature
    );

    // Tampered signature.
    let mut sig = s.clone();
    sig.replace_range(0..2, if &sig[0..2] == "AA" { "BB" } else { "AA" });
    assert_eq!(
        reject(
            &fleet.at_privacy_relay,
            &with_token(&headers, &format!("{h}.{p}.{sig}"))
        ),
        F::BadSignature
    );

    // alg = none (unsigned), with and without a signature segment.
    let none = b64(&json!({"alg":"none","typ":"cf-service+jwt","kid":"calling-staging-1"}));
    assert_eq!(
        reject(
            &fleet.at_privacy_relay,
            &with_token(&headers, &format!("{none}.{p}.{s}"))
        ),
        F::UnsupportedAlgorithm
    );
    assert_eq!(
        reject(
            &fleet.at_privacy_relay,
            &with_token(&headers, &format!("{none}.{p}."))
        ),
        F::MalformedToken
    );

    // Algorithm confusion: HS256 keyed with the (public) verification key bytes.
    let hs = b64(&json!({"alg":"HS256","typ":"cf-service+jwt","kid":"calling-staging-1"}));
    let message = format!("{hs}.{p}");
    let public_x = fleet.calling_key.jwk.x.clone();
    let hmac = jsonwebtoken::crypto::sign(
        message.as_bytes(),
        &jsonwebtoken::EncodingKey::from_secret(public_x.as_bytes()),
        jsonwebtoken::Algorithm::HS256,
    )
    .unwrap();
    assert_eq!(
        reject(
            &fleet.at_privacy_relay,
            &with_token(&headers, &format!("{message}.{hmac}"))
        ),
        F::UnsupportedAlgorithm
    );
    for alg in ["RS256", "ES256", "PS256", "eddsa"] {
        let other = b64(&json!({"alg": alg, "typ":"cf-service+jwt","kid":"calling-staging-1"}));
        assert_eq!(
            reject(
                &fleet.at_privacy_relay,
                &with_token(&headers, &format!("{other}.{p}.{s}"))
            ),
            F::UnsupportedAlgorithm,
            "{alg}"
        );
    }

    // Header parameters that would let a token pick its own key or rules.
    for extra in [
        json!({"jwk": {"kty":"OKP"}}),
        json!({"jku": "https://evil"}),
        json!({"x5u": "https://evil"}),
        json!({"crit": ["exp"]}),
    ] {
        let mut header = decode_segment(&h);
        header
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert_eq!(
            reject(
                &fleet.at_privacy_relay,
                &with_token(&headers, &format!("{}.{p}.{s}", b64(&header)))
            ),
            F::ForbiddenHeaderParameter
        );
    }
    // Generic JWT type is refused (explicit typing).
    let mut header = decode_segment(&h);
    header["typ"] = json!("JWT");
    assert_eq!(
        reject(
            &fleet.at_privacy_relay,
            &with_token(&headers, &format!("{}.{p}.{s}", b64(&header)))
        ),
        F::UnsupportedTokenType
    );

    // Malformed tokens.
    for bad in ["x", "a.b", "a.b.c.d", "eyJ.!!.x", &"A".repeat(5000)] {
        assert_eq!(
            reject(&fleet.at_privacy_relay, &with_token(&headers, bad)),
            F::MalformedToken,
            "{bad:.20}"
        );
    }

    // A key trusted for one issuer cannot mint tokens claiming another issuer.
    let mut claims = decode_segment(&p);
    claims["iss"] = json!("cfmeet.telephony");
    let calling_key = fleet.calling_key.signing_key();
    let lying = forge(&calling_key, &decode_segment(&h), &claims);
    assert_eq!(
        reject(&fleet.at_privacy_relay, &with_token(&headers, &lying)),
        F::IssuerKeyMismatch
    );

    // Unknown claim version.
    let mut claims = decode_segment(&p);
    claims["ver"] = json!(2);
    let v2 = forge(&calling_key, &decode_segment(&h), &claims);
    assert_eq!(
        reject(&fleet.at_privacy_relay, &with_token(&headers, &v2)),
        F::UnsupportedVersion
    );

    // The untouched token still verifies.
    accept(&fleet.at_privacy_relay, &signed);
}

// ---------------------------------------------------------------- H. separation

#[test]
fn h_customer_credentials_are_never_service_credentials() {
    let fleet = Fleet::new(RuntimeEnvironment::Staging);
    let signed = sign_for(&fleet.calling, ServiceId::privacy_relay());
    let customer = format!("cfmk_live_{}_{}", "a".repeat(24), "b".repeat(64));

    // A customer credential in the service channel.
    assert_eq!(
        reject(
            &fleet.at_privacy_relay,
            &with_token(&signed.headers(), &customer)
        ),
        F::MalformedToken
    );

    // A customer Authorization header next to a valid service token: refused, so customer
    // credentials can never be forwarded to (or accepted by) internal routes.
    let mut both = signed.headers();
    both.insert(
        "authorization",
        format!("Bearer {customer}").parse().unwrap(),
    );
    assert_eq!(
        reject(&fleet.at_privacy_relay, &both),
        F::CustomerCredentialPresent
    );

    // A service token in the customer channel only: no service token present.
    let mut wrong_channel = HeaderMap::new();
    wrong_channel.insert(
        "authorization",
        format!("Bearer {}", token_of(&signed)).parse().unwrap(),
    );
    wrong_channel.insert(
        "x-cf-request-id",
        signed.request_id().as_str().parse().unwrap(),
    );
    wrong_channel.insert(
        "x-cf-correlation-id",
        signed.correlation_id().as_str().parse().unwrap(),
    );
    assert_eq!(
        reject(&fleet.at_privacy_relay, &wrong_channel),
        F::CustomerCredentialPresent
    );

    // Service tokens are not shaped like customer credentials.
    assert!(!token_of(&signed).starts_with("cfmk_"));
}
