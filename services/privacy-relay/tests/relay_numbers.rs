//! Part D §34: the temporary relay-number pool — deterministic, concurrency-safe
//! allocation, idempotency per session, cooldown, drain / disable, containment in the
//! session, parent revocation, the kill switch and the safe surface (no number, no
//! provider binding anywhere outside the inventory).

mod common;

use axum::http::StatusCode;
use cf_service_auth::ApplicationEnvironmentKind;
use common::{at, iso, rows, scalar, Consumer, Harness, TEST_RELAY_COOLDOWN_SECONDS};
use serde_json::{json, Value};
use uuid::Uuid;

const CONTACT: &str = "+44 20 7946 0123";
const RELAY_A: &str = "+44 20 7946 0500";
const RELAY_B: &str = "+44 20 7946 0501";
const BINDING_A: &str = "pb-test-a";
const BINDING_B: &str = "pb-test-b";

fn micros(at: chrono::DateTime<chrono::Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

fn consumer() -> Consumer {
    Consumer::new(ApplicationEnvironmentKind::Production)
}

/// A context (valid `from`..`until` minutes) with a principal and a VERIFIED protected
/// contact, allowing public relay.
async fn context(h: &Harness, c: &Consumer, from: i64, until: i64) -> (String, Uuid) {
    let principal = Uuid::new_v4();
    let contact = h.create_contact(c, CONTACT).await["contactReference"]
        .as_str()
        .unwrap()
        .to_string();
    let state = h
        .contact_call(
            axum::http::Method::GET,
            &format!("/v1/privacy-relay/contacts/{contact}"),
            Some(&h.writer(c)),
            None,
        )
        .await;
    if state.json["contact"]["verificationState"] != "VERIFIED" {
        h.verify_contact(c, &contact, "VERIFIED", None).await;
    }
    let mut req = common::context_request(&[principal], "BIDIRECTIONAL", &[]);
    req["participants"] = json!([{ "principalId": principal }, { "contactReferenceId": contact }]);
    req["allowedRelayStrategies"] = json!(["APP_ONLY", "PUBLIC_RELAY_ALLOWED"]);
    req["startsAt"] = json!(iso(at(from)));
    req["expiresAt"] = json!(iso(at(until)));
    let ctx = h.create_context(c, req).await;
    (ctx["contextId"].as_str().unwrap().to_string(), principal)
}

async fn session(h: &Harness, c: &Consumer, context: &str) -> String {
    h.create_session(
        c,
        context,
        json!({ "relayStrategy": "PUBLIC_RELAY_ALLOWED" }),
    )
    .await["relaySessionId"]
        .as_str()
        .unwrap()
        .to_string()
}

fn assignment(r: &common::Response) -> &Value {
    &r.json["assignment"]
}

async fn number_status(h: &Harness, id: Uuid) -> (String, i64) {
    let r = rows(
        h.conn(),
        &format!("SELECT operational_status, generation::text FROM relay_number_inventory WHERE id = '{id}'"),
    )
    .await;
    (r[0][0].clone(), r[0][1].parse().unwrap())
}

#[tokio::test]
async fn one_remaining_number_goes_to_exactly_one_session_and_replays_are_idempotent() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = consumer();
    let n = h.add_relay_number(RELAY_A, BINDING_A).await;
    let (ctx, _) = context(&h, &c, 0, 60).await;
    let mut sessions = Vec::new();
    for _ in 0..6 {
        sessions.push(session(&h, &c, &ctx).await);
    }
    // Six sessions race for one number: exactly one wins, nobody shares it.
    let results =
        futures::future::join_all(sessions.iter().map(|s| h.assign_relay_number(&c, s))).await;
    let won: Vec<_> = results
        .iter()
        .filter(|r| r.status == StatusCode::CREATED)
        .collect();
    assert_eq!(
        won.len(),
        1,
        "{:?}",
        results.iter().map(|r| r.text.clone()).collect::<Vec<_>>()
    );
    for r in results.iter().filter(|r| r.status != StatusCode::CREATED) {
        assert_eq!(
            (r.status, r.code()),
            (StatusCode::CONFLICT, "RELAY_NUMBER_UNAVAILABLE"),
            "{}",
            r.text
        );
    }
    let winner = assignment(won[0]).clone();
    assert_eq!(
        (
            winner["status"].as_str(),
            winner["assignmentGeneration"].as_i64()
        ),
        (Some("ACTIVE"), Some(1))
    );
    assert_eq!(number_status(&h, n).await, ("ASSIGNED".to_string(), 1));
    // The same session asking again (concurrently) gets the same assignment, never a second.
    let sid = winner["relaySessionId"].as_str().unwrap().to_string();
    let again = futures::future::join_all((0..4).map(|_| h.assign_relay_number(&c, &sid))).await;
    for r in &again {
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        assert_eq!(assignment(r)["assignmentId"], winner["assignmentId"]);
        assert_eq!(r.headers["idempotent-replayed"], "true");
    }
    // Different criteria for the same session: a conflict, not a second number.
    let r = h
        .post(
            &format!("/internal/v1/sessions/{sid}/relay-number-assignment"),
            &c,
            json!({ "country": "GB", "capabilities": ["VOICE_INBOUND"] }),
        )
        .await;
    assert_eq!(r.code(), "IDEMPOTENCY_CONFLICT");
    assert_eq!(
        h.count("SELECT count(*) FROM relay_number_assignments")
            .await,
        1
    );
    // The session's relay identity binding is the assignment id.
    let s = h.get(&format!("/internal/v1/sessions/{sid}"), &c).await;
    assert_eq!(s.json["relayIdentityBindingRef"], winner["assignmentId"]);
    // The database alone refuses a second holder of the same number.
    let other = &sessions.iter().find(|s| **s != sid).unwrap();
    let err = common::exec(
        h.conn(),
        &format!(
            "INSERT INTO relay_number_assignments (id, relay_number_id, tenant_id, application_id, environment_id, context_id, \
               relay_session_id, assignment_generation, country_iso2, capabilities, status, assigned_at, expires_at, \
               requested_by_service, request_id, correlation_id, created_at, updated_at) \
             SELECT gen_random_uuid(), '{n}', tenant_id, application_id, environment_id, context_id, id, 1, 'GB', \
               ARRAY['VOICE_INBOUND'], 'ACTIVE', '{t}', '{t2}', 'cfmeet.calling', 'req_{z}', 'corr_{z}', '{t}', '{t}' \
             FROM privacy_relay_sessions WHERE id = '{other}'",
            t = iso(at(1)),
            t2 = iso(at(2)),
            z = "0".repeat(32),
        ),
    )
    .await;
    assert!(
        err.is_err(),
        "a second holder must be refused by the database"
    );
    // ...and the one-holder index alone refuses it, even with every trigger bypassed
    // and a distinct generation (no other layer can mask a missing index).
    let insert = format!(
        "INSERT INTO relay_number_assignments (id, relay_number_id, tenant_id, application_id, environment_id, context_id, \
           relay_session_id, assignment_generation, country_iso2, capabilities, status, assigned_at, expires_at, \
           requested_by_service, request_id, correlation_id, created_at, updated_at) \
         SELECT gen_random_uuid(), '{n}', tenant_id, application_id, environment_id, context_id, id, 7, 'GB', \
           ARRAY['VOICE_INBOUND'], 'ACTIVE', '{t}', '{t2}', 'cfmeet.calling', 'req_{z}', 'corr_{z}', '{t}', '{t}' \
         FROM privacy_relay_sessions WHERE id = '{other}'",
        t = iso(at(1)),
        t2 = iso(at(2)),
        z = "0".repeat(32),
    );
    let raw = common::exec(
        h.conn(),
        &format!("DO $$ BEGIN SET LOCAL session_replication_role = replica; {insert}; END $$"),
    )
    .await
    .expect_err("two ACTIVE holders of one number");
    assert!(
        raw.to_string().contains("uq_relay_assignments_holder"),
        "{raw}"
    );
}

