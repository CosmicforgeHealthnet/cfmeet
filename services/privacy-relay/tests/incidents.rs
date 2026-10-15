//! §86: Privacy Relay incidents — deduplicated while OPEN, raised where the service
//! refuses to guess (ambiguous relay mapping, unreadable contact key; the dead-letter
//! categories are proven in `outbox_publication.rs` / `enforcement.rs`), safe summaries
//! only, append-only evidence, and the operator CLI (`privacy-relay-admin incidents
//! list|resolve`) that resolves with an audited operator identity in one transaction.

mod common;

use std::process::{Command, Output};

use axum::http::{Method, StatusCode};
use cf_service_auth::ApplicationEnvironmentKind;
use common::{at, exec, iso, rows, scalar, test_vault_keys, Consumer, Harness};
use privacy_relay::incidents::{self, IncidentCategory};
use serde_json::{json, Value};
use uuid::Uuid;

const RELAY: &str = "+44 20 7946 0500";
const OTHER_RELAY: &str = "+44 20 7946 0501";
const CONTACT: &str = "+44 20 7946 0123";
const CONTACT_DIGITS: &str = "2079460123";

fn admin(url: &str, args: &[&str], operator_env: Option<&str>) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_privacy-relay-admin"));
    cmd.args(args).env_clear().env("DATABASE_URL", url);
    if let Some(op) = operator_env {
        cmd.env("CF_OPERATOR_ID", op);
    }
    cmd.output().expect("run privacy-relay-admin")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[tokio::test]
async fn incidents_deduplicate_while_open_and_resolve_with_an_audited_operator() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let key = incidents::dedupe_key(IncidentCategory::OutboxDeadLetter, &["MALFORMED"]);
    let first = incidents::raise(h.conn(), IncidentCategory::OutboxDeadLetter, &key, at(1))
        .await
        .unwrap();
    let again = incidents::raise(h.conn(), IncidentCategory::OutboxDeadLetter, &key, at(2))
        .await
        .unwrap();
    assert_eq!(first, again, "deduplicated while OPEN");
    let row = rows(
        h.conn(),
        "SELECT occurrences::text, status, severity, (last_seen_at > first_seen_at)::text FROM privacy_relay_incidents",
    )
    .await;
    assert_eq!(row, vec![vec!["2", "OPEN", "MEDIUM", "true"]]);

    // The operator CLI lists the safe view.
    let list = admin(&h.db.url, &["incidents", "list"], None);
    assert!(list.status.success());
    let out = stdout(&list);
    assert!(out.contains(&first.to_string()) && out.contains("OUTBOX_DEAD_LETTER"));
    assert!(out.contains("occurrences 2"), "{out}");

    // Resolving changes state: refused (exit 2, nothing changed) without an operator.
    let id = first.to_string();
    let refused = admin(&h.db.url, &["incidents", "resolve", "--id", &id], None);
    assert_eq!(refused.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("--operator"));
    assert_eq!(
        h.count("SELECT count(*) FROM privacy_relay_incidents WHERE status = 'OPEN'")
            .await,
        1
    );
    assert_eq!(
        h.count("SELECT count(*) FROM privacy_relay_operator_audit")
            .await,
        0
    );
    let ok = admin(
        &h.db.url,
        &[
            "incidents",
            "resolve",
            "--id",
            &id,
            "--operator",
            "ops.alice",
        ],
        None,
    );
    assert!(
        ok.status.success(),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    assert_eq!(
        rows(
            h.conn(),
            "SELECT status, resolved_by FROM privacy_relay_incidents"
        )
        .await,
        vec![vec!["RESOLVED", "ops.alice"]]
    );
    let audit = rows(
        h.conn(),
        "SELECT actor, action, target_kind, target_id::text, detail->>'category' FROM privacy_relay_operator_audit",
    )
    .await;
    assert_eq!(
        audit,
        vec![vec![
            "ops.alice".to_string(),
            "INCIDENT_RESOLVE".into(),
            "INCIDENT".into(),
            id.clone(),
            "OUTBOX_DEAD_LETTER".into()
        ]]
    );
    // Already resolved: refused, no second audit row.
    let twice = admin(
        &h.db.url,
        &[
            "incidents",
            "resolve",
            "--id",
            &id,
            "--operator",
            "ops.alice",
        ],
        None,
    );
    assert_eq!(twice.status.code(), Some(1));
    assert_eq!(
        h.count("SELECT count(*) FROM privacy_relay_operator_audit")
            .await,
        1
    );
    // The condition recurs: a NEW incident opens (the resolved one is final history).
    let reopened = incidents::raise(h.conn(), IncidentCategory::OutboxDeadLetter, &key, at(3))
        .await
        .unwrap();
    assert_ne!(reopened, first);
    // CF_OPERATOR_ID names the operator when --operator is absent.
    let by_env = admin(
        &h.db.url,
        &["incidents", "resolve", "--id", &reopened.to_string()],
        Some("ops.bob"),
    );
    assert!(by_env.status.success());
    assert_eq!(
        h.count("SELECT count(*) FROM privacy_relay_operator_audit WHERE actor = 'ops.bob'")
            .await,
        1
    );
    assert!(stdout(&admin(&h.db.url, &["incidents", "list"], None))
        .trim()
        .is_empty());
    assert!(stdout(&admin(
        &h.db.url,
        &["incidents", "list", "--all", "true"],
        None
    ))
    .contains("resolved by ops.bob"));

    // Append-only evidence: no delete, no truncate, no reopening, no audit edits.
    for sql in [
        "DELETE FROM privacy_relay_incidents",
        "TRUNCATE privacy_relay_incidents",
        "UPDATE privacy_relay_incidents SET status = 'OPEN', resolved_at = NULL, resolved_by = NULL",
        "UPDATE privacy_relay_incidents SET dedupe_key = 'OUTBOX_DEAD_LETTER:OTHER'",
        "UPDATE privacy_relay_operator_audit SET actor = 'mallory'",
        "DELETE FROM privacy_relay_operator_audit",
        "TRUNCATE privacy_relay_operator_audit",
    ] {
        assert!(exec(h.conn(), sql).await.is_err(), "{sql}");
    }
    // A summary can never carry a number, an address or a SIP URI.
    for summary in ["call +44 20", "number 2079460123", "a@b", "sip:x"] {
        let insert = format!(
            "INSERT INTO privacy_relay_incidents (id, dedupe_key, category, severity, status, summary, \
               first_seen_at, last_seen_at) VALUES (gen_random_uuid(), 'OUTBOX_DEAD_LETTER:X', \
               'OUTBOX_DEAD_LETTER', 'LOW', 'OPEN', '{summary}', now(), now())"
        );
        assert!(exec(h.conn(), &insert).await.is_err(), "{summary}");
    }
}

