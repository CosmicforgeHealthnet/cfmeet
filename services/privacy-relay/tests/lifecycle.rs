//! §29: suspend / resume / revoke, the feature kill switch and its restrictive
//! exceptions.

mod common;

use axum::http::StatusCode;
use cf_service_auth::ApplicationEnvironmentKind as Kind;
use common::*;
use privacy_relay::enforcement::DeliveryFailure;
use serde_json::json;

#[tokio::test]
async fn revoke_is_terminal_and_cascades() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(Kind::Production);
    let [a, b] = principals(2).try_into().unwrap();
    let ctx = h.bidirectional(&c, &[a, b]).await;
    let id = ctx["contextId"].as_str().unwrap().to_string();
    let s1 = h.app_session(&c, &id).await["relaySessionId"]
        .as_str()
        .unwrap()
        .to_string();
    let s2 = h.app_session(&c, &id).await["relaySessionId"]
        .as_str()
        .unwrap()
        .to_string();

    let r = h.lifecycle(&c, &id, "revoke", 1).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["context"]["status"], "REVOKED");
    assert_eq!(r.json["context"]["version"], 2);
    assert_eq!(r.json["enforcement"]["status"], "APPLIED");
    for sid in [&s1, &s2] {
        let s = h
            .get(&format!("/internal/v1/sessions/{sid}"), &c)
            .await
            .json;
        assert_eq!(s["status"], "REVOKED");
        assert_eq!(s["version"], 2);
        assert_eq!(h.reason(&c, sid, a, &[b]).await, "CONTEXT_REVOKED");
    }
    // No new session, no resume, no update, no un-revoke in the database.
    let ns = h
        .command(
            &format!("/internal/v1/contexts/{id}/sessions"),
            &c,
            json!({ "relayStrategy": "APP_ONLY" }),
        )
        .await;
    assert_eq!(
        (ns.status, ns.code()),
        (StatusCode::CONFLICT, "CONTEXT_REVOKED")
    );
    assert_eq!(
        h.lifecycle(&c, &id, "resume", 2).await.code(),
        "CONTEXT_REVOKED"
    );
    let up = h
        .command(
            &format!("/internal/v1/contexts/{id}/update"),
            &c,
            json!({ "expectedVersion": 2, "expiresAt": iso(at(50)) }),
        )
        .await;
    assert_eq!(up.code(), "CONTEXT_REVOKED");
    use sea_orm::ConnectionTrait;
    assert!(h.conn().execute_unprepared(&format!(
        "UPDATE communication_contexts SET status = 'ACTIVE', version = version + 1, revoked_at = NULL WHERE id = '{id}'"
    )).await.is_err());
    // Repeated revoke: safe idempotent answer (no new version, event or job), even with a
    // stale expectedVersion.
    let again = h.lifecycle(&c, &id, "revoke", 1).await;
    assert_eq!(again.status, StatusCode::OK);
    assert_eq!(again.json["context"]["version"], 2);
    assert_eq!(again.json["enforcement"]["status"], "NOT_REQUIRED");
    assert_eq!(
        h.count("SELECT count(*) FROM relay_audit_events WHERE event_type = 'CONTEXT_REVOKED'")
            .await,
        1
    );
    assert_eq!(
        h.count("SELECT count(*) FROM privacy_relay_enforcement_jobs")
            .await,
        1
    );
    let job = h.calling.delivered()[0].payload.clone();
    assert_eq!(job["reason"], "CONTEXT_REVOKED");
    assert_eq!(job["contextVersion"], 2);
    assert_eq!(job["relaySessionIds"], json!([]));
    assert_eq!(job["terminateActiveCalls"], true);
    // A revoke-without-termination policy still revokes authority, but asks for no call
    // termination.
    let mut req = context_request(&[a, b], "BIDIRECTIONAL", &[]);
    req["policy"] = json!({ "terminateActiveCallsOnRevoke": false });
    let soft = h.create_context(&c, req).await;
    let r = h
        .lifecycle(&c, soft["contextId"].as_str().unwrap(), "revoke", 1)
        .await;
    assert_eq!(r.json["context"]["status"], "REVOKED");
    assert_eq!(r.json["enforcement"]["status"], "NOT_REQUIRED");
}