#[tokio::test]
async fn different_sessions_get_different_numbers_and_unavailable_states_are_never_lent() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = consumer();
    let a = h.add_relay_number(RELAY_A, BINDING_A).await;
    let b = h.add_relay_number(RELAY_B, BINDING_B).await;
    let (ctx, _) = context(&h, &c, 0, 60).await;
    let s1 = session(&h, &c, &ctx).await;
    let s2 = session(&h, &c, &ctx).await;
    let s3 = session(&h, &c, &ctx).await;
    let r1 = h.assign_relay_number(&c, &s1).await;
    let r2 = h.assign_relay_number(&c, &s2).await;
    assert_eq!(
        (r1.status, r2.status),
        (StatusCode::CREATED, StatusCode::CREATED)
    );
    // Deterministic order (least recently changed first) and distinct numbers.
    let held = rows(h.conn(), "SELECT relay_number_id::text FROM relay_number_assignments ORDER BY assigned_at, relay_number_id").await;
    let mut held: Vec<String> = held.into_iter().map(|r| r[0].clone()).collect();
    held.sort();
    let mut expected = vec![a.to_string(), b.to_string()];
    expected.sort();
    assert_eq!(held, expected);
    assert_eq!(
        h.assign_relay_number(&c, &s3).await.code(),
        "RELAY_NUMBER_UNAVAILABLE"
    );

    // DRAINING and UNAVAILABLE numbers are never lent; RESERVED waits for activation.
    let (ctx2, _) = context(&h, &c, 0, 60).await;
    let fresh = h.add_relay_number("+44 20 7946 0502", "pb-test-c").await;
    h.relay.drain_relay_number(fresh).await.unwrap();
    let s4 = session(&h, &c, &ctx2).await;
    assert_eq!(
        h.assign_relay_number(&c, &s4).await.code(),
        "RELAY_NUMBER_UNAVAILABLE",
        "DRAINING"
    );
    assert_eq!(h.relay.complete_relay_drains(10).await.unwrap(), 1);
    assert_eq!(number_status(&h, fresh).await.0, "UNAVAILABLE");
    assert_eq!(
        h.assign_relay_number(&c, &s4).await.code(),
        "RELAY_NUMBER_UNAVAILABLE",
        "UNAVAILABLE"
    );
    let reserved = h
        .relay
        .add_relay_number(privacy_relay::relay_numbers::inventory::NewRelayNumber {
            number: "+44 20 7946 0503",
            region: None,
            capabilities: &["VOICE_INBOUND", "VOICE_OUTBOUND"],
            provider_binding_ref: "pb-test-d",
            available: false,
        })
        .await
        .unwrap();
    assert_eq!(
        h.assign_relay_number(&c, &s4).await.code(),
        "RELAY_NUMBER_UNAVAILABLE",
        "RESERVED"
    );
    h.relay.activate_relay_number(reserved).await.unwrap();
    // Capability / country filters.
    let r = h
        .post(
            &format!("/internal/v1/sessions/{s4}/relay-number-assignment"),
            &c,
            json!({ "country": "FR", "capabilities": ["VOICE_INBOUND"] }),
        )
        .await;
    assert_eq!(r.code(), "RELAY_NUMBER_UNAVAILABLE");
    let r = h.assign_relay_number(&c, &s4).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    // Invalid inventory input never reaches the table (provider binding containing the
    // number, SMS capability, national format).
    for (number, binding, caps) in [
        ("+44 20 7946 0504", "pb-442079460504", vec!["VOICE_INBOUND"]),
        ("+44 20 7946 0505", "pb-x", vec!["SMS"]),
        ("020 7946 0506", "pb-y", vec!["VOICE_INBOUND"]),
    ] {
        let r = h
            .relay
            .add_relay_number(privacy_relay::relay_numbers::inventory::NewRelayNumber {
                number,
                region: None,
                capabilities: &caps,
                provider_binding_ref: binding,
                available: true,
            })
            .await;
        assert!(r.is_err(), "{binding}");
    }
}

