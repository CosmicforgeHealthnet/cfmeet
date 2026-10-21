//! Part V J §§130–145 — kill switches, incidents, key-transparency rollback detection and
//! the operator CLI.

mod common;

use async_trait::async_trait;
use axum::http::StatusCode;
use common::{flows::ALL_SCOPES, *};
use messaging::{
    operations::{self, IncidentCategory, KillSwitch, KtCheckpoint, KtVerdict},
    webhook_relay::{self, PlatformEventSink, RelayFailure},
};
use serde_json::{json, Value};

struct Refusing;

#[async_trait]
impl PlatformEventSink for Refusing {
    async fn publish(&self, _: &Value) -> Result<(), RelayFailure> {
        Err(RelayFailure::Rejected)
    }
}

struct Accepting;

#[async_trait]
impl PlatformEventSink for Accepting {
    async fn publish(&self, _: &Value) -> Result<(), RelayFailure> {
        Ok(())
    }
}

#[tokio::test]
async fn kill_switches_stop_capabilities_per_environment_or_globally_and_are_audited() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let now = chrono::Utc::now();
    let c = h.first_party(false);
    let other = h.first_party(false);
    let (conv, people) = h.managed_group(&c, 2).await;
    let (oconv, opeople) = h.managed_group(&other, 2).await;
    let env = c.context.scope.environment_id;

    // Operators are named and reasons are codes.
    assert!(operations::set_switch(
        h.conn(),
        KillSwitch::MessageSend,
        Some(env),
        true,
        "INCIDENT",
        "",
        now
    )
    .await
    .is_err());
    assert!(operations::set_switch(
        h.conn(),
        KillSwitch::MessageSend,
        Some(env),
        true,
        "free text",
        "ops-1",
        now
    )
    .await
    .is_err());
    assert!(operations::set_switch(
        h.conn(),
        KillSwitch::MessageSend,
        Some(env),
        true,
        "ABUSE_WAVE",
        "ops-1",
        now
    )
    .await
    .unwrap());
    // Idempotent: engaging again changes nothing (and is not audited again).
    assert!(!operations::set_switch(
        h.conn(),
        KillSwitch::MessageSend,
        Some(env),
        true,
        "ABUSE_WAVE",
        "ops-1",
        now
    )
    .await
    .unwrap());
    let r = h.send_managed(&c, &conv, people[0], "blocked").await;
    assert_eq!(
        (r.code(), r.reason()),
        ("FEATURE_DISABLED", "KILL_SWITCH_ENGAGED"),
        "{}",
        r.text
    );
    // Another environment is untouched; reads still work.
    let r = h.send_managed(&other, &oconv, opeople[0], "fine").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let cid = conv["conversationId"].as_str().unwrap();
    assert_eq!(
        h.get(
            &c,
            Some(people[0]),
            &format!("/v1/conversations/{cid}/messages")
        )
        .await
        .status,
        StatusCode::OK
    );
    operations::set_switch(
        h.conn(),
        KillSwitch::MessageSend,
        Some(env),
        false,
        "RESOLVED",
        "ops-1",
        now,
    )
    .await
    .unwrap();
    assert_eq!(
        h.send_managed(&c, &conv, people[0], "back").await.status,
        StatusCode::CREATED
    );

    // Global: every environment.
    operations::set_switch(
        h.conn(),
        KillSwitch::ConversationCreate,
        None,
        true,
        "MAINTENANCE",
        "ops-2",
        now,
    )
    .await
    .unwrap();
    let p = h.principal(&other);
    let r = h
        .create_conversation(
            &other,
            p,
            json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" }),
        )
        .await;
    assert_eq!(r.reason(), "KILL_SWITCH_ENGAGED", "{}", r.text);
    // E2EE onboarding (device registration appends to the transparency log).
    operations::set_switch(
        h.conn(),
        KillSwitch::E2eeOnboarding,
        Some(env),
        true,
        "KT_MAINTENANCE",
        "ops-2",
        now,
    )
    .await
    .unwrap();
    let e2ee = h.customer(ALL_SCOPES);
    let (_, ok) = h.register_device(&e2ee, h.principal(&e2ee)).await;
    assert_eq!(
        ok.status,
        StatusCode::CREATED,
        "other environments onboard: {}",
        ok.text
    );
    let (_, refused) = h.register_device(&c, h.principal(&c)).await;
    assert_eq!(refused.reason(), "KILL_SWITCH_ENGAGED", "{}", refused.text);

    // Every change is audited; the audit is append-only.
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_operator_audit WHERE action LIKE 'KILL_SWITCH_%'")
            .await,
        4
    );
    assert!(h.sql("DELETE FROM messaging_operator_audit").await.is_err());
    let list = operations::switches(h.conn()).await.unwrap();
    assert!(list
        .iter()
        .any(|s| s.switch_name == "CONVERSATION_CREATE" && s.scope_key == "GLOBAL" && s.engaged));
}

