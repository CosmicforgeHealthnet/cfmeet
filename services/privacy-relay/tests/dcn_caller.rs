//! Part IV Workstream L: Telephony-only protection of the caller of an inbound call to a
//! Digital Communications Number (`POST /internal/v1/relay-routing/protect-dcn-caller`).
//!
//! The scope comes ONLY from Numbering's signed INBOUND_VOICE grant, verified with
//! Privacy Relay's dedicated routing-grant trust (never its service-auth trust):
//! Telephony asserts nothing. A forged, foreign-issuer, wrong-kind, wrong-audience or
//! expired grant protects nothing; without configured trust the route answers 503.

mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use cf_service_auth::{
    number_grant::{
        GrantScope, GrantTarget, NumberGrantKind, NumberGrantRequest, NumberGrantSigner,
        NumberGrantTrust, NumberGrantVerifier,
    },
    KeyId, RuntimeEnvironment, ServiceId, ServiceSigningKey,
};
use common::{rows, scalar, Harness};
use serde_json::json;
use uuid::Uuid;

const PATH: &str = "/internal/v1/relay-routing/protect-dcn-caller";
const CALLER: &str = "+44 20 7946 0777";

fn trust(keys: &[&ServiceSigningKey]) -> NumberGrantTrust {
    NumberGrantTrust::from_json(
        &json!({
            "bundle_version": 1,
            "token_use": "NUMBER_ROUTING_GRANT",
            "audience": "cfmeet.privacy-relay",
            "runtime_environment": "test",
            "trusted_issuers": [{
                "issuer": "cfmeet.numbering",
                "keys": keys.iter().map(|k| k.public_jwk()).collect::<Vec<_>>(),
                "grant_kinds": ["INBOUND_VOICE"],
            }],
        })
        .to_string(),
        &ServiceId::privacy_relay(),
        RuntimeEnvironment::Test,
    )
    .unwrap()
}

fn signer(issuer: ServiceId, key: ServiceSigningKey) -> NumberGrantSigner {
    NumberGrantSigner::new(
        issuer,
        RuntimeEnvironment::Test,
        key,
        120,
        Arc::new(cf_service_auth::SystemClock),
    )
    .unwrap()
}

fn grant(
    s: &NumberGrantSigner,
    kind: NumberGrantKind,
    audience: ServiceId,
    scope: (Uuid, Uuid, Uuid),
    number: Uuid,
) -> String {
    let outbound = kind.is_outbound();
    s.sign(&NumberGrantRequest {
        kind,
        audience,
        number_resource_id: number,
        assignment_id: Uuid::new_v4(),
        routing_revision: 3,
        scope: GrantScope {
            tenant_id: scope.0,
            application_id: scope.1,
            environment_id: scope.2,
        },
        principal_id: None,
        target: (!outbound).then(|| GrantTarget {
            kind: "PRINCIPAL".into(),
            target_ref: Uuid::new_v4(),
        }),
        operation_ref: outbound.then(|| "leg:op:0001".to_string()),
    })
    .unwrap()
    .expose_for_delivery()
    .to_string()
}