#[tokio::test]
async fn a_released_number_cools_down_before_anyone_else_gets_it() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = consumer();
    let n = h.add_relay_number(RELAY_A, BINDING_A).await;
    let (ctx, _) = context(&h, &c, 0, 300).await;
    let sa = session(&h, &c, &ctx).await;
    let sb = session(&h, &c, &ctx).await;
    let first = h.assign_relay_number(&c, &sa).await;
    let first_id = assignment(&first)["assignmentId"].clone();
    h.clock.set(at(1));
    let v = h.get(&format!("/internal/v1/sessions/{sa}"), &c).await.json["version"]
        .as_i64()
        .unwrap();
    let r = h
        .command(
            &format!("/internal/v1/sessions/{sa}/revoke"),
            &c,
            json!({ "expectedVersion": v }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let got = h
        .get(
            &format!("/internal/v1/sessions/{sa}/relay-number-assignment"),
            &c,
        )
        .await;
    assert_eq!(
        (
            assignment(&got)["status"].as_str(),
            assignment(&got)["endReason"].as_str()
        ),
        (Some("COOLDOWN"), Some("SESSION_REVOKED"))
    );
    assert_eq!(
        assignment(&got)["cooldownUntil"],
        micros(at(1) + chrono::Duration::seconds(TEST_RELAY_COOLDOWN_SECONDS as i64))
    );
    assert_eq!(number_status(&h, n).await, ("COOLDOWN".to_string(), 1));
    // During the cooldown the number is not lent, whatever the worker does.
    h.clock.set(at(30));
    h.relay.run_sweep().await.unwrap();
    assert_eq!(
        h.assign_relay_number(&c, &sb).await.code(),
        "RELAY_NUMBER_UNAVAILABLE"
    );
    // The database refuses to offer it early (bypassing the cooldown in code fails too).
    assert!(common::exec(
        h.conn(),
        &format!(
            "UPDATE relay_number_inventory SET operational_status = 'AVAILABLE' WHERE id = '{n}'"
        )
    )
    .await
    .is_err());
    // After the cooldown the worker finishes it: assignment REVOKED (terminal), number AVAILABLE.
    h.clock.set(at(62));
    let report = h.relay.run_sweep().await.unwrap();
    assert_eq!(report.relay_cooldowns_completed, 1);
    assert_eq!(number_status(&h, n).await, ("AVAILABLE".to_string(), 1));
    let done = h
        .get(
            &format!("/internal/v1/sessions/{sa}/relay-number-assignment"),
            &c,
        )
        .await;
    assert_eq!(assignment(&done)["status"], "REVOKED");
    // A terminal assignment never changes again (no reactivation).
    assert!(common::exec(
        h.conn(),
        &format!(
            "UPDATE relay_number_assignments SET status = 'ACTIVE' WHERE id = {}",
            first_id.to_string().replace('"', "'")
        )
    )
    .await
    .is_err());
    // A revoked session never gets another relay identity (one per session).
    assert!(h
        .assign_relay_number(&c, &sa)
        .await
        .status
        .is_client_error());
    // The next session gets the number at the NEXT generation.
    let second = h.assign_relay_number(&c, &sb).await;
    assert_eq!(second.status, StatusCode::CREATED, "{}", second.text);
    assert_eq!(assignment(&second)["assignmentGeneration"], 2);
    assert_eq!(number_status(&h, n).await, ("ASSIGNED".to_string(), 2));
    // Ledger: the whole inventory history, without tenant data or the number.
    let ledger = rows(
        h.conn(),
        &format!("SELECT event_type, to_status, generation::text FROM relay_number_events WHERE relay_number_id = '{n}' ORDER BY occurred_at, event_type"),
    )
    .await;
    let events: Vec<&str> = ledger.iter().map(|r| r[0].as_str()).collect();
    for e in [
        "RELAY_NUMBER_ADDED",
        "RELAY_NUMBER_ASSIGNED",
        "RELAY_NUMBER_RELEASED",
        "RELAY_NUMBER_COOLDOWN_STARTED",
        "RELAY_NUMBER_AVAILABLE",
    ] {
        assert!(events.contains(&e), "{e}: {events:?}");
    }
    assert!(common::exec(h.conn(), "DELETE FROM relay_number_events")
        .await
        .is_err());
    // The generation never moves backwards.
    assert!(common::exec(
        h.conn(),
        &format!("UPDATE relay_number_inventory SET generation = 1 WHERE id = '{n}'")
    )
    .await
    .is_err());
    // Tenant audit + outbox carry the assignment id, never the number or binding.
    let audit = rows(
        h.conn(),
        &format!("SELECT event_type FROM relay_audit_events WHERE session_id = '{sa}' AND event_type LIKE 'RELAY_NUMBER_%' ORDER BY sequence"),
    )
    .await;
    assert_eq!(
        audit.iter().map(|r| r[0].as_str()).collect::<Vec<_>>(),
        [
            "RELAY_NUMBER_ASSIGNED",
            "RELAY_NUMBER_RELEASED",
            "RELAY_NUMBER_COOLDOWN_STARTED"
        ]
    );
    assert_eq!(
        scalar(h.conn(), "SELECT count(*) FROM privacy_relay_outbox WHERE event_type LIKE 'privacy-relay.relay-number.%'").await,
        4
    );
}

#[tokio::test]
async fn assignments_stay_inside_their_session_and_parents_end_them() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = consumer();
    h.add_relay_number(RELAY_A, BINDING_A).await;
    h.add_relay_number(RELAY_B, BINDING_B).await;
    // Containment: expires with the session's final authority.
    let (ctx, _) = context(&h, &c, 0, 60).await;
    let s = session(&h, &c, &ctx).await;
    let r = h.assign_relay_number(&c, &s).await;
    assert_eq!(assignment(&r)["expiresAt"], micros(at(60)));
    // Narrowing the context narrows the assignment in the same transaction.
    let v = h.version_of(&c, &ctx).await;
    let u = h
        .command(
            &format!("/internal/v1/contexts/{ctx}/update"),
            &c,
            json!({ "expectedVersion": v, "expiresAt": iso(at(40)) }),
        )
        .await;
    assert_eq!(u.status, StatusCode::OK, "{}", u.text);
    let got = h
        .get(
            &format!("/internal/v1/sessions/{s}/relay-number-assignment"),
            &c,
        )
        .await;
    assert_eq!(assignment(&got)["expiresAt"], micros(at(40)));
    // The database refuses an assignment that would outlive its session.
    assert!(common::exec(
        h.conn(),
        &format!(
            "UPDATE relay_number_assignments SET expires_at = '{}' WHERE relay_session_id = '{s}'",
            iso(at(90))
        )
    )
    .await
    .is_err());
    // Expiry: the session (and its assignment) end at the final authority, by worker.
    h.clock.set(at(41));
    let report = h.relay.run_sweep().await.unwrap();
    assert_eq!(report.contexts_expired, 1);
    let got = h
        .get(
            &format!("/internal/v1/sessions/{s}/relay-number-assignment"),
            &c,
        )
        .await;
    assert_eq!(
        (
            assignment(&got)["status"].as_str(),
            assignment(&got)["endReason"].as_str()
        ),
        (Some("COOLDOWN"), Some("SESSION_EXPIRED"))
    );

    // Parent revocation ends routing authority immediately (same transaction).
    let (ctx2, _) = context(&h, &c, 0, 120).await;
    let s2 = session(&h, &c, &ctx2).await;
    assert_eq!(
        h.assign_relay_number(&c, &s2).await.status,
        StatusCode::CREATED
    );
    let v = h.version_of(&c, &ctx2).await;
    assert_eq!(
        h.lifecycle(&c, &ctx2, "revoke", v).await.status,
        StatusCode::OK
    );
    let got = h
        .get(
            &format!("/internal/v1/sessions/{s2}/relay-number-assignment"),
            &c,
        )
        .await;
    assert_eq!(
        (
            assignment(&got)["status"].as_str(),
            assignment(&got)["endReason"].as_str()
        ),
        (Some("COOLDOWN"), Some("SESSION_REVOKED"))
    );
    assert_eq!(
        h.count("SELECT count(*) FROM relay_number_assignments WHERE status = 'ACTIVE'")
            .await,
        0
    );
}