#[tokio::test]
async fn workers_pause_under_their_switch_and_dead_letters_raise_incidents() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let now = chrono::Utc::now();
    let c = h.first_party(false);
    let (conv, people) = h.managed_group(&c, 2).await;
    h.send_managed(&c, &conv, people[0], "x").await;
    let queued = h
        .scalar("SELECT count(*) FROM messaging_webhook_relay WHERE status = 'PENDING'")
        .await;
    assert!(queued >= 2);
    // Paused: nothing relayed, nothing lost, no attempt counted.
    operations::set_switch(
        h.conn(),
        KillSwitch::WebhookRelay,
        None,
        true,
        "CALLING_INCIDENT",
        "ops-1",
        now,
    )
    .await
    .unwrap();
    let r = webhook_relay::relay_due(&h.state, &Accepting, 50)
        .await
        .unwrap();
    assert_eq!((r.relayed, r.paused as i64), (0, queued));
    assert_eq!(h.scalar("SELECT count(*) FROM messaging_webhook_relay WHERE status = 'PENDING' AND attempts = 0").await, queued);
    operations::set_switch(
        h.conn(),
        KillSwitch::WebhookRelay,
        None,
        false,
        "RESOLVED",
        "ops-1",
        now,
    )
    .await
    .unwrap();
    h.sql("UPDATE messaging_webhook_relay SET next_attempt_at = now() - interval '1 hour'")
        .await
        .unwrap();
    // A permanent refusal dead-letters and raises one de-duplicated incident per event.
    let r = webhook_relay::relay_due(&h.state, &Refusing, 50)
        .await
        .unwrap();
    assert_eq!(r.dead_lettered, 1);
    assert!(
        operations::open(h.conn(), IncidentCategory::WebhookRelayDeadLetter)
            .await
            .unwrap()
    );
    let incidents = operations::incidents(h.conn(), false).await.unwrap();
    assert_eq!(incidents.len(), 1);
    let text = serde_json::to_string(&incidents).unwrap();
    assert!(!text.contains("evt_"), "fixed safe summaries only: {text}");
    // Resolution names its operator and is audited once.
    assert!(operations::resolve(h.conn(), incidents[0].id, "", now)
        .await
        .is_err());
    assert!(operations::resolve(h.conn(), incidents[0].id, "ops-3", now)
        .await
        .unwrap());
    assert!(
        !operations::resolve(h.conn(), incidents[0].id, "ops-3", now)
            .await
            .unwrap()
    );
    assert_eq!(
        h.scalar(
            "SELECT count(*) FROM messaging_operator_audit WHERE action = 'INCIDENT_RESOLVED'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn a_rolled_back_or_forked_transparency_log_is_detected_and_appends_fail_closed() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let now = chrono::Utc::now();
    let c = h.customer(ALL_SCOPES);
    // Real devices: every registration and KeyPackage publication appends to the log.
    let _a = h.member(&c, 1).await;
    let _b = h.member(&c, 1).await;
    let dir = std::env::temp_dir().join(format!("kt-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("kt-checkpoint.json");
    // First pass writes the checkpoint; later passes verify and advance it.
    assert!(matches!(
        operations::kt_monitor_once(h.conn(), now, &path)
            .await
            .unwrap(),
        KtVerdict::Consistent { .. }
    ));
    let first = operations::read_checkpoint(&path).unwrap().unwrap();
    assert!(first.tree_size >= 2);
    let _c = h.member(&c, 1).await;
    assert!(matches!(
        operations::kt_monitor_once(h.conn(), now, &path)
            .await
            .unwrap(),
        KtVerdict::Consistent { .. }
    ));
    let second = operations::read_checkpoint(&path).unwrap().unwrap();
    assert!(second.tree_size > first.tree_size);

    // A forked history: same size, different root.
    let forked = KtCheckpoint {
        tree_size: first.tree_size,
        root_hash: "00".repeat(32),
    };
    assert_eq!(
        operations::kt_verify(h.conn(), &forked, now).await.unwrap(),
        KtVerdict::Forked {
            checkpoint_size: first.tree_size
        }
    );
    // A database restored from an older backup: behind the checkpoint the auditors saw.
    let ahead = KtCheckpoint {
        tree_size: second.tree_size + 5,
        root_hash: second.root_hash.clone(),
    };
    operations::write_checkpoint(&path, &ahead).unwrap();
    assert!(matches!(
        operations::kt_monitor_once(h.conn(), now, &path).await.unwrap(),
        KtVerdict::Behind { checkpoint_size, .. } if checkpoint_size == second.tree_size + 5
    ));
    // The checkpoint is never advanced over a rollback.
    assert_eq!(operations::read_checkpoint(&path).unwrap().unwrap(), ahead);
    // One CRITICAL incident; appends now fail closed, reads keep working.
    assert!(
        operations::open(h.conn(), IncidentCategory::KtRollbackDetected)
            .await
            .unwrap()
    );
    assert_eq!(h.scalar("SELECT count(*) FROM messaging_incidents WHERE category = 'KT_ROLLBACK_DETECTED' AND severity = 'CRITICAL'").await, 1);
    let (_, r) = h.register_device(&c, h.principal(&c)).await;
    assert_eq!(
        (r.status, r.reason()),
        (StatusCode::SERVICE_UNAVAILABLE, "KT_LOG_ROLLBACK_DETECTED"),
        "{}",
        r.text
    );
}

#[tokio::test]
async fn the_operator_cli_changes_switches_only_with_a_named_operator() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let bin = env!("CARGO_BIN_EXE_messaging-admin");
    let run = |args: &[&str], operator: Option<&str>| {
        let mut cmd = std::process::Command::new(bin);
        cmd.args(args)
            .env("DATABASE_URL", &h.db.url)
            .env_remove("CF_OPERATOR_ID");
        if let Some(o) = operator {
            cmd.env("CF_OPERATOR_ID", o);
        }
        cmd.output().unwrap()
    };
    let o = run(
        &[
            "kill-switch",
            "engage",
            "--switch",
            "SMS_BRIDGE",
            "--reason",
            "CARRIER_OUTAGE",
        ],
        None,
    );
    assert_eq!(o.status.code(), Some(2), "no operator, nothing changed");
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_kill_switches")
            .await,
        0
    );
    let o = run(
        &[
            "kill-switch",
            "engage",
            "--switch",
            "NOT_A_SWITCH",
            "--reason",
            "X_Y",
        ],
        Some("ops-9"),
    );
    assert_eq!(o.status.code(), Some(2));
    let o = run(
        &[
            "kill-switch",
            "engage",
            "--switch",
            "SMS_BRIDGE",
            "--reason",
            "CARRIER_OUTAGE",
        ],
        Some("ops-9"),
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = run(&["kill-switch", "list"], None);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        out.contains("\"SMS_BRIDGE\"") && out.contains("\"engaged\":true"),
        "{out}"
    );
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_operator_audit WHERE operator_id = 'ops-9'")
            .await,
        1
    );
    // KT checkpoint round trip.
    let dir = std::env::temp_dir().join(format!("kt-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("cp.json");
    let o = run(&["kt", "checkpoint", "--out", file.to_str().unwrap()], None);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = run(
        &["kt", "verify", "--checkpoint", file.to_str().unwrap()],
        None,
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stdout));
}

/// §134: each capability has its own switch. Engaging one refuses exactly that capability
/// (FEATURE_DISABLED / KILL_SWITCH_ENGAGED), leaves the others working, and never turns
/// one security profile into the other.
#[tokio::test]
async fn every_capability_has_an_independent_kill_switch() {
    let store = std::sync::Arc::new(cf_object_storage::MemoryObjectStore::new());
    let Some(h) = Harness::build(move |s| s.storage = store).await else {
        return;
    };
    let now = chrono::Utc::now();
    let c = h.customer(ALL_SCOPES);
    let (conv, people) = h.managed_group(&c, 2).await;
    h.set_policy(
        &c,
        json!({ "contexts": { "GROUP": { "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED", "exportsEnabled": true } } }),
    )
    .await;
    let cid = conv["conversationId"].as_str().unwrap().to_string();
    let env = c.context.scope.environment_id;
    let sent = h
        .send_managed(&c, &conv, people[0], "before the switches")
        .await;
    let mid = sent.json["messageId"].as_str().unwrap().to_string();
    let device = h.active_device(&c, people[0]).await.device_id();
    let refused =
        |r: &Response| r.code() == "FEATURE_DISABLED" && r.reason() == "KILL_SWITCH_ENGAGED";
    let engage = |s: KillSwitch, on: bool| {
        let h = &h;
        async move {
            assert!(
                operations::set_switch(h.conn(), s, Some(env), on, "INCIDENT", "ops-1", now)
                    .await
                    .unwrap()
            );
        }
    };

    // E2EE conversation creation: refused, MANAGED creation unaffected, never a fallback.
    engage(KillSwitch::E2eeConversationCreate, true).await;
    let r = h
        .create_conversation(
            &c,
            people[0],
            json!({ "contextClass": "GROUP", "securityProfile": "PRIVATE_E2EE" }),
        )
        .await;
    assert!(refused(&r), "{}", r.text);
    let m = h
        .create_conversation(
            &c,
            people[0],
            json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" }),
        )
        .await;
    assert_eq!(m.status, StatusCode::CREATED, "{}", m.text);
    assert_eq!(
        h.scalar(
            "SELECT count(*) FROM messaging_conversations WHERE security_profile = 'PRIVATE_E2EE'"
        )
        .await,
        0
    );
    engage(KillSwitch::E2eeConversationCreate, false).await;

    // MANAGED messaging: no new MANAGED conversation, send or edit.
    engage(KillSwitch::ManagedMessaging, true).await;
    assert!(refused(
        &h.send_managed(&c, &conv, people[0], "refused").await
    ));
    assert!(refused(
        &h.patch(
            &c,
            Some(people[0]),
            &format!("/v1/conversations/{cid}/messages/{mid}"),
            json!({ "text": "x" })
        )
        .await
    ));
    assert!(refused(
        &h.create_conversation(
            &c,
            people[0],
            json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" })
        )
        .await
    ));
    // Reading still works.
    assert_eq!(
        h.get(
            &c,
            Some(people[1]),
            &format!("/v1/conversations/{cid}/messages")
        )
        .await
        .status,
        StatusCode::OK
    );
    engage(KillSwitch::ManagedMessaging, false).await;

    // Search, exports, moderation actions, key-transparency proofs.
    for switch in [
        KillSwitch::Search,
        KillSwitch::Exports,
        KillSwitch::ModerationActions,
        KillSwitch::KtProofs,
    ] {
        let call = || async {
            match switch {
                KillSwitch::Search => h.get(&c, Some(people[1]), &format!("/v1/conversations/{cid}/search?q=before")).await,
                KillSwitch::Exports => h.post(&c, None, "/v1/messaging/exports", json!({ "conversationId": cid })).await,
                KillSwitch::ModerationActions => {
                    h.post(&c, None, "/v1/messaging/moderation/actions",
                           json!({ "kind": "RESTRICT_PRINCIPAL", "principalId": people[1], "reason": "SPAM_WAVE",
                                   "durationSeconds": 600, "conversationId": cid }))
                        .await
                }
                _ => h.get(&c, Some(people[0]), &format!("/v1/messaging/key-transparency/proofs?deviceId={device}")).await,
            }
        };
        engage(switch, true).await;
        let r = call().await;
        assert!(refused(&r), "{}: {}", switch.as_str(), r.text);
        // Every OTHER capability keeps working while this one is off.
        assert_eq!(
            h.send_managed(&c, &conv, people[0], "still sending")
                .await
                .status,
            StatusCode::CREATED
        );
        engage(switch, false).await;
        let r = call().await;
        assert!(
            !refused(&r) && r.status.is_success(),
            "{} after release: {}",
            switch.as_str(),
            r.text
        );
    }
    assert_eq!(KillSwitch::ALL.len(), 14);
    for s in KillSwitch::ALL {
        assert_eq!(KillSwitch::parse(s.as_str()), Some(s));
    }
}
