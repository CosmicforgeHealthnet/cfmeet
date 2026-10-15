//! Telephony core (Part E §39–§40): service identity, paid-action authorization proof,
//! durable commands before provider action, idempotency, feature flags, provider ids
//! never becoming CF Meet ids, and (Part H §78) the caller identity: only the session's
//! relay identity Privacy Relay selected, bound to the chosen interconnect, with its
//! assignment reference kept on the leg as identity evidence. Part I §81 / §100: a
//! rolled-back transaction leaves no leg, command or outbox intent, and the leg state
//! graph in Rust and the database trigger agree on every (from, to) pair.

mod common;

use axum::http::{Method, Request, StatusCode};
use common::{Harness, PrMode, RELAY_NUMBER};
use serde_json::json;
use telephony::config::TelecomFlags;
use uuid::Uuid;

const ORIGINATE: &str = "/internal/v1/legs/originate";

#[tokio::test]
async fn only_calling_may_command_telephony() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let contact = h.privacy.add_contact("+2348031234567", "NG");
    // Calling's service identity with the receiver-granted permission: accepted.
    let ok = h
        .calling(
            Method::POST,
            ORIGINATE,
            Some(h.originate_body(contact, Uuid::new_v4(), 1)),
        )
        .await;
    assert_eq!(ok.status, StatusCode::ACCEPTED, "{}", ok.text);
    // A customer API key is never accepted (no service token).
    let customer = h
        .raw(
            Request::builder()
                .method(Method::POST)
                .uri(ORIGINATE)
                .header(
                    "authorization",
                    "Bearer cfmk_live_0123456789abcdef0123456789abcdef",
                ),
            Some(h.originate_body(contact, Uuid::new_v4(), 1)),
        )
        .await;
    assert_eq!(customer.status, StatusCode::UNAUTHORIZED);
    assert_eq!(customer.code(), "INTERNAL_AUTHENTICATION_FAILED");
    // An unknown service is not trusted at all.
    let untrusted = h
        .send_as(
            &h.ids.untrusted,
            Method::POST,
            ORIGINATE,
            Some(&h.consumer),
            Some(h.originate_body(contact, Uuid::new_v4(), 1)),
        )
        .await;
    assert_eq!(untrusted.status, StatusCode::UNAUTHORIZED);
    // Another platform service (Privacy Relay) is not a Telephony issuer.
    let wrong = h
        .send_as(
            &h.ids.privacy_relay,
            Method::POST,
            ORIGINATE,
            Some(&h.consumer),
            Some(h.originate_body(contact, Uuid::new_v4(), 1)),
        )
        .await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    // A trusted caller without the specific permission is refused (receiver-authoritative).
    let Some(reader) = Harness::with(&["telephony-status.read"], common::all_on()).await else {
        return;
    };
    let denied = reader
        .calling(
            Method::POST,
            ORIGINATE,
            Some(reader.originate_body(contact, Uuid::new_v4(), 1)),
        )
        .await;
    assert_eq!(
        (denied.status, denied.code()),
        (StatusCode::FORBIDDEN, "INTERNAL_PERMISSION_DENIED")
    );
    // Status reads need their own permission too; unknown internal paths are not
    // enumerable without a token.
    let anon = h
        .raw(
            Request::builder()
                .method(Method::GET)
                .uri("/internal/v1/nope"),
            None,
        )
        .await;
    assert_eq!(anon.status, StatusCode::UNAUTHORIZED);
    assert_eq!(h.scalar("SELECT count(*) FROM telephony_legs").await, 1);
}

