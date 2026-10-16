//! Part D §33: contact verification (trusted assertion, read-time expiry, BLOCKED /
//! INVALID), the public-network verification guard, Contact Resolution Grants (Calling)
//! and Telephony-only plaintext resolution — every attempt audited, never the number.

mod common;

use axum::http::{Method, StatusCode};
use cf_service_auth::{ApplicationEnvironmentKind, RuntimeEnvironment, ServiceId, TrustPolicy};
use chrono::Duration;
use common::{at, iso, rows, scalar, Consumer, Harness};
use serde_json::{json, Value};
use uuid::Uuid;

const NUMBER: &str = "+44 20 7946 0123";
const E164: &str = "+442079460123";

fn consumer() -> Consumer {
    Consumer::new(ApplicationEnvironmentKind::Production)
}

/// Principal A ↔ protected contact B, PUBLIC_RELAY allowed, an active session.
struct Setup {
    contact: String,
    context: String,
    session: String,
    principal: Uuid,
}

async fn setup(h: &Harness, c: &Consumer, verify: bool) -> Setup {
    let principal = Uuid::new_v4();
    let contact = h.create_contact(c, NUMBER).await["contactReference"]
        .as_str()
        .unwrap()
        .to_string();
    if verify {
        h.verify_contact(c, &contact, "VERIFIED", Some(iso(at(50))))
            .await;
    }
    let mut req = common::context_request(&[principal], "BIDIRECTIONAL", &[]);
    req["participants"] = json!([{ "principalId": principal }, { "contactReferenceId": contact }]);
    req["allowedRelayStrategies"] = json!(["APP_ONLY", "PUBLIC_RELAY_ALLOWED"]);
    let ctx = h.create_context(c, req).await;
    let context = ctx["contextId"].as_str().unwrap().to_string();
    let session = h
        .create_session(
            c,
            &context,
            json!({ "relayStrategy": "PUBLIC_RELAY_ALLOWED" }),
        )
        .await;
    Setup {
        contact,
        context,
        session: session["relaySessionId"].as_str().unwrap().to_string(),
        principal,
    }
}

