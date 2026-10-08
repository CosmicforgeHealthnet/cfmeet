//! Part D §35: inbound callback privacy — Telephony-only resolution of a called relay
//! number to its CURRENT safe mapping (never a destination number), generic refusals,
//! delayed-callback protection (cooldown + generation / time checks), and Telephony-only
//! inbound caller protection that turns a presented number into a ContactReference.

mod common;

use axum::http::{Method, StatusCode};
use cf_service_auth::ApplicationEnvironmentKind;
use chrono::{DateTime, Utc};
use common::{at, iso, rows, scalar, Consumer, Harness};
use serde_json::{json, Value};
use uuid::Uuid;

const RELAY: &str = "+44 20 7946 0500";
const CONTACT_A: &str = "+44 20 7946 0123";
const CONTACT_B: &str = "+44 20 7946 0124";

fn consumer() -> Consumer {
    Consumer::new(ApplicationEnvironmentKind::Production)
}

struct Mapping {
    context: String,
    session: String,
    contact: String,
    principal: Uuid,
}

/// A CALLBACK_RELAY context (principal + VERIFIED protected contact), a session and
/// (when `assign`) its relay-number assignment.
async fn mapping(h: &Harness, c: &Consumer, number: &str, until: i64, assign: bool) -> Mapping {
    let principal = Uuid::new_v4();
    let contact = h.create_contact(c, number).await["contactReference"]
        .as_str()
        .unwrap()
        .to_string();
    h.verify_contact(c, &contact, "VERIFIED", None).await;
    let mut req = common::context_request(&[principal], "BIDIRECTIONAL", &[]);
    req["participants"] = json!([{ "principalId": principal }, { "contactReferenceId": contact }]);
    req["allowedRelayStrategies"] =
        json!(["APP_ONLY", "PUBLIC_RELAY_ALLOWED", "CALLBACK_RELAY_ALLOWED"]);
    req["startsAt"] = json!(iso(at(0)));
    req["expiresAt"] = json!(iso(at(until)));
    let ctx = h.create_context(c, req).await;
    let context = ctx["contextId"].as_str().unwrap().to_string();
    let session = h
        .create_session(
            c,
            &context,
            json!({ "relayStrategy": "CALLBACK_RELAY_ALLOWED" }),
        )
        .await["relaySessionId"]
        .as_str()
        .unwrap()
        .to_string();
    if assign {
        let r = h.assign_relay_number(c, &session).await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    }
    Mapping {
        context,
        session,
        contact,
        principal,
    }
}

async fn resolve(h: &Harness, observed: DateTime<Utc>, extra: Value) -> common::Response {
    let mut body = json!({
        "calledRelayIdentity": RELAY,
        "providerEventReference": format!("evt-{}", Uuid::new_v4().simple()),
        "providerObservedAt": iso(observed),
    });
    for (k, v) in extra.as_object().unwrap() {
        body[k] = v.clone();
    }
    h.telephony("/internal/v1/relay-routing/resolve-inbound", body)
        .await
}

fn not_active(r: &common::Response, what: &str) {
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::CONFLICT, "RELAY_MAPPING_NOT_ACTIVE"),
        "{what}: {}",
        r.text
    );
}

async fn last_reason(h: &Harness) -> String {
    rows(
        h.conn(),
        "SELECT reason FROM relay_inbound_resolutions ORDER BY xmin::text::bigint DESC LIMIT 1",
    )
    .await[0][0]
        .clone()
}