#[tokio::test]
async fn telephony_cannot_execute_originate_without_authorization_proof() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let contact = h.privacy.add_contact("+2348031234567", "NG");
    let refuse = |status: StatusCode, code: &'static str| (status, code);
    type Mutation = Box<dyn Fn(&mut serde_json::Value)>;
    let cases: Vec<(Mutation, (StatusCode, &str))> = vec![
        (
            Box::new(|b| {
                b["authorization"]["validUntil"] = json!("2001-01-01T00:00:00Z");
                b["authorization"]["decidedAt"] = json!("2000-12-31T23:00:00Z");
            }),
            refuse(StatusCode::FORBIDDEN, "AUTHORIZATION_PROOF_EXPIRED"),
        ),
        (
            Box::new(|b| {
                b["authorization"]["relaySessionVersion"] = json!(0);
            }),
            refuse(StatusCode::FORBIDDEN, "AUTHORIZATION_PROOF_INVALID"),
        ),
        (
            Box::new(|b| {
                b["destination"]["grantSecret"] = json!("short");
            }),
            refuse(StatusCode::FORBIDDEN, "AUTHORIZATION_PROOF_INVALID"),
        ),
        (
            Box::new(|b| {
                b["destination"]["grantExpiresAt"] = json!("2001-01-01T00:00:00Z");
            }),
            refuse(StatusCode::FORBIDDEN, "AUTHORIZATION_PROOF_EXPIRED"),
        ),
        (
            Box::new(|b| {
                b["spend"]["reservedAmountMinor"] = json!(0);
            }),
            refuse(StatusCode::FORBIDDEN, "SPEND_RESERVATION_REQUIRED"),
        ),
        (
            Box::new(|b| {
                b["spend"]["maxDurationSeconds"] = json!(0);
            }),
            refuse(StatusCode::FORBIDDEN, "SPEND_RESERVATION_REQUIRED"),
        ),
        (
            Box::new(|b| {
                b.as_object_mut().unwrap().remove("authorization");
            }),
            refuse(StatusCode::BAD_REQUEST, "VALIDATION_ERROR"),
        ),
        (
            Box::new(|b| {
                b.as_object_mut().unwrap().remove("spend");
            }),
            refuse(StatusCode::BAD_REQUEST, "VALIDATION_ERROR"),
        ),
        // No client-chosen caller id, no raw destination, ever.
        (
            Box::new(|b| {
                b["fromNumber"] = json!(RELAY_NUMBER);
            }),
            refuse(StatusCode::BAD_REQUEST, "VALIDATION_ERROR"),
        ),
        (
            Box::new(|b| {
                b["destination"]["e164"] = json!("+2348031234567");
            }),
            refuse(StatusCode::BAD_REQUEST, "VALIDATION_ERROR"),
        ),
        // The signed delegated scope must be the command's scope.
        (
            Box::new(|b| {
                b["scope"]["tenantId"] = json!(Uuid::new_v4());
            }),
            refuse(StatusCode::FORBIDDEN, "AUTHORIZATION_PROOF_INVALID"),
        ),
    ];
    for (mutate, (status, code)) in cases {
        let mut body = h.originate_body(contact, Uuid::new_v4(), 1);
        mutate(&mut body);
        let r = h.calling(Method::POST, ORIGINATE, Some(body)).await;
        assert_eq!((r.status, r.code()), (status, code), "{}", r.text);
    }
    // Without a delegated context at all.
    let r = h
        .send_as(
            &h.ids.calling,
            Method::POST,
            ORIGINATE,
            None,
            Some(h.originate_body(contact, Uuid::new_v4(), 1)),
        )
        .await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::FORBIDDEN, "AUTHORIZATION_PROOF_INVALID")
    );
    // Nothing was persisted, nothing reached a provider, nothing was resolved.
    assert_eq!(h.scalar("SELECT count(*) FROM telephony_legs").await, 0);
    assert_eq!(h.scalar("SELECT count(*) FROM telephony_commands").await, 0);
    h.run().await;
    assert!(h.provider_a.requests().is_empty());
    assert!(h.privacy.resolutions().is_empty());
}

#[tokio::test]
async fn the_command_is_durable_before_any_provider_action() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (leg, body) = h.originate().await;
    // Accepted for execution: persisted, nothing sent yet.
    assert_eq!(h.leg_status(leg).await, "PLANNED");
    assert_eq!(
        h.text(&format!(
            "SELECT status FROM telephony_commands WHERE telephony_leg_id = '{leg}'"
        ))
        .await
        .unwrap(),
        "PENDING"
    );
    assert!(h.provider_a.requests().is_empty());
    assert!(h.privacy.resolutions().is_empty());
    // The worker executes it: resolution with the grant (once, for this execution),
    // fence, provider request, binding, normalized event.
    h.run().await;
    assert_eq!(h.provider_a.effects().len(), 1);
    let resolutions = h.privacy.resolutions();
    assert_eq!(resolutions.len(), 1);
    assert_eq!(
        resolutions[0].1.to_string(),
        body["destination"]["grantId"].as_str().unwrap()
    );
    assert_eq!(h.leg_status(leg).await, "ORIGINATING");
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM telephony_commands WHERE telephony_leg_id = '{leg}' AND status = 'SUCCEEDED' \
             AND transmitted_at IS NOT NULL AND grant_secret IS NULL"
        ))
        .await,
        1
    );
    assert_eq!(h.calling.event_types_for(leg), vec!["LEG_ACCEPTED"]);
    // The leg view and every event speak CF Meet ids only.
    let view = h
        .calling(Method::GET, &format!("/internal/v1/legs/{leg}"), None)
        .await;
    assert_eq!(view.status, StatusCode::OK);
    assert_eq!(view.json["leg"]["callLegId"], body["call"]["callLegId"]);
    assert_eq!(view.json["leg"]["callId"], body["call"]["callId"]);
    let everything = format!("{} {:?}", view.text, h.calling.events());
    for forbidden in ["dp-a", "dp-", "+234", "provider", "grant-secret", "cfm-"] {
        assert!(
            !everything.contains(forbidden),
            "{forbidden} leaked: {everything}"
        );
    }
}