async fn authorize_public(h: &Harness, c: &Consumer, s: &Setup) -> Value {
    let r = h
        .post(
            "/internal/v1/communication/authorize",
            c,
            json!({
                "relaySessionId": s.session,
                "source": { "principalId": s.principal },
                "targets": [{ "contactReferenceId": s.contact }],
                "channel": "VOICE",
                "route": "PUBLIC_RELAY",
                "purpose": "CALL_CREATE",
            }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    r.json.clone()
}

async fn grant(
    h: &Harness,
    c: &Consumer,
    s: &Setup,
    decision: &str,
    call: Uuid,
    leg: Uuid,
) -> common::Response {
    h.post(
        &format!("/internal/v1/contacts/{}/resolution-grants", s.contact),
        c,
        json!({ "relaySessionId": s.session, "decisionId": decision, "callId": call, "callLegId": leg, "purpose": "PUBLIC_NETWORK_CALL_LEG" }),
    )
    .await
}

async fn resolve_as(
    h: &Harness,
    signer: &cf_service_auth::ServiceTokenSigner,
    c: &Consumer,
    contact: &str,
    g: &Value,
    attempt: Uuid,
) -> common::Response {
    h.send_as(
        signer,
        Method::POST,
        &format!("/internal/v1/contacts/{contact}/resolve-for-execution"),
        Some(c),
        Some(json!({
            "grantId": g["grantId"], "grantSecret": g["grantSecret"],
            "callId": g["callId"], "callLegId": g["callLegId"], "executionAttemptId": attempt,
        })),
    )
    .await
}

#[tokio::test]
async fn only_a_trusted_verifier_asserts_verification_with_bounded_evidence() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = consumer();
    let contact = h.create_contact(&c, NUMBER).await;
    assert_eq!(contact["verificationState"], "UNVERIFIED");
    let id = contact["contactReference"].as_str().unwrap();
    let path = format!("/v1/privacy-relay/contacts/{id}/verification");
    let body = json!({ "expectedVersion": 1, "targetState": "VERIFIED", "trustSource": "OTP_PROVIDER", "evidenceReference": "otp-7731" });
    // A writer (privacy.contacts.write) cannot self-verify.
    let r = h
        .contact_call(Method::POST, &path, Some(&h.writer(&c)), Some(body.clone()))
        .await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::FORBIDDEN, "CONTACT_SCOPE_DENIED")
    );
    // Invalid assertions.
    let verifier = h.verifier_token(&c);
    for (bad, what) in [
        (
            json!({ "expectedVersion": 1, "targetState": "EXPIRED", "trustSource": "OTP_PROVIDER" }),
            "EXPIRED is derived",
        ),
        (
            json!({ "expectedVersion": 1, "targetState": "VERIFIED", "trustSource": "otp provider" }),
            "trust source",
        ),
        (
            json!({ "expectedVersion": 1, "targetState": "VERIFIED", "trustSource": "OTP", "evidenceReference": "+442079460123" }),
            "number as evidence",
        ),
        (
            json!({ "expectedVersion": 1, "targetState": "VERIFIED", "trustSource": "OTP", "evidence": { "anything": true } }),
            "arbitrary JSON",
        ),
        (
            json!({ "expectedVersion": 1, "targetState": "VERIFIED", "trustSource": "OTP", "validUntil": iso(at(-1)) }),
            "past validity",
        ),
        (
            json!({ "expectedVersion": 1, "targetState": "BLOCKED", "trustSource": "OTP", "validUntil": iso(at(10)) }),
            "validity on BLOCKED",
        ),
    ] {
        let r = h
            .contact_call(Method::POST, &path, Some(&verifier), Some(bad))
            .await;
        assert_eq!(r.code(), "VALIDATION_ERROR", "{what}: {}", r.text);
    }
    let r = h
        .contact_call(
            Method::POST,
            &path,
            Some(&verifier),
            Some(json!({ "expectedVersion": 9, "targetState": "PENDING", "trustSource": "OTP" })),
        )
        .await;
    assert_eq!(r.code(), "CONTACT_VERSION_CONFLICT");
    // The trusted assertion.
    let r = h
        .contact_call(Method::POST, &path, Some(&verifier), Some(body))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        (
            r.json["contact"]["verificationState"].as_str(),
            r.json["contact"]["version"].as_i64()
        ),
        (Some("VERIFIED"), Some(2))
    );
    let events = rows(
        h.conn(),
        &format!("SELECT from_state, to_state, trust_source, coalesce(evidence_reference, ''), policy_version, actor_kind FROM contact_verification_events WHERE contact_reference = '{id}'"),
    )
    .await;
    assert_eq!(
        events,
        vec![vec![
            "UNVERIFIED",
            "VERIFIED",
            "OTP_PROVIDER",
            "otp-7731",
            "CONTACT_VERIFICATION_V1",
            "DELEGATED_CLIENT"
        ]
        .into_iter()
        .map(String::from)
        .collect::<Vec<_>>()]
    );
    assert!(
        common::exec(h.conn(), "DELETE FROM contact_verification_events")
            .await
            .is_err()
    );
    assert!(common::exec(
        h.conn(),
        "UPDATE contact_verification_events SET to_state = 'BLOCKED'"
    )
    .await
    .is_err());
    // Changing the number resets it.
    let r = h
        .contact_call(
            Method::PATCH,
            &format!("/v1/privacy-relay/contacts/{id}"),
            Some(&h.writer(&c)),
            Some(json!({ "expectedVersion": 2, "telephoneNumber": "+44 20 7946 0124" })),
        )
        .await;
    assert_eq!(r.json["contact"]["verificationState"], "UNVERIFIED");
    // While the vault is disabled only restrictive assertions are accepted.
    h.contact_feature.set(false);
    let r = h
        .contact_call(
            Method::POST,
            &path,
            Some(&verifier),
            Some(json!({ "expectedVersion": 3, "targetState": "VERIFIED", "trustSource": "OTP" })),
        )
        .await;
    assert_eq!(r.code(), "CONTACT_VAULT_DISABLED");
    let r = h.contact_call(Method::POST, &path, Some(&verifier), Some(json!({ "expectedVersion": 3, "targetState": "BLOCKED", "trustSource": "FRAUD_REVIEW" }))).await;
    assert_eq!(
        r.json["contact"]["verificationState"], "BLOCKED",
        "{}",
        r.text
    );
    // Two successful assertions; every refused one that reached the vault is audited too
    // (an unparseable body never reaches it).
    assert_eq!(scalar(h.conn(), &format!("SELECT count(*) FROM contact_access_audit WHERE contact_reference = '{id}' AND operation = 'VERIFY' AND outcome = 'SUCCEEDED'")).await, 2);
    assert_eq!(scalar(h.conn(), &format!("SELECT count(*) FROM contact_access_audit WHERE contact_reference = '{id}' AND operation = 'VERIFY' AND outcome = 'DENIED'")).await, 7);
}

