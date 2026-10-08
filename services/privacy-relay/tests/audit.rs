//! Transactional evidence: state, audit event and outbox commit together; audit events
//! are immutable.

mod common;

use axum::http::StatusCode;
use cf_service_auth::ApplicationEnvironmentKind as Kind;
use common::*;
use serde_json::json;

#[tokio::test]
async fn every_authority_mutation_writes_state_audit_and_outbox_together() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(Kind::Production);
    let [a, b, cc] = principals(3).try_into().unwrap();
    let ctx = h.bidirectional(&c, &[a, b]).await;
    let id = ctx["contextId"].as_str().unwrap().to_string();
    let sid = h.app_session(&c, &id).await["relaySessionId"]
        .as_str()
        .unwrap()
        .to_string();
    h.command(&format!("/internal/v1/contexts/{id}/update"), &c, json!({ "expectedVersion": 1, "participants": [{ "principalId": a }, { "principalId": cc }] })).await;
    h.lifecycle(&c, &id, "suspend", 2).await;
    h.lifecycle(&c, &id, "resume", 3).await;
    h.command(
        &format!("/internal/v1/sessions/{sid}/suspend"),
        &c,
        json!({ "expectedVersion": 2 }),
    )
    .await;
    h.lifecycle(&c, &id, "revoke", 4).await;
    let events = h
        .get(&format!("/internal/v1/contexts/{id}/evidence"), &c)
        .await
        .json["auditEvents"]
        .clone();
    let mut types: Vec<String> = events
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["eventType"].as_str().unwrap().to_string())
        .collect();
    types.reverse();
    assert_eq!(
        types,
        [
            "CONTEXT_CREATED",
            "RELAY_SESSION_CREATED",
            "RELAY_SESSION_UPDATED",
            "CONTEXT_PARTICIPANTS_CHANGED",
            "ENFORCEMENT_REQUESTED",
            "ENFORCEMENT_APPLIED",
            "CONTEXT_SUSPENDED",
            "CONTEXT_RESUMED",
            "RELAY_SESSION_SUSPENDED",
            "RELAY_SESSION_REVOKED",
            "CONTEXT_REVOKED",
            "ENFORCEMENT_REQUESTED",
            "ENFORCEMENT_APPLIED",
        ]
    );
    // Gap-free sequence, one outbox envelope per audit event, all in the platform envelope.
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM relay_audit_events WHERE context_id = '{id}'"
        ))
        .await,
        13
    );
    assert_eq!(
        h.count(&format!(
            "SELECT max(sequence) FROM relay_audit_events WHERE context_id = '{id}'"
        ))
        .await,
        13
    );
    assert_eq!(
        h.count("SELECT count(*) FROM privacy_relay_outbox").await,
        13
    );
    assert_eq!(
        h.count("SELECT count(*) FROM privacy_relay_outbox o JOIN relay_audit_events e ON e.id = o.source_audit_event_id \
                 WHERE o.envelope->>'producer_service' = 'cfmeet.privacy-relay' AND o.envelope->'payload'->>'sequence' = e.sequence::text").await,
        13
    );
}

#[tokio::test]
async fn a_failure_before_the_outbox_rolls_the_whole_command_back() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(Kind::Production);
    let [a, b, cc] = principals(3).try_into().unwrap();
    let ctx = h.bidirectional(&c, &[a, b]).await;
    let id = ctx["contextId"].as_str().unwrap().to_string();
    let snapshot = dump_database(h.conn()).await;
    // Injected failure: the outbox refuses every insert.
    exec(h.conn(), "CREATE FUNCTION test_fail_outbox() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected outbox failure'; END; $$").await.unwrap();
    exec(h.conn(), "CREATE TRIGGER test_fail_outbox BEFORE INSERT ON privacy_relay_outbox FOR EACH ROW EXECUTE FUNCTION test_fail_outbox()").await.unwrap();
    for (path, body) in [
        (
            "/internal/v1/contexts".to_string(),
            context_request(&[a, b], "BIDIRECTIONAL", &[]),
        ),
        (
            format!("/internal/v1/contexts/{id}/update"),
            json!({ "expectedVersion": 1, "participants": [{ "principalId": a }, { "principalId": cc }] }),
        ),
        (
            format!("/internal/v1/contexts/{id}/revoke"),
            json!({ "expectedVersion": 1 }),
        ),
        (
            format!("/internal/v1/contexts/{id}/sessions"),
            json!({ "relayStrategy": "APP_ONLY" }),
        ),
    ] {
        let r = h.command(&path, &c, body).await;
        assert_eq!(
            (r.status, r.code()),
            (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR"),
            "{path}"
        );
        assert!(
            !r.text.contains("injected"),
            "database errors are never returned"
        );
    }
    assert_eq!(
        dump_database(h.conn()).await,
        snapshot,
        "nothing committed: no state, audit, job or command"
    );
    exec(
        h.conn(),
        "DROP TRIGGER test_fail_outbox ON privacy_relay_outbox",
    )
    .await
    .unwrap();
    // The same commands now succeed (no stale idempotency record was left behind).
    assert_eq!(
        h.lifecycle(&c, &id, "revoke", 1).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn audit_events_and_decisions_are_append_only() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(Kind::Production);
    let [a, b] = principals(2).try_into().unwrap();
    let ctx = h.bidirectional(&c, &[a, b]).await;
    let sid = h.app_session(&c, ctx["contextId"].as_str().unwrap()).await["relaySessionId"]
        .as_str()
        .unwrap()
        .to_string();
    h.authorize(&c, &sid, a, &[b]).await;
    for sql in [
        "UPDATE relay_audit_events SET event_type = 'CONTEXT_REVOKED'",
        "UPDATE relay_audit_events SET data = '{}'::jsonb",
        "DELETE FROM relay_audit_events",
        "UPDATE relay_authorization_decisions SET decision = 'AUTHORIZED', reason = 'AUTHORIZED'",
        "DELETE FROM relay_authorization_decisions",
        "UPDATE privacy_relay_outbox SET envelope = '{}'::jsonb",
        "DELETE FROM privacy_relay_outbox",
        "DELETE FROM communication_contexts",
        "DELETE FROM communication_context_participants",
        "DELETE FROM privacy_relay_sessions",
        "DELETE FROM privacy_relay_commands",
        "UPDATE communication_contexts SET tenant_id = gen_random_uuid()",
        "UPDATE communication_contexts SET external_context_reference = 'other'",
        "UPDATE communication_context_participants SET principal_ref = gen_random_uuid()",
    ] {
        assert!(exec(h.conn(), sql).await.is_err(), "{sql}");
    }
    // Appending still works.
    assert_eq!(h.authorize(&c, &sid, b, &[a]).await.status, StatusCode::OK);
    assert_eq!(
        h.count("SELECT count(*) FROM relay_authorization_decisions")
            .await,
        2
    );
}