#[tokio::test]
async fn completion_keeps_the_identity_only_through_grace_and_explicit_release_works() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = consumer();
    h.add_relay_number(RELAY_A, BINDING_A).await;
    h.add_relay_number(RELAY_B, BINDING_B).await;
    // No grace: completion ends the assignment.
    let (ctx, _) = context(&h, &c, 0, 60).await;
    let s = session(&h, &c, &ctx).await;
    h.assign_relay_number(&c, &s).await;
    let v = h.version_of(&c, &ctx).await;
    assert_eq!(
        h.lifecycle(&c, &ctx, "complete", v).await.status,
        StatusCode::OK
    );
    let got = h
        .get(
            &format!("/internal/v1/sessions/{s}/relay-number-assignment"),
            &c,
        )
        .await;
    assert_eq!(assignment(&got)["endReason"], "SESSION_COMPLETED");
    // Explicit release of another session's identity (restrictive, idempotent).
    let (ctx2, _) = context(&h, &c, 0, 60).await;
    let s2 = session(&h, &c, &ctx2).await;
    h.assign_relay_number(&c, &s2).await;
    h.pool_feature.set(false);
    for _ in 0..2 {
        let r = h
            .post(
                &format!("/internal/v1/sessions/{s2}/relay-number-assignment/release"),
                &c,
                json!({}),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        assert_eq!(
            (
                assignment(&r)["status"].as_str(),
                assignment(&r)["endReason"].as_str()
            ),
            (Some("COOLDOWN"), Some("RELEASED"))
        );
    }
    // Unknown session / no assignment.
    let r = h
        .post(
            &format!(
                "/internal/v1/sessions/{}/relay-number-assignment/release",
                Uuid::new_v4()
            ),
            &c,
            json!({}),
        )
        .await;
    assert_eq!(r.code(), "SESSION_NOT_FOUND");
    let s3 = session(&h, &c, &ctx2).await;
    assert_eq!(
        h.get(
            &format!("/internal/v1/sessions/{s3}/relay-number-assignment"),
            &c
        )
        .await
        .code(),
        "RELAY_ASSIGNMENT_NOT_FOUND"
    );
}

