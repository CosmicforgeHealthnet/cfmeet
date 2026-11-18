//! Forwarding rules and the voicemail fallback apply to ordinary person-to-person
//! application calls (one target principal), not only to extension calls: ALWAYS before
//! ringing, UNAVAILABLE when the callee has no device, BUSY when it declines, NO_ANSWER
//! when its offers expire, else the callee's own mailbox; loops back to the caller are
//! blocked, and a callee without a rule or a mailbox keeps the ordinary outcome.

mod common;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use calling::{calls::CallState, enterprise::EnterpriseFlags, platform::scopes::names};
use common::{
    app::Device,
    enterprise::{credential, id_of, strings, EnterpriseWorld, RecordingCapture},
};
use serde_json::{json, Value};
use uuid::Uuid;

type Principal = calling::platform::principals::ResolvedPrincipal;

struct World {
    e: EnterpriseWorld,
    vm_admin: common::IssuedCredential,
}

impl World {
    async fn new() -> Option<Self> {
        Self::with_flags(EnterpriseFlags::ALL_ON).await
    }

    async fn with_flags(flags: EnterpriseFlags) -> Option<Self> {
        let store = Arc::new(cf_object_storage::MemoryObjectStore::new());
        let capture = Arc::new(RecordingCapture::default());
        let e = EnterpriseWorld::configured(move |runtime| {
            runtime.enterprise_flags = flags;
            runtime.artifacts = store;
            runtime.capture = capture;
        })
        .await?;
        let vm_admin = credential(
            e.h(),
            &e.a.w.consumer,
            e.a.w.kind,
            &[
                names::ENTERPRISE_CONFIG_READ,
                names::ENTERPRISE_VOICEMAIL_MANAGE,
            ],
        )
        .await;
        Some(Self { e, vm_admin })
    }

    async fn person(&self, name: &str) -> (Principal, Device) {
        let p = self.e.a.principal(&format!("direct-{name}")).await;
        let d = self.e.a.device(&p, &format!("direct-{name}-phone")).await;
        (p, d)
    }