#[tokio::test]
async fn telephony_alone_resolves_a_called_relay_number_to_safe_references() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = consumer();
    h.add_relay_number(RELAY, "pb-inbound").await;
    h.clock.set(at(1));
    let m = mapping(&h, &c, CONTACT_A, 120, true).await;
    h.clock.set(at(5));
    // National format of the same public number normalizes to the same identity.
    let r = resolve(
        &h,
        at(5),
        json!({ "calledRelayIdentity": "+44 (20) 7946-0500" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let res = &r.json["resolution"];
    assert_eq!(res["outcome"], "RESOLVED");
    assert_eq!(
        (res["contextId"].as_str(), res["relaySessionId"].as_str()),
        (Some(m.context.as_str()), Some(m.session.as_str()))
    );
    assert_eq!(
        (
            res["route"].as_str(),
            res["channel"].as_str(),
            res["strategy"].as_str()
        ),
        (
            Some("CALLBACK_RELAY"),
            Some("VOICE"),
            Some("CALLBACK_RELAY_ALLOWED")
        )
    );
    assert_eq!(res["assignmentGeneration"], 1);
    assert_eq!(
        res["inboundSources"],
        json!([{ "contactReferenceId": m.contact, "allowedTargets": [{ "principalId": m.principal }] }])
    );
    assert_eq!(
        res["validUntil"],
        at(120).to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
    );
    // Safe references only: no relay number, no contact number, no binding.
    for needle in ["442079460500", "2079460123", "pb-inbound"] {
        assert!(!r.text.contains(needle), "{needle}");
    }
    // The decision is recorded without any number.
    let decision = res["decisionId"].as_str().unwrap();
    let dump = common::dump_table(h.conn(), "relay_inbound_resolutions").await;
    assert!(dump.contains(decision));
    for needle in ["442079460500", "2079460500", "2079460123"] {
        assert!(!dump.contains(needle), "{needle}");
    }
    assert!(
        common::exec(h.conn(), "DELETE FROM relay_inbound_resolutions")
            .await
            .is_err()
    );

    // Least privilege: Calling has no inbound permission; an untrusted identity is
    // unauthenticated; a delegated context is refused (the mapping is the scope).
    let body = json!({ "calledRelayIdentity": RELAY, "providerObservedAt": iso(at(5)) });
    let calling = h.ids.calling.clone();
    let r = h
        .send_as(
            &calling,
            Method::POST,
            "/internal/v1/relay-routing/resolve-inbound",
            None,
            Some(body.clone()),
        )
        .await;
    assert_eq!(r.code(), "INTERNAL_PERMISSION_DENIED");
    let untrusted = h.ids.untrusted.clone();
    let r = h
        .send_as(
            &untrusted,
            Method::POST,
            "/internal/v1/relay-routing/resolve-inbound",
            None,
            Some(body.clone()),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let telephony = h.ids.telephony.clone();
    let r = h
        .send_as(
            &telephony,
            Method::POST,
            "/internal/v1/relay-routing/resolve-inbound",
            Some(&c),
            Some(body),
        )
        .await;
    assert_eq!(r.code(), "VALIDATION_ERROR");
    // Bodies are strict: no destination, no tenant, no number as event reference.
    for bad in [
        json!({ "calledRelayIdentity": RELAY, "providerObservedAt": iso(at(5)), "destinationNumber": CONTACT_A }),
        json!({ "calledRelayIdentity": RELAY, "providerObservedAt": iso(at(5)), "tenantId": Uuid::new_v4() }),
        json!({ "calledRelayIdentity": RELAY, "providerObservedAt": iso(at(5)), "providerEventReference": "+442079460123" }),
        json!({ "calledRelayIdentity": RELAY, "providerObservedAt": iso(at(5)), "assignmentGeneration": 0 }),
        json!({ "calledRelayIdentity": RELAY }),
    ] {
        let r = h
            .telephony("/internal/v1/relay-routing/resolve-inbound", bad)
            .await;
        assert_eq!(r.code(), "VALIDATION_ERROR", "{}", r.text);
    }
}

#[tokio::test]
async fn every_inactive_mapping_is_the_same_generic_refusal() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = consumer();
    h.add_relay_number(RELAY, "pb-inbound").await;
    // Unknown / unparseable relay identities.
    for (number, reason) in [
        ("+44 20 7946 0999", "UNKNOWN_RELAY_IDENTITY"),
        ("not-a-number", "UNKNOWN_RELAY_IDENTITY"),
    ] {
        let r = resolve(&h, at(0), json!({ "calledRelayIdentity": number })).await;
        not_active(&r, number);
        assert_eq!(last_reason(&h).await, reason);
    }
    // A number nobody holds.
    not_active(&resolve(&h, at(0), json!({})).await, "unassigned");
    assert_eq!(last_reason(&h).await, "NO_ACTIVE_ASSIGNMENT");
    let m = mapping(&h, &c, CONTACT_A, 60, true).await;
    h.clock.set(at(10));
    // Stale, future and inconsistent provider timestamps.
    not_active(&resolve(&h, at(-10), json!({})).await, "stale");
    assert_eq!(last_reason(&h).await, "STALE_EVENT");
    not_active(&resolve(&h, at(15), json!({})).await, "future");
    assert_eq!(last_reason(&h).await, "EVENT_FROM_FUTURE");
    not_active(
        &resolve(&h, at(9), json!({ "providerStartedAt": iso(at(14)) })).await,
        "inconsistent",
    );
    // Wrong correlation metadata.
    not_active(
        &resolve(&h, at(9), json!({ "assignmentGeneration": 2 })).await,
        "generation",
    );
    assert_eq!(last_reason(&h).await, "GENERATION_MISMATCH");
    not_active(
        &resolve(&h, at(9), json!({ "assignmentId": Uuid::new_v4() })).await,
        "assignment id",
    );
    assert_eq!(last_reason(&h).await, "ASSIGNMENT_MISMATCH");
    // A suspended session denies (policy), then a revoked one has no mapping at all.
    let v = h
        .get(&format!("/internal/v1/sessions/{}", m.session), &c)
        .await
        .json["version"]
        .as_i64()
        .unwrap();
    assert_eq!(
        h.command(
            &format!("/internal/v1/sessions/{}/suspend", m.session),
            &c,
            json!({ "expectedVersion": v })
        )
        .await
        .status,
        StatusCode::OK
    );
    not_active(&resolve(&h, at(10), json!({})).await, "suspended");
    assert_eq!(last_reason(&h).await, "SESSION_NOT_ACTIVE");
    let v = v + 1;
    assert_eq!(
        h.command(
            &format!("/internal/v1/sessions/{}/revoke", m.session),
            &c,
            json!({ "expectedVersion": v })
        )
        .await
        .status,
        StatusCode::OK
    );
    not_active(&resolve(&h, at(10), json!({})).await, "revoked (cooldown)");
    assert_eq!(last_reason(&h).await, "NO_ACTIVE_ASSIGNMENT");
    // Refusals carry nothing about the previous destination.
    let r = resolve(&h, at(10), json!({})).await;
    for needle in [m.context.as_str(), m.session.as_str(), m.contact.as_str()] {
        assert!(!r.text.contains(needle));
    }
    // An assignment past its validity is refused even before any worker runs.
    let n2 = h.add_relay_number("+44 20 7946 0501", "pb-inbound-2").await;
    let _ = n2;
    let m2 = mapping(&h, &c, CONTACT_B, 30, true).await;
    h.clock.set(at(31));
    let r = h
        .telephony(
            "/internal/v1/relay-routing/resolve-inbound",
            json!({ "calledRelayIdentity": "+44 20 7946 0501", "providerObservedAt": iso(at(31)) }),
        )
        .await;
    not_active(&r, "expired assignment");
    assert_eq!(last_reason(&h).await, "ASSIGNMENT_EXPIRED");
    let _ = m2;
    // A public-relay (not callback) session has no inbound authority.
    h.clock.set(at(0));
    let p = mapping(&h, &c, CONTACT_A, 60, false).await;
    let mut req = common::context_request(&[p.principal], "BIDIRECTIONAL", &[]);
    req["participants"] =
        json!([{ "principalId": p.principal }, { "contactReferenceId": p.contact }]);
    req["allowedRelayStrategies"] = json!(["PUBLIC_RELAY_ALLOWED"]);
    let ctx = h.create_context(&c, req).await;
    let s = h
        .create_session(
            &c,
            ctx["contextId"].as_str().unwrap(),
            json!({ "relayStrategy": "PUBLIC_RELAY_ALLOWED" }),
        )
        .await;
    h.add_relay_number("+44 20 7946 0502", "pb-inbound-3").await;
    let mut assigned = false;
    for _ in 0..3 {
        let r = h
            .assign_relay_number(&c, s["relaySessionId"].as_str().unwrap())
            .await;
        assigned |= r.status == StatusCode::CREATED;
    }
    assert!(assigned);
    let r = h
        .telephony(
            "/internal/v1/relay-routing/resolve-inbound",
            json!({ "calledRelayIdentity": "+44 20 7946 0502", "providerObservedAt": iso(at(0)) }),
        )
        .await;
    not_active(&r, "public relay strategy");
    assert_eq!(last_reason(&h).await, "STRATEGY_NOT_ALLOWED");
}

#[tokio::test]
async fn a_delayed_callback_never_reaches_the_next_session() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = consumer();
    let n = h.add_relay_number(RELAY, "pb-inbound").await;
    // Session A holds N (generation 1) from t+1.
    h.clock.set(at(1));
    let a = mapping(&h, &c, CONTACT_A, 300, true).await;
    h.clock.set(at(3));
    let ok = resolve(&h, at(3), json!({ "assignmentGeneration": 1 })).await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.text);
    let a_assignment = ok.json["resolution"]["assignmentId"].clone();
    // A ends: N cools down. A delayed event for A is refused during the cooldown.
    h.clock.set(at(4));
    let v = h
        .get(&format!("/internal/v1/sessions/{}", a.session), &c)
        .await
        .json["version"]
        .as_i64()
        .unwrap();
    assert_eq!(
        h.command(
            &format!("/internal/v1/sessions/{}/revoke", a.session),
            &c,
            json!({ "expectedVersion": v })
        )
        .await
        .status,
        StatusCode::OK
    );
    h.clock.set(at(6));
    not_active(&resolve(&h, at(5), json!({})).await, "during cooldown");
    // After the cooldown N goes to session B (another context and contact): generation 2.
    h.clock.set(at(70));
    h.relay.run_sweep().await.unwrap();
    let b = mapping(&h, &c, CONTACT_B, 300, true).await;
    let b_assigned_at = at(70);
    let gen: i64 = rows(
        h.conn(),
        &format!("SELECT generation::text FROM relay_number_inventory WHERE id = '{n}'"),
    )
    .await[0][0]
        .parse()
        .unwrap();
    assert_eq!(gen, 2);
    h.clock.set(at(72));
    // Replays of A's event: A's generation, A's assignment, or an origin before B's
    // assignment — all refused.
    not_active(
        &resolve(&h, at(71), json!({ "assignmentGeneration": 1 })).await,
        "old generation",
    );
    assert_eq!(last_reason(&h).await, "GENERATION_MISMATCH");
    not_active(
        &resolve(&h, at(71), json!({ "assignmentId": a_assignment })).await,
        "old assignment",
    );
    assert_eq!(last_reason(&h).await, "ASSIGNMENT_MISMATCH");
    not_active(
        &resolve(&h, at(71), json!({ "providerStartedAt": iso(at(5)) })).await,
        "originated before B",
    );
    assert_eq!(last_reason(&h).await, "PREDATES_ASSIGNMENT");
    not_active(
        &resolve(&h, b_assigned_at - chrono::Duration::seconds(1), json!({})).await,
        "observed before B",
    );
    // A fresh event for B resolves to B — and only B.
    let fresh = resolve(
        &h,
        at(72),
        json!({ "assignmentGeneration": 2, "providerStartedAt": iso(at(71)) }),
    )
    .await;
    assert_eq!(fresh.status, StatusCode::OK, "{}", fresh.text);
    let res = &fresh.json["resolution"];
    assert_eq!(
        (res["relaySessionId"].as_str(), res["contextId"].as_str()),
        (Some(b.session.as_str()), Some(b.context.as_str()))
    );
    assert_eq!(
        res["inboundSources"][0]["contactReferenceId"],
        json!(b.contact)
    );
    for old in [a.session.as_str(), a.context.as_str(), a.contact.as_str()] {
        assert!(!fresh.text.contains(old), "no old destination leaks");
    }
}

