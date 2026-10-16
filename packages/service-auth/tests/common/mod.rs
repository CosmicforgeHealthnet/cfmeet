//! Test fleet: independent service identities (calling, privacy-relay, telephony) with
//! their own keys, signers and receiver-side trust bundles. No network, no deployables.
#![allow(dead_code)]

use std::sync::Arc;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use cf_service_auth::{
    CorrelationId, InternalServiceContext, KeyId, ManualClock, OutboundServiceRequest, PublicJwk,
    RequestId, RuntimeEnvironment, ServiceId, ServiceSigningKey, ServiceTokenSigner,
    ServiceTokenVerifier, SignedServiceRequest, TokenTtl, TrustPolicy, VerificationError,
    VerificationFailure,
};
use ed25519_dalek::pkcs8::{DecodePrivateKey, EncodePrivateKey};
use http::HeaderMap;
use serde_json::{json, Value};

/// 2031-03-01T10:00:00Z.
pub const T0: i64 = 1_930_212_000;

pub fn clock() -> ManualClock {
    ManualClock::new(T0)
}

pub fn key(kid: &str) -> ServiceSigningKey {
    ServiceSigningKey::generate(KeyId::parse(kid).unwrap())
}

pub fn signer(
    service: &ServiceId,
    env: RuntimeEnvironment,
    key: ServiceSigningKey,
    clock: &ManualClock,
) -> ServiceTokenSigner {
    ServiceTokenSigner::new(
        service.clone(),
        env,
        key,
        TokenTtl::default(),
        Arc::new(clock.clone()),
    )
}

/// One grant entry of a trust bundle.
pub struct Grant<'a> {
    pub issuer: &'a ServiceId,
    pub keys: Vec<PublicJwk>,
    pub permissions: &'a [&'a str],
}

pub fn bundle(receiver: &ServiceId, env: RuntimeEnvironment, grants: &[Grant<'_>]) -> Value {
    json!({
        "bundle_version": 1,
        "receiver": receiver,
        "runtime_environment": env,
        "trusted_issuers": grants.iter().map(|g| json!({
            "issuer": g.issuer,
            "audience": receiver,
            "runtime_environment": env,
            "keys": g.keys,
            "permissions": g.permissions,
        })).collect::<Vec<_>>(),
    })
}

pub fn verifier(bundle: &Value, clock: &ManualClock) -> ServiceTokenVerifier {
    let receiver = ServiceId::parse(bundle["receiver"].as_str().unwrap()).unwrap();
    let env = RuntimeEnvironment::parse(bundle["runtime_environment"].as_str().unwrap()).unwrap();
    let policy = TrustPolicy::from_json(&bundle.to_string(), &receiver, env).expect("valid bundle");
    ServiceTokenVerifier::new(policy, Arc::new(clock.clone()))
}

/// Key material of one fleet identity, kept so tests can forge tokens with the REAL key.
pub struct FleetKey {
    pub kid: String,
    pub pem: String,
    pub jwk: PublicJwk,
}

impl FleetKey {
    fn of(key: &ServiceSigningKey) -> Self {
        Self {
            kid: key.kid().as_str().to_string(),
            pem: key.to_pkcs8_pem().to_string(),
            jwk: key.public_jwk(),
        }
    }

    pub fn signing_key(&self) -> ServiceSigningKey {
        ServiceSigningKey::from_pkcs8_pem(KeyId::parse(&self.kid).unwrap(), &self.pem).unwrap()
    }
}

/// The three-service fleet in one runtime environment, with the expected production-like
/// policies:
/// * Privacy Relay trusts Calling (authorize, context read) and Telephony (context read,
///   protected-contact resolution);
/// * Telephony trusts Calling (telephony-leg originate / status read);
/// * Calling trusts Privacy Relay and Telephony (operational callbacks).
pub struct Fleet {
    pub env: RuntimeEnvironment,
    pub calling_key: FleetKey,
    pub privacy_relay_key: FleetKey,
    pub telephony_key: FleetKey,
    pub clock: ManualClock,
    pub calling: ServiceTokenSigner,
    pub privacy_relay: ServiceTokenSigner,
    pub telephony: ServiceTokenSigner,
    pub at_calling: ServiceTokenVerifier,
    pub at_privacy_relay: ServiceTokenVerifier,
    pub at_telephony: ServiceTokenVerifier,
}

impl Fleet {
    pub fn new(env: RuntimeEnvironment) -> Self {
        let clock = clock();
        let suffix = env.as_str();
        let (c, p, t) = (
            key(&format!("calling-{suffix}-1")),
            key(&format!("privacy-relay-{suffix}-1")),
            key(&format!("telephony-{suffix}-1")),
        );
        let (cj, pj, tj) = (c.public_jwk(), p.public_jwk(), t.public_jwk());
        let (calling_key, privacy_relay_key, telephony_key) =
            (FleetKey::of(&c), FleetKey::of(&p), FleetKey::of(&t));
        let (calling_id, relay_id, telephony_id) = (
            ServiceId::calling(),
            ServiceId::privacy_relay(),
            ServiceId::telephony(),
        );
        let at_privacy_relay = verifier(
            &bundle(
                &relay_id,
                env,
                &[
                    Grant {
                        issuer: &calling_id,
                        keys: vec![cj.clone()],
                        permissions: &["communication.authorize", "communication-context.read"],
                    },
                    Grant {
                        issuer: &telephony_id,
                        keys: vec![tj.clone()],
                        permissions: &[
                            "communication-context.read",
                            "protected-contact.resolve-for-execution",
                        ],
                    },
                ],
            ),
            &clock,
        );
        let at_telephony = verifier(
            &bundle(
                &telephony_id,
                env,
                &[Grant {
                    issuer: &calling_id,
                    keys: vec![cj.clone()],
                    permissions: &["telephony-leg.originate", "telephony-status.read"],
                }],
            ),
            &clock,
        );
        let at_calling = verifier(
            &bundle(
                &calling_id,
                env,
                &[
                    Grant {
                        issuer: &relay_id,
                        keys: vec![pj],
                        permissions: &["operational-callback.deliver"],
                    },
                    Grant {
                        issuer: &telephony_id,
                        keys: vec![tj],
                        permissions: &["operational-callback.deliver"],
                    },
                ],
            ),
            &clock,
        );
        Self {
            env,
            calling_key,
            privacy_relay_key,
            telephony_key,
            calling: signer(&calling_id, env, c, &clock),
            privacy_relay: signer(&relay_id, env, p, &clock),
            telephony: signer(&telephony_id, env, t, &clock),
            clock,
            at_calling,
            at_privacy_relay,
            at_telephony,
        }
    }
}

impl Fleet {
    pub fn now(&self) -> i64 {
        use cf_service_auth::Clock;
        self.clock.now_unix()
    }
}

pub fn sign_for(signer: &ServiceTokenSigner, audience: ServiceId) -> SignedServiceRequest {
    signer
        .sign(&OutboundServiceRequest::new_operation(audience))
        .expect("signs")
}

pub fn accept(
    verifier: &ServiceTokenVerifier,
    signed: &SignedServiceRequest,
) -> InternalServiceContext {
    verifier
        .verify_headers(&signed.headers())
        .expect("accepted")
}

pub fn reject(verifier: &ServiceTokenVerifier, headers: &HeaderMap) -> VerificationFailure {
    match verifier.verify_headers(headers) {
        Err(VerificationError { failure, .. }) => failure,
        Ok(ctx) => panic!("unexpectedly accepted: {ctx:?}"),
    }
}

/// Replaces the token in `headers` (keeps the id headers).
pub fn with_token(headers: &HeaderMap, token: &str) -> HeaderMap {
    let mut h = headers.clone();
    h.insert(
        "x-cf-service-authorization",
        format!("Bearer {token}").parse().unwrap(),
    );
    h
}

pub fn token_of(signed: &SignedServiceRequest) -> String {
    signed.token().expose_for_transport().to_string()
}

pub fn b64(value: &Value) -> String {
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(value).unwrap())
}