#[tokio::test]
async fn verification_expires_at_read_time_and_blocked_or_invalid_never_authorize() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = consumer();
    let s = setup(&h, &c, false).await;
    // Not verified yet.
    assert_eq!(
        authorize_public(&h, &c, &s).await["reason"],
        "CONTACT_NOT_VERIFIED"
    );
    // Verified until t0+10min.
    h.verify_contact(&c, &s.contact, "VERIFIED", Some(iso(at(10))))
        .await;
    let ok = authorize_public(&h, &c, &s).await;
    assert_eq!(
        (ok["decision"].as_str(), ok["reason"].as_str()),
        (Some("AUTHORIZED"), Some("AUTHORIZED"))
    );
    assert!(h
        .contacts
        .require_public_network_contact_verified(
            privacy_relay::scope::RelayScope::of_delegated(&c.delegated()),
            s.contact.parse().unwrap()
        )
        .await
        .is_ok());
    // Passing validUntil denies immediately, before any worker runs.
    h.clock.set(at(11));
    assert_eq!(
        authorize_public(&h, &c, &s).await["reason"],
        "CONTACT_VERIFICATION_EXPIRED"
    );
    let viewed = h
        .contact_call(
            Method::GET,
            &format!("/v1/privacy-relay/contacts/{}", s.contact),
            Some(&h.writer(&c)),
            None,
        )
        .await;
    assert_eq!(viewed.json["contact"]["verificationState"], "EXPIRED");
    assert_eq!(
        scalar(
            h.conn(),
            "SELECT count(*) FROM protected_contacts WHERE verification_state = 'VERIFIED'"
        )
        .await,
        1,
        "not materialized yet"
    );
    let err = h
        .contacts
        .require_public_network_contact_verified(
            privacy_relay::scope::RelayScope::of_delegated(&c.delegated()),
            s.contact.parse().unwrap(),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code().as_str(), "CONTACT_VERIFICATION_EXPIRED");
    // The worker materializes it (event + version).
    assert_eq!(h.contacts.expire_verifications(10).await.unwrap(), 1);
    assert_eq!(scalar(h.conn(), "SELECT count(*) FROM contact_verification_events WHERE to_state = 'EXPIRED' AND actor_kind = 'SYSTEM'").await, 1);
    // A newer VERIFIED, then BLOCKED / INVALID: earlier VERIFIED events never override.
    h.verify_contact(&c, &s.contact, "VERIFIED", Some(iso(at(50))))
        .await;
    assert_eq!(authorize_public(&h, &c, &s).await["reason"], "AUTHORIZED");
    for state in ["BLOCKED", "INVALID"] {
        h.verify_contact(&c, &s.contact, state, None).await;
        assert_eq!(
            authorize_public(&h, &c, &s).await["reason"],
            "CONTACT_BLOCKED",
            "{state}"
        );
        let err = h
            .contacts
            .require_public_network_contact_verified(
                privacy_relay::scope::RelayScope::of_delegated(&c.delegated()),
                s.contact.parse().unwrap(),
            )
            .await
            .unwrap_err();
        assert_eq!(err.code().as_str(), "CONTACT_BLOCKED");
    }
    // A protected contact is never reachable over the APPLICATION route, and principals
    // have no public-network authority.
    let r = h
        .post("/internal/v1/communication/authorize", &c, json!({
            "relaySessionId": s.session, "source": { "principalId": s.principal }, "targets": [{ "contactReferenceId": s.contact }],
            "channel": "VOICE", "route": "APPLICATION" }))
        .await;
    assert_eq!(r.json["reason"], "CAPABILITY_UNAVAILABLE");
}

