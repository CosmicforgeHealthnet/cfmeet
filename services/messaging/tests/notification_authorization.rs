//! Delayed push authorization: real PostgreSQL/router/outbox and a recording provider.
//! No claim of physical-device or live-provider delivery.
mod common;
use axum::http::{Method, StatusCode};
use common::{flows::*, realtime::*, *};
use messaging::realtime::push;
use serde_json::{json, Value};
use std::time::Duration;

async fn queued(rt: &Rt, c: &Customer, conversation: &Value, alice: &mut Member, bob: &Member) {
    let h = &rt.h;
    let r = h
        .put(
            c,
            Some(bob.principal),
            &format!(
                "/v1/messaging/devices/{}/push-registration",
                bob.device_id()
            ),
            json!({"platform":"LOCAL_TEST","handle":"section-eight-push-test"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let sent = h
        .send_e2ee(c, conversation, alice, "client-side test content")
        .await;
    assert_eq!(sent.status, StatusCode::CREATED, "{}", sent.text);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while h
        .scalar("SELECT count(*) FROM messaging_push_jobs WHERE status='PENDING'")
        .await
        == 0
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "committed event never produced a job"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn queued_push_is_not_delivered_after_member_departure() {
    let Some(rt) = Rt::start().await else { return };
    let h = &rt.h;
    let c = h.customer(ALL_SCOPES);
    let (conversation, mut members) = h.e2ee_group(&c, 2).await;
    let bob = members.pop().unwrap();
    let mut alice = members.pop().unwrap();
    queued(&rt, &c, &conversation, &mut alice, &bob).await;
    let r = h
        .send(
            Method::DELETE,
            Some(&c.token),
            Some(bob.principal),
            &format!(
                "/v1/conversations/{}/members/{}",
                cid(&conversation),
                bob.principal
            ),
            None,
            &[],
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text);
    let report = push::dispatch_due(&h.state, 100, 3).await.unwrap();
    assert_eq!(report.disabled, 1, "{report:?}");
    assert!(rt.push.sent.lock().unwrap().is_empty());
    let metrics = messaging::metrics::render(&h.state).await;
    assert!(metrics.contains("messaging_push_jobs{status=\"DISABLED\"} 1"));
    assert!(metrics.contains("messaging_storage_committed_bytes 0"));
    assert!(!metrics.contains(&bob.device_id().to_string()));
    assert!(!metrics.contains(&bob.principal.to_string()));
}

#[tokio::test]
async fn queued_push_is_not_delivered_after_a_direct_block() {
    let Some(rt) = Rt::start().await else { return };
    let h = &rt.h;
    let c = h.customer(ALL_SCOPES);
    let (conversation, mut alice, bob) = h.e2ee_direct(&c).await;
    queued(&rt, &c, &conversation, &mut alice, &bob).await;
    let r = h
        .post(
            &c,
            Some(bob.principal),
            "/v1/messaging/blocks",
            json!({"principalId":alice.principal}),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text);
    let report = push::dispatch_due(&h.state, 100, 3).await.unwrap();
    assert_eq!(report.disabled, 1, "{report:?}");
    assert!(rt.push.sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn personal_policy_outage_retries_without_calling_the_provider() {
    let Some(rt) = Rt::start().await else { return };
    let h = &rt.h;
    let c = h.customer(ALL_SCOPES);
    let (conversation, mut alice, bob) = h.e2ee_direct(&c).await;
    queued(&rt, &c, &conversation, &mut alice, &bob).await;
    // A client with no configured target fails closed; canonical committed messages
    // remain intact and the push can be retried after the authority recovers.
    let mut state = h.state.clone();
    state.communications = Some(cf_communications_policy::PolicyClient::new(
        state.service_auth.client(),
        cf_communications_policy::Scope {
            tenant_id: c.context.scope.tenant_id,
            application_id: c.context.scope.application_id,
            environment_id: c.context.scope.environment_id,
        },
    ));
    let report = push::dispatch_due(&state, 100, 3).await.unwrap();
    assert_eq!(report.retried, 1, "{report:?}");
    assert!(rt.push.sent.lock().unwrap().is_empty());
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_messages WHERE message_kind='APPLICATION'")
            .await,
        1
    );
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_push_jobs WHERE status='PENDING' AND attempts=1")
            .await,
        1
    );
}

#[tokio::test]
async fn revoking_all_sessions_disables_push_without_deleting_device_identity() {
    let Some(rt) = Rt::start().await else { return };
    let h = &rt.h;
    let c = h.customer(ALL_SCOPES);
    let (conversation, mut alice, bob) = h.e2ee_direct(&c).await;
    queued(&rt, &c, &conversation, &mut alice, &bob).await;
    let r = h
        .post(
            &c,
            Some(bob.principal),
            "/v1/messaging/client-sessions/revoke-all",
            json!({}),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text);
    let report = push::dispatch_due(&h.state, 100, 3).await.unwrap();
    assert_eq!(report.disabled, 1, "{report:?}");
    assert!(rt.push.sent.lock().unwrap().is_empty());
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM messaging_devices WHERE id='{}' AND status='ACTIVE'",
            bob.device_id()
        ))
        .await,
        1
    );
}

#[tokio::test]
async fn privacy_database_failure_does_not_return_default_sharing_preferences() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let person = h.member(&c, 0).await;
    h.sql("ALTER TABLE messaging_presence_settings RENAME TO unavailable_privacy_settings")
        .await
        .unwrap();
    let r = h
        .get(
            &c,
            Some(person.principal),
            "/v1/messaging/presence/settings",
        )
        .await;
    assert!(r.status.is_server_error(), "{}", r.text);
    assert!(
        r.json.get("visibility").is_none(),
        "privacy failures must not advertise sharing"
    );
}