#[tokio::test]
async fn the_kill_switch_blocks_new_assignments_only() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = consumer();
    let n = h.add_relay_number(RELAY_A, BINDING_A).await;
    h.add_relay_number(RELAY_B, BINDING_B).await;
    let (ctx, principal) = context(&h, &c, 0, 300).await;
    let held = session(&h, &c, &ctx).await;
    let first = h.assign_relay_number(&c, &held).await;
    assert_eq!(first.status, StatusCode::CREATED);
    h.pool_feature.set(false);
    // New assignment: refused.
    let s = session(&h, &c, &ctx).await;
    let r = h.assign_relay_number(&c, &s).await;
    assert_eq!(
        (r.status, r.code()),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "RELAY_NUMBER_POOL_DISABLED"
        )
    );
    // Existing assignment: still readable / replayable, revocable; cooldown completes.
    assert_eq!(
        h.assign_relay_number(&c, &held).await.status,
        StatusCode::OK
    );
    let v = h
        .get(&format!("/internal/v1/sessions/{held}"), &c)
        .await
        .json["version"]
        .as_i64()
        .unwrap();
    assert_eq!(
        h.command(
            &format!("/internal/v1/sessions/{held}/revoke"),
            &c,
            json!({ "expectedVersion": v })
        )
        .await
        .status,
        StatusCode::OK
    );
    h.clock.set(at(70));
    assert_eq!(
        h.relay.run_sweep().await.unwrap().relay_cooldowns_completed,
        1
    );
    assert_eq!(number_status(&h, n).await.0, "AVAILABLE");
    // App-only Privacy Relay keeps working: sessions and APPLICATION authorization.
    let app = h
        .create_session(&c, &ctx, json!({ "relayStrategy": "APP_ONLY" }))
        .await;
    let other = Uuid::new_v4();
    let _ = other;
    let r = h
        .post("/internal/v1/communication/authorize", &c, json!({
            "relaySessionId": app["relaySessionId"], "source": { "principalId": principal },
            "targets": [{ "principalId": principal }], "channel": "VOICE", "route": "APPLICATION" }))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    // The relay pool kill switch is independent of PRIVACY_RELAY_ENABLED.
    h.pool_feature.set(true);
    h.feature.set(false);
    assert_eq!(
        h.assign_relay_number(&c, &s).await.code(),
        "PRIVACY_RELAY_DISABLED"
    );
}