/// A CALLBACK_RELAY context (principal + VERIFIED contact) and a session with a relay
/// number assigned; returns the session id.
async fn assigned_session(h: &Harness, c: &Consumer, number: &str) -> String {
    let principal = Uuid::new_v4();
    let contact = h.create_contact(c, number).await["contactReference"]
        .as_str()
        .unwrap()
        .to_string();
    h.verify_contact(c, &contact, "VERIFIED", None).await;
    let mut req = common::context_request(&[principal], "BIDIRECTIONAL", &[]);
    req["participants"] = json!([{ "principalId": principal }, { "contactReferenceId": contact }]);
    req["allowedRelayStrategies"] = json!(["CALLBACK_RELAY_ALLOWED"]);
    req["expiresAt"] = json!(iso(at(120)));
    let ctx = h.create_context(c, req).await;
    let session = h
        .create_session(
            c,
            ctx["contextId"].as_str().unwrap(),
            json!({ "relayStrategy": "CALLBACK_RELAY_ALLOWED" }),
        )
        .await["relaySessionId"]
        .as_str()
        .unwrap()
        .to_string();
    let r = h.assign_relay_number(c, &session).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    session
}

#[tokio::test]
async fn an_ambiguous_relay_mapping_is_refused_and_raises_one_deduplicated_incident() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(ApplicationEnvironmentKind::Production);
    let number = h.add_relay_number(RELAY, "pb-amb-1").await;
    h.add_relay_number(OTHER_RELAY, "pb-amb-2").await;
    h.clock.set(at(1));
    let first = assigned_session(&h, &c, CONTACT).await;
    let second = assigned_session(&h, &c, "+44 20 7946 0124").await;
    // Corruption the schema forbids (one holder per number) is forged behind its back to
    // prove the runtime refuses to choose.
    for sql in [
        "DROP INDEX uq_relay_assignments_holder".to_string(),
        "ALTER TABLE relay_number_assignments DISABLE TRIGGER USER".to_string(),
        format!(
            "UPDATE relay_number_assignments SET relay_number_id = '{number}', assignment_generation = 99 \
             WHERE relay_number_id <> '{number}' AND relay_session_id IN ('{first}', '{second}')"
        ),
    ] {
        exec(h.conn(), &sql).await.unwrap();
    }
    assert_eq!(
        scalar(
            h.conn(),
            &format!("SELECT count(*) FROM relay_number_assignments WHERE relay_number_id = '{number}' AND status = 'ACTIVE'")
        )
        .await,
        2
    );
    for _ in 0..2 {
        let r = h
            .telephony(
                "/internal/v1/relay-routing/resolve-inbound",
                json!({
                    "calledRelayIdentity": RELAY,
                    "providerEventReference": format!("evt-{}", Uuid::new_v4().simple()),
                    "providerObservedAt": iso(at(2)),
                }),
            )
            .await;
        assert_eq!(
            (r.status, r.code()),
            (StatusCode::CONFLICT, "RELAY_MAPPING_AMBIGUOUS"),
            "{}",
            r.text
        );
    }
    let incident = rows(
        h.conn(),
        "SELECT category, severity, occurrences::text, dedupe_key, summary FROM privacy_relay_incidents",
    )
    .await;
    assert_eq!(incident.len(), 1, "{incident:?}");
    assert_eq!(
        incident[0][..3],
        ["RELAY_MAPPING_AMBIGUOUS", "CRITICAL", "2"].map(String::from)
    );
    assert_eq!(
        incident[0][3],
        format!("RELAY_MAPPING_AMBIGUOUS:{}", number.simple())
    );
    assert!(!incident[0][4].contains("7946"));
    assert_eq!(
        scalar(
            h.conn(),
            "SELECT count(*) FROM relay_inbound_resolutions WHERE outcome = 'AMBIGUOUS'"
        )
        .await,
        2
    );
}

