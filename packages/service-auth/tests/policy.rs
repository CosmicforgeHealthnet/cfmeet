//! G. key rotation, I. receiver-authoritative permissions, J. protected-contact
//! permission, N. trust-bundle validation.

mod common;

use cf_service_auth::{
    trust::TrustBundleError as E, CorrelationId, InternalPermission, RequestId, RuntimeEnvironment,
    ServiceId, TrustPolicy, VerificationFailure as F,
};
use common::*;
use serde_json::{json, Value};

fn relay() -> ServiceId {
    ServiceId::privacy_relay()
}

fn load(bundle: &Value) -> Result<TrustPolicy, E> {
    TrustPolicy::from_json(&bundle.to_string(), &relay(), RuntimeEnvironment::Staging)
}

fn base_bundle() -> Value {
    let calling = ServiceId::calling();
    let telephony = ServiceId::telephony();
    bundle(
        &relay(),
        RuntimeEnvironment::Staging,
        &[
            Grant {
                issuer: &calling,
                keys: vec![key("calling-a").public_jwk()],
                permissions: &["communication.authorize"],
            },
            Grant {
                issuer: &telephony,
                keys: vec![key("telephony-a").public_jwk()],
                permissions: &["protected-contact.resolve-for-execution"],
            },
        ],
    )
}

// ---------------------------------------------------------------- G. rotation

#[test]
fn g_key_rotation_overlap_without_downtime() {
    let clock = clock();
    let env = RuntimeEnvironment::Staging;
    let calling = ServiceId::calling();
    let old = key("calling-staging-2026-09");
    let new = key("calling-staging-2026-10");
    let (old_jwk, new_jwk) = (old.public_jwk(), new.public_jwk());
    let old_pem = old.to_pkcs8_pem().to_string();
    let grant = |keys: Vec<cf_service_auth::PublicJwk>| {
        verifier(
            &bundle(
                &relay(),
                env,
                &[Grant {
                    issuer: &calling,
                    keys,
                    permissions: &["communication.authorize"],
                }],
            ),
            &clock,
        )
    };

    // 1. Receiver trusts OLD; sender signs with OLD.
    let old_signer = signer(&calling, env, old, &clock);
    let receiver = grant(vec![old_jwk.clone()]);
    let old_token = sign_for(&old_signer, relay());
    accept(&receiver, &old_token);

    // 2. Receiver trusts OLD + NEW: both accepted.
    let receiver = grant(vec![old_jwk.clone(), new_jwk.clone()]);
    accept(&receiver, &old_token);
    let new_signer = signer(&calling, env, new, &clock);
    let new_token = sign_for(&new_signer, relay());
    assert_eq!(
        accept(&receiver, &new_token).key_id().as_str(),
        "calling-staging-2026-10"
    );

    // 3. Sender switches to NEW; tokens minted with OLD before the switch keep working
    //    until they expire.
    clock.advance(100);
    accept(&receiver, &old_token);
    accept(&receiver, &sign_for(&new_signer, relay()));

    // 4. After max TTL + skew the OLD key is removed: OLD rejected, NEW accepted.
    clock.advance(301 + 30);
    let receiver = grant(vec![new_jwk]);
    let late_old = sign_for(
        &signer(
            &calling,
            env,
            cf_service_auth::ServiceSigningKey::from_pkcs8_pem(
                cf_service_auth::KeyId::parse("calling-staging-2026-09").unwrap(),
                &old_pem,
            )
            .unwrap(),
            &clock,
        ),
        relay(),
    );
    assert_eq!(reject(&receiver, &late_old.headers()), F::UnknownKey);
    accept(&receiver, &sign_for(&new_signer, relay()));
}

// ---------------------------------------------------------------- I. permissions