#[tokio::test]
async fn only_verified_public_relay_sessions_of_the_caller_get_a_number() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = consumer();
    h.add_relay_number(RELAY_A, BINDING_A).await;
    let (ctx, _) = context(&h, &c, 0, 60).await;
    let s = session(&h, &c, &ctx).await;
    // Another tenant / environment cannot see or use the session.
    let foreign = Consumer::new(ApplicationEnvironmentKind::Production);
    assert_eq!(
        h.assign_relay_number(&foreign, &s).await.code(),
        "SESSION_NOT_FOUND"
    );
    assert_eq!(
        h.assign_relay_number(&c.sibling_environment(), &s)
            .await
            .code(),
        "SESSION_NOT_FOUND"
    );
    // APP_ONLY session: no public identity.
    let app = h
        .create_session(&c, &ctx, json!({ "relayStrategy": "APP_ONLY" }))
        .await;
    assert_eq!(
        h.assign_relay_number(&c, app["relaySessionId"].as_str().unwrap())
            .await
            .code(),
        "STRATEGY_NOT_ALLOWED"
    );
    // Principal-only session: no protected contact to relay for.
    let p = Uuid::new_v4();
    let mut req = common::context_request(&[p, Uuid::new_v4()], "BIDIRECTIONAL", &[]);
    req["allowedRelayStrategies"] = json!(["PUBLIC_RELAY_ALLOWED"]);
    let pctx = h.create_context(&c, req).await;
    let ps = session(&h, &c, pctx["contextId"].as_str().unwrap()).await;
    assert_eq!(
        h.assign_relay_number(&c, &ps).await.code(),
        "STRATEGY_NOT_ALLOWED"
    );
    // An unverified (then blocked) contact: refused.
    let contact = h.create_contact(&c, "+44 20 7946 0999").await["contactReference"]
        .as_str()
        .unwrap()
        .to_string();
    let mut req = common::context_request(&[p], "BIDIRECTIONAL", &[]);
    req["participants"] = json!([{ "principalId": p }, { "contactReferenceId": contact }]);
    req["allowedRelayStrategies"] = json!(["PUBLIC_RELAY_ALLOWED"]);
    let uctx = h.create_context(&c, req).await;
    let us = session(&h, &c, uctx["contextId"].as_str().unwrap()).await;
    assert_eq!(
        h.assign_relay_number(&c, &us).await.code(),
        "CONTACT_NOT_VERIFIED"
    );
    h.verify_contact(&c, &contact, "BLOCKED", None).await;
    assert_eq!(
        h.assign_relay_number(&c, &us).await.code(),
        "CONTACT_BLOCKED"
    );
    // Contact Vault off: no public identity either.
    h.contact_feature.set(false);
    assert_eq!(
        h.assign_relay_number(&c, &s).await.code(),
        "CONTACT_VAULT_DISABLED"
    );
    h.contact_feature.set(true);
    // Bad criteria.
    for body in [
        json!({ "country": "gb", "capabilities": ["VOICE_INBOUND"] }),
        json!({ "country": "GB", "capabilities": ["SMS"] }),
        json!({ "country": "GB", "capabilities": [] }),
        json!({ "country": "GB", "capabilities": ["VOICE_INBOUND"], "number": "+442079460500" }),
        json!({ "country": "GB", "capabilities": ["VOICE_INBOUND"], "providerBindingRef": "pb" }),
    ] {
        let r = h
            .post(
                &format!("/internal/v1/sessions/{s}/relay-number-assignment"),
                &c,
                body,
            )
            .await;
        assert_eq!(r.code(), "VALIDATION_ERROR", "{}", r.text);
    }
    // A Calling identity without relay-number.assign is refused.
    let restricted = Harness::with_permissions(&["communication.authorize"])
        .await
        .unwrap();
    let r = restricted.assign_relay_number(&c, &s).await;
    assert_eq!(r.code(), "INTERNAL_PERMISSION_DENIED");
    assert_eq!(
        h.assign_relay_number(&c, &s).await.status,
        StatusCode::CREATED
    );
}