#[tokio::test]
async fn duplicate_commands_have_one_provider_effect() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let contact = h.privacy.add_contact("+2348031234567", "NG");
    let body = h.originate_body(contact, Uuid::new_v4(), 1);
    let (a, b) = tokio::join!(
        h.calling(Method::POST, ORIGINATE, Some(body.clone())),
        h.calling(Method::POST, ORIGINATE, Some(body.clone()))
    );
    let statuses = [a.status, b.status];
    assert!(
        statuses.contains(&StatusCode::ACCEPTED),
        "{statuses:?} {} {}",
        a.text,
        b.text
    );
    assert!(statuses
        .iter()
        .all(|s| *s == StatusCode::ACCEPTED || *s == StatusCode::OK));
    let c = h.calling(Method::POST, ORIGINATE, Some(body.clone())).await;
    assert_eq!(c.status, StatusCode::OK);
    assert_eq!(c.json["replayed"], true);
    let accepted = if a.status == StatusCode::ACCEPTED {
        &a
    } else {
        &b
    };
    assert_eq!(
        c.json["leg"]["telephonyLegId"],
        accepted.json["leg"]["telephonyLegId"]
    );
    // A different body under the same key is a conflict.
    let mut changed = body.clone();
    changed["destination"]["country"] = json!("GH");
    let d = h.calling(Method::POST, ORIGINATE, Some(changed)).await;
    assert_eq!(
        (d.status, d.code()),
        (StatusCode::CONFLICT, "IDEMPOTENCY_CONFLICT")
    );
    // A second execution of the same CallLeg (new key) while the first is live: refused.
    let mut second = body.clone();
    second["idempotencyKey"] = json!("another-key");
    second["commandId"] = json!(Uuid::new_v4());
    second["expectedGeneration"] = json!(2);
    let e = h.calling(Method::POST, ORIGINATE, Some(second)).await;
    assert_eq!(
        (e.status, e.code()),
        (StatusCode::CONFLICT, "EXECUTION_IN_PROGRESS")
    );
    for _ in 0..3 {
        h.run().await;
    }
    assert_eq!(h.provider_a.effects().len(), 1);
    assert_eq!(h.scalar("SELECT count(*) FROM telephony_legs").await, 1);
}

#[tokio::test]
async fn public_network_flags_stop_paid_origination_only() {
    let Some(h) = Harness::with(
        common::CALLING_PERMISSIONS,
        TelecomFlags::new(true, true, true, true),
    )
    .await
    else {
        return;
    };
    let (live, _) = h.originate().await;
    h.run().await;
    h.flags.public_outbound.set(false);
    let contact = h.privacy.add_contact("+2348031234567", "NG");
    let r = h
        .calling(
            Method::POST,
            ORIGINATE,
            Some(h.originate_body(contact, Uuid::new_v4(), 1)),
        )
        .await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::SERVICE_UNAVAILABLE, "PUBLIC_OUTBOUND_DISABLED")
    );
    h.flags.telephony.set(false);
    let r = h
        .calling(
            Method::POST,
            ORIGINATE,
            Some(h.originate_body(contact, Uuid::new_v4(), 1)),
        )
        .await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::SERVICE_UNAVAILABLE, "TELEPHONY_DISABLED")
    );
    // Terminating, reading and health keep working.
    let t = h
        .calling(
            Method::POST,
            &format!("/internal/v1/legs/{live}/terminate"),
            Some(json!({ "schemaVersion": "1", "commandId": Uuid::new_v4(), "idempotencyKey": "t-1",
                         "correlationId": cf_service_auth::CorrelationId::generate().as_str(), "reason": "CALL_ENDED" })),
        )
        .await;
    assert_eq!(t.status, StatusCode::ACCEPTED, "{}", t.text);
    h.run().await;
    assert_eq!(h.leg_status(live).await, "ENDED");
    let health = h
        .raw(Request::builder().method(Method::GET).uri("/health"), None)
        .await;
    assert_eq!(health.status, StatusCode::OK);
    assert_eq!(
        health.json,
        json!({ "status": "ok", "service": "telephony" })
    );
    // A command accepted while enabled but executed after the switch is never sent.
    h.flags.telephony.set(true);
    h.flags.public_outbound.set(true);
    let (pending, _) = h.originate().await;
    h.flags.public_outbound.set(false);
    h.run().await;
    assert_eq!(h.leg_status(pending).await, "FAILED");
    assert_eq!(h.provider_a.effects().len(), 1);
}