#[test]
fn i_permissions_come_only_from_the_receiver_policy() {
    let fleet = Fleet::new(RuntimeEnvironment::Staging);
    let ctx = accept(&fleet.at_privacy_relay, &sign_for(&fleet.calling, relay()));
    assert!(ctx
        .require_permission(InternalPermission::CommunicationAuthorize)
        .is_ok());
    assert!(ctx
        .require_permission(InternalPermission::CommunicationContextRead)
        .is_ok());
    let denied = ctx
        .require_permission(InternalPermission::ProtectedContactResolveForExecution)
        .unwrap_err();
    assert_eq!(denied.service, ServiceId::calling());
    assert!(ctx
        .require_permission(InternalPermission::CallLegExecute)
        .is_err());

    // A caller self-declaring permissions in its own (validly signed) token gains nothing:
    // the closed claim set refuses the token outright.
    let (rid, cid) = (RequestId::generate(), CorrelationId::generate());
    let key = fleet.calling_key.signing_key();
    for extra in [
        json!({"permissions": ["protected-contact.resolve-for-execution"]}),
        json!({"scope": "protected-contact.resolve-for-execution"}),
        json!({"scp": ["call-leg.execute"]}),
        json!({"roles": ["admin"]}),
    ] {
        let mut claims = standard_claims(
            &ServiceId::calling(),
            &relay(),
            fleet.env,
            fleet.now(),
            fleet.now() + 60,
            &rid,
            &cid,
        );
        claims
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let token = forge(&key, &standard_header(&fleet.calling_key.kid), &claims);
        assert_eq!(
            reject(&fleet.at_privacy_relay, &id_headers(&rid, &cid, &token)),
            F::MalformedClaims,
            "{extra}"
        );
    }
    // The signer has no way to request permissions at all: OutboundServiceRequest carries
    // only audience, ids and delegated context (compile-time property, documented here).
}

#[test]
fn i_wildcard_and_super_permissions_are_rejected_as_configuration() {
    for value in ["*", "admin", "all", "root", "communication.*", "admin.all"] {
        let mut b = base_bundle();
        b["trusted_issuers"][0]["permissions"] = json!([value]);
        assert!(
            matches!(load(&b), Err(E::WildcardPermission { .. })),
            "{value}"
        );
    }
    let mut b = base_bundle();
    b["trusted_issuers"][0]["permissions"] = json!(["communication.administer"]);
    assert!(matches!(load(&b), Err(E::UnknownPermission { .. })));
}

// ---------------------------------------------------------------- J. protected contact

#[test]
fn j_protected_contact_resolution_is_telephony_only() {
    let fleet = Fleet::new(RuntimeEnvironment::Production);
    let telephony = accept(
        &fleet.at_privacy_relay,
        &sign_for(&fleet.telephony, relay()),
    );
    assert!(telephony
        .require_permission(InternalPermission::ProtectedContactResolveForExecution)
        .is_ok());
    let calling = accept(&fleet.at_privacy_relay, &sign_for(&fleet.calling, relay()));
    assert!(calling
        .require_permission(InternalPermission::ProtectedContactResolveForExecution)
        .is_err());

    // Configuration granting Calling plaintext-contact resolution is refused at startup.
    let mut b = base_bundle();
    b["trusted_issuers"][0]["permissions"] = json!([
        "communication.authorize",
        "protected-contact.resolve-for-execution"
    ]);
    match load(&b) {
        Err(E::ForbiddenGrant { permission, issuer }) => {
            assert_eq!(
                permission,
                InternalPermission::ProtectedContactResolveForExecution
            );
            assert_eq!(issuer, ServiceId::calling());
        }
        other => panic!("{other:?}"),
    }
    // Nor can any other service grant it (it is received only by Privacy Relay).
    let calling_id = ServiceId::calling();
    let at_calling = bundle(
        &calling_id,
        RuntimeEnvironment::Staging,
        &[Grant {
            issuer: &ServiceId::telephony(),
            keys: vec![key("t").public_jwk()],
            permissions: &["protected-contact.resolve-for-execution"],
        }],
    );
    assert!(matches!(
        TrustPolicy::from_json(
            &at_calling.to_string(),
            &calling_id,
            RuntimeEnvironment::Staging
        ),
        Err(E::PermissionNotReceivedHere { .. })
    ));
}

// ---------------------------------------------------------------- N. trust bundle

