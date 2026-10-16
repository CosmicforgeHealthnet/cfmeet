//! §29: participant reassignment on the SAME context, stale-update protection and
//! immediate loss of authority.

mod common;

use axum::http::StatusCode;
use cf_service_auth::ApplicationEnvironmentKind as Kind;
use common::*;
use serde_json::json;
use uuid::Uuid;

fn update_path(id: &str) -> String {
    format!("/internal/v1/contexts/{id}/update")
}

fn replace(expected: i64, subjects: &[Uuid]) -> serde_json::Value {
    json!({
        "expectedVersion": expected,
        "participants": subjects.iter().map(|s| json!({ "principalId": s })).collect::<Vec<_>>(),
    })
}

#[tokio::test]
async fn reassignment_keeps_the_context_and_revokes_the_removed_participant_immediately() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(Kind::Production);
    let [a, b, cc] = principals(3).try_into().unwrap();
    let ctx = h.bidirectional(&c, &[a, b]).await;
    let id = ctx["contextId"].as_str().unwrap().to_string();
    let session = h.app_session(&c, &id).await;
    let sid = session["relaySessionId"].as_str().unwrap().to_string();
    assert_eq!(h.reason(&c, &sid, a, &[b]).await, "AUTHORIZED");

    // Replace B by C (policy default: terminate active calls of a removed participant).
    let r = h.command(&update_path(&id), &c, replace(1, &[a, cc])).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["context"]["contextId"], id.as_str());
    assert_eq!(r.json["context"]["version"], 2);
    assert_eq!(r.json["enforcement"]["status"], "APPLIED");
    let subjects: Vec<_> = r.json["context"]["participants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["principalId"].as_str().unwrap().to_string())
        .collect();
    assert!(subjects.contains(&a.to_string()) && subjects.contains(&cc.to_string()));
    assert!(!subjects.contains(&b.to_string()));

    // Immediately after the commit: B is out, C is in — for the EXISTING session too.
    assert_eq!(
        h.reason(&c, &sid, a, &[b]).await,
        "PARTICIPANT_NOT_AUTHORIZED"
    );
    assert_eq!(
        h.reason(&c, &sid, b, &[a]).await,
        "PARTICIPANT_NOT_AUTHORIZED"
    );
    assert_eq!(h.reason(&c, &sid, a, &[cc]).await, "AUTHORIZED");
    let s = h
        .get(&format!("/internal/v1/sessions/{sid}"), &c)
        .await
        .json;
    assert_eq!(s["version"], 2);
    assert_eq!(s["sourceContextVersion"], 2);
    assert_eq!(s["participants"].as_array().unwrap().len(), 2);

    // One context, B kept as history (never deleted), and the enforcement command named
    // the removed principal only.
    assert_eq!(
        h.count("SELECT count(*) FROM communication_contexts").await,
        1
    );
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM communication_context_participants WHERE principal_ref = '{b}' \
             AND removed_in_version = 2 AND removed_at IS NOT NULL"
        ))
        .await,
        1
    );
    let delivered = h.calling.delivered();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].payload["reason"], "PARTICIPANT_REMOVED");
    assert_eq!(delivered[0].payload["affectedPrincipalIds"], json!([b]));
    assert_eq!(delivered[0].payload["contextVersion"], 2);
    assert_eq!(delivered[0].payload["terminateActiveCalls"], true);
    assert_eq!(delivered[0].delegated.tenant_id(), c.tenant_id);

    // Evidence: one CONTEXT_PARTICIPANTS_CHANGED with previous / new version and the
    // added / removed participant ids, one RELAY_SESSION_UPDATED.
    let evidence = h
        .get(&format!("/internal/v1/contexts/{id}/evidence"), &c)
        .await
        .json;
    let events: Vec<_> = evidence["auditEvents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["eventType"].as_str().unwrap().to_string())
        .collect();
    assert!(events.contains(&"CONTEXT_PARTICIPANTS_CHANGED".to_string()));
    assert!(events.contains(&"RELAY_SESSION_UPDATED".to_string()));
    let changed = evidence["auditEvents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["eventType"] == "CONTEXT_PARTICIPANTS_CHANGED")
        .unwrap();
    assert_eq!(changed["data"]["previous_version"], 1);
    assert_eq!(changed["data"]["version"], 2);
    assert_eq!(
        changed["data"]["participants_removed"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        changed["data"]["participants_added"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(evidence["participantHistory"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn removal_without_active_call_termination_still_denies_new_authority() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(Kind::Production);
    let [a, b, cc] = principals(3).try_into().unwrap();
    let mut req = context_request(&[a, b], "BIDIRECTIONAL", &[]);
    req["policy"] = json!({ "terminateActiveCallsOnParticipantRemoval": false });
    let ctx = h.create_context(&c, req).await;
    let id = ctx["contextId"].as_str().unwrap().to_string();
    let sid = h.app_session(&c, &id).await["relaySessionId"]
        .as_str()
        .unwrap()
        .to_string();
    let r = h.command(&update_path(&id), &c, replace(1, &[a, cc])).await;
    assert_eq!(r.json["enforcement"]["status"], "NOT_REQUIRED");
    assert!(
        h.calling.delivered().is_empty(),
        "existing calls may continue by policy"
    );
    assert_eq!(
        h.count("SELECT count(*) FROM privacy_relay_enforcement_jobs")
            .await,
        0
    );
    // ...but no NEW authority for the removed participant.
    assert_eq!(
        h.reason(&c, &sid, a, &[b]).await,
        "PARTICIPANT_NOT_AUTHORIZED"
    );
}

#[tokio::test]
async fn stale_updates_conflict_and_never_restore_removed_authority() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(Kind::Production);
    let [a, b, cc, d] = principals(4).try_into().unwrap();
    let ctx = h.bidirectional(&c, &[a, b]).await;
    let id = ctx["contextId"].as_str().unwrap().to_string();
    let sid = h.app_session(&c, &id).await["relaySessionId"]
        .as_str()
        .unwrap()
        .to_string();
    // Bring the context to version 5.
    for (v, set) in [
        (1, vec![a, b, cc]),
        (2, vec![a, b]),
        (3, vec![a, b, d]),
        (4, vec![a, b]),
    ] {
        let r = h.command(&update_path(&id), &c, replace(v, &set)).await;
        assert_eq!(r.json["context"]["version"], v + 1, "{}", r.text);
    }
    // Two concurrent updates expecting 5: remove B / keep B (add C).
    for round in 0..5 {
        let v = h.version_of(&c, &id).await;
        let path = update_path(&id);
        let (remove_b, keep_b) = futures::join!(
            h.command(&path, &c, replace(v, &[a, d])),
            h.command(&path, &c, replace(v, &[a, b, cc])),
        );
        let ok = [&remove_b, &keep_b]
            .iter()
            .filter(|r| r.status == StatusCode::OK)
            .count();
        assert_eq!(ok, 1, "round {round}: exactly one wins");
        let loser = if remove_b.status == StatusCode::OK {
            &keep_b
        } else {
            &remove_b
        };
        assert_eq!(
            (loser.status, loser.code()),
            (StatusCode::CONFLICT, "CONTEXT_VERSION_CONFLICT")
        );
        assert_eq!(h.version_of(&c, &id).await, v + 1);
        if remove_b.status == StatusCode::OK {
            // The losing stale write could not restore B.
            assert_eq!(
                h.reason(&c, &sid, a, &[b]).await,
                "PARTICIPANT_NOT_AUTHORIZED"
            );
            // Replaying the stale request later still conflicts.
            let late = h
                .command(&update_path(&id), &c, replace(v, &[a, b, cc]))
                .await;
            assert_eq!(late.code(), "CONTEXT_VERSION_CONFLICT");
            assert_eq!(
                h.reason(&c, &sid, a, &[b]).await,
                "PARTICIPANT_NOT_AUTHORIZED"
            );
        }
        // Reset for the next round.
        let v = h.version_of(&c, &id).await;
        h.command(&update_path(&id), &c, replace(v, &[a, b])).await;
    }
    // Exactly one active membership per subject at all times (database-enforced).
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM communication_context_participants WHERE context_id = '{id}' AND removed_at IS NULL"
        ))
        .await,
        2
    );
}

#[tokio::test]
async fn protected_authorization_during_reassignment_is_consistent() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(Kind::Production);
    let [a, b, cc] = principals(3).try_into().unwrap();
    for round in 0..5 {
        let ctx = h.bidirectional(&c, &[a, b]).await;
        let id = ctx["contextId"].as_str().unwrap().to_string();
        let sid = h.app_session(&c, &id).await["relaySessionId"]
            .as_str()
            .unwrap()
            .to_string();
        let path = update_path(&id);
        let targets = [b];
        let (auth, update) = futures::join!(
            h.authorize(&c, &sid, a, &targets),
            h.command(&path, &c, replace(1, &[a, cc])),
        );
        assert_eq!(update.status, StatusCode::OK);
        // Either the decision saw version 1 (and is AUTHORIZED at version 1) or version 2
        // (and is denied): never an authorization of B at version 2.
        match auth.json["contextVersion"].as_i64().unwrap() {
            1 => assert_eq!(auth.json["decision"], "AUTHORIZED", "round {round}"),
            2 => assert_eq!(
                auth.json["reason"], "PARTICIPANT_NOT_AUTHORIZED",
                "round {round}"
            ),
            other => panic!("unexpected version {other}"),
        }
        assert_eq!(
            h.reason(&c, &sid, a, &[b]).await,
            "PARTICIPANT_NOT_AUTHORIZED"
        );
    }
}

#[tokio::test]
async fn direction_channel_validity_and_policy_updates_each_increment_once() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(Kind::Production);
    let [a, b] = principals(2).try_into().unwrap();
    let ctx = h.bidirectional(&c, &[a, b]).await;
    let id = ctx["contextId"].as_str().unwrap().to_string();
    let sid = h.app_session(&c, &id).await["relaySessionId"]
        .as_str()
        .unwrap()
        .to_string();
    let mut v = 1;
    for change in [
        json!({ "directionPolicy": "EXPLICIT", "directions": [{ "from": { "principalId": a }, "to": { "principalId": b } }] }),
        json!({ "allowedChannels": ["VOICE", "MESSAGING"] }),
        json!({ "expiresAt": iso(at(45)) }),
        json!({ "policy": { "graceAllowsNewCalls": true } }),
        json!({ "allowedRelayStrategies": ["APP_ONLY", "APP_PREFERRED"] }),
    ] {
        let mut body = change.clone();
        body["expectedVersion"] = json!(v);
        let r = h.command(&update_path(&id), &c, body).await;
        assert_eq!(r.status, StatusCode::OK, "{change}: {}", r.text);
        v += 1;
        assert_eq!(r.json["context"]["version"], v, "{change}");
    }
    // The inherited session followed: explicit A -> B only, clamped validity.
    assert_eq!(h.reason(&c, &sid, a, &[b]).await, "AUTHORIZED");
    assert_eq!(h.reason(&c, &sid, b, &[a]).await, "DIRECTION_NOT_ALLOWED");
    let s = h
        .get(&format!("/internal/v1/sessions/{sid}"), &c)
        .await
        .json;
    assert_eq!(s["expiresAt"], "2031-03-01T10:45:00.000000Z");
    assert_eq!(s["directionPolicy"], "EXPLICIT");
    assert_eq!(s["sourceContextVersion"], v);
    // An identical "update" is a no-op: no version change.
    let r = h
        .command(
            &update_path(&id),
            &c,
            json!({ "expectedVersion": v, "allowedChannels": ["VOICE", "MESSAGING"] }),
        )
        .await;
    assert_eq!(r.json["context"]["version"], v);
    // A status patch does not exist.
    let r = h
        .command(
            &update_path(&id),
            &c,
            json!({ "expectedVersion": v, "status": "REVOKED" }),
        )
        .await;
    assert_eq!(r.code(), "VALIDATION_ERROR");
    let r = h
        .command(&update_path(&id), &c, json!({ "expectedVersion": v }))
        .await;
    assert_eq!(r.code(), "VALIDATION_ERROR", "an empty update is refused");
}