#[tokio::test]
async fn privacy_relay_down_fails_closed_before_any_destination_exists() {
    let Some(h) = Harness::new().await else {
        return;
    };
    h.privacy.set_mode(PrMode::Unavailable);
    let failures = || {
        telephony::metrics::counter(
            "telephony_dependency_failures_total",
            &[
                ("dependency", "PRIVACY_RELAY"),
                ("operation", "RESOLVE_FOR_EXECUTION"),
            ],
        )
    };
    let before = failures();
    let (leg, _) = h.originate().await;
    // Retries while the grant is valid, never sends.
    h.run().await;
    assert!(h.provider_a.requests().is_empty());
    assert_eq!(h.leg_status(leg).await, "PLANNED");
    // Telephony observed the outage: ONE open incident (repeats are occurrences) and a
    // bounded metric.
    h.advance(2);
    h.run().await;
    let relay_incidents =
        "SELECT count(*) FROM telephony_incidents WHERE category = 'PRIVACY_RELAY_UNAVAILABLE' \
                           AND status = 'OPEN' AND dedupe_key = 'privacy-relay-unavailable'";
    assert_eq!(h.scalar(relay_incidents).await, 1);
    assert_eq!(
        h.scalar(
            "SELECT occurrences::bigint FROM telephony_incidents WHERE category = 'PRIVACY_RELAY_UNAVAILABLE'"
        )
        .await,
        2
    );
    assert!(failures() >= before + 2);
    // Denied: definite, not sent — and Privacy Relay answered, so the incident resolves.
    h.privacy.set_mode(PrMode::Deny);
    h.advance(5);
    h.run().await;
    assert_eq!(h.scalar(relay_incidents).await, 0);
    assert_eq!(h.leg_status(leg).await, "FAILED");
    assert!(h.provider_a.requests().is_empty());
    assert_eq!(
        h.text(&format!(
            "SELECT end_reason FROM telephony_legs WHERE id = '{leg}'"
        ))
        .await
        .unwrap(),
        "NOT_SENT"
    );
    assert_eq!(h.calling.event_types_for(leg), vec!["LEG_FAILED"]);
    let failed = h
        .calling
        .events()
        .into_iter()
        .find(|e| e["telephonyLegId"] == json!(leg))
        .unwrap();
    assert_eq!(failed["definite"], true);
}

/// The command's result code and the leg's status of one origination.
async fn originated(h: &Harness) -> (Uuid, String, Option<String>) {
    let (leg, _) = h.originate().await;
    h.run().await;
    let code = h
        .text(&format!(
            "SELECT result_code FROM telephony_commands WHERE telephony_leg_id = '{leg}' AND kind = 'ORIGINATE'"
        ))
        .await;
    (leg, h.leg_status(leg).await, code)
}