#[test]
fn n_trust_bundle_validation() {
    assert!(load(&base_bundle()).is_ok());

    // Duplicate key id (even across issuers).
    let mut b = base_bundle();
    b["trusted_issuers"][1]["keys"][0]["kid"] = json!("calling-a");
    assert!(matches!(load(&b), Err(E::DuplicateKeyId(_))));
    let mut b = base_bundle();
    let k = b["trusted_issuers"][0]["keys"][0].clone();
    b["trusted_issuers"][0]["keys"] = json!([k.clone(), k]);
    assert!(matches!(load(&b), Err(E::DuplicateKeyId(_))));

    // Duplicate peer.
    let mut b = base_bundle();
    let mut dup = b["trusted_issuers"][0].clone();
    dup["keys"][0] = serde_json::to_value(key("calling-b").public_jwk()).unwrap();
    b["trusted_issuers"].as_array_mut().unwrap().push(dup);
    assert!(matches!(load(&b), Err(E::DuplicateIssuer(_))));

    // Wildcard audience / receiver / issuer.
    let mut b = base_bundle();
    b["trusted_issuers"][0]["audience"] = json!("*");
    assert!(matches!(
        load(&b),
        Err(E::InvalidServiceId {
            field: "audience",
            ..
        })
    ));
    let mut b = base_bundle();
    b["trusted_issuers"][0]["audience"] = json!("cfmeet.telephony");
    assert!(matches!(load(&b), Err(E::AudienceMismatch { .. })));
    let mut b = base_bundle();
    b["receiver"] = json!("cfmeet.*");
    assert!(matches!(
        load(&b),
        Err(E::InvalidServiceId {
            field: "receiver",
            ..
        })
    ));
    let mut b = base_bundle();
    b["trusted_issuers"][0]["issuer"] = json!("*");
    assert!(matches!(
        load(&b),
        Err(E::InvalidServiceId {
            field: "issuer",
            ..
        })
    ));

    // Wrong local environment / receiver.
    assert!(matches!(
        TrustPolicy::from_json(
            &base_bundle().to_string(),
            &relay(),
            RuntimeEnvironment::Production
        ),
        Err(E::RuntimeEnvironmentMismatch { .. })
    ));
    assert!(matches!(
        TrustPolicy::from_json(
            &base_bundle().to_string(),
            &ServiceId::telephony(),
            RuntimeEnvironment::Staging
        ),
        Err(E::ReceiverMismatch { .. })
    ));
    let mut b = base_bundle();
    b["trusted_issuers"][0]["runtime_environment"] = json!("production");
    assert!(matches!(
        load(&b),
        Err(E::GrantRuntimeEnvironmentMismatch { .. })
    ));

    // Invalid public keys.
    for (x, kty, crv) in [
        ("AAAA", "OKP", "Ed25519"),
        (
            "not-base64-at-all-but-43-characters-long!!",
            "OKP",
            "Ed25519",
        ),
        (
            &*{
                let mut identity = [0u8; 32];
                identity[0] = 1;
                base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, identity)
            },
            "OKP",
            "Ed25519",
        ),
        (
            "9QB95kkNvMe6SsLIvkxfFr1v7B_COHnQR0B0gRct7sU",
            "oct",
            "Ed25519",
        ),
        (
            "9QB95kkNvMe6SsLIvkxfFr1v7B_COHnQR0B0gRct7sU",
            "OKP",
            "X25519",
        ),
    ] {
        let mut b = base_bundle();
        b["trusted_issuers"][0]["keys"][0]["x"] = json!(x);
        b["trusted_issuers"][0]["keys"][0]["kty"] = json!(kty);
        b["trusted_issuers"][0]["keys"][0]["crv"] = json!(crv);
        assert!(
            matches!(
                load(&b),
                Err(E::InvalidPublicKey(_) | E::UnsupportedKeyType { .. })
            ),
            "{x} {kty} {crv}"
        );
    }

    // Empty permissions / empty permission / no keys / self trust.
    let mut b = base_bundle();
    b["trusted_issuers"][0]["permissions"] = json!([]);
    assert!(matches!(load(&b), Err(E::EmptyPermissions(_))));
    let mut b = base_bundle();
    b["trusted_issuers"][0]["permissions"] = json!([""]);
    assert!(matches!(load(&b), Err(E::EmptyPermission { .. })));
    let mut b = base_bundle();
    b["trusted_issuers"][0]["keys"] = json!([]);
    assert!(matches!(load(&b), Err(E::NoKeys(_))));
    let mut b = base_bundle();
    b["trusted_issuers"][0]["issuer"] = json!("cfmeet.privacy-relay");
    assert!(matches!(load(&b), Err(E::SelfTrust(_))));
    let mut b = base_bundle();
    b["trusted_issuers"][0]["permissions"] =
        json!(["communication.authorize", "communication.authorize"]);
    assert!(matches!(load(&b), Err(E::DuplicatePermission { .. })));

    // Unknown fields and versions.
    let mut b = base_bundle();
    b["trusted_issuers"][0]["allow_all"] = json!(true);
    assert!(matches!(load(&b), Err(E::Malformed(_))));
    let mut b = base_bundle();
    b["bundle_version"] = json!(2);
    assert!(matches!(load(&b), Err(E::UnsupportedVersion(2))));
    assert!(matches!(
        load(&json!("not an object")),
        Err(E::Malformed(_))
    ));
}

#[test]
fn n_missing_bundle_file_fails_closed() {
    assert!(matches!(
        TrustPolicy::from_file(
            std::path::Path::new("/nonexistent/bundle.json"),
            &relay(),
            RuntimeEnvironment::Staging
        ),
        Err(E::Unreadable { .. })
    ));
}