pub fn decode_segment(segment: &str) -> Value {
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(segment).unwrap()).unwrap()
}

/// Signs arbitrary header/claims with `key` (to forge tokens a correct signer would never
/// produce: long lifetimes, extra claims, wrong audience, …).
pub fn forge(key: &ServiceSigningKey, header: &Value, claims: &Value) -> String {
    let signing = ed25519_dalek::SigningKey::from_pkcs8_pem(&key.to_pkcs8_pem()).unwrap();
    let encoding =
        jsonwebtoken::EncodingKey::from_ed_der(signing.to_pkcs8_der().unwrap().as_bytes());
    let message = format!("{}.{}", b64(header), b64(claims));
    let signature = jsonwebtoken::crypto::sign(
        message.as_bytes(),
        &encoding,
        jsonwebtoken::Algorithm::EdDSA,
    )
    .unwrap();
    format!("{message}.{signature}")
}

pub fn standard_header(kid: &str) -> Value {
    json!({ "alg": "EdDSA", "typ": "cf-service+jwt", "kid": kid })
}

pub fn standard_claims(
    iss: &ServiceId,
    aud: &ServiceId,
    env: RuntimeEnvironment,
    iat: i64,
    exp: i64,
    request_id: &RequestId,
    correlation_id: &CorrelationId,
) -> Value {
    json!({
        "ver": 1,
        "iss": iss,
        "aud": aud,
        "runtime_environment": env,
        "iat": iat,
        "nbf": iat,
        "exp": exp,
        "jti": "0123456789abcdef0123456789abcdef",
        "request_id": request_id,
        "correlation_id": correlation_id,
    })
}

pub fn id_headers(
    request_id: &RequestId,
    correlation_id: &CorrelationId,
    token: &str,
) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(
        "x-cf-service-authorization",
        format!("Bearer {token}").parse().unwrap(),
    );
    h.insert("x-cf-request-id", request_id.as_str().parse().unwrap());
    h.insert(
        "x-cf-correlation-id",
        correlation_id.as_str().parse().unwrap(),
    );
    h
}