#[tokio::test]
async fn the_caller_identity_must_be_the_relay_identity_bound_to_the_chosen_interconnect() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let now = h.clock_now();
    // A third interconnect (another provider) with its own relay number.
    h.sql(&format!(
        "INSERT INTO telecom_providers (id, adapter, capabilities, status, credential_ref, max_concurrent_legs, created_at, updated_at, health) \
         VALUES ('dp-c', 'DETERMINISTIC_TEST', ARRAY['OUTBOUND_VOICE'], 'ENABLED', 'dp-secret', 100, '{now}', '{now}', 'HEALTHY'); \
         INSERT INTO telecom_routes (id, provider_id, countries, capabilities, priority, status, max_concurrent_legs, \
           max_call_seconds, created_at, updated_at, health) \
         VALUES ('ng-c', 'dp-c', ARRAY['NG'], ARRAY['VOICE_OUTBOUND'], 30, 'ENABLED', 50, 3600, '{now}', '{now}', 'HEALTHY'); \
         INSERT INTO telecom_number_bindings (public_e164, route_id, capabilities, status, created_at, updated_at) \
         VALUES ('+2342010000003', 'ng-c', ARRAY['VOICE_OUTBOUND'], 'ENABLED', '{now}', '{now}'), \
                ('+2342010000004', 'ng-a', ARRAY['VOICE_OUTBOUND'], 'DISABLED', '{now}', '{now}'), \
                ('+2342010000005', 'ng-a', ARRAY['VOICE_INBOUND'], 'ENABLED', '{now}', '{now}')"
    ))
    .await;
    for (identity, why) in [
        // Not in telecom_number_bindings at all.
        (Some("+2342010000002"), "unbound"),
        // Bound only to another provider's interconnect.
        (Some("+2342010000003"), "bound to another provider"),
        // Bound, but the binding is disabled.
        (Some("+2342010000004"), "binding disabled"),
        // Bound for inbound only.
        (Some("+2342010000005"), "inbound-only binding"),
        // Privacy Relay selected no identity.
        (None, "missing"),
    ] {
        *h.privacy.relay_number.lock().unwrap() = identity.map(str::to_string);
        let (leg, status, code) = originated(&h).await;
        assert_eq!(status, "FAILED", "{why}");
        assert_eq!(
            code.as_deref(),
            Some("CALLER_IDENTITY_UNAVAILABLE"),
            "{why}"
        );
        assert_eq!(
            h.text(&format!(
                "SELECT end_reason FROM telephony_legs WHERE id = '{leg}'"
            ))
            .await
            .as_deref(),
            Some("NOT_SENT"),
            "{why}"
        );
        assert!(
            h.provider_a.requests().is_empty() && h.provider_b.requests().is_empty(),
            "{why}: zero provider originates"
        );
        assert_eq!(
            h.scalar(&format!(
                "SELECT count(*) FROM telephony_legs WHERE id = '{leg}' AND relay_assignment_ref IS NOT NULL"
            ))
            .await,
            0,
            "{why}: no identity evidence for a call that never left"
        );
    }
    // The session's bound relay identity: the request carries it and the leg keeps
    // Privacy Relay's assignment reference as identity evidence.
    *h.privacy.relay_number.lock().unwrap() = Some(RELAY_NUMBER.to_string());
    let assignment = Uuid::new_v4();
    *h.privacy.assignment_ref.lock().unwrap() = Some(assignment);
    let (leg, status, _) = originated(&h).await;
    assert_eq!(status, "ORIGINATING");
    assert_eq!(h.provider_a.requests().len(), 1);
    assert_eq!(
        h.text(&format!(
            "SELECT relay_assignment_ref::text FROM telephony_legs WHERE id = '{leg}'"
        ))
        .await,
        Some(assignment.to_string())
    );
    // Bound to provider b only: the executor skips a (identity not bound there) and
    // takes b — never a with an unbound identity.
    h.sql("UPDATE telecom_number_bindings SET status = 'DISABLED' WHERE route_id = 'ng-a'")
        .await;
    let (leg, status, _) = originated(&h).await;
    assert_eq!(status, "ORIGINATING");
    assert_eq!(
        h.text(&format!(
            "SELECT route_id FROM telephony_legs WHERE id = '{leg}'"
        ))
        .await
        .as_deref(),
        Some("ng-b")
    );
    assert_eq!(
        (h.provider_a.requests().len(), h.provider_b.requests().len()),
        (1, 1)
    );
}

#[tokio::test]
async fn health_and_readiness_are_safe() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let r = h.calling(Method::GET, "/internal/v1/readiness", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["sipEdge"], "UNAVAILABLE");
    assert_eq!(r.json["interconnects"]["configured"], 2);
    for forbidden in ["dp-a", "secret", "password", "trunk/"] {
        assert!(!r.text.contains(forbidden), "{forbidden}");
    }
    let a = h
        .calling(
            Method::GET,
            "/internal/v1/routes/availability?country=NG&capability=VOICE_OUTBOUND",
            None,
        )
        .await;
    assert_eq!(a.status, StatusCode::OK, "{}", a.text);
    let refs: Vec<&str> = a.json["routes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["routeRef"].as_str().unwrap())
        .collect();
    assert_eq!(refs, ["ng-a", "ng-b"]);
    assert!(!a.text.contains("dp-"));
}