#[tokio::test]
async fn suspend_and_resume_are_versioned_and_bounded() {
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
    let r = h.lifecycle(&c, &id, "suspend", 1).await;
    assert_eq!(r.json["context"]["status"], "SUSPENDED");
    // Default policy: suspension does not terminate active calls…
    assert_eq!(r.json["enforcement"]["status"], "NOT_REQUIRED");
    // …but new authority is denied immediately.
    assert_eq!(h.reason(&c, &sid, a, &[b]).await, "CONTEXT_SUSPENDED");
    let ns = h
        .command(
            &format!("/internal/v1/contexts/{id}/sessions"),
            &c,
            json!({ "relayStrategy": "APP_ONLY" }),
        )
        .await;
    assert_eq!(ns.code(), "CONTEXT_SUSPENDED");
    // Stale resume.
    assert_eq!(
        h.lifecycle(&c, &id, "resume", 1).await.code(),
        "CONTEXT_VERSION_CONFLICT"
    );
    let r = h.lifecycle(&c, &id, "resume", 2).await;
    assert_eq!(
        (
            r.json["context"]["status"].as_str(),
            r.json["context"]["version"].as_i64()
        ),
        (Some("ACTIVE"), Some(3))
    );
    assert_eq!(h.reason(&c, &sid, a, &[b]).await, "AUTHORIZED");
    // Suspend, then revoke: a stale resume after the revoke must fail.
    h.lifecycle(&c, &id, "suspend", 3).await;
    h.lifecycle(&c, &id, "revoke", 4).await;
    assert_eq!(
        h.lifecycle(&c, &id, "resume", 4).await.code(),
        "CONTEXT_REVOKED"
    );
    // Resume after the validity ended is refused.
    let ctx = h.bidirectional(&c, &[a, b]).await;
    let id = ctx["contextId"].as_str().unwrap().to_string();
    h.lifecycle(&c, &id, "suspend", 1).await;
    h.clock.set(at(61));
    assert_eq!(
        h.lifecycle(&c, &id, "resume", 2).await.code(),
        "CONTEXT_EXPIRED"
    );
    // A suspend-terminates policy asks Calling to end active calls.
    h.clock.set(at(0));
    let mut req = context_request(&[a, b], "BIDIRECTIONAL", &[]);
    req["policy"] = json!({ "terminateActiveCallsOnSuspend": true });
    let hard = h.create_context(&c, req).await;
    let r = h
        .lifecycle(&c, hard["contextId"].as_str().unwrap(), "suspend", 1)
        .await;
    assert_eq!(r.json["enforcement"]["status"], "APPLIED");
    assert_eq!(
        h.calling.delivered().last().unwrap().payload["reason"],
        "CONTEXT_SUSPENDED"
    );
}