#[tokio::test]
async fn a_dcn_caller_is_protected_only_in_the_scope_numbering_signed() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let key = ServiceSigningKey::generate(KeyId::parse("number-grant-test-1").unwrap());
    let verifier = NumberGrantVerifier::new(trust(&[&key]), Arc::new(cf_service_auth::SystemClock));
    let numbering = signer(ServiceId::numbering(), key);
    h.state = h.state.clone().with_number_grants(Some(verifier));
    let scope = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let number = Uuid::new_v4();
    let token = grant(
        &numbering,
        NumberGrantKind::InboundVoice,
        ServiceId::calling(),
        scope,
        number,
    );
    let r = h
        .telephony(
            PATH,
            json!({ "numberGrant": token, "telephoneNumber": CALLER }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let contact = r.json["inboundCaller"]["contactReference"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(r.json["inboundCaller"]["numberResourceId"], json!(number));
    // The caller's digits, in any formatting, never appear (a random UUID may contain a
    // short digit run, so the check uses the whole subscriber number).
    let digits: String = r.text.chars().filter(char::is_ascii_digit).collect();
    assert!(
        !r.text.contains("20 7946 0777")
            && !r.text.contains("2079460777")
            && !digits.contains("442079460777"),
        "no number in the answer"
    );
    // Stored in the grant's scope (and only there); the same caller again is the same
    // contact.
    assert_eq!(
        scalar(
            h.conn(),
            &format!(
                "SELECT count(*) FROM protected_contacts WHERE id = '{contact}' AND tenant_id = '{}' \
                 AND application_id = '{}' AND environment_id = '{}'",
                scope.0, scope.1, scope.2
            )
        )
        .await,
        1
    );
    let again = h
        .telephony(
            PATH,
            json!({ "numberGrant": token, "telephoneNumber": "+442079460777" }),
        )
        .await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.text);
    assert_eq!(
        again.json["inboundCaller"]["contactReference"],
        json!(contact)
    );
    // The access record names the DCN purpose, never the number.
    let audit = rows(
        h.conn(),
        &format!("SELECT DISTINCT purpose FROM contact_access_audit WHERE contact_reference = '{contact}'"),
    )
    .await;
    assert_eq!(
        audit,
        vec![vec!["DCN_INBOUND_CALLER_PROTECTION".to_string()]]
    );

    // Refusals: another issuer's key, an outbound grant, a grant for another audience,
    // a forged signature, and a request asserting its own scope.
    let rogue = signer(
        ServiceId::numbering(),
        ServiceSigningKey::generate(KeyId::parse("rogue-key-1").unwrap()),
    );
    let foreign = grant(
        &rogue,
        NumberGrantKind::InboundVoice,
        ServiceId::calling(),
        scope,
        number,
    );
    let outbound = grant(
        &numbering,
        NumberGrantKind::CallerIdentity,
        ServiceId::telephony(),
        scope,
        number,
    );
    let wrong_audience = grant(
        &numbering,
        NumberGrantKind::InboundVoice,
        ServiceId::sms_gateway(),
        scope,
        number,
    );
    let mut forged = token.clone();
    forged.push('x');
    for (bad, what) in [
        (foreign, "foreign key"),
        (outbound, "outbound grant"),
        (wrong_audience, "audience"),
        (forged, "forged"),
    ] {
        let r = h
            .telephony(
                PATH,
                json!({ "numberGrant": bad, "telephoneNumber": CALLER }),
            )
            .await;
        assert_eq!(
            (r.status, r.code()),
            (StatusCode::CONFLICT, "RELAY_MAPPING_NOT_ACTIVE"),
            "{what}: {}",
            r.text
        );
    }
    let r = h
        .telephony(
            PATH,
            json!({ "numberGrant": token, "telephoneNumber": CALLER, "tenantId": scope.0 }),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    // Only Telephony holds protected-contact.protect-inbound.
    let calling = h.ids.calling.clone();
    let r = h
        .send_as(
            &calling,
            axum::http::Method::POST,
            PATH,
            None,
            Some(json!({ "numberGrant": token, "telephoneNumber": CALLER })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    assert_eq!(
        scalar(h.conn(), "SELECT count(*) FROM protected_contacts").await,
        1
    );
}

#[tokio::test]
async fn without_routing_grant_trust_dcn_callers_are_refused() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let key = ServiceSigningKey::generate(KeyId::parse("number-grant-test-1").unwrap());
    let token = grant(
        &signer(ServiceId::numbering(), key),
        NumberGrantKind::InboundVoice,
        ServiceId::calling(),
        (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()),
        Uuid::new_v4(),
    );
    let r = h
        .telephony(
            PATH,
            json!({ "numberGrant": token, "telephoneNumber": CALLER }),
        )
        .await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text);
    assert_eq!(
        scalar(h.conn(), "SELECT count(*) FROM protected_contacts").await,
        0
    );
}