#[tokio::test]
async fn an_inbound_caller_becomes_a_contact_reference_inside_the_vault_only() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = consumer();
    h.add_relay_number(RELAY, "pb-inbound").await;
    let m = mapping(&h, &c, CONTACT_A, 120, true).await;
    h.clock.set(at(2));
    let resolution = resolve(&h, at(2), json!({})).await;
    let decision = resolution.json["resolution"]["decisionId"]
        .as_str()
        .unwrap()
        .to_string();
    let protect = |number: &'static str, decision: String| {
        let h = &h;
        async move {
            h.telephony(
                "/internal/v1/relay-routing/protect-inbound-caller",
                json!({ "inboundResolutionId": decision, "telephoneNumber": number }),
            )
            .await
        }
    };
    // The known contact calls back (presented in national format; the relay number's
    // country applies): recognized as its existing ContactReference, may reach P.
    let r = protect("020 7946 0123", decision.clone()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let caller = &r.json["inboundCaller"];
    assert_eq!(
        (
            caller["contactReference"].as_str(),
            caller["created"].as_bool()
        ),
        (Some(m.contact.as_str()), Some(false))
    );
    assert_eq!(
        caller["allowedTargets"],
        json!([{ "principalId": m.principal }])
    );
    // An unknown caller: a new UNVERIFIED contact of THAT scope, reaching nobody.
    let r = protect("+44 20 7946 0777", decision.clone()).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let stranger = r.json["inboundCaller"]["contactReference"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(r.json["inboundCaller"]["allowedTargets"], json!([]));
    let scope = rows(h.conn(), &format!("SELECT tenant_id::text, verification_state FROM protected_contacts WHERE id = '{stranger}'")).await;
    assert_eq!(
        scope[0],
        vec![c.tenant_id.to_string(), "UNVERIFIED".to_string()]
    );
    // The raw caller numbers exist nowhere outside the vault's ciphertext.
    for table in [
        "contact_access_audit",
        "relay_inbound_resolutions",
        "relay_audit_events",
        "privacy_relay_outbox",
        "relay_authorization_decisions",
        "relay_number_events",
    ] {
        let dump = common::dump_table(h.conn(), table).await;
        for needle in ["2079460123", "2079460777", "442079460777"] {
            assert!(!dump.contains(needle), "{table} contains {needle}");
        }
    }
    let protected = common::dump_table(h.conn(), "protected_contacts").await;
    assert!(
        !protected.contains("2079460777"),
        "only ciphertext (and the masked display) in the vault"
    );
    // Audited as PROTECT_INBOUND with the mapping's context / session and Telephony.
    let audit = rows(
        h.conn(),
        "SELECT actor_service, purpose, context_ref::text, relay_session_ref::text, coalesce(reason_code, '') FROM contact_access_audit WHERE operation = 'PROTECT_INBOUND' ORDER BY xmin::text::bigint",
    )
    .await;
    assert_eq!(audit.len(), 2);
    assert_eq!(
        audit[0],
        vec![
            "cfmeet.telephony",
            "INBOUND_CALLER_PROTECTION",
            &m.context,
            &m.session,
            "DEDUPLICATED"
        ]
        .into_iter()
        .map(String::from)
        .collect::<Vec<_>>()
    );
    // Only a recent RESOLVED resolution scopes a protection.
    h.clock.set(at(5));
    let r = protect("+44 20 7946 0778", decision.clone()).await;
    assert_eq!(
        r.code(),
        "RELAY_MAPPING_NOT_ACTIVE",
        "expired protection window"
    );
    let refused = resolve(&h, at(5), json!({ "assignmentGeneration": 9 })).await;
    let _ = refused;
    let refused_id = rows(h.conn(), "SELECT id::text FROM relay_inbound_resolutions WHERE outcome <> 'RESOLVED' ORDER BY xmin::text::bigint DESC LIMIT 1").await[0][0].clone();
    assert_eq!(
        protect("+44 20 7946 0778", refused_id).await.code(),
        "RELAY_MAPPING_NOT_ACTIVE"
    );
    assert_eq!(
        protect("+44 20 7946 0778", Uuid::new_v4().to_string())
            .await
            .code(),
        "RELAY_MAPPING_NOT_ACTIVE"
    );
    // Calling cannot protect inbound callers.
    let fresh = resolve(&h, at(5), json!({})).await.json["resolution"]["decisionId"]
        .as_str()
        .unwrap()
        .to_string();
    let calling = h.ids.calling.clone();
    let r = h
        .send_as(
            &calling,
            Method::POST,
            "/internal/v1/relay-routing/protect-inbound-caller",
            None,
            Some(json!({ "inboundResolutionId": fresh, "telephoneNumber": "+44 20 7946 0779" })),
        )
        .await;
    assert_eq!(r.code(), "INTERNAL_PERMISSION_DENIED");
    // Invalid presented numbers are refused without echoing them.
    let r = protect("tel:+442079460779", fresh.clone()).await;
    assert_eq!(r.code(), "CONTACT_INVALID");
    assert!(!r.text.contains("2079460779"));
    // Contact Vault off: nothing is protected (restrictive default).
    h.contact_feature.set(false);
    assert_eq!(
        protect("+44 20 7946 0780", fresh).await.code(),
        "CONTACT_VAULT_DISABLED"
    );
    assert_eq!(
        scalar(h.conn(), "SELECT count(*) FROM protected_contacts").await,
        2
    );
}