#[tokio::test]
async fn an_unreadable_contact_key_fails_closed_and_raises_an_incident() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(ApplicationEnvironmentKind::Production);
    let principal = Uuid::new_v4();
    let contact = h.create_contact(&c, CONTACT).await["contactReference"]
        .as_str()
        .unwrap()
        .to_string();
    h.verify_contact(&c, &contact, "VERIFIED", None).await;
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
        .await["relaySessionId"]
        .as_str()
        .unwrap()
        .to_string();
    // Redeployed with keyrings that lack the stored contact's key version.
    h.set_vault_keys(test_vault_keys("ck-other-1", "fk-test-1"));
    // The startup guard refuses (and raises the incident).
    assert!(h.contacts.verify_referenced_key_versions().await.is_err());
    let r = resolve_for_execution(&h, &c, principal, &contact, &session).await;
    assert_eq!(r.status, StatusCode::INTERNAL_SERVER_ERROR, "{}", r.text);
    assert!(
        !r.text.contains(CONTACT_DIGITS),
        "never a plaintext fallback"
    );
    let incident = rows(
        h.conn(),
        "SELECT dedupe_key, occurrences::text, severity FROM privacy_relay_incidents \
         WHERE category = 'CONTACT_KEY_UNAVAILABLE' ORDER BY dedupe_key",
    )
    .await;
    assert_eq!(
        incident,
        vec![vec![
            "CONTACT_KEY_UNAVAILABLE:ENCRYPTION:UNKNOWN_KEY_VERSION:ck-test-1".to_string(),
            "2".into(),
            "CRITICAL".into()
        ]]
    );
    assert!(!common::dump_table(h.conn(), "privacy_relay_incidents")
        .await
        .contains(CONTACT_DIGITS));
}

/// authorize (PUBLIC_RELAY) -> grant -> Telephony resolve-for-execution.
pub async fn resolve_for_execution(
    h: &Harness,
    c: &Consumer,
    principal: Uuid,
    contact: &str,
    session: &str,
) -> common::Response {
    let d = h
        .post(
            "/internal/v1/communication/authorize",
            c,
            json!({ "relaySessionId": session, "source": { "principalId": principal },
                    "targets": [{ "contactReferenceId": contact }], "channel": "VOICE",
                    "route": "PUBLIC_RELAY", "purpose": "CALL_CREATE" }),
        )
        .await;
    assert_eq!(d.json["decision"], "AUTHORIZED", "{}", d.text);
    let (call, leg) = (Uuid::new_v4(), Uuid::new_v4());
    let g = h
        .post(
            &format!("/internal/v1/contacts/{contact}/resolution-grants"),
            c,
            json!({ "relaySessionId": session, "decisionId": d.json["decisionId"], "callId": call,
                    "callLegId": leg, "purpose": "PUBLIC_NETWORK_CALL_LEG" }),
        )
        .await;
    assert_eq!(g.status, StatusCode::CREATED, "{}", g.text);
    let grant: &Value = &g.json["grant"];
    let telephony = h.ids.telephony.clone();
    h.send_as(
        &telephony,
        Method::POST,
        &format!("/internal/v1/contacts/{contact}/resolve-for-execution"),
        Some(c),
        Some(
            json!({ "grantId": grant["grantId"], "grantSecret": grant["grantSecret"],
                     "callId": call, "callLegId": leg, "executionAttemptId": Uuid::new_v4() }),
        ),
    )
    .await
}