#[tokio::test]
async fn neither_the_number_nor_its_provider_binding_leaves_the_inventory() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = consumer();
    let n = h.add_relay_number(RELAY_A, BINDING_A).await;
    let (ctx, _) = context(&h, &c, 0, 60).await;
    let s = session(&h, &c, &ctx).await;
    let r = h.assign_relay_number(&c, &s).await;
    let view = h.get(&format!("/internal/v1/sessions/{s}"), &c).await;
    let release = h
        .post(
            &format!("/internal/v1/sessions/{s}/relay-number-assignment/release"),
            &c,
            json!({}),
        )
        .await;
    let dumps = [
        r.text.clone(),
        view.text.clone(),
        release.text.clone(),
        common::dump_table(h.conn(), "relay_number_assignments").await,
        common::dump_table(h.conn(), "relay_audit_events").await,
        common::dump_table(h.conn(), "privacy_relay_outbox").await,
        common::dump_table(h.conn(), "relay_number_events").await,
        common::dump_table(h.conn(), "privacy_relay_sessions").await,
    ];
    for (i, d) in dumps.iter().enumerate() {
        for needle in ["442079460500", "2079460500", BINDING_A, &n.to_string()] {
            // The inventory id is internal: only Privacy Relay's own assignment / ledger
            // tables reference it.
            let internal = (i == 3 || i == 6) && needle == n.to_string();
            assert!(internal || !d.contains(needle), "#{i} contains {needle}");
        }
    }
    // Operator output is masked.
    let listed = h.relay.list_relay_numbers().await.unwrap();
    assert!(!listed[0].masked().contains("2079460500"));
    assert!(!format!("{:?}", listed[0]).contains("2079460500"));
    assert_eq!(
        listed[0].masked(),
        "+44\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}0500"
    );
}

#[tokio::test]
async fn disabling_a_number_revokes_its_routing_authority_now() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = consumer();
    let n = h.add_relay_number(RELAY_A, BINDING_A).await;
    let (ctx, _) = context(&h, &c, 0, 60).await;
    let s = session(&h, &c, &ctx).await;
    h.assign_relay_number(&c, &s).await;
    // Draining keeps the current assignment; disabling ends it.
    h.relay.drain_relay_number(n).await.unwrap();
    assert_eq!(
        assignment(
            &h.get(
                &format!("/internal/v1/sessions/{s}/relay-number-assignment"),
                &c
            )
            .await
        )["status"],
        "ACTIVE"
    );
    h.relay.disable_relay_number(n).await.unwrap();
    let got = h
        .get(
            &format!("/internal/v1/sessions/{s}/relay-number-assignment"),
            &c,
        )
        .await;
    assert_eq!(
        (
            assignment(&got)["status"].as_str(),
            assignment(&got)["endReason"].as_str()
        ),
        (Some("COOLDOWN"), Some("NUMBER_DISABLED"))
    );
    assert_eq!(number_status(&h, n).await.0, "UNAVAILABLE");
    // After the cooldown the assignment is REVOKED and the number stays UNAVAILABLE.
    h.clock.set(at(61));
    h.relay.run_sweep().await.unwrap();
    assert_eq!(
        assignment(
            &h.get(
                &format!("/internal/v1/sessions/{s}/relay-number-assignment"),
                &c
            )
            .await
        )["status"],
        "REVOKED"
    );
    assert_eq!(number_status(&h, n).await.0, "UNAVAILABLE");
    // An operator may restore it: RESERVED, then AVAILABLE.
    h.relay.enable_relay_number(n).await.unwrap();
    h.relay.activate_relay_number(n).await.unwrap();
    assert_eq!(number_status(&h, n).await, ("AVAILABLE".to_string(), 1));
}