#[tokio::test]
async fn session_lifecycle_is_independent_and_restrictive() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(Kind::Production);
    let [a, b] = principals(2).try_into().unwrap();
    let ctx = h.bidirectional(&c, &[a, b]).await;
    let id = ctx["contextId"].as_str().unwrap().to_string();
    let s1 = h.app_session(&c, &id).await["relaySessionId"]
        .as_str()
        .unwrap()
        .to_string();
    let s2 = h.app_session(&c, &id).await["relaySessionId"]
        .as_str()
        .unwrap()
        .to_string();
    let path = |s: &str, action: &str| format!("/internal/v1/sessions/{s}/{action}");
    let r = h
        .command(&path(&s1, "suspend"), &c, json!({ "expectedVersion": 1 }))
        .await;
    assert_eq!(r.json["session"]["status"], "SUSPENDED", "{}", r.text);
    assert_eq!(h.reason(&c, &s1, a, &[b]).await, "SESSION_NOT_ACTIVE");
    assert_eq!(h.reason(&c, &s2, a, &[b]).await, "AUTHORIZED");
    let r = h
        .command(&path(&s1, "resume"), &c, json!({ "expectedVersion": 1 }))
        .await;
    assert_eq!(r.code(), "SESSION_VERSION_CONFLICT");
    let r = h
        .command(&path(&s1, "resume"), &c, json!({ "expectedVersion": 2 }))
        .await;
    assert_eq!(r.json["session"]["status"], "ACTIVE");
    let r = h
        .command(&path(&s1, "revoke"), &c, json!({ "expectedVersion": 3 }))
        .await;
    assert_eq!(r.json["session"]["status"], "REVOKED");
    assert_eq!(r.json["enforcement"]["status"], "APPLIED");
    let job = h.calling.delivered().last().unwrap().payload.clone();
    assert_eq!(job["reason"], "SESSION_REVOKED");
    assert_eq!(job["relaySessionIds"], json!([s1]));
    assert_eq!(h.reason(&c, &s1, a, &[b]).await, "SESSION_NOT_ACTIVE");
    let r = h
        .command(&path(&s1, "resume"), &c, json!({ "expectedVersion": 4 }))
        .await;
    assert_eq!(r.code(), "SESSION_NOT_ACTIVE");
    // The context and the other session are unaffected.
    assert_eq!(h.version_of(&c, &id).await, 1);
    assert_eq!(h.reason(&c, &s2, a, &[b]).await, "AUTHORIZED");
}

#[tokio::test]
async fn kill_switch_denies_new_authority_but_never_blocks_removing_it() {
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
    let expiring = h.bidirectional(&c, &[a, b]).await;
    let expiring_id = expiring["contextId"].as_str().unwrap().to_string();

    h.feature.set(false);
    let r = h
        .command(
            "/internal/v1/contexts",
            &c,
            context_request(&[a, b], "BIDIRECTIONAL", &[]),
        )
        .await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::SERVICE_UNAVAILABLE, "PRIVACY_RELAY_DISABLED")
    );
    let r = h
        .command(
            &format!("/internal/v1/contexts/{id}/sessions"),
            &c,
            json!({ "relayStrategy": "APP_ONLY" }),
        )
        .await;
    assert_eq!(r.code(), "PRIVACY_RELAY_DISABLED");
    assert_eq!(h.reason(&c, &sid, a, &[b]).await, "FEATURE_DISABLED");
    // Reads keep working.
    assert_eq!(
        h.get(&format!("/internal/v1/contexts/{id}"), &c)
            .await
            .status,
        StatusCode::OK
    );
    // Restrictive operations keep working: suspend, revoke, expiry, enforcement retry.
    assert_eq!(
        h.lifecycle(&c, &id, "suspend", 1).await.json["context"]["status"],
        "SUSPENDED"
    );
    assert_eq!(
        h.lifecycle(&c, &id, "resume", 2).await.code(),
        "PRIVACY_RELAY_DISABLED"
    );
    h.calling.fail_with(Some(DeliveryFailure::Unavailable));
    let r = h.lifecycle(&c, &id, "revoke", 2).await;
    assert_eq!(r.json["context"]["status"], "REVOKED");
    assert_eq!(r.json["enforcement"]["status"], "PENDING");
    h.calling.fail_with(None);
    h.clock.set(at(120));
    let report = h.relay.run_sweep().await.unwrap();
    assert_eq!(report.contexts_expired, 1, "expiry runs while disabled");
    assert!(
        report.enforcement_applied >= 1,
        "enforcement retry runs while disabled"
    );
    let e = h
        .get(&format!("/internal/v1/contexts/{expiring_id}"), &c)
        .await;
    assert_eq!(e.json["status"], "EXPIRED");
    assert_eq!(
        h.count("SELECT count(*) FROM privacy_relay_enforcement_jobs WHERE status <> 'APPLIED'")
            .await,
        0
    );
    // Re-enabled: authority is evaluated normally again (still revoked here).
    h.feature.set(true);
    assert_eq!(h.reason(&c, &sid, a, &[b]).await, "CONTEXT_REVOKED");
}