#[tokio::test]
async fn a_rolled_back_transaction_leaves_no_leg_command_or_outbox_intent() {
    let Some(h) = Harness::new().await else {
        return;
    };
    // Intake: the command row (written after the leg, in the same transaction) fails.
    h.sql(
        "CREATE FUNCTION test_refuse_command() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN \
           RAISE EXCEPTION 'database failure (injected)'; END $$",
    )
    .await;
    h.sql(
        "CREATE TRIGGER test_refuse_command BEFORE INSERT ON telephony_commands \
         FOR EACH ROW EXECUTE FUNCTION test_refuse_command()",
    )
    .await;
    let contact = h.privacy.add_contact("+2348031234567", "NG");
    let call_leg = Uuid::new_v4();
    let body = h.originate_body(contact, call_leg, 1);
    let r = h.calling(Method::POST, ORIGINATE, Some(body.clone())).await;
    assert!(r.status.is_server_error(), "{} {}", r.status, r.text);
    for table in [
        "telephony_legs",
        "telephony_commands",
        "telephony_outbox",
        "provider_call_bindings",
    ] {
        assert_eq!(
            h.scalar(&format!("SELECT count(*) FROM {table}")).await,
            0,
            "{table}"
        );
    }
    h.run().await;
    assert!(h.provider_a.requests().is_empty());
    assert!(h.calling.events().is_empty());
    // The same command succeeds once the database accepts it (nothing half-written
    // blocks it: no live execution, no idempotency row).
    h.sql("DROP TRIGGER test_refuse_command ON telephony_commands")
        .await;
    let r = h.calling(Method::POST, ORIGINATE, Some(body)).await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text);
    let leg: Uuid = r.json["leg"]["telephonyLegId"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    h.run().await;
    assert_eq!(h.calling.event_types_for(leg), vec!["LEG_ACCEPTED"]);

    // A state transition: the outbox write of a provider fact fails, so the provider
    // event record, the leg change and the event intent all roll back together.
    h.sql(
        "CREATE FUNCTION test_refuse_ringing() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN \
           IF NEW.event_type = 'LEG_RINGING' THEN RAISE EXCEPTION 'database failure (injected)'; END IF; \
           RETURN NEW; END $$",
    )
    .await;
    h.sql(
        "CREATE TRIGGER test_refuse_ringing BEFORE INSERT ON telephony_outbox \
         FOR EACH ROW EXECUTE FUNCTION test_refuse_ringing()",
    )
    .await;
    let correlation = h.correlation_of(leg).await;
    let ringing = h.provider_a.progress(
        &correlation,
        telephony::domain::NormalizedEvent::LegRinging,
        h.now(),
    );
    assert!(h
        .svc
        .ingest_provider_event("dp-a", ringing.clone())
        .await
        .is_err());
    assert_eq!(h.leg_status(leg).await, "ORIGINATING");
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM provider_events WHERE provider_event_id = '{}'",
            ringing.provider_event_id
        ))
        .await,
        0
    );
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM telephony_outbox WHERE telephony_leg_id = '{leg}' AND event_type = 'LEG_RINGING'"
        ))
        .await,
        0
    );
    assert_eq!(
        h.scalar(&format!(
            "SELECT event_sequence FROM telephony_legs WHERE id = '{leg}'"
        ))
        .await,
        1,
        "no sequence number was consumed"
    );
    // The provider's redelivery then applies exactly once.
    h.sql("DROP TRIGGER test_refuse_ringing ON telephony_outbox")
        .await;
    h.svc
        .ingest_provider_event("dp-a", ringing.clone())
        .await
        .unwrap();
    h.svc.ingest_provider_event("dp-a", ringing).await.unwrap();
    h.run().await;
    assert_eq!(h.leg_status(leg).await, "RINGING");
    assert_eq!(
        h.calling.event_types_for(leg),
        vec!["LEG_ACCEPTED", "LEG_RINGING"]
    );
}