#[tokio::test]
async fn concurrent_workers_release_and_cool_down_each_assignment_exactly_once() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = consumer();
    let mut numbers = Vec::new();
    for i in 0..4 {
        numbers.push(
            h.add_relay_number(&format!("+44 20 7946 06{i:02}"), &format!("pb-w{i}"))
                .await,
        );
    }
    let (ctx, _) = context(&h, &c, 0, 30).await;
    for _ in 0..4 {
        let s = session(&h, &c, &ctx).await;
        assert_eq!(
            h.assign_relay_number(&c, &s).await.status,
            StatusCode::CREATED
        );
    }
    // Four instances sweep at once after the context's end: one expiry per assignment.
    h.clock.set(at(31));
    let workers: Vec<_> = (0..4).map(|_| h.sibling()).collect();
    futures::future::join_all(workers.iter().map(|w| w.run_sweep()))
        .await
        .into_iter()
        .for_each(|r| {
            r.unwrap();
        });
    assert_eq!(
        h.count("SELECT count(*) FROM relay_number_assignments WHERE status = 'COOLDOWN'")
            .await,
        4
    );
    assert_eq!(
        h.count(
            "SELECT count(*) FROM relay_audit_events WHERE event_type = 'RELAY_NUMBER_RELEASED'"
        )
        .await,
        4
    );
    // ...and one cooldown completion per assignment.
    h.clock.set(at(92));
    let reports = futures::future::join_all(workers.iter().map(|w| w.run_sweep())).await;
    let completed: usize = reports
        .iter()
        .map(|r| r.as_ref().unwrap().relay_cooldowns_completed)
        .sum();
    assert_eq!(completed, 4);
    assert_eq!(
        h.count(
            "SELECT count(*) FROM relay_number_events WHERE event_type = 'RELAY_NUMBER_AVAILABLE'"
        )
        .await,
        4
    );
    assert_eq!(
        h.count(
            "SELECT count(*) FROM relay_number_inventory WHERE operational_status = 'AVAILABLE'"
        )
        .await,
        4
    );
    assert_eq!(
        h.count("SELECT count(*) FROM relay_number_assignments WHERE status = 'EXPIRED'")
            .await,
        4
    );
}

#[tokio::test]
async fn reads_and_releases_from_another_application_or_environment_are_not_found() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = consumer();
    h.add_relay_number(RELAY_A, BINDING_A).await;
    let (ctx, _) = context(&h, &c, 0, 60).await;
    let s = session(&h, &c, &ctx).await;
    assert_eq!(
        h.assign_relay_number(&c, &s).await.status,
        StatusCode::CREATED
    );
    let other_application = Consumer {
        application_id: Uuid::new_v4(),
        environment_id: Uuid::new_v4(),
        ..c.clone()
    };
    let other_environment = c.sibling_environment();
    let norm = |r: &common::Response| {
        let mut v = r.normalized_error();
        v["error"]["message"] = Value::Null;
        (r.status, v)
    };
    for foreign in [&other_application, &other_environment] {
        let unknown = Uuid::new_v4();
        let theirs = h
            .get(
                &format!("/internal/v1/sessions/{s}/relay-number-assignment"),
                foreign,
            )
            .await;
        let none = h
            .get(
                &format!("/internal/v1/sessions/{unknown}/relay-number-assignment"),
                foreign,
            )
            .await;
        assert_eq!(theirs.code(), "SESSION_NOT_FOUND", "{}", theirs.text);
        assert_eq!(norm(&theirs), norm(&none));
        let theirs = h
            .post(
                &format!("/internal/v1/sessions/{s}/relay-number-assignment/release"),
                foreign,
                json!({}),
            )
            .await;
        let none = h
            .post(
                &format!("/internal/v1/sessions/{unknown}/relay-number-assignment/release"),
                foreign,
                json!({}),
            )
            .await;
        assert_eq!(theirs.code(), "SESSION_NOT_FOUND", "{}", theirs.text);
        assert_eq!(norm(&theirs), norm(&none));
    }
    // The owner still holds an ACTIVE assignment; nothing leaked or changed.
    let mine = h
        .get(
            &format!("/internal/v1/sessions/{s}/relay-number-assignment"),
            &c,
        )
        .await;
    assert_eq!(assignment(&mine)["status"], "ACTIVE");
    assert_eq!(
        h.count("SELECT count(*) FROM relay_number_assignments WHERE status = 'ACTIVE'")
            .await,
        1
    );
}