#[tokio::test]
async fn telephony_alone_resolves_a_verified_contact_for_one_authorized_call_leg() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = consumer();
    let s = setup(&h, &c, true).await;
    let decision = authorize_public(&h, &c, &s).await;
    let decision_id = decision["decisionId"].as_str().unwrap();
    let (call, leg) = (Uuid::new_v4(), Uuid::new_v4());
    let r = grant(&h, &c, &s, decision_id, call, leg).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let g = r.json["grant"].clone();
    assert!(
        !r.text.contains(E164) && !r.text.contains("2079460123"),
        "a grant carries no number"
    );
    assert_eq!(
        (g["callId"].as_str(), g["callLegId"].as_str()),
        (
            Some(call.to_string().as_str()),
            Some(leg.to_string().as_str())
        )
    );
    let stored = rows(
        h.conn(),
        &format!(
            "SELECT secret_hash, status FROM contact_resolution_grants WHERE id = '{}'",
            g["grantId"].as_str().unwrap()
        ),
    )
    .await;
    assert_ne!(
        stored[0][0],
        g["grantSecret"].as_str().unwrap(),
        "only the secret's hash is stored"
    );
    assert_eq!(stored[0][1], "ISSUED");

    // Calling holds no resolution permission; an untrusted service is unauthenticated.
    let calling = h.ids.calling.clone();
    let attempt = Uuid::new_v4();
    let r = resolve_as(&h, &calling, &c, &s.contact, &g, attempt).await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::FORBIDDEN, "INTERNAL_PERMISSION_DENIED")
    );
    let untrusted = h.ids.untrusted.clone();
    assert_eq!(
        resolve_as(&h, &untrusted, &c, &s.contact, &g, attempt)
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    // Telephony with the valid grant.
    let telephony = h.ids.telephony.clone();
    let r = resolve_as(&h, &telephony, &c, &s.contact, &g, attempt).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["resolution"]["e164"], E164);
    assert_eq!(r.json["resolution"]["callLegId"], leg.to_string());
    assert_eq!(r.headers["cache-control"], "no-store");
    // The same execution attempt may retry; any other use of the grant is refused.
    assert_eq!(
        resolve_as(&h, &telephony, &c, &s.contact, &g, attempt)
            .await
            .json["resolution"]["e164"],
        E164
    );
    let r = resolve_as(&h, &telephony, &c, &s.contact, &g, Uuid::new_v4()).await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::FORBIDDEN, "CONTACT_RESOLUTION_DENIED")
    );

    // Audit: who, which contact, call / leg / context / session / decision / grant, purpose.
    let audit = rows(
        h.conn(),
        &format!(
            "SELECT operation, outcome, actor_service, purpose, coalesce(call_ref::text, ''), coalesce(call_leg_ref::text, ''), \
               coalesce(context_ref::text, ''), coalesce(relay_session_ref::text, ''), coalesce(decision_ref::text, ''), coalesce(reason_code, '') \
             FROM contact_access_audit WHERE contact_reference = '{}' AND operation IN ('GRANT_FOR_EXECUTION', 'RESOLVE_FOR_EXECUTION') ORDER BY created_at, outcome",
            s.contact
        ),
    )
    .await;
    let ok = audit
        .iter()
        .find(|r| r[0] == "RESOLVE_FOR_EXECUTION" && r[1] == "SUCCEEDED")
        .unwrap();
    assert_eq!(
        (
            ok[2].as_str(),
            ok[3].as_str(),
            ok[4].as_str(),
            ok[5].as_str(),
            ok[6].as_str(),
            ok[7].as_str(),
            ok[8].as_str()
        ),
        (
            "cfmeet.telephony",
            "PUBLIC_NETWORK_CALL_LEG",
            call.to_string().as_str(),
            leg.to_string().as_str(),
            s.context.as_str(),
            s.session.as_str(),
            decision_id
        )
    );
    assert!(audit
        .iter()
        .any(|r| r[0] == "GRANT_FOR_EXECUTION" && r[1] == "SUCCEEDED" && r[2] == "cfmeet.calling"));
    assert!(audit.iter().any(|r| r[0] == "RESOLVE_FOR_EXECUTION"
        && r[1] == "DENIED"
        && r[9] == "CONTACT_RESOLUTION_DENIED"));
    let everything = common::dump_database(h.conn()).await;
    assert!(!everything.contains(E164) && !everything.contains("2079460123"));
    assert!(!everything.contains(g["grantSecret"].as_str().unwrap()));
}