/// Every (from, to) pair of leg states: the Rust graph (`LegStatus::allowed_targets`)
/// and the database trigger `trg_telephony_legs_transition` agree — both as declared
/// (the trigger's edge list) and as enforced (an UPDATE of a real row per pair).
#[tokio::test]
async fn every_leg_state_pair_agrees_between_the_rust_graph_and_the_database() {
    use sea_orm::{ConnectionTrait, TransactionTrait};
    use std::collections::BTreeSet;
    use telephony::domain::LegStatus;
    let Some(h) = Harness::new().await else {
        return;
    };
    // Declared: the trigger's arguments are exactly the Rust edges.
    let declared: BTreeSet<String> = h
        .text(
            "SELECT array_to_string(string_to_array(substring(encode(tgargs, 'escape') from position('\\000' in encode(tgargs, 'escape')) + 4), '\\000'), ',') \
             FROM pg_trigger WHERE tgname = 'trg_telephony_legs_transition'",
        )
        .await
        .unwrap()
        .split(',')
        .filter(|e| !e.is_empty())
        .map(str::to_string)
        .collect();
    let rust: BTreeSet<String> = LegStatus::ALL
        .iter()
        .flat_map(|from| {
            from.allowed_targets()
                .iter()
                .map(move |to| format!("{}>{}", from.as_str(), to.as_str()))
        })
        .collect();
    assert_eq!(declared, rust, "trigger edges vs Rust graph");

    // Enforced: one row per source state, every target attempted (rolled back).
    let now = h.clock_now();
    let mut table = Vec::new();
    for from in LegStatus::ALL {
        let id = Uuid::new_v4();
        let terminal = from.is_terminal();
        let (ended_at, end_reason) = if terminal {
            (format!("'{now}'"), "'COMPLETED'")
        } else {
            ("NULL".to_string(), "NULL")
        };
        let failure = if *from == LegStatus::Failed {
            "'EXECUTION'"
        } else {
            "NULL"
        };
        h.sql(&format!(
            "INSERT INTO telephony_legs (id, calling_call_id, calling_call_leg_id, tenant_id, application_id, \
               environment_id, direction, route_class, status, generation, inbound_resolution_id, \
               max_duration_seconds, media_attachment, usage_status, ended_at, end_reason, failure_category, \
               created_at, updated_at) \
             VALUES ('{id}', gen_random_uuid(), gen_random_uuid(), gen_random_uuid(), gen_random_uuid(), \
               gen_random_uuid(), 'INBOUND', 'PSTN_TO_APP', '{}', 1, gen_random_uuid(), 3600, 'NONE', 'PENDING', \
               {ended_at}, {end_reason}, {failure}, '{now}', '{now}')",
            from.as_str()
        ))
        .await;
        for to in LegStatus::ALL {
            if to == from {
                continue;
            }
            // The target's own row invariants (terminal stamps), so only the graph decides.
            let stamps = match (from.is_terminal(), to) {
                (false, LegStatus::Ended) => {
                    format!(", ended_at = '{now}', end_reason = 'COMPLETED'")
                }
                (false, LegStatus::Failed) => format!(
                    ", ended_at = '{now}', end_reason = 'FAILED', failure_category = 'EXECUTION'"
                ),
                _ => String::new(),
            };
            // Each attempt in its own transaction, always rolled back.
            let txn = h.db.conn.begin().await.unwrap();
            let attempt = txn
                .execute_unprepared(&format!(
                    "UPDATE telephony_legs SET status = '{}'{stamps} WHERE id = '{id}'",
                    to.as_str()
                ))
                .await
                .map(|_| ())
                .map_err(|e| e.to_string());
            txn.rollback().await.unwrap();
            let refused_by_graph = attempt
                .as_ref()
                .err()
                .is_some_and(|e| e.contains("is not allowed"));
            if let Err(e) = &attempt {
                assert!(
                    refused_by_graph,
                    "{} -> {}: refused by something other than the graph: {e}",
                    from.as_str(),
                    to.as_str()
                );
            }
            table.push((*from, *to, !refused_by_graph));
        }
    }
    assert_eq!(
        table.len(),
        LegStatus::ALL.len() * (LegStatus::ALL.len() - 1)
    );
    for (from, to, database) in &table {
        assert_eq!(
            *database,
            from.can_transition_to(*to),
            "{} -> {}: database {} vs Rust {}",
            from.as_str(),
            to.as_str(),
            database,
            from.can_transition_to(*to)
        );
    }
    // Both directions are exercised: allowed and refused pairs exist.
    assert!(table.iter().any(|(_, _, ok)| *ok) && table.iter().any(|(_, _, ok)| !*ok));
}