    async fn rule(&self, owner: &Principal, condition: &str, target: &Principal) {
        let r = self
            .e
            .admin(
                Method::POST,
                "/v1/enterprise/forwarding-rules",
                Some(json!({ "principalId": owner.id(), "condition": condition,
                             "destination": { "type": "PRINCIPAL", "id": target.id().to_string() } })),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    }

    async fn mailbox(&self, owner: &Principal) -> Uuid {
        let r = self
            .e
            .send(
                self.vm_admin.secret.expose_once(),
                Method::POST,
                "/v1/enterprise/voicemail/mailboxes",
                Some(
                    json!({ "owner": { "type": "PRINCIPAL", "id": owner.id().to_string() },
                             "displayLabel": "My voicemail", "maxMessageSeconds": 120,
                             "retentionDays": 30 }),
                ),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        id_of(&r.json, "mailboxId")
    }

    /// A direct VOICE call from `caller` to `callee`.
    async fn call(&self, caller: &(Principal, Device), callee: &Principal) -> (Uuid, Value) {
        let r = self
            .e
            .a
            .create_call(
                "VOICE",
                &caller.0,
                &caller.1,
                &[callee],
                &Uuid::new_v4().to_string(),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        let id = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
        (id, r.json)
    }

    async fn steps(&self, call: Uuid) -> Vec<String> {
        strings(
            self.e.h().conn(),
            &format!(
                "SELECT decision || ':' || reason FROM enterprise_routing_steps \
                  WHERE call_id = '{call}' ORDER BY step_number"
            ),
        )
        .await
    }

    async fn expire(&self) {
        self.e.h().clock.advance(chrono::Duration::seconds(46));
        self.e.h().state.app.expire_due(100).await.unwrap();
    }

    async fn answer(&self, call: Uuid, device: &Device) {
        let a = &self.e.a;
        let invitation = a
            .invitation_for(call, device.endpoint)
            .await
            .expect("the forward target rings");
        let (_, v) = a.state(call).await;
        let r = a.accept(call, invitation, device, v).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        assert_eq!(a.state(call).await.0, CallState::Answered);
    }
}

#[tokio::test]
async fn no_answer_and_busy_follow_the_callees_rules_on_the_same_call() {
    let Some(w) = World::new().await else {
        return;
    };
    let alice = w.person("alice").await;
    let (bob, bob_d) = w.person("bob").await;
    let (carol, carol_d) = w.person("carol").await;
    w.rule(&bob, "NO_ANSWER", &carol).await;
    w.rule(&bob, "BUSY", &carol).await;

    // NO_ANSWER: Bob's offer expires, Carol rings on the SAME call and answers.
    let (call, created) = w.call(&alice, &bob).await;
    // The direct call's representation is unchanged (no enterprise block).
    assert!(created.get("enterprise").is_none(), "{created}");
    assert!(w.e.a.invitation_for(call, bob_d.endpoint).await.is_some());
    assert!(w.e.a.invitation_for(call, carol_d.endpoint).await.is_none());
    w.expire().await;
    assert_eq!(
        w.steps(call).await,
        [
            "ENTER:DIRECT_CALL",
            "FORWARD:FORWARD_NO_ANSWER",
            "RING_PRINCIPAL:DESTINATION_RESOLVED"
        ]
    );
    w.answer(call, &carol_d).await;
    let view = w.e.a.call_view(call).await;
    assert!(view.get("enterprise").is_none() || view["enterprise"].is_null());

    // BUSY: Bob declines, Carol rings.
    let (call, _) = w.call(&alice, &bob).await;
    let invitation = w.e.a.invitation_for(call, bob_d.endpoint).await.unwrap();
    let (_, v) = w.e.a.state(call).await;
    let r = w.e.a.decline(call, invitation, &bob_d, v).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert!(w
        .steps(call)
        .await
        .contains(&"FORWARD:FORWARD_BUSY".to_string()));
    w.answer(call, &carol_d).await;
}

#[tokio::test]
async fn always_forwards_before_ringing_and_unavailable_replaces_the_failure() {
    let Some(w) = World::new().await else {
        return;
    };
    let alice = w.person("alice").await;
    let (bob, bob_d) = w.person("bob").await;
    let (carol, carol_d) = w.person("carol").await;
    w.rule(&bob, "ALWAYS", &carol).await;
    let (call, _) = w.call(&alice, &bob).await;
    assert!(w.e.a.invitation_for(call, bob_d.endpoint).await.is_none());
    assert_eq!(
        w.steps(call).await,
        [
            "ENTER:DIRECT_CALL",
            "FORWARD:FORWARD_ALWAYS",
            "RING_PRINCIPAL:DESTINATION_RESOLVED"
        ]
    );
    w.answer(call, &carol_d).await;

    // UNAVAILABLE: Dave has no device at all; without a rule the call fails at once, with
    // one it is forwarded instead.
    let dave = w.e.a.principal("direct-dave").await;
    let (call, created) = w.call(&alice, &dave).await;
    assert_eq!(created["call"]["state"], "FAILED", "{created}");
    w.rule(&dave, "UNAVAILABLE", &carol).await;
    let (call2, created) = w.call(&alice, &dave).await;
    assert_ne!(call, call2);
    assert_ne!(created["call"]["state"], "FAILED", "{created}");
    assert!(w
        .steps(call2)
        .await
        .contains(&"FORWARD:FORWARD_UNAVAILABLE".to_string()));
    w.answer(call2, &carol_d).await;
}

#[tokio::test]
async fn the_callees_mailbox_takes_an_unanswered_call() {
    let Some(w) = World::new().await else {
        return;
    };
    let alice = w.person("alice").await;
    let (bob, _bob_d) = w.person("bob").await;
    let mailbox = w.mailbox(&bob).await;
    let (call, _) = w.call(&alice, &bob).await;
    w.expire().await;
    assert_eq!(
        w.steps(call).await,
        [
            "ENTER:DIRECT_CALL",
            "VOICEMAIL:VOICEMAIL_NO_ANSWER",
            "VOICEMAIL:RECORDING"
        ]
    );
    let messages = strings(
        w.e.h().conn(),
        &format!("SELECT mailbox_id::text FROM voicemail_messages WHERE call_id = '{call}'"),
    )
    .await;
    assert_eq!(messages, [mailbox.to_string()]);
    assert!(!w.e.a.state(call).await.0.is_terminal());

    // A callee without a device and with a mailbox: straight to voicemail, not FAILED.
    let erin = w.e.a.principal("direct-erin").await;
    w.mailbox(&erin).await;
    let (call, created) = w.call(&alice, &erin).await;
    assert_ne!(created["call"]["state"], "FAILED", "{created}");
    assert!(w
        .steps(call)
        .await
        .contains(&"VOICEMAIL:VOICEMAIL_UNAVAILABLE".to_string()));
}

#[tokio::test]
async fn loops_back_to_the_caller_and_callees_without_handlers_keep_the_ordinary_outcome() {
    let Some(w) = World::new().await else {
        return;
    };
    let alice = w.person("alice").await;
    let (bob, _) = w.person("bob").await;
    let (carol, _) = w.person("carol").await;
    // Bob forwards to the caller herself: blocked, the ordinary NO_ANSWER applies.
    w.rule(&bob, "NO_ANSWER", &alice.0).await;
    let (call, _) = w.call(&alice, &bob).await;
    w.expire().await;
    assert_eq!(w.e.a.state(call).await.0, CallState::NoAnswer);
    assert!(w
        .steps(call)
        .await
        .contains(&"NO_ROUTE:LOOP_BLOCKED".to_string()));
    // Carol has nothing configured: no routing context, the usual missed call.
    let (call, _) = w.call(&alice, &carol).await;
    w.expire().await;
    assert_eq!(w.e.a.state(call).await.0, CallState::NoAnswer);
    assert!(w.steps(call).await.is_empty());
}

#[tokio::test]
async fn the_enterprise_kill_switch_turns_direct_forwarding_off() {
    let Some(w) = World::with_flags(EnterpriseFlags {
        enterprise_calling: false,
        ..EnterpriseFlags::ALL_ON
    })
    .await
    else {
        return;
    };
    let alice = w.person("alice").await;
    let (bob, _) = w.person("bob").await;
    let (carol, carol_d) = w.person("carol").await;
    w.rule(&bob, "NO_ANSWER", &carol).await;
    let (call, _) = w.call(&alice, &bob).await;
    w.expire().await;
    assert_eq!(w.e.a.state(call).await.0, CallState::NoAnswer);
    assert!(w.e.a.invitation_for(call, carol_d.endpoint).await.is_none());
    assert!(w.steps(call).await.is_empty());
}