#[tokio::test]
async fn grants_are_bound_to_contact_leg_scope_time_and_current_authority() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = consumer();
    let s = setup(&h, &c, true).await;
    let decision = authorize_public(&h, &c, &s).await["decisionId"]
        .as_str()
        .unwrap()
        .to_string();
    let (call, leg) = (Uuid::new_v4(), Uuid::new_v4());
    let telephony = h.ids.telephony.clone();
    let issue = |leg: Uuid| {
        let (h, c, s, decision) = (&h, &c, &s, decision.clone());
        async move {
            let r = grant(h, c, s, &decision, call, leg).await;
            assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
            r.json["grant"].clone()
        }
    };
    let deny = |r: common::Response, what: &str| {
        assert_eq!(
            (r.status, r.code()),
            (StatusCode::FORBIDDEN, "CONTACT_RESOLUTION_DENIED"),
            "{what}: {}",
            r.text
        );
    };
    // Wrong secret, another call leg, another contact.
    let g = issue(leg).await;
    let mut wrong = g.clone();
    wrong["grantSecret"] = json!("A".repeat(43));
    deny(
        resolve_as(&h, &telephony, &c, &s.contact, &wrong, Uuid::new_v4()).await,
        "secret",
    );
    let mut other_leg = g.clone();
    other_leg["callLegId"] = json!(Uuid::new_v4());
    deny(
        resolve_as(&h, &telephony, &c, &s.contact, &other_leg, Uuid::new_v4()).await,
        "leg",
    );
    let other_contact = h.create_contact(&c, "+44 20 7946 0999").await["contactReference"]
        .as_str()
        .unwrap()
        .to_string();
    h.verify_contact(&c, &other_contact, "VERIFIED", None).await;
    deny(
        resolve_as(&h, &telephony, &c, &other_contact, &g, Uuid::new_v4()).await,
        "contact",
    );
    // Another tenant's delegated scope.
    deny(
        resolve_as(&h, &telephony, &consumer(), &s.contact, &g, Uuid::new_v4()).await,
        "tenant",
    );
    // A re-request for the same leg revokes the earlier grant.
    let g2 = issue(leg).await;
    deny(
        resolve_as(&h, &telephony, &c, &s.contact, &g, Uuid::new_v4()).await,
        "revoked",
    );
    // Expiry (120 s).
    h.clock.advance(Duration::seconds(121));
    let r = resolve_as(&h, &telephony, &c, &s.contact, &g2, Uuid::new_v4()).await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::FORBIDDEN, "CONTACT_RESOLUTION_GRANT_EXPIRED")
    );
    // A change of authority after the grant (reassignment bumps the context version).
    let g3 = {
        let fresh = authorize_public(&h, &c, &s).await["decisionId"]
            .as_str()
            .unwrap()
            .to_string();
        let r = grant(&h, &c, &s, &fresh, call, Uuid::new_v4()).await;
        r.json["grant"].clone()
    };
    let other_principal = Uuid::new_v4();
    let r = h
        .command(&format!("/internal/v1/contexts/{}/update", s.context), &c, json!({ "expectedVersion": 1,
            "participants": [{ "principalId": s.principal }, { "principalId": other_principal }, { "contactReferenceId": s.contact }] }))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    deny(
        resolve_as(&h, &telephony, &c, &s.contact, &g3, Uuid::new_v4()).await,
        "authority changed",
    );
    // ...and the old decision cannot mint a new grant either.
    let r = grant(&h, &c, &s, &decision, call, Uuid::new_v4()).await;
    deny(r, "stale decision");
    // A blocked contact cannot get a grant even with a current decision.
    let fresh = authorize_public(&h, &c, &s).await["decisionId"]
        .as_str()
        .unwrap()
        .to_string();
    h.verify_contact(&c, &s.contact, "BLOCKED", None).await;
    let r = grant(&h, &c, &s, &fresh, call, Uuid::new_v4()).await;
    assert_eq!(r.code(), "CONTACT_BLOCKED");
    // A DENIED / application-route decision never yields a grant.
    let app = h
        .post("/internal/v1/communication/authorize", &c, json!({
            "relaySessionId": s.session, "source": { "principalId": s.principal }, "targets": [{ "principalId": other_principal }],
            "channel": "VOICE", "route": "APPLICATION" }))
        .await;
    h.verify_contact(&c, &s.contact, "VERIFIED", None).await;
    deny(
        grant(
            &h,
            &c,
            &s,
            app.json["decisionId"].as_str().unwrap(),
            call,
            Uuid::new_v4(),
        )
        .await,
        "application decision",
    );
    // Grant expiry is materialized by the worker.
    h.clock.advance(Duration::seconds(300));
    assert!(h.contacts.expire_grants(100).await.unwrap() >= 1);
    assert_eq!(
        scalar(
            h.conn(),
            "SELECT count(*) FROM contact_resolution_grants WHERE status = 'ISSUED'"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn resolution_permission_is_receiver_authoritative_and_telephony_only() {
    // A Privacy Relay trust bundle can never grant Calling (or anyone but Telephony) the
    // plaintext resolution permission.
    let bundle = |issuer: &str, permission: &str| {
        json!({
            "bundle_version": 1, "receiver": "cfmeet.privacy-relay", "runtime_environment": "test",
            "trusted_issuers": [{ "issuer": issuer, "audience": "cfmeet.privacy-relay", "runtime_environment": "test",
                "keys": [cf_service_auth::ServiceSigningKey::generate(cf_service_auth::KeyId::parse("k-1").unwrap()).public_jwk()],
                "permissions": [permission] }],
        })
        .to_string()
    };
    let load = |issuer: &str, permission: &str| {
        TrustPolicy::from_json(
            &bundle(issuer, permission),
            &ServiceId::privacy_relay(),
            RuntimeEnvironment::Test,
        )
    };
    assert!(load("cfmeet.calling", "protected-contact.resolve-for-execution").is_err());
    assert!(load(
        "cfmeet.numbering",
        "protected-contact.resolve-for-execution"
    )
    .is_err());
    assert!(load(
        "cfmeet.telephony",
        "protected-contact.resolve-for-execution"
    )
    .is_ok());
    assert!(load("cfmeet.calling", "protected-contact.grant-for-execution").is_ok());
    assert!(load("cfmeet.telephony", "protected-contact.grant-for-execution").is_err());

    // A Telephony identity WITHOUT the receiver grant is refused too.
    let Some(h) = Harness::with_permissions(common::ALL_CALLING_PERMISSIONS).await else {
        return;
    };
    let c = consumer();
    let s = setup(&h, &c, true).await;
    let decision = authorize_public(&h, &c, &s).await["decisionId"]
        .as_str()
        .unwrap()
        .to_string();
    let g = grant(&h, &c, &s, &decision, Uuid::new_v4(), Uuid::new_v4())
        .await
        .json["grant"]
        .clone();
    // Calling without the grant permission cannot issue grants.
    let Some(h2) = Harness::with_permissions(&[
        "communication.authorize",
        "communication-context.write",
        "relay-session.write",
    ])
    .await
    else {
        return;
    };
    let r = h2
        .post(&format!("/internal/v1/contacts/{}/resolution-grants", s.contact), &c, json!({
            "relaySessionId": s.session, "decisionId": decision, "callId": Uuid::new_v4(), "callLegId": Uuid::new_v4(), "purpose": "PUBLIC_NETWORK_CALL_LEG" }))
        .await;
    assert_eq!(r.code(), "INTERNAL_PERMISSION_DENIED");
    let _ = g;
}

#[tokio::test]
async fn a_contact_blocked_or_expired_after_its_grant_is_never_resolved() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = consumer();
    let telephony = h.ids.telephony.clone();
    // BLOCKED after the grant was issued.
    let s = setup(&h, &c, true).await;
    let decision = authorize_public(&h, &c, &s).await;
    let (call, leg) = (Uuid::new_v4(), Uuid::new_v4());
    let g = grant(
        &h,
        &c,
        &s,
        decision["decisionId"].as_str().unwrap(),
        call,
        leg,
    )
    .await
    .json["grant"]
        .clone();
    h.verify_contact(&c, &s.contact, "BLOCKED", None).await;
    let r = resolve_as(&h, &telephony, &c, &s.contact, &g, Uuid::new_v4()).await;
    assert_eq!(r.code(), "CONTACT_BLOCKED", "{}", r.text);
    assert!(!r.text.contains(E164));
    // Verification EXPIRED (read time) between the grant and its use.
    let h = Harness::new().await.unwrap();
    let telephony = h.ids.telephony.clone();
    let principal = Uuid::new_v4();
    let contact = h.create_contact(&c, NUMBER).await["contactReference"]
        .as_str()
        .unwrap()
        .to_string();
    h.verify_contact(&c, &contact, "VERIFIED", Some(iso(at(1))))
        .await;
    let mut req = common::context_request(&[principal], "BIDIRECTIONAL", &[]);
    req["participants"] = json!([{ "principalId": principal }, { "contactReferenceId": contact }]);
    req["allowedRelayStrategies"] = json!(["PUBLIC_RELAY_ALLOWED"]);
    let ctx = h.create_context(&c, req).await;
    let session = h
        .create_session(
            &c,
            ctx["contextId"].as_str().unwrap(),
            json!({ "relayStrategy": "PUBLIC_RELAY_ALLOWED" }),
        )
        .await;
    let s = Setup {
        contact,
        context: ctx["contextId"].as_str().unwrap().to_string(),
        session: session["relaySessionId"].as_str().unwrap().to_string(),
        principal,
    };
    let decision = authorize_public(&h, &c, &s).await;
    let g = grant(
        &h,
        &c,
        &s,
        decision["decisionId"].as_str().unwrap(),
        call,
        leg,
    )
    .await
    .json["grant"]
        .clone();
    h.clock.set(at(1) + Duration::seconds(5));
    let r = resolve_as(&h, &telephony, &c, &s.contact, &g, Uuid::new_v4()).await;
    assert_eq!(r.code(), "CONTACT_VERIFICATION_EXPIRED", "{}", r.text);
    assert!(!r.text.contains(E164));
    assert_eq!(
        scalar(h.conn(), &format!("SELECT count(*) FROM contact_access_audit WHERE operation = 'RESOLVE_FOR_EXECUTION' AND outcome = 'DENIED' AND contact_reference = '{}'", s.contact)).await,
        1
    );
}
